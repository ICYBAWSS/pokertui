// pokertui relay: a Durable Object per table (message fan-out). Nothing here can
// read a game: every packet is encrypted with a key derived from the room code.

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
    return new Response("pokertui relay\n", { status: 200 });
  },
};
