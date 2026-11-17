//! The optional global leaderboard.
//!
//! * Every human player co-signs each finished hand (`AckBook`), so a result only
//!   counts if everyone at the table agreed to it.
//! * Complete, signed hands are sealed to the relay's public key and queued on disk
//!   (`Outbox`). If the relay is unreachable or over its free quota nothing is lost:
//!   the queue waits and is sent after the next daily reset.

use crate::crypto::{Identity, Sealed, parse_enc_pubkey, seal, sha256_hex, verify_b64};
use crate::game::TableView;
use crate::net::cf;
use anyhow::{Context, Result};
use async_tungstenite::tokio::connect_async;
use async_tungstenite::tungstenite::Message as Ws;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const MAX_QUEUE: usize = 5_000;
pub const MAX_AGE_MS: u64 = 30 * 24 * 3600 * 1000;
pub const BATCH: usize = 50;
pub const SEND_AT_COUNT: usize = 20;
pub const SEND_AT_AGE_MS: u64 = 10 * 60 * 1000;
const DAY_MS: u64 = 24 * 3600 * 1000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerResult {
    pub pubkey: String,
    /// Chips won (+) or lost (-) in this hand.
    pub delta: i64,
}

/// One finished hand plus every human player's signature over `hand_hash`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bundle {
    pub hand_hash: String,
    pub room: String,
    pub hand_no: u32,
    pub commit: String,
    pub ts_ms: u64,
    pub bb: u64,
    pub players: Vec<PlayerResult>,
    pub sigs: BTreeMap<String, String>,
}

/// Must match `handHash` in relay/cloudflare/src/leaderboard.js.
pub fn hand_hash(room: &str, hand_no: u32, commit: &str, players: &[PlayerResult]) -> String {
    let mut sorted: Vec<&PlayerResult> = players.iter().collect();
    sorted.sort_by(|a, b| a.pubkey.cmp(&b.pubkey));
    let body = sorted.iter().map(|p| format!("{}:{}", p.pubkey, p.delta)).collect::<Vec<_>>().join(",");
    sha256_hex(format!("pokertui-result-v1|{room}|{hand_no}|{commit}|{body}").as_bytes())
}

/// The result of the hand a view just finished, without signatures. `None` if
/// fewer than two humans played (bot games never count).
pub fn summarize(view: &TableView) -> Option<Bundle> {
    let r = view.last_result.as_ref()?;
    let mut players: Vec<PlayerResult> = r
        .stacks
        .iter()
        .filter_map(|(seat, start, end)| {
            let pk = &view.seats.get(*seat)?.pubkey;
            (!pk.is_empty() && !pk.starts_with("bot:"))
                .then(|| PlayerResult { pubkey: pk.clone(), delta: *end as i64 - *start as i64 })
        })
        .collect();
    if players.len() < 2 {
        return None;
    }
    players.sort_by(|a, b| a.pubkey.cmp(&b.pubkey));
    Some(Bundle {
        hand_hash: hand_hash(&view.room, r.hand_no, &r.commit, &players),
        room: view.room.clone(),
        hand_no: r.hand_no,
        commit: r.commit.clone(),
        ts_ms: view.host_ts_ms,
        bb: view.big_blind.max(1),
        players,
        sigs: BTreeMap::new(),
    })
}

/// Collects co-signatures. Each node signs the result it computed itself, and only
/// accepts others' signatures over that same hash.
pub struct AckBook {
    me: Arc<Identity>,
    mine: HashMap<String, Bundle>,
    acks: HashMap<String, BTreeMap<String, String>>,
}

impl AckBook {
    pub fn new(me: Arc<Identity>) -> Self {
        Self { me, mine: HashMap::new(), acks: HashMap::new() }
    }

