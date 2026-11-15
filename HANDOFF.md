# HANDOFF — pokertui

Terminal Texas Hold'em with **no server of your own**: rooms are a topic on a public
MQTT-over-WebSocket broker. The host's client is the dealer, every packet is signed,
hole cards are sealed to each player's key, and each deck is committed before the
deal and revealed after the hand.

- **Location:** `/Users/rayhan/pokertui` (≈5,400 lines of Rust)
- **Status:** engine, crypto, transport, host/client loops and TUI are implemented and
  compile; 30 unit tests pass; a live 3-player hand completes over the public broker
  in ~5 s (EMQX) / ~3 s (Mosquitto). No known open protocol bugs; see §3 for the two
  that were fixed.
- **Rust:** 1.97.1, edition 2024.

---

## 1. Run it

```bash
cd ~/pokertui
cargo build --release            # binary: target/release/poker

# terminal 1 — host a table
./target/release/poker host --name alice --blinds 25/50 --buyin 5000

# terminal 2 — join with the 8-char code it prints (e.g. K7Q2-9XZP)
./target/release/poker join K7Q2-9XZP --name bob

# extras
./target/release/poker spectate K7Q2-9XZP
./target/release/poker art                      # print the embedded lobby splash
./target/release/poker verify ~/.local/share/pokertui/history/<room>/hand-0001.json
./target/release/poker replay  <same file>
./target/release/poker --broker wss://test.mosquitto.org:8081/mqtt host
```

Just run `poker`: splash → name (first run) → **Host** (pick Quick Game / High Roller /
Turbo / Heads-Up / Custom table settings) or **Join** (type the 8-char code). The
subcommands above still skip the menu.

Keys at the table: `1` fold · `2` check/call · `3` raise box (`M` min, `H` ½ pot, `P` pot,
`A` max, ←→ ±BB, or type an amount) · `4` all-in · `Enter` deal (host) · `t` chat ·
`?` help · `Esc` leave (back to the menu). TUI logs go to
`~/Library/Application Support/dev.pokertui.pokertui/poker.log`.

Tests:

```bash
cargo test                                             # 30 unit tests, all green
cargo test --test net_live -- --ignored --nocapture    # live host+2 clients (~5 s)
# RUST_LOG=pokertui=debug POKER_BROKER=wss://... for traced runs on another broker
cargo clippy --all-targets -- -D warnings              # not yet clean, see §7
cargo fmt
```

---

## 2. What is verified working

| Area | Evidence |
|---|---|
| MQTT over WSS to public brokers | `examples/probe.rs` — CONNECT/SUBSCRIBE/PUBLISH/retained round-trip against `broker.emqx.io:8084` |
| Cards, shuffle, commitment | `card.rs`/`crypto.rs` tests; 52-card permutation, wheel straights, kicker ordering |
| 7-card evaluator | ~10 category/ordering tests in `game/eval.rs` |
| Betting engine | heads-up & 3-handed blinds/order, folds, check-down, side pots, odd chips, min-raise rules, out-of-turn rejection |
| Chip conservation | 250 seeded × 25 hands = 6,250 randomized hands, invariant asserted after **every** action |
| Deck verification | `verify_record` passes on a real hand, fails on tampered deck/commit |
| Signed envelopes | sign/verify/tamper tests |
| Sealed hole cards | only the recipient key can open (`crypto` test) |
| Full network hand | `tests/net_live.rs` — host + 2 clients, 5-card board, pot awarded |
| TUI | draws; needs a manual look at 80×24 and 120×40 (no snapshot test yet) |

---

## 3. Slow-hand bugs (fixed)

The live hand used to take ~100 s. Two independent causes:

1. **Host never learned its own seat.** `Host::bootstrap` didn't emit
   `UiEvent::Joined { seat }`, so the host UI had `my_seat = None`, never offered moves,
   and every host turn ended on the 20 s turn timer. Fixed in `src/net/host.rs`
   (`bootstrap`). ~100 s → ~23 s.
