//! The joining client: verifies host snapshots, applies newer versions, and
//! decrypts its own hole cards.

use super::mqtt::Link;
use super::protocol::{Envelope, Payload, Topics};
use super::{NetCommand, UiEvent, sanitize_text};
use crate::crypto::Identity;
use crate::game::{Card, TableView};
use crate::net::roomcode::RoomId;
use anyhow::Result;
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::sync::mpsc::Sender;
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
    my_cards: (u32, Vec<Card>),
    last_result: Option<u32>,
    last_join: Option<Instant>,
    reported_down: bool,
    done: Option<String>,
}

pub async fn run_client(
    cfg: ClientConfig,
    tx: Sender<UiEvent>,
    mut rx: mpsc::UnboundedReceiver<NetCommand>,
) -> Result<()> {
    let mut link = Link::connect(cfg.broker.as_deref()).await?;
    let topics = Topics::new(&cfg.room);
    link.subscribe(&topics.events()).await?;
    link.subscribe(&topics.snapshot()).await?;
    link.subscribe(&topics.secrets(&cfg.identity.pubkey_b64()))
        .await?;

    let _ = tx.send(UiEvent::Connected {
        broker: link.broker.clone(),
    });
    if cfg.spectate {
        let _ = tx.send(UiEvent::Info(format!(
            "spectating room {} · relay {}",
            cfg.code, link.broker
        )));
    } else {
        let _ = tx.send(UiEvent::Info(format!(
            "joining room {} via {}",
            cfg.code, link.broker
        )));
    }

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
                    && let Err(e) = client.on_message(msg).await
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
                client.tick(&mut link).await;
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

impl Client {
    fn new(cfg: ClientConfig, topics: Topics, tx: Sender<UiEvent>) -> Self {
        Self {
            cfg,
            topics,
            tx,
            seq: 0,
            view: None,
            seat: None,
            joined: false,
            host: None,
            my_cards: (0, Vec::new()),
            last_result: None,
            last_join: None,
            reported_down: false,
            done: None,
        }
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
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
        let bytes = match env.to_bytes() {
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

    async fn on_message(&mut self, msg: super::mqtt::Message) -> Result<()> {
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
        let is_secret = msg.topic.ends_with(&format!("/secrets/{}", self.cfg.identity.pubkey_b64()));
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

        match env.payload {
            Payload::State { view } => {
                if view.host != env.from {
                    tracing::debug!("state host does not match sender");
                    return Ok(());
                }
                match &self.host {
                    Some(host) if host != &env.from => return Ok(()),
                    None => self.host = Some(env.from.clone()),
                    _ => {}
                }
                self.apply_view(view);
            }
            Payload::JoinAccept {
                to,
                seat,
                view,
                params: _,
            } => {
                if to != self.cfg.identity.pubkey_b64() {
                    return Ok(());
                }
                self.host = Some(env.from.clone());
                self.seat = Some(seat);
                self.joined = true;
                let _ = self.tx.send(UiEvent::Joined { seat });
                self.apply_view(view);
            }
            Payload::JoinReject { to, reason } => {
                if to == self.cfg.identity.pubkey_b64() {
                    let _ = self.tx.send(UiEvent::Error(reason.clone()));
                    self.done = Some(reason);
                }
            }
            Payload::Chat { from, text } => {
                let _ = self.tx.send(UiEvent::Chat { from, text });
            }
            Payload::Error { to, message } => {
                if to == self.cfg.identity.pubkey_b64() {
                    let _ = self.tx.send(UiEvent::Error(message));
                }
            }
            Payload::Closed { reason } => {
                if self.host.as_ref() == Some(&env.from) {
                    self.done = Some(reason);
                }
            }
            Payload::RequestState { .. } => {}
            _ => {}
        }
        Ok(())
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
            let _ = self.tx.send(UiEvent::HandResult(result.clone()));
        }
        let _ = self.tx.send(UiEvent::View(view.clone()));
        self.view = Some(view);
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
            NetCommand::Quit => {}
        }
    }

    async fn tick(&mut self, link: &mut Link) {
        if !link.is_connected() {
            if !self.reported_down {
                self.reported_down = true;
                let _ = self
                    .tx
                    .send(UiEvent::Disconnected("relay connection lost".into()));
            }
            return;
        }
        if self.reported_down {
            self.reported_down = false;
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

        // Heartbeat so the host can show who is still around.
        if self.joined {
            self.publish(link, &self.topics.events(), Payload::Ping {
                    version: self.view.as_ref().map(|v| v.version).unwrap_or(0),
                    cards_hand: self.my_cards.0,
                }, false)
                .await;
        }
    }

}
