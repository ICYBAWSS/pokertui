#!/usr/bin/env python3
"""Zero-dependency MQTT-over-WebSocket relay probe.

Answers the only question that matters before shipping pokertui to a new
network: *can this machine reach a public MQTT broker over TLS WebSockets,
publish, subscribe and read a retained message?*

It speaks raw WebSocket (RFC 6455) and MQTT 3.1.1 with the standard library
only, so it runs on a stock Raspberry Pi OS without pip.

    python3 scripts/pi_probe.py
    python3 scripts/pi_probe.py --messages 10
    python3 scripts/pi_probe.py --broker broker.emqx.io:8084/mqtt
"""

import argparse
import base64
import hashlib
import json
import os
import socket
import ssl
import struct
import sys
import time

DEFAULT_BROKERS = [
    "broker.emqx.io:8084/mqtt",
    "test.mosquitto.org:8081/mqtt",
    "broker.hivemq.com:8884/mqtt",
]

WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


# --------------------------------------------------------------------------- #
# WebSocket
# --------------------------------------------------------------------------- #
class WebSocket:
    def __init__(self, sock, leftover=b""):
        self.sock = sock
        self.buf = bytearray(leftover)

    def _read_exact(self, n, deadline):
        while len(self.buf) < n:
            remaining = deadline - time.time()
            if remaining <= 0:
                raise TimeoutError("timed out reading websocket frame")
            self.sock.settimeout(remaining)
            chunk = self.sock.recv(65536)
            if not chunk:
                raise ConnectionError("websocket closed by peer")
            self.buf.extend(chunk)
        out = bytes(self.buf[:n])
        del self.buf[:n]
        return out

    def send(self, data, opcode=0x2):
        header = bytearray([0x80 | opcode])
        n = len(data)
        if n < 126:
            header.append(0x80 | n)
        elif n < 65536:
            header.append(0x80 | 126)
            header += struct.pack("!H", n)
        else:
            header.append(0x80 | 127)
            header += struct.pack("!Q", n)
        mask = os.urandom(4)
        header += mask
        masked = bytes(b ^ mask[i % 4] for i, b in enumerate(data))
        self.sock.sendall(bytes(header) + masked)

    def recv(self, deadline):
        while True:
            b1, b2 = self._read_exact(2, deadline)
            opcode = b1 & 0x0F
            masked = b2 & 0x80
            length = b2 & 0x7F
            if length == 126:
                length = struct.unpack("!H", self._read_exact(2, deadline))[0]
            elif length == 127:
                length = struct.unpack("!Q", self._read_exact(8, deadline))[0]
            mask = self._read_exact(4, deadline) if masked else None
            payload = self._read_exact(length, deadline) if length else b""
            if mask:
                payload = bytes(c ^ mask[i % 4] for i, c in enumerate(payload))
            if opcode == 0x8:
                raise ConnectionError("websocket close frame")
            if opcode == 0x9:  # ping
                self.send(payload, 0xA)
                continue
            if opcode == 0xA:  # pong
                continue
            return payload


def ws_connect(host, port, path, timeout=10.0):
    sock = socket.create_connection((host, port), timeout=timeout)
    context = ssl.create_default_context()
    sock = context.wrap_socket(sock, server_hostname=host)
    key = base64.b64encode(os.urandom(16)).decode()
    request = (
        "GET {path} HTTP/1.1\r\n"
        "Host: {host}:{port}\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        "Sec-WebSocket-Key: {key}\r\n"
        "Sec-WebSocket-Version: 13\r\n"
        "Sec-WebSocket-Protocol: mqtt\r\n"
        "\r\n"
    ).format(path=path, host=host, port=port, key=key)
    sock.sendall(request.encode())

    raw = b""
    deadline = time.time() + timeout
    while b"\r\n\r\n" not in raw:
        sock.settimeout(max(0.1, deadline - time.time()))
        chunk = sock.recv(4096)
        if not chunk:
            raise ConnectionError("TLS connection closed during upgrade")
        raw += chunk
    head, _, leftover = raw.partition(b"\r\n\r\n")
    lines = head.decode("latin-1").split("\r\n")
    status = lines[0]
    headers = {}
    for line in lines[1:]:
        if ":" in line:
            name, value = line.split(":", 1)
            headers[name.strip().lower()] = value.strip()
    if "101" not in status:
        raise ConnectionError("websocket upgrade failed: %s" % status)
    expected = base64.b64encode(
        hashlib.sha1((key + WS_GUID).encode()).digest()
    ).decode()
    if headers.get("sec-websocket-accept") != expected:
        raise ConnectionError("bad Sec-WebSocket-Accept")
    if headers.get("sec-websocket-protocol", "mqtt").strip() != "mqtt":
        raise ConnectionError("broker did not negotiate the mqtt subprotocol")
    return WebSocket(sock, leftover)


