# pokertui relay (optional)

A tiny Cloudflare Worker that does two jobs. Both are optional: the game works over public
MQTT brokers without it.

1. **Relay.** One Durable Object per table passes encrypted packets between players. It
   can't read a game: packets are encrypted with a key derived from the room code.
   Useful when a network blocks the brokers' unusual ports; Cloudflare uses 443.
2. **Global leaderboard.** Players co-sign every hand; the Worker accepts a hand only if
   *every* human player signed the same result, and only once. A D1 database keeps ratings.

## Deploy

```bash
cd relay/cloudflare
npm i -g wrangler && wrangler login
node scripts/gen-key.mjs                      # prints SEAL_SECRET and SEAL_PUBLIC
wrangler secret put SEAL_SECRET               # paste the secret
wrangler d1 create pokertui-leaderboard       # paste the database_id into wrangler.toml
wrangler d1 execute pokertui-leaderboard --remote --file schema.sql
# put SEAL_PUBLIC into wrangler.toml, then:
wrangler deploy
```

## Use it

```bash
poker --broker cf+wss://pokertui-relay.YOU.workers.dev host        # play through the relay
poker leaderboard --relay cf+wss://pokertui-relay.YOU.workers.dev --key SEAL_PUBLIC on
poker leaderboard show-state     # optional: show your state (guessed from your connection)
poker rankings --global
```

Everyone using the same relay shares one board. Check Cloudflare's current free-tier
limits before depending on this: when they run out, results queue on each player's machine
and upload after the next daily reset.

## Tests

```bash
npm test      # Node 22: relay logic, leaderboard rules, and fixtures made by the Rust client
```

`test/fixture.json` is produced by the Rust side (`cargo run --example lb_fixture`), so the
tests prove the Worker reads exactly what the game writes.
