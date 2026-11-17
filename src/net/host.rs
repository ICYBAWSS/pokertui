//! The authoritative host: owns the table, deals cards, validates actions and
//! publishes signed snapshots. Also runs bots, tournaments, the joint shuffle
//! and the delayed spectator feed.

use super::mqtt::Link;
use super::protocol::{Envelope, Payload, Topics, now_ms};
use super::{NetCommand, UiEvent, sanitize_text};
use crate::bot::{self, Skill};
use crate::crypto::{Identity, b64, b64_decode, mix_seed, parse_enc_pubkey, random_bytes, seal, seed_commit};
use crate::game::{Card, Move, Phase, SeedReveal, Table, TableParams, TableView};
use crate::leaderboard::AckBook;
use crate::net::roomcode::RoomId;
use crate::store;
use anyhow::{Result, bail};
use rand_chacha::ChaCha20Rng;
use rand_chacha::rand_core::{Rng, SeedableRng};
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct HostConfig {
    pub identity: Arc<Identity>,
    pub code: String,
    pub room: RoomId,
    pub name: String,
    pub params: TableParams,
    pub broker: Option<String>,
    pub auto_rebuy: bool,
    /// Bots seated at start.
    pub bots: Vec<Skill>,
    /// Bots wait a random time in this range before acting, so they feel human.
    pub bot_think_ms: (u64, u64),
    /// Seconds of delay for the hole-card spectator feed; 0 turns it off.
    pub spectate_secs: u64,
    /// The host re-broadcasts at least this often so clients know it is alive.
    pub heartbeat_secs: u64,
    /// Write hand history and bans to disk.
    pub persist: bool,
    /// Tournaments deal the next hand this many seconds after one ends.
    pub auto_deal_secs: u64,
    /// New players wait until the host accepts them (returning players don't).
    pub approve_joins: bool,
}

impl HostConfig {
    pub fn new(identity: Arc<Identity>, code: String, room: RoomId, name: String, params: TableParams) -> Self {
        Self {
            identity,
            code,
            room,
            name,
            params,
            broker: None,
            auto_rebuy: false,
            bots: Vec::new(),
            bot_think_ms: (600, 2_000),
            spectate_secs: 0,
            heartbeat_secs: 5,
            persist: true,
            auto_deal_secs: 5,
            approve_joins: false,
        }
    }
}

/// Waiting for players' randomness before dealing.
struct Pending {
    secret: [u8; 32],
    commit: String,
    contribs: BTreeMap<usize, [u8; 32]>,
    expect: Vec<usize>,
    started: Instant,
}

struct Waiting {
    pubkey: String,
    name: String,
    buyin: u64,
    enc_pub: String,
    seen: Instant,
}

struct Frame {
    at: Instant,
    hand: u32,
    view: TableView,
}

pub struct Host {
    cfg: HostConfig,
    table: Table,
    topics: Topics,
    tx: Sender<UiEvent>,
    seq: u64,
    host_seat: usize,
    epoch: u32,
    last_seen: Vec<Option<Instant>>,
    last_chat: Vec<Option<Instant>>,
    last_settled: Option<u32>,
    last_version: u64,
    last_broadcast: Instant,
    dealt_at: Instant,
    reported_disconnect: bool,
    banned: HashSet<String>,
    kick_queue: Vec<usize>,
    pending: Option<Pending>,
    rng: ChaCha20Rng,
    bot_due: Option<(usize, Instant)>,
    hand_over_at: Option<Instant>,
    tourney_start_ms: Option<u64>,
    tourney_level: usize,
    frames: VecDeque<Frame>,
    acks: AckBook,
    pending_ack: Option<Payload>,
    waiting: Vec<Waiting>,
}

pub async fn run_host(
    cfg: HostConfig,
    tx: Sender<UiEvent>,
    rx: mpsc::UnboundedReceiver<NetCommand>,
) -> Result<()> {
    let link = Link::connect_room(cfg.broker.as_deref(), &Topics::new(&cfg.room).room).await?;
    run_host_on(link, cfg, tx, rx).await
}

pub async fn run_host_on(
    mut link: Link,
    cfg: HostConfig,
    tx: Sender<UiEvent>,
    rx: mpsc::UnboundedReceiver<NetCommand>,
) -> Result<()> {
    let topics = Topics::new(&cfg.room);
    link.subscribe(&topics.events()).await?;
    let mut host = Host::new(cfg, topics, tx.clone());
    host.bootstrap(&mut link).await?;
    host_loop(link, host, tx, rx).await
}

