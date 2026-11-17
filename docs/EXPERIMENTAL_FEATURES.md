# Experimental features: design & build plan

> ## Implementation status (branch `experimental-features`)
>
> The sections below are the original design. This table says what was actually built
> and where it differs. "Tested" means an automated test, not a manual play-through of the TUI.
>
> | # | Feature | Status | Where / how it differs from the design |
> |---|---|---|---|
> | 1 | Offline fake-relay tests | **Built, tested** | `MemoryBus` in `src/net/mqtt.rs`; harness in `tests/common`; 13 scenarios in `tests/net_offline.rs`, including a lossy relay. |
> | 2 | Reconnect as the same player | **Built, tested** | Same seat, stack and hole cards come back. `poker rejoin` and a "Rejoin" menu item (saved `last_room`). Two timeouts in a row sit a player out; `i` sits back in. The "free the seat after 10 minutes" rule was **not** built. |
> | 3 | Host kick / ban | **Built, tested** | Keys `k` / `K` then a seat number; bans survive a host restart. A kick mid-hand folds the player first so chips stay balanced. |
> | 4 | Anti-stacking (joint seed) | **Built, tested** | Host commits to a secret, every human adds randomness, and the seed is the hash of all of it. Clients check it after every hand. |
> | 5 | Host migration | **Built, tested** | After 15 s of silence the lowest-seat connected player takes over (same room, new epoch). The interrupted hand is cancelled and stacks restored. The old host is sat out until it rejoins. |
> | 6 | Bots | **Built, tested** | `src/bot.rs`: Easy / Medium / Hard (Monte Carlo equity). `+` adds one, `poker host --bots easy,hard`. Bots never see the deck. |
> | 7 | Local rankings | **Built, tested** | `src/rank.rs`, menu screen and `poker rankings`. |
> | 7b | Global leaderboard | **Built and tested in pieces, never deployed** | Worker + D1 code in `relay/cloudflare`, Rust client in `src/leaderboard.rs`. Tested against a local stand-in relay and with Rust-to-JavaScript fixtures. Not run on real Cloudflare: that needs your account. |
> | 7c | Queue and retry when the quota is out | **Built, tested** | Outbox file, sealed to the relay key, retry at the next 00:00 UTC plus jitter. |
> | 8 | Tournaments | **Built, tested** | Menu "Sit & Go" or `--sitngo`. Prize points feed the local rankings only. |
> | 9 | Delayed spectating | **Built, tested** | `poker host --spectate-delay 60`. Hole cards go out only after the hand has ended. Spectators see them as face-up cards. |
> | 10 | Cloudflare relay | **Built, tested against a stand-in** | `relay/cloudflare` plus `cf+wss://` broker URLs. Deploy steps are in its README. |
> | 11 | Mental poker | **Library only** | `src/mental.rs` implements and tests the whole protocol (shuffle, per-card locks, private and public deals, cheat detection). It is **not wired into the live table**, so there is no "trustless deck" setting yet. |
>
> **Changes beyond the original list** (from the cheat review):
> - Room codes are now **12 characters** (60 bits). The broker only ever sees a hash of the code, and every packet is encrypted with a key derived from it, so listing topics on a public broker reveals nothing and nobody can join without the code.
> - Clients **replay every hand** from the signed move list and flag a host whose pots or stacks don't add up.
> - Clients flag a host that **shortens the turn clock**, and reject non-increasing sequence numbers from the host.
> - Duplicate names at one table are refused.
> - A sitting-out seat is never dealt cards or asked to act (this was a latent bug).
> - Protocol version bumped to 2 (`pokertui/v2/...`); v1 and v2 clients cannot see each other's tables.
>
> **Known limits** (none of these are new, but the review turned them up):
> - The host still sees every card in a normal game; only mental poker (not wired in) fixes that.
> - Players who collude, or one person running two windows, cannot be detected in software. The global leaderboard limits the damage instead: a win only earns rating from an opponent who has already played at least 5 other people, and any one opponent can pay out at most 25 rating points in total. Playing yourself from fresh accounts earns nothing; a ring of six accounts is capped at about 125 points each.
> - A seated player can still pretend to be the host to *new joiners* after a migration.
> - Join requests are not rate-limited, but they need the room code.



## 0. How to read this / ground rules

**Today's setup in one paragraph.** The host's computer is the dealer. It owns the
`Table` (`src/game/table.rs`), shuffles, deals, and signs every update. Everyone talks
through a public MQTT broker on three topics per room (`events`, a retained `snapshot`,
and `secrets/<player>` for sealed hole cards). Clients check every signature and only
trust the host they first heard from.

**One rule that affects every feature: bump the protocol version first.**
`Envelope::verify` re-serializes the payload to check the signature. An older client
that reads a message with a field it doesn't know will silently drop the field, the
re-serialized bytes won't match, and the message gets rejected. So adding *any* field to
`TableView` or `Payload` breaks old clients. Step one on this branch is to set
`PROTOCOL_VERSION = 2` and change the topic prefix to `pokertui/v2/…` in
`src/net/protocol.rs`. Old and new versions then never see each other's rooms, so
nothing half-works.

