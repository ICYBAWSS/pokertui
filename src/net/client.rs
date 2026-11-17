//! The joining client: verifies host snapshots, applies newer versions,
//! decrypts its own hole cards, audits every finished hand, and takes over as
//! host if the host vanishes and this player is next in line.

use super::host::{Host, HostConfig, host_loop};
use super::mqtt::Link;
use super::protocol::{Envelope, Payload, Topics, now_ms};
use super::{NetCommand, UiEvent, sanitize_text};
use crate::crypto::{Identity, b64, random_bytes};
use crate::game::{Card, HandResult, TableParams, TableView, record_from_view, verify_record};
use crate::leaderboard::AckBook;
use crate::net::roomcode::RoomId;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct ClientConfig {
    pub identity: Arc<Identity>,
    pub code: String,
    pub room: RoomId,
    pub name: String,
    pub buyin: u64,
    pub broker: Option<String>,
    pub spectate: bool,
    /// Silence from the host for this long means it is gone; each player in
    /// line then gets one such window to take over.
    pub takeover_secs: u64,
    /// Settings to use if this client becomes the host.
    pub promote_with: Option<HostConfig>,
}

impl ClientConfig {
    pub fn new(identity: Arc<Identity>, code: String, room: RoomId, name: String, buyin: u64) -> Self {
        Self {
            identity,
            code,
            room,
            name,
            buyin,
            broker: None,
            spectate: false,
            takeover_secs: 15,
            promote_with: None,
        }
    }
}

struct Client {
    cfg: ClientConfig,
    topics: Topics,
    tx: Sender<UiEvent>,
    seq: u64,
    view: Option<TableView>,
    seat: Option<usize>,
    joined: bool,
    host: Option<String>,
    epoch: u32,
    host_seq: u64,
    last_host_msg: Instant,
    my_cards: (u32, Vec<Card>),
    last_result: Option<u32>,
    last_join: Option<Instant>,
    reported_down: bool,
    done: Option<String>,
    /// My randomness for upcoming shuffles, by hand number.
    entropy: HashMap<u32, [u8; 32]>,
    /// The host's shuffle commitment for each upcoming hand.
    commits: HashMap<u32, String>,
    asked_commit: Option<String>,
    acks: AckBook,
    pending_ack: Option<Payload>,
}

pub async fn run_client(
    cfg: ClientConfig,
    tx: Sender<UiEvent>,
    rx: mpsc::UnboundedReceiver<NetCommand>,
) -> Result<()> {
    let link = Link::connect_room(cfg.broker.as_deref(), &Topics::new(&cfg.room).room).await?;
    run_client_on(link, cfg, tx, rx).await
}

pub async fn run_client_on(
    mut link: Link,
    cfg: ClientConfig,
    tx: Sender<UiEvent>,
    mut rx: mpsc::UnboundedReceiver<NetCommand>,
) -> Result<()> {
    let topics = Topics::new(&cfg.room);
    link.subscribe(&topics.events()).await?;
    link.subscribe(&topics.snapshot()).await?;
    link.subscribe(&topics.secrets(&cfg.identity.pubkey_b64()))
        .await?;
    if cfg.spectate {
        link.subscribe(&topics.spectate()).await?;
    }

    let _ = tx.send(UiEvent::Connected {
        broker: link.broker.clone(),
    });

    let mut client = Client::new(cfg, topics, tx.clone());
    if !client.cfg.spectate {
        client.send_join(&mut link).await;
    }

    let started = Instant::now();
    let mut ticker = tokio::time::interval(Duration::from_millis(500));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            msg = link.recv() => {
                if let Some(msg) = msg
                    && let Err(e) = client.on_message(&mut link, msg).await
                {
                    tracing::warn!(error = %e, "client message handling failed");
                }
            }
            cmd = rx.recv() => {
                match cmd {
                    None | Some(NetCommand::Quit) => break,
                    Some(cmd) => client.on_command(&mut link, cmd).await,
                }
            }
            _ = ticker.tick() => {
                if let Some(view) = client.tick(&mut link).await {
                    return promote(link, client, view, tx, rx).await;
                }
                if !client.joined
                    && !client.cfg.spectate
                    && started.elapsed() > Duration::from_secs(25)
                {
                    let _ = tx.send(UiEvent::Closed(
                        "no host answered on that room code".into(),
                    ));
                    break;
                }
            }
        }
        if let Some(reason) = client.done.take() {
            let _ = tx.send(UiEvent::Closed(reason));
            break;
        }
    }

    let _ = tx.send(UiEvent::Closed("left the table".into()));
    Ok(())
}

