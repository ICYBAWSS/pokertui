# pokertui relay (optional)

A tiny Cloudflare Worker: one Durable Object per table passes encrypted packets between
players. It can't read a game: packets are encrypted with a key derived from the room code.
Useful when a network blocks the public MQTT brokers' unusual ports; Cloudflare uses 443.
The game works over the public brokers without it.

## Deploy

```bash
cd relay/cloudflare
npm i -g wrangler && wrangler login
wrangler deploy
```

## Use it

```bash
poker --broker cf+wss://pokertui-relay.YOU.workers.dev host
```

## Tests

```bash
npm test      # Node 22
```