2. **broker.emqx.io silently drops bursts.** Packet tracing showed the host publishing
   8 QoS 1 messages in <1 ms at the deal; the broker PubAcked all 8 but never delivered
   some of them (the v5 state and one player's sealed cards) to connected, acking
   subscribers. HiveMQ/Mosquitto didn't drop. Fixed in the protocol, not by pacing:
   `Ping` now carries the client's `version` and `cards_hand`; the host resends state
   when a ping is behind its last broadcast (≤1/s, so pings racing an in-flight update
   are ignored) and re-seals cards to a seated player missing them. Lost packets on
   any broker now heal in ~1 s. ~23 s → ~5 s.

Guard test: `net::protocol::tests::every_table_view_survives_envelope_round_trip`
(`verify` re-serializes the payload, so a non-round-tripping view would be dropped
silently — ruled out, kept as a regression check).

Still true and worth knowing: every state change publishes **twice** (events + retained
snapshot). Clients receive the snapshot topic live too, so the events copy could be
dropped to halve traffic — not needed now that loss self-heals.

---

## 4. Architecture

```
alice (host, authoritative)                       bob (client)
┌──────────────────────────────┐            ┌──────────────────────────────┐
│ ratatui UI (main thread)     │            │ ratatui UI (main thread)     │
│   ▲ std mpsc (UiEvent)       │            │   ▲ std mpsc (UiEvent)       │
│ pure game::Table             │            │ client mirror (verify,       │
│   │ effects                 │            │ dedupe by version)           │
│ net::host (tokio select)     │            │ net::client                  │
└──────────────┬───────────────┘            └──────────────┬───────────────┘
               └──── wss://broker.emqx.io:8084/mqtt ───────┘
```

**Topics** (`src/net/protocol.rs`):

```
pokertui/v1/<room-hex>/events                 signed envelopes, QoS 1
pokertui/v1/<room-hex>/snapshot               retained signed TableView (late joiners)
pokertui/v1/<room-hex>/secrets/<player-pub>   sealed hole cards, QoS 1
```

**Room id:** 5 random bytes → 8 Crockford base32 chars (`K7Q2-9XZP`). The code is the
capability; the topic is derived from the id, so there is no directory to scan.

**Message flow**

1. Client publishes `JoinRequest{name,buyin,enc_pub}` to `events`.
2. Host validates, seats them, publishes a broadcast `JoinAccept{to,seat,params,view}`
   plus a full `State{view}` (also retained on `snapshot`).
3. Host `Start` → `Table::start_hand` shuffles, commits
   `sha256("pokertui-deck-v1" ‖ nonce16 ‖ deck52)`, deals, seals each player's two
   cards to their X25519 key, broadcasts state.
4. Turns: host sets `deadline_ms`, broadcasts; client sends
   `Action{hand, version, mv}`; host rejects stale versions and resends state.
5. Showdown: side pots, odd chip to first winner left of the button, result embedded in
   `State.last_result` together with the full deck reveal + nonce.
6. Host persists `~/.local/share/pokertui/history/<room>/hand-NNNN.json`; `poker verify`
   re-checks the commitment, the 52-card permutation, and that every shown card is in
   the committed deck.

**Trust model (intentional):** the host can stack the deck *before* committing — that is
undetectable without mental poker. It cannot swap cards *during* a hand, because the
reveal must hash to the pre-hand commitment. Hole cards are sealed per player, so even
someone who knows the room code and subscribes to `secrets/…` cannot read another
player's cards. There is no host migration; if the host dies the table freezes.

---

## 5. File map

```
Cargo.toml                ratatui 0.30, rumqttc 0.25 (websocket), tokio, clap 4.6,
                          ed25519-dalek 3, x25519-dalek 3, chacha20poly1305 0.11,
                          hkdf 0.13, sha2 0.11, rand_chacha 0.10, base64, directories 6
src/main.rs               clap CLI, runtime wiring, TUI loop, plain mode, verify/replay
src/app.rs                table UI state, key handling, raise picker, animation timestamps
src/menu.rs               splash/name/main/game-type/settings/code screens + presets
src/anim.rs               easing, shimmer, pulse, deterministic fireworks
src/ui.rs                 table/oval seats/cards/action bar/log/help/notice/celebration
src/art.rs                include_str! of assets/art/*, centering/clipping
src/crypto.rs             ed25519 identity, sealed boxes, deck commit, base64 helpers
src/game/cards.rs         Card (u8), deck, Fisher–Yates over ChaCha20, serde as "As"
src/game/eval.rs          eval5 / eval7 (21 combos), HandScore Ord, descriptions
src/game/table.rs         Table state machine, betting, side pots, showdown, HandRecord,
                          verify_record, unit tests (incl. 6,250-hand simulation)
src/net/protocol.rs       Envelope (sign/verify), Payload enum, Topics, room topics
src/net/roomcode.rs       Crockford base32 encode/decode
src/net/mqtt.rs           Link: WSS connect, reconnect, resubscribe, retained publish
src/net/host.rs           authoritative host loop, timers, sealing, persistence
src/net/client.rs         join/retry, host pinning, version filter, decryption
src/store.rs              config.toml, identity.json (0600), hand history
tests/net_live.rs         ignored live test (host + 2 bots + real broker)
examples/probe.rs         transport gate (run: cargo run --example probe)
assets/art/lobby.txt      84×24 slot-cabinet splash ("GOOD LUCK")
assets/art/win.txt        96×16 fireworks strip for the win overlay
scripts/                  (empty — see §6 for the art commands to put here)
```

---

## 6. Art pipeline (ascii-magic-art)

The art is **dev-time generated, runtime-embedded**. The binary never calls Node or
fetches anything.

```bash
npx ascii-magic-art@1.1.0 slot-reel-letters --frame --look=ascii > /tmp/lobby.txt
# crop rows 6..29, columns 1..84 -> assets/art/lobby.txt   (done)
npx ascii-magic-art@1.1.0 fireworks --frame --look=ascii > /tmp/win.txt
# crop rows 15..30, columns 1..100 -> assets/art/win.txt   (done)
```

`poker art` prints the lobby piece; `--no-art` disables both; lobby art only renders
when the terminal is at least 84×26, otherwise a compact wordmark is used.

Worth adding `scripts/gen-art.sh` with the crop steps (Python is used in-shell today).

---

## 7. Known gaps / next steps (in priority order)

1. **Manual TUI pass:** `poker host` + `poker join` in two terminals. Check 80×24,
   120×40, seat rotation, showdown reveal, card colors, and the win overlay. There is
   no `TestBackend` snapshot test yet.
2. **`cargo clippy -- -D warnings`** is not clean yet (dead-code/unused warnings were
   being fixed as we went; check `seat_tags`'s unused `app` param and the `_unused`
   helpers already removed).
3. **Plain mode** (`--plain`) works but reprints the whole state on every input loop;
   it should print only on version change.
4. **Reconnect/reclaim:** the client persists no session key per room, so restarting as
   the same player creates a new identity and the host sees a new seat request.
5. **Host seat display:** confirm in the TUI the host's `my_seat` drives the
   action bar and the `(you)` tag.
6. **Snapshot staleness:** retained snapshots from a crashed host can look alive; add a
   `host_seen` timestamp (already in `message`) or a heartbeat TTL check in the client.
7. **Tests to add:** `TestBackend` snapshots, protocol golden fixtures, an offline
   host↔client integration test using a fake in-memory `Link`.
8. **Optional:** Cloudflare Durable-Objects relay adapter (free tier: 100k req/day,
   13k GB-s/day, hard cap), Nostr transport, mental-poker shuffle, tournaments,
   spectate-with-delay, rebuy UI.

---

## 8. Gotchas discovered (save the next person time)

- **rumqttc WSS:** `MqttOptions::new(id, host, port)` ignores `port` for Ws/Wss — pass a
  full URL as `host`, e.g. `wss://broker.emqx.io:8084/mqtt`. Otherwise you get
  `Invalid Url: Couldn't parse host from url.` (`src/net/mqtt.rs`).
- **rumqttc sends `Sec-WebSocket-Protocol: mqtt` and validates the response** — public
  brokers do respond with it; no hand-rolled codec was needed.
- **rand ecosystem:** `rand_core 0.10` splits `Rng` (next_u32) from `RngCore`;
  `SeedableRng` must be imported for `ChaCha20Rng::from_seed`. All key material is seeded
  from `getrandom` to avoid rand/rand_core version coupling.
- **clap `env = ...`** needs the `env` feature (added).
- **UI channels:** `UiEvent` uses `std::sync::mpsc` (net → UI); `NetCommand` uses
  `tokio::sync::mpsc::UnboundedSender` (UI → net). Mixing these up is the compile error
  you will hit first.
- **Deck reveal:** keep the full committed 52-card array and a `draw_idx` cursor. An
  earlier bug reset the deck but not the cursor between hands, so boards vanished while
  hole cards still dealt — caught by the randomized simulation.
- **Lone actor:** if only one player still has chips and nothing to call, the betting
  round must auto-close, otherwise one player raises into an all-in opponent forever.
- **Chip conservation** must be asserted after **every** mutation, including fold,
  timeout and rebuy paths — it caught two real bugs.

---

## 9. Environment notes

- Public brokers probed OK from this machine: `broker.emqx.io:8084` (fastest,
  ~200–550 ms RTT), `test.mosquitto.org:8081` (8091 is the authenticated listener: `NotAuthorized`), `broker.hivemq.com:8884` (slow CONNACK).
  **None of them listen on 443** — a port-443-only network will block MQTT. `--broker`
  exists for a self-hosted TLS broker behind Cloudflare Tunnel / Tailscale Funnel.
- ntfy.sh is **not** viable for gameplay (250 messages/day per visitor IP).
- Art provenance: `ascii-magic-art@1.1.0`, stills only (`--frame`); the npm package has
  no poker table, so the lobby uses its slot cabinet.
- **Git:** `cargo init` found the existing repo at `/Users/rayhan/.git` and did **not**
  create one inside `~/pokertui`. Nothing has been committed. Run `git init` inside
  `~/pokertui` before committing, otherwise you will be staging the entire home
  directory.