/// The host's event loop. A client that takes over a dead host enters here too.
pub async fn host_loop(
    mut link: Link,
    mut host: Host,
    tx: Sender<UiEvent>,
    mut rx: mpsc::UnboundedReceiver<NetCommand>,
) -> Result<()> {
    let snapshot_topic = host.topics.snapshot();
    let mut ticker = tokio::time::interval(Duration::from_millis(250));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            msg = link.recv() => {
                if let Some(msg) = msg
                    && let Err(e) = host.on_message(&mut link, msg).await
                {
                    tracing::warn!(error = %e, "host message handling failed");
                }
            }
            cmd = rx.recv() => {
                match cmd {
                    None | Some(NetCommand::Quit) => break,
                    Some(cmd) => {
                        if let Err(e) = host.on_command(&mut link, cmd).await {
                            tracing::warn!(error = %e, "host command failed");
                        }
                    }
                }
            }
            _ = ticker.tick() => {
                if let Err(e) = host.tick(&mut link).await {
                    tracing::warn!(error = %e, "host tick failed");
                }
            }
        }
    }

    // Tell everyone, then clear the retained snapshot so the table does not
    // appear alive after exit.
    if let Ok(env) = host.envelope(Payload::Closed { reason: "the host closed the table".into() })
        && let Ok(bytes) = env.to_bytes()
    {
        host.publish(&mut link, &host.topics.events(), bytes, false).await;
    }
    let _ = link.publish(&snapshot_topic, Vec::new(), true).await;
    let _ = tx.send(UiEvent::Closed("table closed".into()));
    Ok(())
}

impl Host {
    pub fn new(cfg: HostConfig, topics: Topics, tx: Sender<UiEvent>) -> Self {
        let mut cfg = cfg;
        if cfg.params.tourney.is_some() {
            cfg.auto_rebuy = false;
        }
        let table = Table::new(cfg.params.clone());
        let cfg_identity = cfg.identity.clone();
        let seats = table.params.max_seats;
        let banned = if cfg.persist { load_bans(&cfg.code) } else { HashSet::new() };
        Self {
            cfg,
            table,
            topics,
            tx,
            seq: now_ms(),
            host_seat: 0,
            epoch: 0,
            last_seen: vec![None; seats],
            last_chat: vec![None; seats],
            last_settled: None,
            last_version: 0,
            last_broadcast: Instant::now(),
            dealt_at: Instant::now(),
            reported_disconnect: false,
            banned,
            kick_queue: Vec::new(),
            pending: None,
            rng: ChaCha20Rng::from_seed(random_bytes::<32>().unwrap_or([7; 32])),
            bot_due: None,
            hand_over_at: None,
            tourney_start_ms: None,
            tourney_level: 0,
            frames: VecDeque::new(),
            acks: AckBook::new(cfg_identity),
            pending_ack: None,
            waiting: Vec::new(),
        }
    }

    /// Rebuild a host from the last view after the old one vanished. The hand
    /// in progress is cancelled and everyone gets their pre-hand stack back.
    pub fn from_view(cfg: HostConfig, topics: Topics, tx: Sender<UiEvent>, view: &TableView) -> Result<Self> {
        let mut cfg = cfg;
        cfg.params = view.params.clone();
        let mut host = Host::new(cfg, topics, tx);
        host.table = Table::from_view(view);
        let seat = host
            .table
            .seat_of(&host.cfg.identity.pubkey_b64())
            .ok_or_else(|| anyhow::anyhow!("not seated at this table"))?;
        host.host_seat = seat;
        host.epoch = view.epoch + 1;
        host.banned.extend(view.banned.iter().cloned());
        // The old host sits out until it comes back (rejoining clears this).
        if let Some(old) = host.table.seat_of(&view.host)
            && let Some(Some(s)) = host.table.seats.get_mut(old)
        {
            s.connected = false;
            s.sitting_out = true;
        }
        let now = Instant::now();
        for (i, s) in host.table.players() {
            if s.connected
                && let Some(slot) = host.last_seen.get_mut(i)
            {
                *slot = Some(now);
            }
        }
        Ok(host)
    }

    pub async fn bootstrap(&mut self, link: &mut Link) -> Result<()> {
        let seat = self.table.join(
            &self.cfg.name,
            &self.cfg.identity.pubkey_b64(),
            &self.cfg.identity.enc_pub_b64(),
            self.cfg.params.buyin,
        )?;
        self.host_seat = seat;
        self.table.set_ready(seat, true);
        self.last_seen[seat] = Some(Instant::now());
        for skill in self.cfg.bots.clone() {
            let _ = self.add_bot(skill);
        }

        let _ = self.tx.send(UiEvent::Connected {
            broker: link.broker.clone(),
        });
        let _ = self.tx.send(UiEvent::Joined { seat });
        self.broadcast(link).await
    }

    /// Announce a takeover: same room, same seats, new host key.
    pub async fn bootstrap_promoted(&mut self, link: &mut Link) -> Result<()> {
        let _ = self.tx.send(UiEvent::Joined { seat: self.host_seat });
        let _ = self.tx.send(UiEvent::Promoted);
        let _ = self.tx.send(UiEvent::Info(
            "the old host left: you are the host now. The interrupted hand was cancelled.".into(),
        ));
        self.table.note("host changed: the interrupted hand was cancelled");
        self.table.bump();
        self.broadcast(link).await
    }