/// The host is gone and this client is next in line: become the host in place,
/// on the same relay connection, room and seats.
async fn promote(
    mut link: Link,
    client: Client,
    view: TableView,
    tx: Sender<UiEvent>,
    rx: mpsc::UnboundedReceiver<NetCommand>,
) -> Result<()> {
    let cfg = &client.cfg;
    let mut hc = cfg.promote_with.clone().unwrap_or_else(|| {
        HostConfig::new(
            cfg.identity.clone(),
            cfg.code.clone(),
            cfg.room,
            cfg.name.clone(),
            TableParams::default(),
        )
    });
    hc.identity = cfg.identity.clone();
    hc.code = cfg.code.clone();
    hc.room = cfg.room;
    hc.name = cfg.name.clone();
    hc.broker = cfg.broker.clone();
    let mut host = Host::from_view(hc, client.topics.clone(), tx.clone(), &view)?;
    host.bootstrap_promoted(&mut link).await?;
    host_loop(link, host, tx, rx).await
}

impl Client {
    fn new(cfg: ClientConfig, topics: Topics, tx: Sender<UiEvent>) -> Self {
        let identity = cfg.identity.clone();
        Self {
            cfg,
            topics,
            tx,
            seq: now_ms(),
            view: None,
            seat: None,
            joined: false,
            host: None,
            epoch: 0,
            host_seq: 0,
            last_host_msg: Instant::now(),
            my_cards: (0, Vec::new()),
            last_result: None,
            last_join: None,
            reported_down: false,
            done: None,
            entropy: HashMap::new(),
            commits: HashMap::new(),
            asked_commit: None,
            acks: AckBook::new(identity),
            pending_ack: None,
        }
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn me(&self) -> String {
        self.cfg.identity.pubkey_b64()
    }

    async fn publish(&mut self, link: &mut Link, topic: &str, payload: Payload, retain: bool) {
        let seq = self.next_seq();
        let room = self.topics.room.clone();
        let env = match Envelope::new(&self.cfg.identity, &room, seq, payload) {
            Ok(env) => env,
            Err(e) => {
                tracing::warn!(error = %e, "could not sign packet");
                return;
            }
        };
        let bytes = match env.to_bytes().and_then(|b| self.topics.wrap(&b)) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(error = %e, "could not encode packet");
                return;
            }
        };
        if let Err(e) = link.publish(topic, bytes, retain).await {
            tracing::warn!(error = %e, "publish failed");
        }
    }

    async fn send_join(&mut self, link: &mut Link) {
        self.last_join = Some(Instant::now());
        self.publish(
            link,
            &self.topics.events(),
            Payload::JoinRequest {
                name: self.cfg.name.clone(),
                buyin: self.cfg.buyin,
                enc_pub: self.cfg.identity.enc_pub_b64(),
            },
            false,
        )
        .await;
    }

    /// Players who could take over, in turn order: seated, online, human, not the host.
    fn candidates(&self) -> Vec<String> {
        let Some(view) = &self.view else {
            return Vec::new();
        };
        let host = self.host.clone().unwrap_or_default();
        view.seats
            .iter()
            .filter(|s| !s.name.is_empty() && s.connected && !s.bot && !s.pubkey.is_empty() && s.pubkey != host)
            .map(|s| s.pubkey.clone())
            .collect()
    }

    /// A different key may host only after a takeover: a newer epoch, from a
    /// player who was in line. Anything else is someone impersonating the host.
    fn accept_new_host(&mut self, from: &str, view: &TableView) -> bool {
        if view.epoch > self.epoch && self.candidates().iter().any(|c| c == from) {
            self.host = Some(from.to_string());
            self.epoch = view.epoch;
            let name = view
                .seats
                .iter()
                .find(|s| s.pubkey == from)
                .map(|s| s.name.clone())
                .unwrap_or_default();
            let _ = self.tx.send(UiEvent::Info(format!("{name} is the new host")));
            true
        } else {
            false
        }
    }

    async fn on_message(&mut self, link: &mut Link, msg: super::mqtt::Message) -> Result<()> {
        let Ok(plain) = self.topics.unwrap(&msg.payload) else {
            return Ok(());
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
        let is_secret = msg.topic.ends_with(&format!("/secrets/{}", self.me()));
        if is_secret {
            if let Payload::Sealed { sealed } = &env.payload {
                match self.cfg.identity.open(sealed) {
                    Ok(plain) => match serde_json::from_slice::<Payload>(&plain) {
                        Ok(Payload::Deal { hand, cards }) => {
                            self.my_cards = (hand, cards.clone());
                            let _ = self.tx.send(UiEvent::MyCards { hand, cards });
                        }
                        Ok(_) => {}
                        Err(e) => tracing::warn!(error = %e, "bad sealed payload"),
                    },
                    Err(e) => tracing::warn!(error = %e, "sealed payload not for us"),
                }
            }
            return Ok(());
        }

        // Anything the host authored must carry a newer sequence number than the
        // last one we accepted, which kills replays of old signed packets.
        let from_host = self.host.as_deref() == Some(env.from.as_str());
        if from_host
            && matches!(env.payload, Payload::State { .. } | Payload::Closed { .. } | Payload::Kicked { .. } | Payload::Delayed { .. })
        {
            if env.seq < self.host_seq {
                return Ok(());
            }
            self.host_seq = env.seq;
        }
        if from_host {
            self.last_host_msg = Instant::now();
        }

        match env.payload {
            Payload::State { view } => {
                if view.host != env.from {
                    tracing::debug!("state host does not match sender");
                    return Ok(());
                }
                match &self.host {
                    Some(host) if host != &env.from => {
                        if !self.accept_new_host(&env.from, &view) {
                            return Ok(());
                        }
                        self.host_seq = env.seq;
                        self.last_host_msg = Instant::now();
                    }
                    None => self.host = Some(env.from.clone()),
                    _ => {}
                }
                self.last_host_msg = Instant::now();
                self.note_shuffle(link, &view).await;
                if self.cfg.spectate && view.spectate {
                    return Ok(()); // spectators of delayed tables only watch the delayed feed
                }
                self.apply_view(view);
            }
            Payload::Delayed { view } => {
                if self.cfg.spectate && from_host {
                    self.apply_view(view);
                }
            }
            Payload::JoinAccept {
                to,
                seat,
                view,
                params: _,
            } => {
                if to != self.me() || view.host != env.from {
                    return Ok(());
                }
                self.host = Some(env.from.clone());
                self.epoch = view.epoch;
                self.last_host_msg = Instant::now();
                self.seat = Some(seat);
                self.joined = true;
                let _ = self.tx.send(UiEvent::Joined { seat });
                self.apply_view(view);
            }
            Payload::JoinReject { to, reason } => {
                if to == self.me() {
                    let _ = self.tx.send(UiEvent::Error(reason.clone()));
                    self.done = Some(reason);
                }
            }
            Payload::Kicked { to, reason, .. } => {
                if to == self.me() && from_host {
                    self.done = Some(reason);
                }
            }
            Payload::ResultAck { hand_hash, sig } => {
                self.acks.on_ack(&env.from, &hand_hash, &sig);
                self.emit_bundles();
            }
            Payload::Chat { from, text } => {
                let _ = self.tx.send(UiEvent::Chat { from, text });
            }
            Payload::Error { to, message } => {
                if to == self.me() {
                    let _ = self.tx.send(UiEvent::Error(message));
                }
            }
            Payload::Closed { reason } => {
                if from_host {
                    self.done = Some(reason);
                }
            }
            _ => {}
        }
        self.flush_ack(link).await;
        Ok(())
    }

    fn emit_bundles(&mut self) {
        for b in self.acks.take_complete() {
            let _ = self.tx.send(UiEvent::Bundle(b));
        }
    }

    async fn flush_ack(&mut self, link: &mut Link) {
        if let Some(ack) = self.pending_ack.take() {
            self.publish(link, &self.topics.events(), ack, false).await;
        }
    }

    /// The host has committed to a secret for the next shuffle: remember it and
    /// add my own randomness so the host cannot pick the deck alone.
    async fn note_shuffle(&mut self, link: &mut Link, view: &TableView) {
        let Some(commit) = &view.seed_commit else {
            return;
        };
        let hand = view.hand_no + 1;
        self.commits.entry(hand).or_insert_with(|| commit.clone());
        if self.cfg.spectate || self.seat.is_none() || self.asked_commit.as_ref() == Some(commit) {
            return;
        }
        self.asked_commit = Some(commit.clone());
        let Ok(r) = random_bytes::<32>() else {
            return;
        };
        self.entropy.insert(hand, r);
        self.publish(
            link,
            &self.topics.events(),
            Payload::Entropy { hand, r: b64(&r) },
            false,
        )
        .await;
    }

    fn apply_view(&mut self, view: TableView) {
        if let Some(current) = &self.view
            && view.version <= current.version
        {
            return;
        }
        if let Some(result) = &view.last_result
            && self.last_result != Some(result.hand_no)
        {
            self.last_result = Some(result.hand_no);
            self.audit(&view, result);
            if !self.cfg.spectate
                && let Some((hand_hash, sig)) = self.acks.on_result(&view)
            {
                self.pending_ack = Some(Payload::ResultAck { hand_hash, sig });
                self.emit_bundles();
            }
            let _ = self.tx.send(UiEvent::HandResult(result.clone()));
        }
        // A new turn must start with (nearly) the table's full clock; a host that
        // quietly shortens someone's turn so they time out gets flagged.
        if let Some(old) = &self.view
            && view.phase == crate::game::Phase::Betting
            && view.actor.is_some()
            && (view.actor != old.actor || view.hand_no != old.hand_no)
            && view.params.turn_ms > 0
            && view.deadline_ms > 0
            && view.deadline_ms.saturating_sub(view.host_ts_ms) + 3_000 < view.params.turn_ms
        {
            let _ = self.tx.send(UiEvent::Error(
                "⚠ the host gave the player to act less than the table's turn clock".into(),
            ));
        }
        let _ = self.tx.send(UiEvent::View(view.clone()));
        self.view = Some(view);
    }

    /// Check a finished hand: deck commitment, joint shuffle, and a full replay
    /// of every move. A host that lied about anything fails here.
    fn audit(&mut self, view: &TableView, result: &HandResult) {
        let mut problems = Vec::new();
        match record_from_view(view) {
            Some(record) => {
                if let Err(e) = verify_record(&record) {
                    problems.push(e.to_string());
                }
            }
            None => problems.push("no hand record".into()),
        }
        match &result.seed {
            Some(seed) => {
                if let Some(c) = self.commits.get(&result.hand_no)
                    && c != &seed.commit
                {
                    problems.push("the host changed its shuffle commitment".into());
                }
                if let (Some(mine), Some(seat)) = (self.entropy.get(&result.hand_no), self.seat) {
                    let used = seed.contribs.iter().find(|(s, _)| *s == seat).map(|(_, r)| r.as_str());
                    if used != Some(b64(mine).as_str()) {
                        problems.push("your shuffle input was not used (lost in transit, or left out)".into());
                    }
                }
            }
            None if self.commits.contains_key(&result.hand_no) => {
                problems.push("the host skipped the shuffle it announced".into());
            }
            None => {}
        }
        if !problems.is_empty() {
            let _ = self.tx.send(UiEvent::Error(format!(
                "⚠ hand #{} failed verification: {}",
                result.hand_no,
                problems.join("; ")
            )));
        }
    }

    async fn on_command(&mut self, link: &mut Link, cmd: NetCommand) {
        match cmd {
            NetCommand::Action(mv) => {
                let Some(view) = &self.view else {
                    let _ = self.tx.send(UiEvent::Error("no table state yet".into()));
                    return;
                };
                if self.seat.is_none() {
                    let _ = self
                        .tx
                        .send(UiEvent::Error("you are spectating".into()));
                    return;
                }
                let (hand, version) = (view.hand_no, view.version);
                self.publish(
                    link,
                    &self.topics.events(),
                    Payload::Action {
                        hand,
                        version,
                        mv,
                    },
                    false,
                )
                .await;
            }
            NetCommand::Chat(text) => {
                let text = sanitize_text(&text, 400);
                if text.is_empty() {
                    return;
                }
                self.publish(
                    link,
                    &self.topics.events(),
                    Payload::Chat {
                        from: String::new(),
                        text,
                    },
                    false,
                )
                .await;
            }
            NetCommand::Ready(ready) => {
                self.publish(
                    link,
                    &self.topics.events(),
                    Payload::Ready { ready },
                    false,
                )
                .await;
            }
            NetCommand::Leave => {
                self.publish(link, &self.topics.events(), Payload::Leave {}, false)
                    .await;
                self.joined = false;
                self.seat = None;
            }
            NetCommand::Start => {
                let _ = self
                    .tx
                    .send(UiEvent::Info("only the host can deal".into()));
            }
            NetCommand::Kick { .. } | NetCommand::AddBot(_) | NetCommand::RemoveBot(_) => {
                let _ = self
                    .tx
                    .send(UiEvent::Info("only the host can do that".into()));
            }
            NetCommand::Quit => {}
        }
    }

    /// Returns the last view when this client must take over as host.
    async fn tick(&mut self, link: &mut Link) -> Option<TableView> {
        if !link.is_connected() {
            if !self.reported_down {
                self.reported_down = true;
                let _ = self
                    .tx
                    .send(UiEvent::Disconnected("relay connection lost".into()));
            }
            return None;
        }
        if self.reported_down {
            self.reported_down = false;
            self.last_host_msg = Instant::now(); // our silence, not the host's
            let _ = self.tx.send(UiEvent::Connected {
                broker: link.broker.clone(),
            });
            self.publish(
                link,
                &self.topics.events(),
                Payload::RequestState {
                    version: self.view.as_ref().map(|v| v.version).unwrap_or(0),
                },
                false,
            )
            .await;
            if !self.joined && !self.cfg.spectate {
                self.send_join(link).await;
            }
        }

        if !self.joined
            && !self.cfg.spectate
            && self
                .last_join
                .map(|t| t.elapsed() > Duration::from_secs(2))
                .unwrap_or(true)
        {
            self.send_join(link).await;
        }

        self.flush_ack(link).await;

        // Heartbeat so the host can show who is still around.
        if self.joined {
            self.publish(link, &self.topics.events(), Payload::Ping {
                    version: self.view.as_ref().map(|v| v.version).unwrap_or(0),
                    cards_hand: self.my_cards.0,
                }, false)
                .await;
        }

        // Host silent for too long: whoever is next in line takes over.
        let window = Duration::from_secs(self.cfg.takeover_secs.max(1));
        let silent = self.last_host_msg.elapsed();
        if self.joined && !self.cfg.spectate && silent > window {
            let turn = ((silent - window).as_secs_f64() / window.as_secs_f64()) as usize;
            let cands = self.candidates();
            match cands.get(turn) {
                Some(c) if *c == self.me() => return self.view.clone(),
                Some(_) => {}
                None => {
                    self.done = Some("the host disappeared and nobody could take over".into());
                }
            }
        }
        None
    }
}
