// One table's relay: topic subscriptions, retained messages, fan-out.
// Pure logic over { storage, sockets() } so it can be tested without Cloudflare.

export const MAX_FRAME = 64 * 1024;
const MAX_TOPICS = 16;
const IDLE_WIPE_MS = 24 * 3600 * 1000;

export class RoomCore {
  constructor({ storage, sockets, now = () => Date.now() }) {
    this.storage = storage;
    this.sockets = sockets;
    this.now = now;
    this.buckets = new WeakMap();
  }

  allow(ws) {
    // Token bucket: 100 frames/s sustained, bursts of 300. Protects the free quota.
    const t = this.now();
    const b = this.buckets.get(ws) ?? { tokens: 300, at: t };
    b.tokens = Math.min(300, b.tokens + ((t - b.at) / 1000) * 100);
    b.at = t;
    this.buckets.set(ws, b);
    if (b.tokens < 1) return false;
    b.tokens -= 1;
    return true;
  }

  async onMessage(ws, raw) {
    if (typeof raw !== "string" || raw.length > MAX_FRAME || !this.allow(ws)) return;
    let f;
    try { f = JSON.parse(raw); } catch { return; }
    if (typeof f.topic !== "string" || f.topic.length > 200) return;
    if (f.op === "sub") {
      const topics = this.topicsOf(ws);
      if (!topics.includes(f.topic) && topics.length < MAX_TOPICS) {
        topics.push(f.topic);
        ws.serializeAttachment(topics);
      }
      const kept = await this.storage.get("r:" + f.topic);
      if (kept) ws.send(JSON.stringify({ op: "msg", topic: f.topic, data: kept, retain: true }));
    } else if (f.op === "pub" && typeof f.data === "string") {
      if (f.retain) {
        if (f.data === "") await this.storage.delete("r:" + f.topic);
        else await this.storage.put("r:" + f.topic, f.data);
      }
      const out = JSON.stringify({ op: "msg", topic: f.topic, data: f.data, retain: false });
      for (const s of this.sockets()) {
        if (this.topicsOf(s).includes(f.topic)) {
          try { s.send(out); } catch { /* socket closing */ }
        }
      }
      await this.storage.setAlarm?.(this.now() + IDLE_WIPE_MS);
    }
  }

  topicsOf(ws) {
    try { return ws.deserializeAttachment() ?? []; } catch { return []; }
  }

  /** Alarm: an idle room forgets its retained messages. */
  async onAlarm() {
    if (this.sockets().length === 0) await this.storage.deleteAll();
  }
}