**Build order** (each step makes the next ones easier):

| # | Feature | Size | Why this position |
|---|---|---|---|
| 1 | Offline fake-relay tests | S | Every other feature gets tested with it |
| 2 | Reconnect as the same player | S | Mostly already works, so it's a quick win |
| 3 | Host kick / ban | S | Small, and needed before strangers join |
| 4 | Anti-stacking (joint shuffle seed) | M | Closes the biggest known cheat cheaply |
| 5 | Host migration / reclaim seats | L | Needs 1, 2 and the version bump |
| 6 | Bots with skill levels | M | Self-contained, and makes 8 and 9 testable alone |
| 7 | Saved player rankings (local board) | S | Needs pubkeys in the view (added here) |
| 8 | Tournaments (sit-and-go) | M | Uses 6 for testing and 7 for points |
| 9 | Delayed spectating | M | Independent |
| 10 | Cloudflare Durable Objects relay | M | Independent; also the home for the global board |
| 10b | Global leaderboard, all players (§7b) | M | Needs the Worker from 10 |
| 11 | Mental poker shuffle | XL | Biggest and riskiest, so it goes last as an opt-in mode |

---

## 1. Offline fake-relay integration tests

**What it is.** Run a full host and 2–3 clients inside one test process, talking through
an in-memory "broker" instead of the internet. Tests become fast (milliseconds), work
offline, and give the same result every run. We can also make the fake broker *drop*
messages on purpose, to prove the self-healing (pings → resend) still works.

**How it works today.** `Link` (`src/net/mqtt.rs`) is a struct around `rumqttc`.
`run_host` and `run_client` call `Link::connect()` themselves, so a test can't hand them
a different transport. The only network test, `tests/net_live.rs`, needs the real
internet and is `#[ignore]`d.

**How I'll build it.**
1. Inside `Link`, put the backend in a private enum: `enum Backend { Mqtt(AsyncClient), Memory(MemoryBus) }`.
   The public API stays exactly the same (`subscribe`, `publish`, `recv`,
   `is_connected`, `broker`), so host and client code doesn't change. (An enum rather
   than a trait: there will be two or three backends, and an enum is the simplest thing that works.)
2. `MemoryBus` = `Arc<Mutex<Hub>>`, where `Hub` holds a list of
   `(topic, UnboundedSender<Message>)` subscribers and a `HashMap<topic, Vec<u8>>` of
   retained messages. Publishing delivers to every matching subscriber. Subscribing
   immediately replays the retained message, just like a real broker.
3. Fault injection: `Hub { drop_every: Option<u32> }` (or a seeded RNG drop rate) quietly
   swallows some publishes, imitating EMQX losing bursts.
4. Split `run_host(cfg, tx, rx)` into a thin wrapper plus `run_host_on(link, cfg, tx, rx)`.
   Do the same for the client. The wrappers still call `Link::connect`, so production
   is unchanged.
5. New `tests/net_offline.rs`, adapted from `tests/net_live.rs`:
   - happy path: host + 2 clients play a hand to showdown and chips are conserved
   - lossy path: 20% drop rate, and the hand still finishes within N seconds
   - later features add their tests here (kick, reconnect, migration, tournament)
6. Use a short `turn_ms` (e.g. 500) in tests so timeouts run fast.

**Done when** `cargo test` (without `--ignored`) runs a full networked hand in under a second.

---

## 2. Reconnecting as the same player

**What it is.** If your terminal crashes, your Wi-Fi drops, or you close the window by
accident, running `poker` again puts you back in *your* seat with *your* chips and
your current hole cards.

**How it works today.** Most of this is already there:
- Your identity (signing key) is saved in `identity.json` (`store::load_or_create_identity`).
- `Table::join` returns your *existing* seat when your public key is already seated.
- After a reconnect, the client's `Ping` reports `cards_hand`. If it's behind, the host
  re-seals your hole cards (`host.rs`, `Payload::Ping` branch).

**What's missing:**
- (a) Nothing remembers *which room* you were in, so you have to retype the code.
- (b) Pressing Esc sends `Leave`, which frees the seat. That's correct for leaving on
  purpose, but you can't tell it apart from a crash in the UI.
- (c) A player who never comes back keeps their seat forever and stalls each hand until
  the turn timer runs out.
- (d) A second `poker` process on the same computer gets a throwaway identity (the
  `identity.lock` logic), so it can't reclaim a seat. That's intended; we'll just document it.

**How I'll build it.**
1. `store::Config` gets `last_room: Option<String>`. Save it when `UiEvent::Joined`
   arrives, and clear it on a deliberate Leave or when the table closes.
2. The menu (`src/menu.rs`) shows a **"Rejoin K7Q2-9XZP"** item at the top when
   `last_room` is set. Add a `poker rejoin` subcommand in `main.rs` too.
3. Host side (`host.rs` `tick`): use the existing but unused `Seat::timeout_streak`.
   Increase it on each auto-fold or auto-check from the timer. At 2, set
   `sitting_out = true` so the table stops waiting on them. Any action or ping from
   that player clears it.
4. Seat reservation: a seat that's been disconnected *and* sitting out for more than 10
   minutes gets removed between hands (never mid-hand, since `Table::leave` already refuses that).