# --------------------------------------------------------------------------- #
# MQTT 3.1.1
# --------------------------------------------------------------------------- #
def varint(n):
    out = bytearray()
    while True:
        digit = n % 128
        n //= 128
        out.append(digit | (0x80 if n else 0))
        if not n:
            return bytes(out)


def utf8(text):
    data = text.encode()
    return struct.pack("!H", len(data)) + data


def mqtt_packet(header, body):
    return bytes([header]) + varint(len(body)) + body


class Mqtt:
    def __init__(self, ws):
        self.ws = ws
        self.buf = bytearray()

    def send(self, packet):
        self.ws.send(packet)

    def read(self, timeout):
        """Return the next complete MQTT packet, or None on timeout."""
        deadline = time.time() + timeout
        while len(self.buf) < 2:
            remaining = deadline - time.time()
            if remaining <= 0:
                return None
            try:
                self.buf.extend(self.ws.recv(deadline))
            except TimeoutError:
                return None

        index, multiplier, length = 1, 1, 0
        while True:
            while index >= len(self.buf):
                remaining = deadline - time.time()
                if remaining <= 0:
                    return None
                self.buf.extend(self.ws.recv(deadline))
            digit = self.buf[index]
            length += (digit & 0x7F) * multiplier
            multiplier *= 128
            index += 1
            if not digit & 0x80:
                break
            if multiplier > 128 ** 3:
                raise ConnectionError("malformed MQTT remaining length")

        total = index + length
        while len(self.buf) < total:
            remaining = deadline - time.time()
            if remaining <= 0:
                return None
            self.buf.extend(self.ws.recv(deadline))
        packet = bytes(self.buf[:total])
        del self.buf[:total]
        return packet

    def connect(self, client_id):
        body = utf8("MQTT") + bytes([4, 0x02]) + struct.pack("!H", 30) + utf8(client_id)
        self.send(mqtt_packet(0x10, body))
        packet = self.read(10)
        if not packet or packet[0] >> 4 != 2:
            raise ConnectionError("no CONNACK")
        if packet[-1] != 0:
            raise ConnectionError("CONNACK refused, code %d" % packet[-1])

    def subscribe(self, topic, packet_id=1):
        body = struct.pack("!H", packet_id) + utf8(topic) + bytes([1])
        self.send(mqtt_packet(0x82, body))
        while True:
            packet = self.read(10)
            if packet is None:
                raise ConnectionError("no SUBACK")
            if packet[0] >> 4 == 9:  # SUBACK
                return
            if packet[0] >> 4 == 3:  # PUBLISH that raced ahead of the SUBACK
                continue

    def publish(self, topic, payload, qos=1, retain=False, packet_id=1):
        if isinstance(payload, str):
            payload = payload.encode()
        body = utf8(topic)
        if qos:
            body += struct.pack("!H", packet_id)
        body += payload
        header = 0x30 | (qos << 1) | (1 if retain else 0)
        self.send(mqtt_packet(header, body))

    def disconnect(self):
        try:
            self.send(b"\xe0\x00")
        except Exception:
            pass


def parse_publish(packet):
    """Return (topic, payload, retain, qos, packet_id) for a PUBLISH."""
    retain = bool(packet[0] & 0x01)
    qos = (packet[0] >> 1) & 0x03
    index = 1
    multiplier, length = 1, 0
    while True:
        digit = packet[index]
        length += (digit & 0x7F) * multiplier
        multiplier *= 128
        index += 1
        if not digit & 0x80:
            break
    topic_len = struct.unpack("!H", packet[index:index + 2])[0]
    index += 2
    topic = packet[index:index + topic_len].decode("utf-8", "replace")
    index += topic_len
    packet_id = None
    if qos:
        packet_id = struct.unpack("!H", packet[index:index + 2])[0]
        index += 2
    payload = packet[index:]
    return topic, payload, retain, qos, packet_id


