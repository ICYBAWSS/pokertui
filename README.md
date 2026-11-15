<h1 align="center">pokertui</h1>

<p align="center"><b>Terminal Texas Hold'em. No server. Just an 8-character room code.</b></p>

<p align="center">
  <img alt="Rust" src="https://img.shields.io/badge/rust-2024-orange">
  <img alt="no server" src="https://img.shields.io/badge/backend-none-brightgreen">
  <img alt="tests" src="https://img.shields.io/badge/tests-42%20unit%20%2B%201%20live-blue">
</p>

`pokertui` is a no-limit Hold'em table that lives in your terminal. One player
hosts, everyone else joins with a code like `K7Q2-9XZP`. There is no backend to
deploy: packets travel through a public MQTT-over-WebSocket broker, the host's
client is the dealer, and every packet is signed.

```
npm? no. account? no. port forwarding? no.   ──  a room is a topic
```

## Why this works (and where it doesn't)

A public pub/sub relay is a dumb pipe, and poker needs an authority and hidden
information. `pokertui` solves that in the client:

- **Host-authoritative table.** The host shuffles, deals, validates every action
  and broadcasts a signed `TableView` snapshot with a monotonically increasing
  version. Clients apply only newer versions.
- **Signed packets.** Every message is an ed25519-signed envelope covering room,
  sender, sequence, timestamp and payload. A relay can drop or duplicate packets,
  but it cannot forge or rewrite them.
- **Sealed hole cards.** Hole cards never appear in public state. The host sends
  them encrypted (X25519 ECDH + ChaCha20-Poly1305) to each player's key, on a
  per-player topic. Knowing the room code lets you *watch* public state, not read
  anyone's cards.
- **Deck commitment.** Before dealing, the host publishes
  `sha256("pokertui-deck-v1" ‖ nonce ‖ deck)`. After the hand it reveals the nonce
  and full deck. `poker verify hand.json` proves the deck was not changed mid-hand.

Honest limits: the host can stack the deck *before* committing (that needs mental
poker to fix), there is no host migration, and public brokers are best-effort and
not meant for production traffic. Play with people you trust: the host deals.

## Install

```bash
cargo install --git https://github.com/ICYBAWSS/pokertui --locked
poker
```

That's it: `poker` opens the menu. **Host** picks a game type, **Join** asks for the
code your friend shares.

| game type | blinds | stacks | seats | clock |
|---|---|---|---|---|
| Quick Game | 25/50 | 5,000 | 6 | 30 s |
| High Roller | 500/1,000 | 100,000 | 6 | 45 s |
| Turbo | 50/100 | 3,000 | 6 | 15 s |
| Heads-Up | 50/100 | 5,000 | 2 | 30 s |
| Custom | you choose starting stack, both blinds, seats (2–9), timer, auto top-up | | | |

Press `E` on any preset to tweak it first; your last custom settings are remembered.

## Quick start (from a clone)

```bash
cargo build --release
./target/release/poker              # menu: splash → name → host/join → table

# or skip the menu
./target/release/poker host  --name alice --blinds 25/50 --buyin 5000
./target/release/poker join  K7Q2-9XZP --name bob
./target/release/poker spectate K7Q2-9XZP

# hand history: ~/Library/Application Support/dev.pokertui.pokertui/history (macOS)
#               ~/.local/share/pokertui/history (Linux)
./target/release/poker verify <history-dir>/<room>/hand-0001.json
./target/release/poker replay  <same file>
./target/release/poker art                  # print the embedded lobby splash
```

Terminal size: 60×18 minimum, 100×30+ looks best. `--no-art` disables the splash,
`--plain` gives a line-based mode for dumb terminals.

### Controls

Menus: arrow keys + Enter, or press the number shown. At the table:

| key | action |
|---|---|
| `1` | fold |
| `2` | check, or call the bet |
| `3` | raise box: `M` min · `H` ½ pot · `P` pot · `A` max · ←→ ± big blind · or type an amount, then Enter |
| `4` | all-in |
| `Enter` | deal the next hand (host) |
| `t` | chat |
| `?` | help |
| `Esc` | leave the table (back to the menu; the host leaving closes the table) |
| `^C` | quit |

