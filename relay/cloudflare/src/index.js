// pokertui relay: a Durable Object per table (message fan-out) plus the global
// leaderboard over one more websocket endpoint. Nothing here can read a game:
// every packet is encrypted with a key derived from the room code.

import { handleFrame } from "./leaderboard.js";
import { RoomCore } from "./room.js";

export class Room {
  constructor(ctx, env) {
    this.ctx = ctx;
    this.core = new RoomCore({ storage: ctx.storage, sockets: () => ctx.getWebSockets() });
  }
  async fetch() {
    const [client, server] = Object.values(new WebSocketPair());
    this.ctx.acceptWebSocket(server); // hibernation: idle rooms cost nothing
    return new Response(null, { status: 101, webSocket: client });
  }
  async webSocketMessage(ws, message) { await this.core.onMessage(ws, message); }
  async webSocketClose(ws) { try { ws.close(); } catch { /* already closed */ } }
  async alarm() { await this.core.onAlarm(); }
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const upgrade = request.headers.get("Upgrade") === "websocket";
    const room = url.pathname.match(/^\/room\/([0-9a-f]{24})$/);
    if (room && upgrade) {
      return env.ROOM.get(env.ROOM.idFromName(room[1])).fetch(request);
    }
    if (url.pathname === "/lb" && upgrade) {
      const [client, server] = Object.values(new WebSocketPair());
      server.accept();
      // Cloudflare tells the Worker where this connection came from. Only the state
      // name is ever stored, and only if the player opted in; never the IP.
      const where = { region: request.cf?.region, country: request.cf?.country };
      let frames = 0;
      server.addEventListener("message", async (e) => {
        if (++frames > 20) return server.close(1008, "too many frames");
        let frame;
        try { frame = JSON.parse(e.data); } catch { return; }
        const reply = await handleFrame(env.DB, frame, {
          ...where, now: Date.now(), sealSecret: env.SEAL_SECRET, sealPublic: env.SEAL_PUBLIC,
        }).catch((err) => ({ ok: false, error: String(err.message ?? err) }));
        server.send(JSON.stringify({ id: frame.id, ...reply }));
      });
      return new Response(null, { status: 101, webSocket: client });
    }
    return new Response("pokertui relay\n", { status: 200 });
  },
};