def wait_for_publish(client, timeout=8.0, want_topic=None):
    deadline = time.time() + timeout
    while time.time() < deadline:
        packet = client.read(max(0.1, deadline - time.time()))
        if packet is None:
            continue
        kind = packet[0] >> 4
        if kind == 3:
            topic, payload, retain, qos, packet_id = parse_publish(packet)
            if qos == 1 and packet_id is not None:
                client.send(bytes([0x40, 0x02]) + struct.pack("!H", packet_id))
            if want_topic is None or topic == want_topic:
                return topic, payload, retain
    return None


# --------------------------------------------------------------------------- #
# Probe
# --------------------------------------------------------------------------- #
def parse_broker(text):
    hostpart, _, path = text.partition("/")
    host, _, port = hostpart.partition(":")
    return host, int(port or 8084), "/" + path if path else "/mqtt"


def probe(broker, messages):
    host, port, path = parse_broker(broker)
    tag = "%s:%d%s" % (host, port, path)
    print("== %s ==" % tag)
    print("  TLS + websocket upgrade ...", end=" ", flush=True)
    tls_start = time.time()
    sub_ws = ws_connect(host, port, path)
    print("ok (%.0f ms)" % ((time.time() - tls_start) * 1000))

    sub = Mqtt(sub_ws)
    sub_id = "pi-sub-%d" % os.getpid()
    sub.connect(sub_id)
    print("  MQTT CONNACK ............. ok")

    topic = "pokertui/pi-probe-%d/events" % os.getpid()
    sub.subscribe(topic)

    pub_ws = ws_connect(host, port, path)
    pub = Mqtt(pub_ws)
    pub.connect("pi-pub-%d" % os.getpid())

    rtts = []
    lost = 0
    for i in range(messages):
        payload = json.dumps({"seq": i, "probe": "pokertui"})
        start = time.time()
        pub.publish(topic, payload, qos=1, packet_id=(i % 60000) + 1)
        got = wait_for_publish(sub, timeout=5.0, want_topic=topic)
        if got is None:
            lost += 1
            print("  message %d ............. LOST after 5 s" % i)
            continue
        rtts.append((time.time() - start) * 1000)

    if rtts:
        print(
            "  pub/sub RTT .............. min %.0f / avg %.0f / max %.0f ms  (%d/%d)"
            % (min(rtts), sum(rtts) / len(rtts), max(rtts), len(rtts), messages)
        )
    else:
        print("  pub/sub RTT .............. no messages delivered")

    retained_ok = False
    try:
        marker = json.dumps({"retained": True, "pid": os.getpid()})
        pub.publish(topic, marker, qos=1, retain=True, packet_id=999)
        time.sleep(0.5)
        late_ws = ws_connect(host, port, path)
        late = Mqtt(late_ws)
        late.connect("pi-late-%d" % os.getpid())
        late.subscribe(topic)
        got = wait_for_publish(late, timeout=6.0, want_topic=topic)
        retained_ok = bool(got and got[2])
        late.disconnect()
        pub.publish(topic, b"", qos=0, retain=True)
    except Exception as exc:  # noqa: BLE001 - best-effort cleanup
        print("  retained check ........... skipped: %s" % exc)

    print("  retained message ......... %s" % ("ok" if retained_ok else "FAILED"))
    sub.disconnect()
    pub.disconnect()

    if lost:
        raise RuntimeError("%d/%d messages were not delivered" % (lost, messages))
    if not retained_ok:
        raise RuntimeError("retained delivery failed")
    print("  RESULT: usable for pokertui")
    print()


def main():
    parser = argparse.ArgumentParser(description="Probe public MQTT-over-WSS relays")
    parser.add_argument("--broker", action="append", help="host:port/path (repeatable)")
    parser.add_argument("--messages", type=int, default=5)
    args = parser.parse_args()

    print("python %s on %s" % (sys.version.split()[0], sys.platform))
    try:
        print("tls: %s" % ssl.OPENSSL_VERSION)
    except AttributeError:
        pass
    print()

    brokers = args.broker or DEFAULT_BROKERS
    failures = 0
    for broker in brokers:
        try:
            probe(broker, args.messages)
        except Exception as exc:  # noqa: BLE001 - one bad broker must not stop the rest
            failures += 1
            print("  FAILED: %s\n" % exc)

    if failures:
        print("%d/%d brokers failed" % (failures, len(brokers)))
        return 1
    print("all %d brokers reachable" % len(brokers))
    return 0


if __name__ == "__main__":
    sys.exit(main())