Letter keys `f` / `c` / `x` / `r` / `a` also work. TUI logs go to `poker.log` in the
same app-data folder as the hand history.

## How it works

```
alice (host, authoritative)                      bob (client)
┌───────────────────────────────┐          ┌───────────────────────────────┐
│ ratatui UI   ▲ std mpsc       │          │ ratatui UI   ▲ std mpsc       │
│ game::Table  (pure rules)     │          │ client mirror (verify +       │
│ net::host  (tokio select)     │          │ dedupe by version)            │
└───────────────┬───────────────┘          └───────────────┬───────────────┘
                └──────── wss://broker.emqx.io:8084 ───────┘
```

Topics for a room `<hex>`:

```
pokertui/v1/<hex>/events                 signed envelopes, QoS 1
pokertui/v1/<hex>/snapshot               retained signed snapshot (late joiners)
pokertui/v1/<hex>/secrets/<player-key>   sealed hole cards
```

The deck commitment, side pots, odd-chip rule, min-raise reopening and heads-up
blind order are covered by unit tests plus a 6,250-hand randomized simulation that
asserts chip conservation after **every** action.

## Relays and ports

| broker | endpoint | notes |
|---|---|---|
| EMQX | `wss://broker.emqx.io:8084/mqtt` | default, fastest here |
| Mosquitto | `wss://test.mosquitto.org:8081/mqtt` | public test server |
| HiveMQ | `wss://broker.hivemq.com:8884/mqtt` | works, slow CONNACK |

**None of these listen on port 443**, so a corporate/hotel network that only
allows 443 will block them. `--broker` accepts any WSS endpoint; a self-hosted
Mosquitto/EMQX behind Cloudflare Tunnel or Tailscale Funnel on 443 is the escape
hatch. ntfy.sh is *not* usable for gameplay (250 messages/day per IP).

## Testing from a Raspberry Pi (or any new network)

No pip needed — the probe speaks WebSocket + MQTT with the Python standard library:

```bash
python3 scripts/pi_probe.py            # all public brokers, RTT, retained delivery
python3 scripts/pi_probe.py --broker broker.emqx.io:8084/mqtt --messages 10
```

Then build and play:

```bash
sudo apt update && sudo apt install -y build-essential pkg-config
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
git clone https://github.com/ICYBAWSS/pokertui.git && cd pokertui
cargo build --release            # Pi 3: prefer `cargo build --release -j2`
./target/release/poker           # host on the Pi, join from the laptop (or reverse)
```

Cross-machine smoke test: host on one box, join from the other, play one hand.
A full 3-player hand over EMQX takes ≈5 s including joins.

## Development

```bash
cargo test                                        # 42 unit tests
cargo test --test net_live -- --ignored --nocapture   # real broker, host + 2 clients
cargo run --example probe                         # transport gate only
cargo fmt && cargo clippy --all-targets
```

```
src/game/     pure rules: cards, 7-card eval, betting, pots, history
src/net/      protocol (sign/verify), room codes, MQTT link, host, client
src/crypto.rs ed25519 identity, sealed boxes, deck commitments
src/ui.rs     table, seats, cards, action bar, log
src/menu.rs   splash / name / host-join / settings screens
src/art.rs    embedded ASCII art (no runtime Node dependency)
scripts/      pi_probe.py, gen-art.sh
```

ASCII art stills are generated offline with
[`ascii-magic-art`](https://www.ascii-magic.com/ascii) (`npx ascii-magic-art … --frame`),
cropped and embedded with `include_str!`. Regenerate with `scripts/gen-art.sh`.

## Roadmap

- [ ] host migration / reclaimable seats
- [ ] session key persistence per room (reconnect as the same player)
- [ ] host kick / ban
- [ ] offline fake-relay integration test
- [ ] optional Cloudflare Durable Objects relay (free tier: 100k req/day)
- [ ] mental-poker shuffles, tournaments, delayed spectating

## License

MIT — see [LICENSE](LICENSE).