    /// A hand just finished: returns `(hand_hash, signature)` to broadcast if I played in it.
    pub fn on_result(&mut self, view: &TableView) -> Option<(String, String)> {
        let b = summarize(view)?;
        let me = self.me.pubkey_b64();
        if !b.players.iter().any(|p| p.pubkey == me) {
            return None;
        }
        let sig = self.me.sign_b64(b.hand_hash.as_bytes());
        let hash = b.hand_hash.clone();
        self.acks.entry(hash.clone()).or_default().insert(me, sig.clone());
        self.mine.insert(hash.clone(), b);
        while self.mine.len() > 200 {
            // Hands somebody never signed would pile up; forget the oldest.
            if let Some(old) = self.mine.values().min_by_key(|b| b.hand_no).map(|b| b.hand_hash.clone()) {
                self.mine.remove(&old);
                self.acks.remove(&old);
            }
        }
        Some((hash, sig))
    }

    pub fn on_ack(&mut self, from: &str, hash: &str, sig: &str) {
        if self.acks.len() < 1_000 && verify_b64(from, hash.as_bytes(), sig).is_ok() {
            self.acks.entry(hash.to_string()).or_default().insert(from.to_string(), sig.to_string());
        }
    }

    /// Hands every one of whose players has signed.
    pub fn take_complete(&mut self) -> Vec<Bundle> {
        let ready: Vec<String> = self
            .mine
            .iter()
            .filter(|(h, b)| {
                self.acks.get(*h).is_some_and(|m| b.players.iter().all(|p| m.contains_key(&p.pubkey)))
            })
            .map(|(h, _)| h.clone())
            .collect();
        let mut out = Vec::new();
        for h in ready {
            if let (Some(mut b), Some(sigs)) = (self.mine.remove(&h), self.acks.remove(&h)) {
                b.sigs = sigs.into_iter().filter(|(k, _)| b.players.iter().any(|p| &p.pubkey == k)).collect();
                out.push(b);
            }
        }
        out
    }
}