    fn add_bot(&mut self, skill: Skill) -> Result<usize> {
        let taken: HashSet<String> = self.table.players().map(|(_, s)| s.name.clone()).collect();
        let base = bot::NAMES
            .iter()
            .map(|n| format!("{n} ({})", skill.label()))
            .find(|n| !taken.contains(n))
            .unwrap_or_else(|| format!("Bot {}", taken.len()));
        let seat = self.table.join_bot(&base, skill)?;
        self.last_seen[seat] = Some(Instant::now());
        Ok(seat)
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn envelope(&mut self, payload: Payload) -> Result<Envelope> {
        let seq = self.next_seq();
        let room = self.topics.room.clone();
        Envelope::new(&self.cfg.identity, &room, seq, payload)
    }

    fn tourney_next_level_ms(&self) -> u64 {
        match (&self.table.params.tourney, self.tourney_start_ms) {
            (Some(t), Some(start)) if self.tourney_level + 1 < t.levels.len() => {
                start + (self.tourney_level as u64 + 1) * t.level_secs * 1000
            }
            _ => 0,
        }
    }

    fn view(&self) -> TableView {
        let mut view = self.table.view(&self.topics.room);
        view.host = self.cfg.identity.pubkey_b64();
        view.epoch = self.epoch;
        view.host_ts_ms = now_ms();
        view.seed_commit = self.pending.as_ref().map(|p| p.commit.clone());
        view.banned = self.banned.iter().cloned().collect();
        view.tourney = self.table.tourney_view(self.tourney_level, self.tourney_next_level_ms());
        view.spectate = self.cfg.spectate_secs > 0;
        view
    }

    /// Encrypts for the room, then hands to the relay.
    async fn publish(&mut self, link: &mut Link, topic: &str, payload: Vec<u8>, retain: bool) {
        let payload = if payload.is_empty() {
            payload
        } else {
            match self.topics.wrap(&payload) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(error = %e, "could not encrypt packet");
                    return;
                }
            }
        };
        if let Err(e) = link.publish(topic, payload, retain).await {
            tracing::warn!(error = %e, "publish failed");
            if !self.reported_disconnect {
                self.reported_disconnect = true;
                let _ = self.tx.send(UiEvent::Disconnected(e.to_string()));
            }
        } else {
            self.reported_disconnect = false;
        }
    }

    async fn broadcast(&mut self, link: &mut Link) -> Result<()> {
        let view = self.view();
        let env = self.envelope(Payload::State { view: view.clone() })?;
        let bytes = env.to_bytes()?;
        self.publish(link, &self.topics.events(), bytes.clone(), false)
            .await;
        self.publish(link, &self.topics.snapshot(), bytes, true).await;
        self.last_version = view.version;
        self.last_broadcast = Instant::now();
        if self.cfg.spectate_secs > 0 {
            let mut shown = view.clone();
            for sv in &mut shown.seats {
                if let Some(Some(s)) = self.table.seats.get(sv.seat)
                    && !s.hole.is_empty()
                {
                    sv.reveal = s.hole.clone();
                }
            }
            self.frames.push_back(Frame { at: Instant::now(), hand: view.hand_no, view: shown });
            while self.frames.len() > 2_000 {
                self.frames.pop_front();
            }
        }
        let _ = self.tx.send(UiEvent::View(view));
        if let Some(ack) = self.pending_ack.take() {
            let env = self.envelope(ack)?;
            self.publish(link, &self.topics.events(), env.to_bytes()?, false).await;
        }
        Ok(())
    }

    fn emit_bundles(&mut self) {
        for b in self.acks.take_complete() {
            let _ = self.tx.send(UiEvent::Bundle(b));
        }
    }

    async fn on_message(&mut self, link: &mut Link, msg: super::mqtt::Message) -> Result<()> {
        if msg.topic != self.topics.events() {
            return Ok(());
        }
        let Ok(plain) = self.topics.unwrap(&msg.payload) else {
            return Ok(()); // not encrypted for this room
        };
        let env = match Envelope::parse(&plain) {
            Ok(env) => env,
            Err(e) => {
                tracing::debug!(error = %e, "ignoring unverifiable packet");
                return Ok(());
            }
        };
        if env.room != self.topics.room {
            return Ok(());
        }
        if env.from == self.cfg.identity.pubkey_b64() {
            return Ok(());
        }
        let seat_of = self.table.seat_of(&env.from);
        if let Some(seat) = seat_of {
            if let Some(slot) = self.last_seen.get_mut(seat) {
                *slot = Some(Instant::now());
            }
            self.table.set_connected(seat, true);
        }

        match env.payload {
            Payload::JoinRequest {
                name,
                buyin,
                enc_pub,
            } => self.on_join_request(link, &env.from, &name, buyin, &enc_pub).await,
            Payload::Action { hand, version, mv } => {
                let Some(seat) = seat_of else {
                    return self
                        .send_error(link, &env.from, "you are not seated at this table")
                        .await;
                };
                self.on_action(link, seat, hand, version, mv).await
            }
            Payload::Ready { ready } => {
                if let Some(seat) = seat_of {
                    self.table.set_ready(seat, ready);
                    if ready
                        && let Some(Some(s)) = self.table.seats.get_mut(seat)
                    {
                        s.sitting_out = false;
                        s.timeout_streak = 0;
                    }
                    self.table.bump();
                    self.broadcast(link).await?;
                }
                Ok(())
            }
            Payload::Entropy { hand, r } => {
                if let Some(seat) = seat_of
                    && let Some(p) = self.pending.as_mut()
                    && hand == self.table.hand_no + 1
                    && p.expect.contains(&seat)
                    && let Ok(bytes) = b64_decode(&r)
                    && let Ok(r) = <[u8; 32]>::try_from(bytes)
                {
                    p.contribs.entry(seat).or_insert(r);
                }
                Ok(())
            }
            Payload::Chat { text, .. } => {
                let Some(seat) = seat_of else {
                    return Ok(());
                };
                self.on_chat(link, seat, &text).await
            }
            Payload::Ping { version, cards_hand } => {
                // Public brokers silently drop bursts (EMQX does), so pings
                // report what the client holds and we resend what it lacks.
                // The 1 s guard skips pings that raced an update in flight.
                if version < self.last_version
                    && self.last_broadcast.elapsed() > Duration::from_secs(1)
                {
                    self.broadcast(link).await?;
                }
                if let Some(seat) = seat_of
                    && self.table.phase == Phase::Betting
                    && cards_hand != self.table.hand_no
                    && self.dealt_at.elapsed() > Duration::from_secs(1)
                {
                    self.deal(link, Some(seat)).await;
                }
                Ok(())
            }
            Payload::ResultAck { hand_hash, sig } => {
                self.acks.on_ack(&env.from, &hand_hash, &sig);
                self.emit_bundles();
                Ok(())
            }
            Payload::RequestState { .. } => self.broadcast(link).await,
            Payload::Leave {} => {
                if let Some(seat) = seat_of {
                    self.table.leave(seat)?;
                    self.broadcast(link).await?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    async fn reject(&mut self, link: &mut Link, to: &str, reason: &str) -> Result<()> {
        let env = self.envelope(Payload::JoinReject {
            to: to.to_string(),
            reason: reason.to_string(),
        })?;
        self.publish(link, &self.topics.events(), env.to_bytes()?, false).await;
        Ok(())
    }

    fn announce_waiting(&self) {
        let _ = self.tx.send(UiEvent::Waiting(self.waiting.iter().map(|w| w.name.clone()).collect()));
    }

    /// With approval on, strangers wait for the host; seated players walk straight back in.
    async fn on_join_request(
        &mut self,
        link: &mut Link,
        pubkey: &str,
        name: &str,
        buyin: u64,
        enc_pub: &str,
    ) -> Result<()> {
        let known = self.table.seat_of(pubkey).is_some();
        if !self.cfg.approve_joins || known || self.banned.contains(pubkey) {
            return self.on_join(link, pubkey, name, buyin, enc_pub).await;
        }
        if let Some(w) = self.waiting.iter_mut().find(|w| w.pubkey == pubkey) {
            w.seen = Instant::now(); // the client re-asks every couple of seconds
        } else if self.waiting.len() < 10 {
            self.waiting.push(Waiting {
                pubkey: pubkey.to_string(),
                name: sanitize_text(name, 16),
                buyin,
                enc_pub: enc_pub.to_string(),
                seen: Instant::now(),
            });
            self.announce_waiting();
        } else {
            return self.send_error(link, pubkey, "too many people waiting to join").await;
        }
        let env = self.envelope(Payload::JoinPending { to: pubkey.to_string() })?;
        self.publish(link, &self.topics.events(), env.to_bytes()?, false).await;
        Ok(())
    }

    async fn on_join(
        &mut self,
        link: &mut Link,
        pubkey: &str,
        name: &str,
        buyin: u64,
        enc_pub: &str,
    ) -> Result<()> {
        if self.banned.contains(pubkey) {
            return self.reject(link, pubkey, "you are banned from this table").await;
        }
        let name = sanitize_text(name, 16);
        let name = if name.is_empty() {
            format!("player{}", &pubkey[..4.min(pubkey.len())])
        } else {
            name
        };
        if parse_enc_pubkey(enc_pub).is_err() {
            return self.send_error(link, pubkey, "bad encryption key").await;
        }
        let existing = self.table.seat_of(pubkey);
        if existing.is_none()
            && self
                .table
                .players()
                .any(|(_, s)| s.name.eq_ignore_ascii_case(&name))
        {
            return self.reject(link, pubkey, "that name is already taken at this table").await;
        }
        match self.table.join(&name, pubkey, enc_pub, buyin) {
            Ok(seat) => {
                self.table.set_connected(seat, true);
                if let Some(slot) = self.last_seen.get_mut(seat) {
                    *slot = Some(Instant::now());
                }
                if existing.is_some() {
                    // Same player coming back: give the seat and cards back.
                    if let Some(Some(s)) = self.table.seats.get_mut(seat) {
                        s.sitting_out = false;
                        s.timeout_streak = 0;
                    }
                    self.table.bump();
                    if self.table.phase == Phase::Betting {
                        self.deal(link, Some(seat)).await;
                    }
                }
                let view = self.view();
                let env = self.envelope(Payload::JoinAccept {
                    to: pubkey.to_string(),
                    seat,
                    params: self.table.params.clone(),
                    view: view.clone(),
                })?;
                self.publish(link, &self.topics.events(), env.to_bytes()?, false)
                    .await;
                self.broadcast(link).await
            }
            Err(e) => self.send_error(link, pubkey, &e.to_string()).await,
        }
    }

    async fn on_action(
        &mut self,
        link: &mut Link,
        seat: usize,
        hand: u32,
        version: u64,
        mv: Move,
    ) -> Result<()> {
        let pubkey = self
            .table
            .seats
            .get(seat)
            .and_then(|s| s.as_ref())
            .map(|s| s.pubkey.clone())
            .unwrap_or_default();
        if hand != self.table.hand_no {
            return self
                .send_error(link, &pubkey, "that hand is already over")
                .await;
        }
        if version != self.table.version {
            self.send_error(link, &pubkey, "table changed, resyncing").await?;
            return self.broadcast(link).await;
        }
        match self.table.apply(seat, mv, now_ms()) {
            Ok(_) => {
                if let Some(Some(s)) = self.table.seats.get_mut(seat) {
                    s.timeout_streak = 0;
                }
                self.after_change(link).await
            }
            Err(e) => {
                self.send_error(link, &pubkey, &e.to_string()).await?;
                self.broadcast(link).await
            }
        }
    }

    async fn on_chat(&mut self, link: &mut Link, seat: usize, text: &str) -> Result<()> {
        let now = Instant::now();
        if let Some(last) = self.last_chat.get(seat).and_then(|v| *v)
            && now.duration_since(last) < Duration::from_millis(700)
        {
            return Ok(());
        }
        if let Some(slot) = self.last_chat.get_mut(seat) {
            *slot = Some(now);
        }
        let text = sanitize_text(text, 400);
        if text.is_empty() {
            return Ok(());
        }
        let name = self
            .table
            .seats
            .get(seat)
            .and_then(|s| s.as_ref())
            .map(|s| s.name.clone())
            .unwrap_or_default();
        let env = self.envelope(Payload::Chat {
            from: name.clone(),
            text: text.clone(),
        })?;
        self.publish(link, &self.topics.events(), env.to_bytes()?, false)
            .await;
        let _ = self.tx.send(UiEvent::Chat { from: name, text });
        Ok(())
    }

    async fn send_error(&mut self, link: &mut Link, to: &str, message: &str) -> Result<()> {
        let env = self.envelope(Payload::Error {
            to: to.to_string(),
            message: message.to_string(),
        })?;
        self.publish(link, &self.topics.events(), env.to_bytes()?, false)
            .await;
        Ok(())
    }

    async fn after_change(&mut self, link: &mut Link) -> Result<()> {
        if self.table.phase == Phase::HandOver {
            self.settle_hand().await;
        }
        self.set_deadline();
        self.bot_due = None;
        self.broadcast(link).await
    }

    fn set_deadline(&mut self) {
        if self.cfg.params.turn_ms == 0 {
            self.table.set_deadline(0);
            return;
        }
        if self.table.phase == Phase::Betting && self.table.actor.is_some() {
            self.table
                .set_deadline(now_ms() + self.cfg.params.turn_ms);
        } else {
            self.table.set_deadline(0);
        }
    }

    async fn settle_hand(&mut self) {
        let hand_no = self.table.hand_no;
        if self.last_settled == Some(hand_no) {
            return;
        }
        self.last_settled = Some(hand_no);
        self.hand_over_at = Some(Instant::now());
        if let Some(result) = self.table.last_result.clone() {
            let _ = self.tx.send(UiEvent::HandResult(result));
        }
        if self.cfg.persist
            && let Some(record) = self.table.hand_record(&self.topics.room)
            && let Err(e) = store::save_hand(&self.cfg.code, &record)
        {
            let _ = self.tx.send(UiEvent::Info(format!("could not save history: {e}")));
        }
        if self.cfg.auto_rebuy {
            let buyin = self.cfg.params.buyin;
            let mut broke = Vec::new();
            for (seat, s) in self.table.players_mut() {
                if s.stack == 0 && s.bot.is_none() {
                    s.stack = buyin;
                    s.sitting_out = false;
                    broke.push(seat);
                }
            }
            if !broke.is_empty() {
                self.table.note(format!("rebought seat(s) {broke:?} for {buyin}"));
            }
        }
        // Bots that went broke get a fresh stack so a practice table keeps going.
        if self.table.params.tourney.is_none() {
            let buyin = self.cfg.params.buyin;
            for (_, s) in self.table.players_mut() {
                if s.stack == 0 && s.bot.is_some() {
                    s.stack = buyin;
                }
            }
        }
        self.flush_kicks();
        // Sign off on the result for the leaderboard; the ack goes out with the next broadcast.
        let view = self.view();
        if let Some((hand_hash, sig)) = self.acks.on_result(&view) {
            self.pending_ack = Some(Payload::ResultAck { hand_hash, sig });
        }
        self.emit_bundles();
    }

    /// Seals hole cards to each player, or to `only` when resending.
    async fn deal(&mut self, link: &mut Link, only: Option<usize>) {
        let hand = self.table.hand_no;
        if only.is_none() {
            self.dealt_at = Instant::now();
        }
        let players: Vec<(usize, String, String, Vec<Card>, bool)> = self
            .table
            .players()
            .map(|(i, s)| (i, s.pubkey.clone(), s.enc_pub.clone(), s.hole.clone(), s.bot.is_some()))
            .collect();
        for (seat, pubkey, enc_pub, cards, is_bot) in players {
            if cards.len() != 2 || is_bot || only.is_some_and(|o| o != seat) {
                continue;
            }
            if seat == self.host_seat {
                let _ = self.tx.send(UiEvent::MyCards { hand, cards });
                continue;
            }
            let Ok(enc_key) = parse_enc_pubkey(&enc_pub) else {
                continue;
            };
            let inner = Payload::Deal {
                hand,
                cards: cards.clone(),
            };
            let Ok(plain) = serde_json::to_vec(&inner) else {
                continue;
            };
            let Ok(sealed) = seal(&enc_key, &plain) else {
                continue;
            };
            let Ok(env) = self.envelope(Payload::Sealed { sealed }) else {
                continue;
            };
            let Ok(bytes) = env.to_bytes() else {
                continue;
            };
            self.publish(link, &self.topics.secrets(&pubkey), bytes, false)
                .await;
        }
    }

    /// Step one of dealing: commit to a secret, then wait briefly for every
    /// human player's randomness so the host alone cannot pick the deck.
    async fn start_hand(&mut self, link: &mut Link) -> Result<()> {
        if self.table.phase == Phase::Betting {
            bail!("hand already in progress");
        }
        if self.pending.is_some() {
            bail!("already shuffling");
        }
        let eligible: Vec<usize> = self
            .table
            .players()
            .filter(|(_, s)| s.stack > 0 && !s.sitting_out)
            .map(|(i, _)| i)
            .collect();
        if eligible.len() < 2 {
            bail!("need at least two players with chips");
        }
        let secret = random_bytes::<32>()?;
        let expect: Vec<usize> = eligible
            .into_iter()
            .filter(|i| {
                *i != self.host_seat
                    && self.table.seats[*i].as_ref().is_some_and(|s| s.bot.is_none() && s.connected)
            })
            .collect();
        let none_to_wait_for = expect.is_empty();
        self.pending = Some(Pending {
            commit: b64(&seed_commit(&secret)),
            secret,
            contribs: BTreeMap::new(),
            expect,
            started: Instant::now(),
        });
        if none_to_wait_for {
            return self.finish_deal(link).await;
        }
        self.table.set_message("shuffling: everyone adds randomness to the deck");
        self.table.bump();
        self.broadcast(link).await
    }

    async fn finish_deal(&mut self, link: &mut Link) -> Result<()> {
        let Some(p) = self.pending.take() else {
            return Ok(());
        };
        let contribs: Vec<(usize, [u8; 32])> = p.contribs.iter().map(|(s, r)| (*s, *r)).collect();
        let seed = mix_seed(&p.secret, &contribs);
        self.table.set_seed_reveal(Some(SeedReveal {
            commit: p.commit,
            host: b64(&p.secret),
            contribs: contribs.iter().map(|(s, r)| (*s, b64(r))).collect(),
        }));
        self.apply_tourney_level();
        if let Err(e) = self.table.start_hand_seeded(now_ms(), seed) {
            self.table.set_seed_reveal(None);
            self.table.bump();
            self.broadcast(link).await?;
            return Err(e);
        }
        self.set_deadline();
        self.deal(link, None).await;
        self.last_settled = None;
        self.bot_due = None;
        self.broadcast(link).await
    }

    fn apply_tourney_level(&mut self) {
        let Some(t) = self.table.params.tourney.clone() else {
            return;
        };
        let now = now_ms();
        let start = *self.tourney_start_ms.get_or_insert(now);
        let lvl = t.level_at(now.saturating_sub(start) / 1000);
        self.tourney_level = lvl;
        let (sb, bb) = t.levels[lvl];
        if (sb, bb) != (self.table.params.small_blind, self.table.params.big_blind) {
            self.table.set_blinds(sb, bb);
            self.table.note(format!("blinds are now {sb}/{bb}"));
        }
    }

    fn save_bans(&self) {
        if !self.cfg.persist {
            return;
        }
        let json = serde_json::json!({ "banned": self.banned.iter().collect::<Vec<_>>() });
        let _ = store::save_session(&self.cfg.code, &json.to_string());
    }

    async fn kick(&mut self, link: &mut Link, seat: usize, ban: bool) -> Result<()> {
        if seat == self.host_seat {
            bail!("you cannot remove yourself");
        }
        let Some(s) = self.table.seats.get(seat).and_then(|s| s.as_ref()) else {
            bail!("no such seat");
        };
        let (pubkey, is_bot) = (s.pubkey.clone(), s.bot.is_some());
        if !is_bot {
            if ban {
                self.banned.insert(pubkey.clone());
                self.save_bans();
            }
            let reason = if ban { "you were banned by the host" } else { "you were removed by the host" };
            let env = self.envelope(Payload::Kicked { to: pubkey, reason: reason.into(), ban })?;
            self.publish(link, &self.topics.events(), env.to_bytes()?, false).await;
        }
        self.kick_queue.push(seat);
        self.flush_kicks();
        self.fold_kicked(link).await?;
        self.table.bump();
        self.broadcast(link).await
    }

    /// Remove queued seats once they are no longer holding live cards.
    fn flush_kicks(&mut self) {
        for seat in std::mem::take(&mut self.kick_queue) {
            let live = self
                .table
                .seats
                .get(seat)
                .and_then(|s| s.as_ref())
                .is_some_and(|s| s.in_hand());
            if self.table.phase == Phase::Betting && live {
                self.kick_queue.push(seat);
            } else {
                let _ = self.table.leave(seat);
            }
        }
    }

    /// A removed player's live hand is folded so the table never waits on them.
    async fn fold_kicked(&mut self, link: &mut Link) -> Result<()> {
        if self.table.phase == Phase::Betting
            && let Some(a) = self.table.actor
            && self.kick_queue.contains(&a)
            && self.table.apply(a, Move::Fold, now_ms()).is_ok()
        {
            self.after_change(link).await?;
        }
        Ok(())
    }

    async fn on_command(&mut self, link: &mut Link, cmd: NetCommand) -> Result<()> {
        match cmd {
            NetCommand::Start => {
                if let Err(e) = self.start_hand(link).await {
                    let _ = self.tx.send(UiEvent::Error(e.to_string()));
                }
                Ok(())
            }
            NetCommand::Action(mv) => {
                if self.table.actor == Some(self.host_seat) {
                    self.on_action(
                        link,
                        self.host_seat,
                        self.table.hand_no,
                        self.table.version,
                        mv,
                    )
                    .await
                } else {
                    let _ = self
                        .tx
                        .send(UiEvent::Error("it is not your turn".into()));
                    Ok(())
                }
            }
            NetCommand::Chat(text) => {
                let text = sanitize_text(&text, 400);
                if text.is_empty() {
                    return Ok(());
                }
                let env = self.envelope(Payload::Chat {
                    from: self.cfg.name.clone(),
                    text: text.clone(),
                })?;
                self.publish(link, &self.topics.events(), env.to_bytes()?, false)
                    .await;
                let _ = self.tx.send(UiEvent::Chat {
                    from: self.cfg.name.clone(),
                    text,
                });
                Ok(())
            }
            NetCommand::Ready(ready) => {
                self.table.set_ready(self.host_seat, ready);
                self.broadcast(link).await
            }
            NetCommand::Leave => {
                self.table.leave(self.host_seat)?;
                self.broadcast(link).await
            }
            NetCommand::Kick { seat, ban } => {
                if let Err(e) = self.kick(link, seat, ban).await {
                    let _ = self.tx.send(UiEvent::Error(e.to_string()));
                }
                Ok(())
            }
            NetCommand::AddBot(skill) => {
                match self.add_bot(skill) {
                    Ok(_) => self.broadcast(link).await?,
                    Err(e) => {
                        let _ = self.tx.send(UiEvent::Error(e.to_string()));
                    }
                }
                Ok(())
            }
            NetCommand::RemoveBot(seat) => {
                let is_bot = self
                    .table
                    .seats
                    .get(seat)
                    .and_then(|s| s.as_ref())
                    .is_some_and(|s| s.bot.is_some());
                if is_bot {
                    self.kick(link, seat, false).await?;
                }
                Ok(())
            }
            NetCommand::Approve(accept) => {
                if self.waiting.is_empty() {
                    return Ok(());
                }
                let w = self.waiting.remove(0);
                self.announce_waiting();
                if accept {
                    self.on_join(link, &w.pubkey, &w.name, w.buyin, &w.enc_pub).await
                } else {
                    self.reject(link, &w.pubkey, "the host declined your request to join").await
                }
            }
            NetCommand::Quit => Ok(()),
        }
    }

    async fn bot_turn(&mut self, link: &mut Link) -> Result<()> {
        if self.table.phase != Phase::Betting {
            self.bot_due = None;
            return Ok(());
        }
        let Some(seat) = self.table.actor else {
            return Ok(());
        };
        let Some(skill) = self.table.seats.get(seat).and_then(|s| s.as_ref()).and_then(|s| s.bot) else {
            self.bot_due = None;
            return Ok(());
        };
        match self.bot_due {
            Some((s, due)) if s == seat => {
                if Instant::now() < due {
                    return Ok(());
                }
            }
            _ => {
                let (lo, hi) = self.cfg.bot_think_ms;
                let wait = lo + if hi > lo { self.rng.next_u64() % (hi - lo) } else { 0 };
                self.bot_due = Some((seat, Instant::now() + Duration::from_millis(wait)));
                if wait > 0 {
                    return Ok(());
                }
            }
        }
        let Some(view) = self.table.bot_view(seat) else {
            return Ok(());
        };
        let mv = bot::decide(skill, &view, &mut self.rng);
        let applied = self.table.apply(seat, mv, now_ms()).is_ok()
            || self
                .table
                .apply(seat, if view.legal.can_check { Move::Check } else { Move::Fold }, now_ms())
                .is_ok();
        if applied {
            self.after_change(link).await?;
        }
        Ok(())
    }

    async fn tick(&mut self, link: &mut Link) -> Result<()> {
        // Host heartbeat.
        if let Some(slot) = self.last_seen.get_mut(self.host_seat) {
            *slot = Some(Instant::now());
        }

        if !link.is_connected() && !self.reported_disconnect {
            self.reported_disconnect = true;
            let _ = self
                .tx
                .send(UiEvent::Disconnected("relay connection lost".into()));
        } else if link.is_connected() {
            self.reported_disconnect = false;
        }

        // Mark silent players as away.
        let now = Instant::now();
        let mut changed = false;
        let seats: Vec<usize> = self.table.players().map(|(i, _)| i).collect();
        for seat in seats {
            let silent = self
                .last_seen
                .get(seat)
                .and_then(|v| *v)
                .map(|t| now.duration_since(t) > Duration::from_secs(45))
                .unwrap_or(false);
            if silent
                && let Some(Some(s)) = self.table.seats.get_mut(seat)
                && s.connected
                && s.bot.is_none()
            {
                s.connected = false;
                changed = true;
            }
        }
        if changed {
            self.broadcast(link).await?;
        }

        // Joint shuffle: deal once everyone has contributed, or after a short wait.
        if let Some(p) = &self.pending
            && (p.contribs.len() >= p.expect.len() || p.started.elapsed() > Duration::from_millis(1_500))
            && let Err(e) = self.finish_deal(link).await
        {
            let _ = self.tx.send(UiEvent::Error(e.to_string()));
        }

        // People who stopped asking (closed their terminal) leave the waiting list.
        let before = self.waiting.len();
        self.waiting.retain(|w| w.seen.elapsed() < Duration::from_secs(10));
        if self.waiting.len() != before {
            self.announce_waiting();
        }

        self.fold_kicked(link).await?;
        self.bot_turn(link).await?;

        // Tournaments deal the next hand on their own.
        if self.table.params.tourney.is_some()
            && self.table.phase == Phase::HandOver
            && self.pending.is_none()
            && self.hand_over_at.is_some_and(|t| t.elapsed() >= Duration::from_secs(self.cfg.auto_deal_secs))
            && self.table.players().filter(|(_, s)| s.stack > 0).count() >= 2
        {
            self.hand_over_at = None;
            if let Err(e) = self.start_hand(link).await {
                let _ = self.tx.send(UiEvent::Error(e.to_string()));
            }
        }

        // Delayed spectator feed: only frames whose hand is over, and old enough.
        let delay = Duration::from_secs(self.cfg.spectate_secs);
        while self.cfg.spectate_secs > 0
            && let Some(f) = self.frames.front()
            && f.at.elapsed() >= delay
            && (f.hand < self.table.hand_no || self.table.phase != Phase::Betting)
        {
            let Some(f) = self.frames.pop_front() else { break };
            let env = self.envelope(Payload::Delayed { view: f.view })?;
            self.publish(link, &self.topics.spectate(), env.to_bytes()?, false).await;
        }

        // Turn timer (bots act on their own schedule).
        if self.table.phase == Phase::Betting
            && self.table.deadline_ms > 0
            && now_ms() >= self.table.deadline_ms
            && let Some(seat) = self.table.actor
            && self.table.seats[seat].as_ref().is_some_and(|s| s.bot.is_none())
        {
            let legal = self.table.legal(seat);
            let mv = if legal.map(|l| l.can_check).unwrap_or(false) {
                Move::Check
            } else {
                Move::Fold
            };
            let name = self
                .table
                .seats
                .get(seat)
                .and_then(|s| s.as_ref())
                .map(|s| s.name.clone())
                .unwrap_or_default();
            let verb = if mv == Move::Check { "check" } else { "fold" };
            if self.table.apply(seat, mv, now_ms()).is_ok() {
                let mut sat_out = false;
                if let Some(Some(s)) = self.table.seats.get_mut(seat) {
                    s.timeout_streak += 1;
                    if s.timeout_streak >= 2 && seat != self.host_seat {
                        s.sitting_out = true;
                        sat_out = true;
                    }
                }
                let extra = if sat_out { " and is now sitting out" } else { "" };
                let _ = self
                    .tx
                    .send(UiEvent::Info(format!("{name} timed out and auto-{verb}ed{extra}")));
                self.after_change(link).await?;
            }
        }

        // Heartbeat so clients can tell a quiet table from a dead host.
        if self.last_broadcast.elapsed() > Duration::from_secs(self.cfg.heartbeat_secs.max(1)) {
            self.broadcast(link).await?;
        }
        Ok(())
    }
}

fn load_bans(code: &str) -> HashSet<String> {
    store::load_session(code)
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| v.get("banned").cloned())
        .and_then(|v| serde_json::from_value::<Vec<String>>(v).ok())
        .map(|v| v.into_iter().collect())
        .unwrap_or_default()
}
