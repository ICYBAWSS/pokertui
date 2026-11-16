//! The authoritative host: owns the table, deals cards, validates actions and
//! publishes signed snapshots.

use super::mqtt::Link;
use super::protocol::{Envelope, Payload, Topics, now_ms};
use super::{NetCommand, UiEvent, sanitize_text};
use crate::crypto::{Identity, parse_enc_pubkey, seal};
use crate::game::{Move, Phase, Table, TableParams, TableView};
use crate::net::roomcode::RoomId;
use crate::store;
use anyhow::{Result, bail};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::sync::mpsc::Sender;
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
}

pub struct Host {
    cfg: HostConfig,
    table: Table,
    topics: Topics,
    tx: Sender<UiEvent>,
    seq: u64,
    host_seat: usize,
    last_seen: Vec<Option<Instant>>,
    last_chat: Vec<Option<Instant>>,
    last_settled: Option<u32>,
    last_version: u64,
    last_broadcast: Instant,
    dealt_at: Instant,
    reported_disconnect: bool,
}

pub async fn run_host(
    cfg: HostConfig,
    tx: Sender<UiEvent>,
    mut rx: mpsc::UnboundedReceiver<NetCommand>,
) -> Result<()> {
    let mut link = Link::connect(cfg.broker.as_deref()).await?;
    let topics = Topics::new(&cfg.room);
    let snapshot_topic = topics.snapshot();
    link.subscribe(&topics.events()).await?;

    let mut host = Host::new(cfg, topics, tx.clone());
    host.bootstrap(&mut link).await?;

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
        let _ = link.publish(&host.topics.events(), bytes, false).await;
    }
    let _ = link.publish(&snapshot_topic, Vec::new(), true).await;
    let _ = tx.send(UiEvent::Closed("table closed".into()));
    Ok(())
}

impl Host {
    fn new(cfg: HostConfig, topics: Topics, tx: Sender<UiEvent>) -> Self {
        let seats = cfg.params.max_seats;
        let table = Table::new(cfg.params.clone());
        Self {
            cfg,
            table,
            topics,
            tx,
            seq: 0,
            host_seat: 0,
            last_seen: vec![None; seats],
            last_chat: vec![None; seats],
            last_settled: None,
            last_version: 0,
            last_broadcast: Instant::now(),
            dealt_at: Instant::now(),
            reported_disconnect: false,
        }
    }

    async fn bootstrap(&mut self, link: &mut Link) -> Result<()> {
        let seat = self.table.join(
            &self.cfg.name,
            &self.cfg.identity.pubkey_b64(),
            &self.cfg.identity.enc_pub_b64(),
            self.cfg.params.buyin,
        )?;
        self.host_seat = seat;
        self.table.set_ready(seat, true);
        self.last_seen[seat] = Some(Instant::now());

        let _ = self.tx.send(UiEvent::Connected {
            broker: link.broker.clone(),
        });
        let _ = self.tx.send(UiEvent::Joined { seat });
        self.broadcast(link).await
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn envelope(&mut self, payload: Payload) -> Result<Envelope> {
        let seq = self.next_seq();
        let room = self.cfg.room_hex();
        Envelope::new(&self.cfg.identity, &room, seq, payload)
    }

    fn view(&self) -> TableView {
        let mut view = self.table.view(&self.topics.room);
        view.host = self.cfg.identity.pubkey_b64();
        view
    }

    async fn publish(&mut self, link: &mut Link, topic: &str, payload: Vec<u8>, retain: bool) {
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
        let _ = self.tx.send(UiEvent::View(view));
        Ok(())
    }

    async fn on_message(&mut self, link: &mut Link, msg: super::mqtt::Message) -> Result<()> {
        if msg.topic != self.topics.events() {
            return Ok(());
        }
        let env = match Envelope::parse(&msg.payload) {
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
            } => self.on_join(link, &env.from, &name, buyin, &enc_pub).await,
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
                    self.broadcast(link).await?;
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

    async fn on_join(
        &mut self,
        link: &mut Link,
        pubkey: &str,
        name: &str,
        buyin: u64,
        enc_pub: &str,
    ) -> Result<()> {
        let name = sanitize_text(name, 16);
        let name = if name.is_empty() {
            format!("player{}", &pubkey[..4.min(pubkey.len())])
        } else {
            name
        };
        if parse_enc_pubkey(enc_pub).is_err() {
            return self.send_error(link, pubkey, "bad encryption key").await;
        }
        match self.table.join(&name, pubkey, enc_pub, buyin) {
            Ok(seat) => {
                self.table.set_connected(seat, true);
                if let Some(slot) = self.last_seen.get_mut(seat) {
                    *slot = Some(Instant::now());
                }
                let view = self.view();
                let env = self.envelope(Payload::JoinAccept {
                    to: pubkey.to_string(),
                    seat,
                    params: self.cfg.params.clone(),
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
            Ok(_) => self.after_change(link).await,
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
        if let Some(result) = self.table.last_result.clone() {
            let _ = self.tx.send(UiEvent::HandResult(result));
        }
        if let Some(record) = self.table.hand_record(&self.topics.room) {
            match store::save_hand(&self.cfg.code, &record) {
                Ok(_) => {}
                Err(e) => {
                    let _ = self.tx.send(UiEvent::Info(format!("could not save history: {e}")));
                }
            }
        }
        if self.cfg.auto_rebuy {
            let buyin = self.cfg.params.buyin;
            let mut broke = Vec::new();
            for (seat, s) in self.table.players_mut() {
                if s.stack == 0 {
                    s.stack = buyin;
                    s.sitting_out = false;
                    broke.push(seat);
                }
            }
            if !broke.is_empty() {
                self.table.note(format!("rebought seat(s) {broke:?} for {buyin}"));
            }
        }
    }

    /// Seals hole cards to each player, or to `only` when resending.
    async fn deal(&mut self, link: &mut Link, only: Option<usize>) {
        let hand = self.table.hand_no;
        if only.is_none() {
            self.dealt_at = Instant::now();
        }
        let players: Vec<(usize, String, String, Vec<crate::game::Card>)> = self
            .table
            .players()
            .map(|(i, s)| (i, s.pubkey.clone(), s.enc_pub.clone(), s.hole.clone()))
            .collect();
        for (seat, pubkey, enc_pub, cards) in players {
            if cards.len() != 2 || only.is_some_and(|o| o != seat) {
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

    async fn start_hand(&mut self, link: &mut Link) -> Result<()> {
        if self.table.phase == Phase::Betting {
            bail!("hand already in progress");
        }
        self.table.start_hand(now_ms())?;
        self.set_deadline();
        self.deal(link, None).await;
        self.last_settled = None;
        self.broadcast(link).await
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
            NetCommand::Quit => Ok(()),
        }
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
            {
                s.connected = false;
                changed = true;
            }
        }
        if changed {
            self.broadcast(link).await?;
        }

        // Turn timer.
        if self.table.phase == Phase::Betting
            && self.table.deadline_ms > 0
            && now_ms() >= self.table.deadline_ms
            && let Some(seat) = self.table.actor
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
                let _ = self
                    .tx
                    .send(UiEvent::Info(format!("{name} timed out and auto-{verb}ed")));
                self.after_change(link).await?;
            }
        }
        Ok(())
    }
}

impl HostConfig {
    pub fn room_hex(&self) -> String {
        crate::net::roomcode::hex(&self.room)
    }
}