// ------------------------------------------------------------------ outbox

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Queued {
    pub hand_hash: String,
    pub queued_ms: u64,
    pub sealed: Sealed,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Outbox {
    pub items: Vec<Queued>,
    /// Do not contact the relay before this time (ms since the epoch).
    pub next_try_ms: u64,
    pub failures: u32,
    /// The profile line last accepted by the relay.
    pub profile_sent: String,
    /// Set when the relay refused the username ("taken").
    pub profile_error: Option<String>,
}

/// When to try again. Over quota → the next daily reset (00:00 UTC) plus a random
/// delay so every client doesn't hit the relay in the same second. Anything else
/// backs off 1 → 5 → 30 minutes.
pub fn next_retry_ms(now_ms: u64, failures: u32, quota: bool, jitter_ms: u64) -> u64 {
    if quota {
        return (now_ms / DAY_MS + 1) * DAY_MS + jitter_ms.min(10 * 60 * 1000);
    }
    let wait = [60_000, 300_000, 1_800_000][(failures as usize).min(2)];
    now_ms + wait
}

impl Outbox {
    pub fn load_from(dir: &Path) -> Outbox {
        std::fs::read_to_string(dir.join("outbox.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save_to(&self, dir: &Path) {
        if let Ok(text) = serde_json::to_string(self) {
            let _ = std::fs::write(dir.join("outbox.json"), text);
        }
    }

    /// Seal a finished hand to the relay's key and queue it. Safe to call twice.
    pub fn push(&mut self, bundle: &Bundle, server: &x25519_dalek::PublicKey, now_ms: u64) -> Result<()> {
        if self.items.iter().any(|q| q.hand_hash == bundle.hand_hash) {
            return Ok(());
        }
        let sealed = seal(server, &serde_json::to_vec(bundle)?)?;
        self.items.push(Queued { hand_hash: bundle.hand_hash.clone(), queued_ms: now_ms, sealed });
        self.trim(now_ms);
        Ok(())
    }

    /// Drop hands the relay would refuse as too old, and the oldest beyond the cap.
    pub fn trim(&mut self, now_ms: u64) {
        self.items.retain(|q| now_ms.saturating_sub(q.queued_ms) < MAX_AGE_MS);
        if self.items.len() > MAX_QUEUE {
            let extra = self.items.len() - MAX_QUEUE;
            self.items.drain(..extra);
        }
    }

    /// Time to send? Enough hands waiting, or the oldest one has waited long enough.
    pub fn due(&self, now_ms: u64) -> bool {
        now_ms >= self.next_try_ms
            && !self.items.is_empty()
            && (self.items.len() >= SEND_AT_COUNT
                || self.items.iter().map(|q| q.queued_ms).min().is_some_and(|t| now_ms.saturating_sub(t) >= SEND_AT_AGE_MS))
    }

    pub fn status(&self, now_ms: u64) -> Option<String> {
        if self.items.is_empty() {
            return self.profile_error.clone();
        }
        let wait = if self.next_try_ms > now_ms {
            let s = (self.next_try_ms - now_ms) / 1000;
            format!(", next try in {}h {:02}m", s / 3600, s % 3600 / 60)
        } else {
            String::new()
        };
        Some(format!("{} results waiting to upload{wait}", self.items.len()))
    }
}

static DISK: Mutex<()> = Mutex::new(());

/// Load, change and save the outbox under one process-wide lock.
pub fn with_outbox<R>(dir: &Path, f: impl FnOnce(&mut Outbox) -> R) -> R {
    let _guard = DISK.lock().unwrap_or_else(|e| e.into_inner());
    let mut ob = Outbox::load_from(dir);
    let r = f(&mut ob);
    ob.save_to(dir);
    r
}

// ------------------------------------------------------------------ talking to the relay

#[derive(Clone)]
pub struct LbConfig {
    /// `cf+wss://your-relay.workers.dev`
    pub relay: String,
    pub key: x25519_dalek::PublicKey,
    pub identity: Arc<Identity>,
    pub username: String,
    pub listed: bool,
    pub show_state: bool,
}

#[derive(Debug)]
pub struct Failure {
    pub quota: bool,
    pub msg: String,
}

fn looks_like_quota(text: &str) -> bool {
    ["429", "1015", "1027", "quota", "limit", "exceeded"].iter().any(|k| text.to_lowercase().contains(k))
}

/// Send request frames to `<relay>/lb` and collect one reply per frame.
pub async fn exchange(relay: &str, frames: Vec<serde_json::Value>, timeout: Duration) -> Result<Vec<serde_json::Value>, Failure> {
    let fail = |msg: String| Failure { quota: looks_like_quota(&msg), msg };
    let base = cf::ws_base(relay).ok_or_else(|| fail("relay must look like cf+wss://host".into()))?;
    let run = async {
        let (ws, _) = connect_async(format!("{base}/lb").as_str()).await.map_err(|e| fail(e.to_string()))?;
        let (mut sink, mut stream) = ws.split();
        let n = frames.len();
        for (i, mut f) in frames.into_iter().enumerate() {
            f["id"] = i.into();
            sink.send(Ws::Text(f.to_string().into())).await.map_err(|e| fail(e.to_string()))?;
        }
        let mut replies = vec![serde_json::Value::Null; n];
        let mut got = 0;
        while got < n {
            match stream.next().await {
                Some(Ok(Ws::Text(t))) => {
                    let v: serde_json::Value = serde_json::from_str(t.as_str()).map_err(|e| fail(e.to_string()))?;
                    if let Some(i) = v["id"].as_u64().map(|i| i as usize).filter(|i| *i < n) {
                        replies[i] = v;
                        got += 1;
                    }
                }
                Some(Ok(Ws::Close(c))) => return Err(fail(format!("closed: {c:?}"))),
                Some(Ok(_)) => {}
                _ => return Err(fail("connection dropped".into())),
            }
        }
        let _ = sink.close().await;
        Ok(replies)
    };
    tokio::time::timeout(timeout, run).await.map_err(|_| fail("timed out".into()))?
}

fn profile_line(c: &LbConfig) -> String {
    format!("pokertui-profile-v1|{}|{}|{}", c.username, c.listed as u8, c.show_state as u8)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct FlushReport {
    pub sent: usize,
    pub kept: usize,
    pub failed: Option<String>,
}

/// One upload attempt. Nothing is dropped unless the relay accepted or permanently
/// refused it. `force` ignores the batching thresholds (not the retry time).
pub async fn flush(dir: &Path, cfg: &LbConfig, now_ms: u64, jitter_ms: u64, force: bool) -> FlushReport {
    let (batch, profile) = with_outbox(dir, |ob| {
        ob.trim(now_ms);
        let go = now_ms >= ob.next_try_ms && !ob.items.is_empty() && (force || ob.due(now_ms));
        let want_profile = !cfg.username.is_empty() && ob.profile_sent != profile_line(cfg) && now_ms >= ob.next_try_ms;
        (go.then(|| ob.items.iter().take(BATCH).cloned().collect::<Vec<_>>()), want_profile)
    });
    if batch.is_none() && !profile {
        return FlushReport { sent: 0, kept: with_outbox(dir, |ob| ob.items.len()), failed: None };
    }
    let mut frames = Vec::new();
    if profile {
        frames.push(serde_json::json!({
            "op": "lb_profile",
            "pubkey": cfg.identity.pubkey_b64(),
            "username": cfg.username,
            "listed": cfg.listed,
            "show_state": cfg.show_state,
            "sig": cfg.identity.sign_b64(profile_line(cfg).as_bytes()),
        }));
    }
    if let Some(b) = &batch {
        frames.push(serde_json::json!({ "op": "lb_hands", "items": b.iter().map(|q| &q.sealed).collect::<Vec<_>>() }));
    }
    match exchange(&cfg.relay, frames, Duration::from_secs(20)).await {
        Err(f) => with_outbox(dir, |ob| {
            ob.next_try_ms = next_retry_ms(now_ms, ob.failures, f.quota, jitter_ms);
            ob.failures += 1;
            FlushReport { sent: 0, kept: ob.items.len(), failed: Some(f.msg) }
        }),
        Ok(replies) => with_outbox(dir, |ob| {
            let mut replies = replies.into_iter();
            if profile && let Some(r) = replies.next() {
                if r["ok"].as_bool() == Some(true) {
                    ob.profile_sent = profile_line(cfg);
                    ob.profile_error = None;
                } else {
                    ob.profile_error = r["error"].as_str().map(|e| if e == "taken" { "username taken: pick another".to_string() } else { e.to_string() });
                }
            }
            let mut sent = 0;
            if let (Some(b), Some(r)) = (&batch, replies.next()) {
                if r["ok"].as_bool() == Some(true) {
                    let results = r["results"].as_array().cloned().unwrap_or_default();
                    // Anything the relay answered (accepted, duplicate or refused) is done with.
                    let done: Vec<&str> = b
                        .iter()
                        .zip(results.iter())
                        .filter(|(_, s)| s.as_str().is_some())
                        .map(|(q, _)| q.hand_hash.as_str())
                        .collect();
                    sent = done.len();
                    ob.items.retain(|q| !done.contains(&q.hand_hash.as_str()));
                } else {
                    let quota = r["error"].as_str().is_some_and(looks_like_quota);
                    ob.next_try_ms = next_retry_ms(now_ms, ob.failures, quota, jitter_ms);
                    ob.failures += 1;
                    return FlushReport { sent: 0, kept: ob.items.len(), failed: r["error"].as_str().map(String::from) };
                }
            }
            ob.failures = 0;
            ob.next_try_ms = 0;
            FlushReport { sent, kept: ob.items.len(), failed: None }
        }),
    }
}

// ------------------------------------------------------------------ the board itself

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardRow {
    pub rank: u32,
    pub username: String,
    pub state: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Board {
    pub fetched_ms: u64,
    pub rows: Vec<BoardRow>,
}

pub fn cached_board(dir: &Path) -> Option<Board> {
    serde_json::from_str(&std::fs::read_to_string(dir.join("leaderboard-cache.json")).ok()?).ok()
}

pub async fn fetch_board(dir: &Path, relay: &str, now_ms: u64) -> Result<Board> {
    let replies = exchange(relay, vec![serde_json::json!({ "op": "lb_get", "top": 100 })], Duration::from_secs(15))
        .await
        .map_err(|f| anyhow::anyhow!(f.msg))?;
    let rows: Vec<BoardRow> =
        serde_json::from_value(replies[0]["rows"].clone()).context("the relay sent an unreadable leaderboard")?;
    let board = Board { fetched_ms: now_ms, rows };
    let _ = std::fs::write(dir.join("leaderboard-cache.json"), serde_json::to_string(&board)?);
    Ok(board)
}

pub fn server_key(b64: &str) -> Result<x25519_dalek::PublicKey> {
    parse_enc_pubkey(b64).context("relay_key must be the public key printed by relay/cloudflare/scripts/gen-key.mjs")
}

/// Background task: uploads queued hands when due. Runs for the life of the program.
pub async fn uploader(dir: std::path::PathBuf, cfg: LbConfig) {
    loop {
        let now = crate::net::protocol::now_ms();
        let jitter = crate::crypto::random_bytes::<4>().map(|b| u32::from_le_bytes(b) as u64 % 600_000).unwrap_or(0);
        let _ = flush(&dir, &cfg, now, jitter, false).await;
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::{Move, Phase, Table, TableParams};

    fn hand_between(ids: &[&Identity]) -> TableView {
        let mut t = Table::new(TableParams { max_seats: 4, small_blind: 10, big_blind: 20, buyin: 1_000, turn_ms: 0, tourney: None });
        for (i, id) in ids.iter().enumerate() {
            t.join(&format!("p{i}"), &id.pubkey_b64(), &id.enc_pub_b64(), 1_000).unwrap();
        }
        t.start_hand(0).unwrap();
        while t.phase == Phase::Betting {
            let a = t.actor.unwrap();
            let l = t.legal(a).unwrap();
            t.apply(a, if l.can_check { Move::Check } else { Move::Call }, 0).unwrap();
        }
        let mut v = t.view("room");
        v.host_ts_ms = 1_700_000_000_000;
        v
    }

    #[test]
    fn every_player_gets_the_same_hash_and_the_bundle_needs_all_signatures() {
        let ids: Vec<Arc<Identity>> = (0..3).map(|_| Arc::new(Identity::generate().unwrap())).collect();
        let view = hand_between(&[&ids[0], &ids[1], &ids[2]]);
        let mut books: Vec<AckBook> = ids.iter().map(|i| AckBook::new(i.clone())).collect();
        let acks: Vec<(String, String)> = books.iter_mut().map(|b| b.on_result(&view).unwrap()).collect();
        assert!(acks.iter().all(|(h, _)| *h == acks[0].0), "same hash everywhere");

        // Without the third signature the first node's bundle is not complete.
        books[0].on_ack(&ids[1].pubkey_b64(), &acks[1].0, &acks[1].1);
        assert!(books[0].take_complete().is_empty());
        // A forged signature (someone else's) does not count.
        books[0].on_ack(&ids[2].pubkey_b64(), &acks[2].0, &acks[1].1);
        assert!(books[0].take_complete().is_empty());
        books[0].on_ack(&ids[2].pubkey_b64(), &acks[2].0, &acks[2].1);
        let done = books[0].take_complete();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].sigs.len(), 3);
        assert!(books[0].take_complete().is_empty(), "handed out once");
    }

    #[test]
    fn bot_games_and_spectators_do_not_count() {
        let id = Arc::new(Identity::generate().unwrap());
        let mut t = Table::new(TableParams { max_seats: 3, small_blind: 10, big_blind: 20, buyin: 500, turn_ms: 0, tourney: None });
        t.join("me", &id.pubkey_b64(), &id.enc_pub_b64(), 500).unwrap();
        t.join_bot("bot", crate::bot::Skill::Easy).unwrap();
        t.start_hand(0).unwrap();
        while t.phase == Phase::Betting {
            let a = t.actor.unwrap();
            t.apply(a, Move::Fold, 0).unwrap();
        }
        assert!(summarize(&t.view("room")).is_none(), "one human only");
        let other = Identity::generate().unwrap();
        let view = hand_between(&[&id, &other]);
        assert!(AckBook::new(Arc::new(Identity::generate().unwrap())).on_result(&view).is_none(), "not a player");
    }

    #[test]
    fn retry_waits_for_the_daily_reset_with_jitter() {
        let noon = 5 * DAY_MS + 12 * 3600 * 1000;
        let t = next_retry_ms(noon, 0, true, 123_000);
        assert_eq!(t, 6 * DAY_MS + 123_000, "next 00:00 UTC plus jitter");
        assert_eq!(next_retry_ms(noon, 0, true, u64::MAX), 6 * DAY_MS + 600_000, "jitter capped at 10 min");
        assert_eq!(next_retry_ms(noon, 0, false, 0), noon + 60_000);
        assert_eq!(next_retry_ms(noon, 1, false, 0), noon + 300_000);
        assert_eq!(next_retry_ms(noon, 7, false, 0), noon + 1_800_000);
    }

    fn bundle(n: u32) -> Bundle {
        let players = vec![
            PlayerResult { pubkey: "a".into(), delta: 10 },
            PlayerResult { pubkey: "b".into(), delta: -10 },
        ];
        Bundle {
            hand_hash: hand_hash("room", n, "c", &players),
            room: "room".into(),
            hand_no: n,
            commit: "c".into(),
            ts_ms: 0,
            bb: 20,
            players,
            sigs: BTreeMap::new(),
        }
    }

    #[test]
    fn outbox_dedupes_caps_expires_and_batches() {
        let server = Identity::generate().unwrap().enc_pub();
        let mut ob = Outbox::default();
        ob.push(&bundle(1), &server, 1_000).unwrap();
        ob.push(&bundle(1), &server, 1_000).unwrap();
        assert_eq!(ob.items.len(), 1, "same hand twice");
        assert!(!ob.due(2_000), "too few, too fresh");
        assert!(ob.due(1_000 + SEND_AT_AGE_MS), "oldest has waited long enough");
        ob.next_try_ms = 5_000_000;
        assert!(!ob.due(1_000 + SEND_AT_AGE_MS), "retry time not reached");
        ob.next_try_ms = 0;

        for n in 2..=SEND_AT_COUNT as u32 {
            ob.push(&bundle(n), &server, 1_000).unwrap();
        }
        assert!(ob.due(1_001), "enough hands waiting");

        ob.trim(1_000 + MAX_AGE_MS + 1);
        assert!(ob.items.is_empty(), "30-day expiry");

        let mut big = Outbox::default();
        for n in 0..(MAX_QUEUE as u32 + 10) {
            big.items.push(Queued { hand_hash: n.to_string(), queued_ms: 1, sealed: ob_sealed(&server) });
        }
        big.trim(2);
        assert_eq!(big.items.len(), MAX_QUEUE);
        assert_eq!(big.items[0].hand_hash, "10", "oldest dropped first");
    }

    fn ob_sealed(server: &x25519_dalek::PublicKey) -> Sealed {
        seal(server, b"x").unwrap()
    }

    /// A relay that is over quota (HTTP 429 on connect) for the first `refuse` connections,
    /// then accepts everything. It opens each sealed hand with the relay key and re-verifies
    /// the signatures, like the real Worker.
    async fn fake_relay(server: Arc<Identity>, refuse: usize) -> (String, Arc<Mutex<Vec<Bundle>>>) {
        use async_tungstenite::tokio::accept_hdr_async;
        use async_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("cf+ws://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let got: Arc<Mutex<Vec<Bundle>>> = Arc::default();
        let seen = Arc::new(Mutex::new(0usize));
        let out = got.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { continue };
                let (server, got, seen) = (server.clone(), out.clone(), seen.clone());
                tokio::spawn(async move {
                    let n = {
                        let mut s = seen.lock().unwrap();
                        *s += 1;
                        *s
                    };
                    let cb = move |_: &Request, resp: Response| -> Result<Response, ErrorResponse> {
                        if n <= refuse {
                            let mut e = ErrorResponse::new(Some("quota".into()));
                            *e.status_mut() = async_tungstenite::tungstenite::http::StatusCode::TOO_MANY_REQUESTS;
                            return Err(e);
                        }
                        Ok(resp)
                    };
                    let Ok(ws) = accept_hdr_async(stream, cb).await else { return };
                    let (mut sink, mut stream) = ws.split();
                    while let Some(Ok(Ws::Text(t))) = stream.next().await {
                        let f: serde_json::Value = serde_json::from_str(t.as_str()).unwrap();
                        let reply = match f["op"].as_str() {
                            Some("lb_profile") => serde_json::json!({"id": f["id"], "ok": true}),
                            Some("lb_hands") => {
                                let mut results = Vec::new();
                                for item in f["items"].as_array().unwrap() {
                                    let sealed: Sealed = serde_json::from_value(item.clone()).unwrap();
                                    let b: Bundle = serde_json::from_slice(&server.open(&sealed).unwrap()).unwrap();
                                    let ok = b.players.iter().all(|p| {
                                        b.sigs.get(&p.pubkey).is_some_and(|s| verify_b64(&p.pubkey, b.hand_hash.as_bytes(), s).is_ok())
                                    });
                                    results.push(if ok { got.lock().unwrap().push(b); "accepted" } else { "rejected: signature" });
                                }
                                serde_json::json!({"id": f["id"], "ok": true, "results": results})
                            }
                            _ => serde_json::json!({"id": f["id"], "ok": false, "error": "unknown op"}),
                        };
                        let _ = sink.send(Ws::Text(reply.to_string().into())).await;
                    }
                });
            }
        });
        (url, got)
    }

    /// The headline behaviour: the relay is over quota, results queue up on disk,
    /// and everything is delivered exactly once after the reset.
    #[tokio::test]
    async fn results_wait_out_a_quota_and_arrive_after_the_reset() {
        let server = Arc::new(Identity::generate().unwrap());
        let (url, got) = fake_relay(server.clone(), 2).await;
        let dir = std::env::temp_dir().join(format!("pokertui-lb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _ = std::fs::remove_file(dir.join("outbox.json"));

        let (a, b) = (Arc::new(Identity::generate().unwrap()), Arc::new(Identity::generate().unwrap()));
        let cfg = LbConfig {
            relay: url,
            key: server.enc_pub(),
            identity: a.clone(),
            username: "tester".into(),
            listed: true,
            show_state: false,
        };
        // Two real, fully co-signed hands.
        let mut queued = Vec::new();
        for _ in 0..2 {
            let view = hand_between(&[&a, &b]);
            let (mut ba, mut bb) = (AckBook::new(a.clone()), AckBook::new(b.clone()));
            let (ha, sa) = ba.on_result(&view).unwrap();
            let (hb, sb) = bb.on_result(&view).unwrap();
            ba.on_ack(&b.pubkey_b64(), &hb, &sb);
            bb.on_ack(&a.pubkey_b64(), &ha, &sa);
            queued.push(ba.take_complete().pop().unwrap());
        }
        let day = 100 * DAY_MS;
        with_outbox(&dir, |ob| {
            for b in queued.iter().chain(&queued) {
                ob.push(b, &server.enc_pub(), day + 1_000).unwrap(); // second pass is a no-op
            }
        });
        assert_eq!(with_outbox(&dir, |ob| ob.items.len()), 2);

        // Quota: refused. Nothing lost, retry set to the next reset.
        let now = day + 12 * 3600 * 1000;
        let r = flush(&dir, &cfg, now, 5_000, true).await;
        assert!(r.failed.is_some() && r.sent == 0 && r.kept == 2, "{r:?}");
        let ob = with_outbox(&dir, |ob| ob.clone());
        assert_eq!(ob.next_try_ms, day + DAY_MS + 5_000, "waits for 00:00 UTC + jitter");

        // Before the reset it does not even try.
        let r = flush(&dir, &cfg, now + 1_000, 0, true).await;
        assert_eq!(r, FlushReport { sent: 0, kept: 2, failed: None });
        assert!(got.lock().unwrap().is_empty());

        // After the reset the first connection is still refused (second 429) ...
        let after = day + DAY_MS + 6_000;
        let r = flush(&dir, &cfg, after, 0, true).await;
        assert!(r.failed.is_some(), "{r:?}");
        // ... and then it gets through, exactly once.
        let later = with_outbox(&dir, |ob| ob.next_try_ms) + 1;
        let r = flush(&dir, &cfg, later, 0, true).await;
        assert_eq!(r.failed, None, "{r:?}");
        assert_eq!((r.sent, r.kept), (2, 0));
        assert_eq!(got.lock().unwrap().len(), 2);
        assert_eq!(with_outbox(&dir, |ob| (ob.failures, ob.next_try_ms, ob.profile_sent.clone())).2, profile_line(&cfg));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
