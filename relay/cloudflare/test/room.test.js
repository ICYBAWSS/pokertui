import test from "node:test";
import assert from "node:assert/strict";
import { RoomCore, MAX_FRAME } from "../src/room.js";

function fakeSocket() {
  let attachment = null;
  const sent = [];
  return { sent, send: (m) => sent.push(JSON.parse(m)), serializeAttachment: (v) => (attachment = v), deserializeAttachment: () => attachment };
}
function fakeRoom(now = () => 0) {
  const store = new Map(), sockets = [];
  const storage = { get: async (k) => store.get(k), put: async (k, v) => store.set(k, v), delete: async (k) => store.delete(k), deleteAll: async () => store.clear(), setAlarm: async () => {} };
  return { core: new RoomCore({ storage, sockets: () => sockets, now }), sockets, store };
}
const frame = (o) => JSON.stringify(o);

test("publishes reach only subscribers of that topic", async () => {
  const { core, sockets } = fakeRoom();
  const [a, b, c] = [fakeSocket(), fakeSocket(), fakeSocket()];
  sockets.push(a, b, c);
  await core.onMessage(a, frame({ op: "sub", topic: "t/events" }));
  await core.onMessage(b, frame({ op: "sub", topic: "t/events" }));
  await core.onMessage(c, frame({ op: "sub", topic: "t/other" }));
  await core.onMessage(c, frame({ op: "pub", topic: "t/events", data: "aGk", retain: false }));
  assert.deepEqual(a.sent, [{ op: "msg", topic: "t/events", data: "aGk", retain: false }]);
  assert.equal(b.sent.length, 1);
  assert.equal(c.sent.length, 0);
});

test("retained messages are replayed to late subscribers, and an empty one clears them", async () => {
  const { core, sockets } = fakeRoom();
  const [pub, late, later] = [fakeSocket(), fakeSocket(), fakeSocket()];
  sockets.push(pub, late, later);
  await core.onMessage(pub, frame({ op: "pub", topic: "t/snapshot", data: "c25hcA", retain: true }));
  await core.onMessage(late, frame({ op: "sub", topic: "t/snapshot" }));
  assert.deepEqual(late.sent, [{ op: "msg", topic: "t/snapshot", data: "c25hcA", retain: true }]);
  await core.onMessage(pub, frame({ op: "pub", topic: "t/snapshot", data: "", retain: true }));
  await core.onMessage(later, frame({ op: "sub", topic: "t/snapshot" }));
  assert.deepEqual(later.sent, [], "cleared snapshot is gone");
});

test("oversized frames, bad JSON and floods are dropped", async () => {
  let t = 0;
  const { core, sockets } = fakeRoom(() => t);
  const a = fakeSocket();
  sockets.push(a);
  await core.onMessage(a, frame({ op: "sub", topic: "x" }));
  await core.onMessage(a, "{nope");
  await core.onMessage(a, frame({ op: "pub", topic: "x", data: "A".repeat(MAX_FRAME), retain: false }));
  assert.equal(a.sent.length, 0);
  for (let i = 0; i < 1000; i++) await core.onMessage(a, frame({ op: "pub", topic: "x", data: "aGk", retain: false }));
  assert.ok(a.sent.length <= 301, `burst limit held: delivered ${a.sent.length}`);
  const before = a.sent.length;
  t += 1000;
  await core.onMessage(a, frame({ op: "pub", topic: "x", data: "aGk", retain: false }));
  assert.equal(a.sent.length, before + 1, "tokens refill over time");
});

test("an idle room forgets its retained messages", async () => {
  const { core, sockets, store } = fakeRoom();
  const a = fakeSocket();
  sockets.push(a);
  await core.onMessage(a, frame({ op: "pub", topic: "t", data: "eA", retain: true }));
  assert.equal(store.size, 1);
  await core.onAlarm();
  assert.equal(store.size, 1, "someone is still connected");
  sockets.length = 0;
  await core.onAlarm();
  assert.equal(store.size, 0);
});