5. When a reconnecting player's `JoinRequest` matches a seated pubkey, the host also
   clears `sitting_out` and re-seals cards right away, instead of waiting for the next ping.

**Test:** an offline test kills a client mid-hand, starts a new client with the same
`Identity`, and checks it gets the same seat, the same stack and its hole cards.

---

## 3. Host kick / ban

**What it is.** The host can remove a player (kick) or remove them and block them from
coming back (ban).

**How I'll build it.**
1. New `NetCommand::Kick { seat, ban: bool }` (UI → host) and new
   `Payload::Kicked { to, reason, ban }` (host → everyone).
2. Host logic (`host.rs`):
   - Not in a hand, or the player already folded: remove the seat with `table.leave`
     straight away and broadcast.
   - In a hand: fold them now (`table.apply(seat, Move::Fold)` if it's their turn,
     otherwise set a `pending_kicks: Vec<usize>` that folds them as soon as they're
     the actor), then remove them in `settle_hand`. This keeps chip conservation intact.
   - `ban` adds their pubkey to `banned: HashSet<String>`. `on_join` sends the existing
     `JoinReject` when it sees a banned key (the client already handles that by showing the reason and exiting).
   - Save bans to `sessions/<room>.json` (the `store::save_session` helper already
     exists and isn't used yet), so a host restart keeps them.
3. Client: on `Kicked` addressed to me, set `done = Some(reason)` (same path as `Closed`).
   It also clears `last_room` so "Rejoin" doesn't offer that table again.
4. UI (`app.rs` / `ui.rs`): host-only key `k` opens a small seat picker
   ("Kick who? 1–6, then K = kick, B = ban"). The help overlay lists it.

**Honest limit (goes in the doc):** a ban blocks a *key*. Someone can make a new key by
deleting `identity.json`. Among friends that's fine. A hard ban would need accounts,
which we don't want.

---

## 4. Anti-stacking: everyone helps shuffle (joint seed)

**The problem.** The deck comes from `shuffle(deck, ChaCha20Rng::from_seed(seed))`, where
the host alone picks the 32-byte `seed` (`Table::start_hand_seeded`). The commitment
proves the host didn't *change* the deck mid-hand. It does **not** stop the host from
trying seeds until it finds a deck it likes, and then committing to that one.

**The fix (commit-then-contribute).** Make the seed depend on input from every player,
in an order where the host can't cheat:

1. **Host locks in first.** Before the hand, the host picks a secret `r_host` and
   publishes only its hash `H(r_host)` inside the `State` (new field
   `TableView.seed_commit`).
2. **Players add randomness.** Each seated player replies
   `Payload::Entropy { hand, r }` with 32 fresh random bytes. They can send it in the
   open, because nobody knows `r_host` yet, so no one can predict the deck.
3. **Host mixes.** The host waits up to ~1.5 s, then computes
   `seed = SHA256("pokertui-seed-v1" ‖ r_host ‖ r_p1 ‖ r_p2 ‖ …)` (players sorted by seat)
   and deals as usual.
4. **Host reveals at the end.** `HandResult` gains `seed_host: r_host` and
   `seed_contribs: Vec<(seat, r)>`.
5. **Everyone checks.** Clients verify that `H(r_host)` matches the commitment, that
   *their own* `r` is in the list, and that the shuffle from that seed gives exactly the
   committed deck. `poker verify` checks the same thing for saved hands.

**Why this works, in plain terms.** The host commits to its number before seeing yours.
You pick yours without knowing the host's. Neither side can steer the result.

**Edge cases.**
- A player's entropy gets lost: the host deals without it, the player sees they're
  missing from the list, and the client shows a warning. This doesn't freeze the table.
- The host leaves someone out on purpose: same warning. It's visible, so you know.
- Timing: `start_hand` turns into "publish commit → wait for entropy → deal". This is
  about one broker round trip, so the extra delay is ~0.3–1 s.

**What it does NOT fix:** the host still *knows* every card, because it's the dealer.
Only mental poker (§11) fixes that. The doc's trust model section gets updated to say
exactly this.

**Code:** `crypto.rs` (seed derivation + verify), `table.rs` (`start_hand_seeded`
already takes a seed, so it's unchanged, and `verify_record` gets extended), `host.rs`
(a new `Phase::Shuffling`-style wait state), `client.rs` (send entropy when
`seed_commit` appears, verify at the result).

---

## 5. Host migration / reclaiming seats

**What it is.** Today, if the host quits or crashes, the table freezes (HANDOFF §4).
With migration, another player takes over as host automatically, and the game
continues with the same stacks. When the old host comes back, they rejoin as a normal player.

**Main constraint.** The host knows the deck. A new host can't continue the *current*
hand without knowing the old deck, and we don't want that. So **migration happens
between hands: the in-progress hand is cancelled and everyone gets their chips back.**

**How I'll build it.**
1. **Heartbeat.** The host already broadcasts often. Add `TableView.host_ts_ms`, and have
   the host re-broadcast at least every 5 s even when nothing changed. A client
   that hears nothing from the host for **15 s** treats the host as dead.
2. **Successor election (no voting needed).** From the last signed view, the successor is
   the **lowest seat number that was `connected` and isn't the old host**. Every client
   works this out from the same view and gets the same answer, so no extra messages are needed.
3. **Restoring state.** `SeatView` gets `start_stack` (the stack at the start of the hand).
   New `Table::from_view(params, &view)` rebuilds seats, names, pubkeys, enc keys (also
   added to `SeatView`, since they're public anyway), button and `hand_no`. Stacks come
   from `start_stack`, which cancels the interrupted hand. Phase goes back to `Lobby`.
4. **Taking over.** The successor's client loop returns a new value,
   `ClientExit::Promote(view)`. `main.rs` then starts `run_host_on(link, …)` with
   `Host::from_view`. It's the same room and topics, and the new host overwrites the
   retained snapshot.
5. **Clients accepting the new host.** `client.rs` currently pins the first host forever
   (`self.host`). Change that: accept a `State` from a new key only if
   `view.epoch > current.epoch` (new field, starts at 0, +1 on each migration) **and**
   the sender is the successor this client calculated itself. That stops random people
   from claiming to be host.
6. **Reclaiming.** The old host relaunches → "Rejoin" (§2) → it's just a client now, and
   `Table::join` finds its pubkey and returns its seat.
7. **Bans and settings carry over** via the view (`params` + a `banned` list in the
   view, which is public anyway).

**Honest limits:** if the successor is also gone, the next client in line waits another
15 s and takes over. Hand history from before migration stays on the old host's computer.

**Test (offline bus):** start a hand, drop the host's link, then check that a client
becomes host within ~16 s, stacks equal the pre-hand stacks, and a new hand can be dealt.

---

## 6. Bots with configurable skill

**What it is.** The host can fill seats with computer players: **Easy**, **Medium** or
**Hard**. Good for practice, short-handed games and testing.

**Design: bots live inside the host.** No network and no extra process. A bot is a normal
`Seat` with a new field `bot: Option<Skill>`, a fake pubkey (`bot:1`) and no enc key.
`Host::deal` skips sealing for bot seats. In `Host::tick`, when the actor is a bot and
its "think time" (0.6–2 s, random) has passed, the host calls the bot brain and applies
the move like any other action.

**Fair play rule:** the bot brain only gets what a real player at that seat would see:
its own two cards, the board, pot, bets, `Legal` and the number of opponents. It never
sees the deck. This is enforced by the function signature:

```rust
// src/bot.rs
pub fn decide(skill: Skill, seen: &BotView, rng: &mut ChaCha20Rng) -> Move
```

**The three brains:**
- **Easy:** plays almost any two cards, calls a lot, rarely raises, never bluffs big.
  Mostly random, weighted toward check/call.
- **Medium:** a simple preflop chart (pairs, high cards, suited connectors get played;
  junk gets folded unless it can check), then after the flop it looks at its made hand
  using the existing `eval7` and compares to **pot odds** (call when hand strength ≥ the
  price). Small fixed bet sizes.
- **Hard:** **Monte Carlo equity.** Deal random cards to the opponents and the rest of the
  board ~400–800 times with the existing evaluator, and count how often it wins. That
  win % vs pot odds decides fold/call/raise. Bet sizing scales with equity, and it bluffs
  on scary boards ~10–15% of the time so it can't be read easily. Takes a few ms per decision.

**UI:** in the host lobby, `b` adds a bot (cycling skill), and the table settings screen
gets "Bots: 0–5, Skill: Easy/Med/Hard". Bot names come from a short list ("Dealer Dan
(Hard)").

**Tests:** `bot.rs` unit tests (never makes an illegal move over 10,000 random spots;
Hard folds 7-2 offsuit to a big raise and doesn't fold aces preflop), plus a 1,000-hand
bots-only simulation that checks chip conservation (reusing the existing simulation helper in `table.rs`).

---

## 7. Saved rankings of players (yours + everyone's)

**What it is.** Two leaderboards with the same columns (hands played, hands won, net
winnings, tournament finishes):
- **My tables:** the people *you've* played with, saved on your computer. Works offline. (Steps 1–5 below.)
- **Global:** **every pokertui player in total**, ranked against each other, served from
  one shared online leaderboard. (Section 7b below; built after the Cloudflare relay, §10.)

Both are shown from the menu ("Rankings", with Tab switching My tables ↔ Global) and
from `poker rankings [--global]`.

**Who's who.** Names can change and repeat, but each player's signing key doesn't. So
rankings are keyed by **public key**, showing the latest name seen.

**How I'll build it.**
1. `SeatView` gets `pubkey` (it's already public, since it's in every signed packet the player sends).
2. New `src/rank.rs` (or a section in `store.rs`): `rankings.json` in the data dir,
   `HashMap<pubkey, PlayerStats { name, hands, won, net_bb: f64, sessions, tourney_points }>`.
3. Every client (host included) updates it on each `UiEvent::HandResult`: +1 hand for
   everyone dealt in, +1 win for pot winners, net change = stack after − stack before.
   It's stored in **big blinds**, so a High Roller win doesn't swamp Quick Game numbers.
4. Bots are skipped (pubkey starts with `bot:`), and so are hands from tables where you were only spectating.
5. A screen sorted by net BB, with columns: Name · Hands · Win% · Net BB · Tourney pts.
   `poker rankings --reset` clears it.

**Honest limit (local board):** it's built from what hosts signed, so a cheating host
could feed you fake results. That's fine for a board of your own friends. The global board
below closes this gap.

### 7b. Global leaderboard (all players in total)

**The problem.** There's no server today. A global ranking needs one place that
collects results from every table. And because anyone can run a modified client, it must
not accept made-up results.

**Where it lives.** The same Cloudflare Worker as §10, plus a small **D1** (Cloudflare's
built-in SQLite) database. One table: `players(pubkey PK, username UNIQUE, state NULL, show_state, listed,
hands, won, net_bb, tourney_points, rating, updated_at)` and one table: `hands(hand_hash PK, room, submitted_at)`.
The game is still fully playable without it. The global board is extra.

**Stopping fake results: every player signs off.**
1. When a hand ends, each *human* player's client builds a summary of the result
   (`hand_hash = SHA256(room ‖ hand_no ‖ deck commit ‖ each seat's pubkey + chip change)`)
   and checks it matches what it actually saw.
2. Each player signs it with their existing ed25519 key: `Payload::ResultAck { hand_hash, sig }`.
3. The host collects the acks and posts `{summary, all signatures}` to
   `POST /leaderboard/hands` (in batches, so it's one request every few hands).
4. The Worker counts the hand **only if every human player in it signed the same
   `hand_hash`**, and only once (`hand_hash` is the primary key, so re-submits get ignored).
   A host can't invent a win for itself, because the losers would have to sign it.
5. Bot hands, hands with fewer than 2 humans, and spectator-only participation are never counted.

**Usernames instead of names.** The first-run screen that asks "Choose a name"
(`src/menu.rs`, the name screen) becomes **"Choose a username"**, with one short dim line
under the box: *"Shown on the public leaderboard. Don't use your real name."* The same
wording goes on the settings screen where you can change it later, and `config.toml` /
code rename `name` → `username` (still read the old `name =` key so existing configs keep working).
Rules: 3–16 characters, letters/numbers/`_`/`-` only (the current `sanitize_text(…, 16)`
limit, made stricter). On the global board, usernames must be unique: the Worker
answers "taken, try another" and the screen asks again.

**What the board shows.** Exactly three columns, in this order:

| Rank | Username | State |
|---|---|---|
| 1 | river_rat | California |
| 2 | ace_king | Texas |
| 3 | nitbox | — |

- **Rank** comes from a hidden **rating** (Elo-style, starting at 1000). After each counted
  hand, winners go up and losers go down, scaled by the swing in big blinds and capped per
  hand. A minimum of 50 hands is needed to appear. Raw net chips aren't used, because they
  favour people who simply play a lot.
- **Username** is your chosen username (above).
- **State** is **optional and off by default**. It's worked out from your IP address *by
  Cloudflare*: every request to a Worker comes with `request.cf.region` (e.g. "California")
  and `request.cf.country`. The Worker saves **only that state name, never the IP
  itself**. US players get their state. Players outside the US get their country (e.g.
  "Ontario, CA" style is skipped for simplicity, so it's just "Canada"). If you haven't
  turned it on, the column shows "—".
- Your own detailed numbers (hands, win %, net BB, tourney points) appear in a
  "My stats" panel below the table, not as extra columns.

**How your username and state get there.** The host's submissions only prove *results*.
Your profile comes from *you*: on startup (if you're opted in), your client opens a
short WebSocket to the Worker and sends a signed
`{op:"lb_profile", pubkey, username, show_state, sig}`. Because the request comes from
*your* connection, Cloudflare's location is *your* state, not the host's. This happens
even if the game itself runs over a public MQTT broker. Updating it later (new username,
state toggled off) is the same call, and turning state off deletes the stored value.

**Reading it.** `{op:"lb_get", top:100}` and `{op:"lb_me", pubkey}` (your row + rank). The
client caches the last response for an hour so the menu opens instantly.

**Privacy & opt-in.**
- Being on the global board is **opt-in** ("Show me on the global leaderboard", default off).
  If you're not opted in, your acks are still sent so your table's hands count for
  others, but your row is hidden.
- "Show my state" is a **separate opt-in** under it (default off), with the hint
  *"Guessed from your internet connection; may be wrong on VPNs or mobile data."*
- Stored per player: pubkey, username, optional state, stats. Never IP addresses or room codes.

**Honest limits.**
- **Chip-dumping between friends** (two accounts you control, or a friend who
  deliberately loses to you) can't be fully stopped. Mitigations: the per-hand rating
  cap, ignoring pairs of players who only ever play each other, and a host-side
  `poker leaderboard --hide <pubkey>` admin tool for the relay owner.
- **New key = fresh start.** Deleting `identity.json` resets you. That's the same limit as bans (§3).
- **Who runs it:** the global board lives on *your* Cloudflare account. Everyone using the
  default relay URL shares one board. Someone who deploys their own relay gets their own separate board.

### 7c. When the free quota runs out: save locally, send after the reset

**What it is.** If Cloudflare's free daily limit is hit (or Cloudflare is just down),
nobody loses any results. Every player's computer keeps a small queue of finished hands
and uploads it once the Worker is reachable again. The game itself is never blocked by this.

**It really is light.** One finished hand is about 300–500 bytes (summary + the
players' signatures). 1,000 hands is roughly half a megabyte, which is a few weeks of
heavy play. The queue is capped at 5,000 hands (~2 MB); past that the oldest hands are
dropped and a one-line notice says so.

**How I'll build it.**
1. **Queue file.** `outbox.json` in the data dir (`store.rs`, next to `rankings.json`).
   Each entry is the full signed bundle for one hand: `{hand_hash, summary, sigs}`.
   Every human client builds this bundle itself. It already sees all the
   `ResultAck` messages on the table's `events` topic, so it doesn't depend on the host
   staying alive.
2. **Batching also helps the quota.** Hands are *not* sent one at a time. The client
   uploads when it has 20 waiting or every 10 minutes, whichever comes first, as one
   request (up to 50 hands). That is far fewer requests than one per hand, so we hit
   the daily cap much less often to begin with.
3. **Spotting "quota's out".** Any failed upload counts: a 429, a Cloudflare limit
   error page, a timeout, or no connection. The hands stay in the queue. We don't
   need to tell the causes apart.
4. **When to retry.** Cloudflare's free-plan limits reset daily at **00:00 UTC** (to be
   double-checked against their docs when this is built). So after a failure the client
   retries at the next 00:00 UTC **plus a random 0–10 minute delay**, so thousands of
   machines don't hit the Worker in the same second right after reset. If the failure
   looks unrelated to quota (no internet), it uses a normal backoff (1 min → 5 min →
   30 min). It also tries once on every launch and when the Rankings screen opens.
4b. **Sealed to the server.** The Worker has one X25519 keypair. Its **public** key ships inside
   the app; the **private** key lives only as a Worker secret (`wrangler secret put`).
   Each queued bundle is sealed with the existing `crypto::seal` before it is written to
   `outbox.json` and uploaded as-is. The file on disk and the upload are unreadable to anyone but
   the Worker. The players' co-signatures stay *inside* the sealed bundle, because encryption
   hides content but cannot prove it is true. The client keeps its own stats in
   `rankings.json` (§7), so it never needs to read the sealed queue back.
5. **Safe to resend.** The Worker uses `hand_hash` as a primary key and ignores repeats,
   so it doesn't matter that every player uploads the same hand. The signatures cover
   `hand_hash`, so a hand sent days late is still valid. The Worker accepts hands up
   to 30 days old, and applies a batch in order of the hand's own timestamp so ratings
   come out the same as if they'd arrived live.
6. **Profile changes queue too.** A new username or state toggle is held as a single
   pending `lb_profile` update and sent with the next upload.
7. **What the screen shows.** The Rankings screen shows the last board it downloaded,
   stamped "as of 14:02 UTC", and one status line: *"Global board offline: 14 results
   waiting, next try after reset in 3h 12m."* When the queue empties it goes away.
8. **If the table's relay is the thing that ran out.** The MQTT brokers are the default
   and are unaffected. Only tables that chose the Cloudflare relay (§10) would be hit,
   and `Link::connect` can fall back to the public MQTT brokers when the relay refuses
   a new connection, so new tables still start.

**Honest limits.**
- Results show up on the global board late, up to about a day, not instantly. Your
  own local rankings (§7) update immediately regardless.
- If a player never opens the game again before 30 days pass, their copy of a hand
  expires. It still counts if any other player in that hand uploads it in time, which
  is why every player keeps a copy.
- Moving to the paid Cloudflare tier removes the problem. The queue stays as a safety net.

**Tests:** offline test with a fake Worker that returns 429 for the first N requests:
the queue grows, nothing is lost, and everything is accepted exactly once after "reset";
a queue over 5,000 entries drops only the oldest; the retry time is always the next
00:00 UTC plus 0–10 minutes of jitter.

**Code:** `relay/cloudflare/` (+ `leaderboard.ts`, `schema.sql`, D1 binding in
`wrangler.toml`), `src/rank.rs` (ack building/verification, HTTP fetch via the relay's
WebSocket, so no new HTTP dependency is needed: add `{op:"lb_get"}` / `{op:"lb_post"}` frames
to the §10 protocol), `client.rs`/`host.rs` (`ResultAck`), `menu.rs` (Global tab).

**Tests:** Worker unit tests (rejects a hand missing one signature, rejects a forged
signature, ignores duplicates, rejects a taken username, stores state only when
`show_state` is true and clears it when switched off, never writes the IP); Rust test that every client in an offline-bus hand
produces the identical `hand_hash`.

---

## 8. Tournaments (sit-and-go)

**What it is.** Everyone buys in once for the same stack. Blinds go up on a timer. Lose
your chips and you're out. Last player standing wins, and finishing places are recorded
(and fed into rankings, §7).

**Scope:** single-table sit-and-go only. Multi-table, late registration, rebuys and
antes are skipped for now because they add a lot of rules for little benefit at a
6-seat table.

**How I'll build it.**
1. `TableParams` gets `tournament: Option<TourneyParams { levels: Vec<(sb, bb)>, level_secs }>`.
   A preset blind schedule (e.g. 25/50 → 50/100 → 75/150 → 100/200 → 150/300 → 200/400 →
   300/600 …, 5 min each) is added to `menu.rs` as a **"Sit & Go"** game type.
2. Host tracks `level`, `level_started_at`. In `start_hand`, if the level time has passed,
   the level goes up, and new `Table::set_blinds(sb, bb)` applies from the *next* hand (never mid-hand).
3. **Eliminations:** in `settle_hand`, every seat that hit 0 chips this hand gets a
   finishing place (`players_left + 1`; ties are ranked by stack at hand start). It stays
   seated as "OUT" so everyone sees the result, but `eligible()` already skips 0-stack
   seats. `auto_rebuy` is forced off.
4. Joining is closed once the first hand is dealt (spectating still works).
5. **Auto-deal:** in tournaments the host deals the next hand 5 s after the last one ends,
   so the game keeps moving without the host pressing Enter.
6. **Showing it:** `TableView.tournament = Some({ level, sb, bb, next_level_at_ms, places })`.
   The header shows "Level 3 · 75/150 · next in 2:41". When one player is left, a
   standings overlay appears (reusing the win-overlay code in `ui.rs`).
7. **Rankings:** points by place (e.g. 1st = 10, 2nd = 6, 3rd = 4, then 2, 1, 0).

**Tests:** table unit test (blinds go up only between hands, places assigned correctly,
chips conserved); offline test with 3 Easy bots playing to a winner.

---

## 9. Delayed spectating

**What it is.** Spectating already exists (`poker spectate CODE`), but spectators only
see what players see: no hole cards. "Delayed spectating" is TV-style: **spectators
see everyone's hole cards, but on a delay**, so it can't be used to cheat live.

**The tricky part, explained.** Anything on the broker can be read by anyone who has
the room code, including a player running a second "spectator" window. So hole cards
must never go out while that hand is still being played.

**Rule:** a hand's spectator feed (with hole cards) is published only **after that hand
is finished**, and then played back at its original pace, starting at least
`spectate_delay` (default 60 s) after the hand began.

**How I'll build it.**
1. Host setting `spectator_holecards: bool` (default **off**) + `spectate_delay_secs`.
   When on, the lobby shows a warning: "Spectators (and anyone with the code) will see
   folded hands after each hand ends."
2. Host keeps `VecDeque<Frame { at: Instant, view: TableView, holes: Vec<(seat, [Card;2])> }>`,
   adding a frame on every broadcast during a hand.
3. When the hand ends, those frames are queued for release. `tick` publishes each frame
   as a signed `Payload::Delayed { view, holes }` on a new topic `…/spectate`, once
   `frame.at + delay` has passed *and* its hand is over.
4. Spectator client: subscribes to `…/spectate` instead of `events` when the room has
   delayed mode on (a flag in the snapshot). The UI uses the existing card-drawing code
   to show hole cards face up, with a "DELAYED · hand #12" badge.
5. If delayed mode is off, spectating works exactly as it does today.

**What still leaks (stated honestly):** once a hand is over, its folded (mucked) cards are
visible to anyone with the code. That's the price of the feature, which is why it's
opt-in.

---

## 10. Optional Cloudflare Durable Objects relay

**What it is.** A tiny relay you can deploy free to your own Cloudflare account, as an
alternative to public MQTT brokers. It helps on networks that block the brokers' odd
ports (HANDOFF §9: none of them use 443; Cloudflare does). Nothing gets more trusted:
every packet is still signed and hole cards are still encrypted, so the relay just
passes bytes along, like the public brokers do.

**How I'll build it.**
1. **Relay** (new folder `relay/cloudflare/`, TypeScript, about 100 lines + `wrangler.toml`):
   - Worker: `GET /room/<roomhex>` upgrades to a WebSocket and forwards to the Durable
     Object `idFromName(roomhex)`, so **one object per room**.
   - Durable Object: keeps the connected sockets (using the **WebSocket Hibernation API**,
     so idle rooms cost nothing), each socket's subscribed topics, and the last
     retained message per topic in DO storage.
   - Messages are small JSON frames: `{op:"sub",topic}`, `{op:"pub",topic,data(base64),retain}`;
     the server sends `{op:"msg",topic,data,retain}`. Packets over 64 KB are rejected,
     and there's a simple per-socket rate limit to protect your free quota.
2. **Rust side:** a third `Link` backend (`Backend::Cf`), picked when the broker URL
   starts with `cf+wss://`. It uses `async-tungstenite`, **already in `Cargo.lock`**
   as a dependency of `rumqttc`, so it only needs to be listed in `Cargo.toml`. Reconnect and
   resubscribe logic copies the MQTT backend's pattern.
3. Usage: `poker --broker cf+wss://pokertui-relay.<you>.workers.dev host`, or set `broker` in `config.toml`.
4. README section: "Deploy your own relay in 3 commands" (`npm i -g wrangler`, `wrangler login`, `wrangler deploy`).
5. Free-tier limits will be checked against Cloudflare's current pricing page when this
   is built. HANDOFF's numbers may be out of date by then.

**Test:** the offline tests already cover game logic. For the relay, one ignored live test
(like `net_live.rs`) runs against `wrangler dev` locally.

---

## 11. Mental poker shuffling (opt-in "Trustless deck" mode)

**What it is.** The strongest version of anti-cheating: **nobody, not even the host,
knows any card until it's meant to be seen.** Every player helps shuffle and lock the
deck, and a card is only unlocked when everyone agrees to unlock it.

**The idea, explained with padlocks.** Picture 52 cards in boxes:
1. Alice puts her padlock on every box, shuffles them, and passes them on.
2. Bob adds *his* padlock to every box, shuffles again, and passes them on. Now nobody
   knows which box is which card, because nobody saw both shuffles.
3. To show Bob his card, everyone *except* Bob removes their padlock from that box. Only
   Bob's lock is left, and only Bob can open it.
4. To show a board card, everyone removes their lock in public.

The special property is that padlocks can come off **in any order** ("commutative"
encryption). We get that with elliptic-curve math:
- Each card is a fixed point on the Ristretto curve (`Card_i = hash_to_point("pokertui-card" ‖ i)`).
- Locking with secret number `k` = multiplying the point by `k`. Unlocking = multiplying by `k⁻¹`.
- `a·(b·P) = b·(a·P)`, so locks come off in any order.
- Library: `curve25519-dalek`, **already in `Cargo.lock`** (used by ed25519/x25519), so
  it only needs to be listed directly.

**Protocol steps (N players, in seat order):**
1. **Shuffle round:** each player in turn multiplies all 52 points by their secret `k_i`,
   shuffles, and publishes the result. (N sequential messages.)
2. **Per-card lock round:** each player in turn removes `k_i` and puts a *separate* key
   `k_i,j` on each card position `j`, so single cards can be unlocked later. (N more messages.)
3. **Dealing:** card positions are fixed (hole cards 0..2N, then the board). For each
   player's hole cards, every *other* player sends their `k_i,j` sealed to that player
   (the existing `seal`), and the player unlocks locally. For board cards, everyone
   publishes their key in the open when the street is reached.
4. **End of hand:** everyone publishes all their keys. Every client (and `poker verify`)
   replays the whole transcript and checks that every step was a real shuffle of real cards. Anyone who
   cheated (duplicate cards, swapped values) is caught and named.

**What it costs (why it's opt-in):**
- **Speed:** about 2N extra broker round trips before each hand, so roughly 3–6 s at 6
  players, plus a short wait at each street for key releases.
- **Everyone must stay online.** If a player disappears mid-hand, their keys are missing
  and the hand can't continue. Rule: if a needed key doesn't arrive within 20 s, the hand
  is **cancelled with chips returned** (same mechanism as host migration, §5), and the
  missing player is sat out. Folded players still have to stay connected until the hand ends.
- **Bots:** their keys are held by the host, which is fine because they're the host's bots.
- **Cheating is caught *after* the hand, not prevented.** (Stopping it *during* the hand
  needs zero-knowledge shuffle proofs. Those are much more code, so we skip them unless
  this mode actually gets used.)

**How I'll build it.**
1. `src/mental.rs`: pure functions (`lock_all`, `shuffle_and_lock`, `relock_per_card`,
   `unlock`, `verify_transcript`) with heavy unit tests that run on one machine with no network.
2. New payloads: `MpShuffle { hand, step, deck: Vec<point> }`, `MpKeys { hand, positions, keys }`
   (sealed or public), `MpReveal { hand, all_keys }`.
3. Host still runs betting and turn order. It just no longer knows the cards. Showdown:
   players reveal their own hole keys, or everyone's keys come out at hand end.
4. Table setting **"Trustless deck (slower)"**, off by default. When off, §4's joint seed is used.
5. Offline-bus tests: a 3-player hand completes, a cheating player who swaps a card is
   detected, and a player who drops mid-hand leads to a clean refund.

---

## Appendix: files most features touch

- `src/net/protocol.rs`: version bump, new payloads, new topics (`spectate`)
- `src/game/table.rs`: `SeatView` (+pubkey, enc_pub, start_stack, bot), `TableView`
  (+epoch, host_ts_ms, seed_commit, tournament), `Table::from_view`, `set_blinds`
- `src/net/host.rs`: kick/ban, entropy wait, bots, tournament levels, spectator frames
- `src/net/client.rs`: entropy, host handover, kicked, delayed feed
- `src/net/mqtt.rs`: backend enum (Memory, Cf)
- `src/store.rs`: `last_room`, rankings, bans via `save_session`, `outbox.json` (§7c upload queue)
- `src/menu.rs`, `src/app.rs`, `src/ui.rs`: "Choose a username" screen + warning, Rejoin,
  Rankings (Rank · Username · State), Sit & Go, bot/kick keys
- New: `src/bot.rs`, `src/mental.rs`, `src/rank.rs`, `tests/net_offline.rs`, `relay/cloudflare/`

