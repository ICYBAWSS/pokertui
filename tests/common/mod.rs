//! Shared harness: a host and clients in one process over an in-memory relay.
#![allow(dead_code)]

use pokertui::crypto::Identity;
use pokertui::game::{Card, HandResult, Move, Phase, TableParams, TableView};
use pokertui::net::{
    ClientConfig, HostConfig, MemoryBus, NetCommand, UiEvent, roomcode, run_client_on, run_host_on,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

#[derive(Default)]
pub struct NodeState {
    pub seat: Option<usize>,
    pub view: Option<TableView>,
    pub views: Vec<TableView>,
    pub results: Vec<HandResult>,
    pub errors: Vec<String>,
    pub infos: Vec<String>,
    pub closed: Option<String>,
    pub cards: Option<(u32, Vec<Card>)>,
    pub promoted: bool,
    pub bundles: Vec<pokertui::leaderboard::Bundle>,
    pub waiting: Vec<String>,
}

pub struct Node {
    pub name: String,
    pub id: Arc<Identity>,
    pub cmd: UnboundedSender<NetCommand>,
    pub st: Arc<Mutex<NodeState>>,
    /// While true the node plays "check or call" on its own turn.
    pub auto: Arc<AtomicBool>,
    pub task: JoinHandle<()>,
}

impl Node {
    pub fn state(&self) -> MutexGuard<'_, NodeState> {
        self.st.lock().unwrap()
    }
    pub fn send(&self, cmd: NetCommand) {
        let _ = self.cmd.send(cmd);
    }
    pub fn set_auto(&self, on: bool) {
        self.auto.store(on, Ordering::SeqCst);
    }
    pub fn hand_no(&self) -> u32 {
        self.state().view.as_ref().map(|v| v.hand_no).unwrap_or(0)
    }
    pub fn phase(&self) -> Option<Phase> {
        self.state().view.as_ref().map(|v| v.phase)
    }
}

pub struct Room {
    /// Set to run over a real websocket relay instead of the in-memory bus.
    pub broker: Option<String>,
    pub bus: MemoryBus,
    pub room: roomcode::RoomId,
    pub code: String,
    pub params: TableParams,
}

pub fn room(bus: MemoryBus, seats: usize, buyin: u64) -> Room {
    let (room, code) = roomcode::generate().unwrap();
    Room {
        broker: None,
        bus,
        room,
        code,
        params: TableParams {
            max_seats: seats,
            small_blind: 25,
            big_blind: 50,
            buyin,
            turn_ms: 0,
            tourney: None,
        },
    }
}

fn pump(rx: Receiver<UiEvent>, st: Arc<Mutex<NodeState>>, cmd: UnboundedSender<NetCommand>, auto: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let mut last_sent: Option<(u32, u64)> = None;
        while let Ok(ev) = rx.recv() {
            let mut s = st.lock().unwrap();
            match ev {
                UiEvent::Joined { seat } => s.seat = Some(seat),
                UiEvent::View(v) => {
                    if auto.load(Ordering::SeqCst)
                        && v.actor.is_some()
                        && v.actor == s.seat
                        && last_sent != Some((v.hand_no, v.version))
                        && let Some(legal) = &v.actor_legal
                    {
                        last_sent = Some((v.hand_no, v.version));
                        let _ = cmd.send(NetCommand::Action(if legal.can_check { Move::Check } else { Move::Call }));
                    }
                    s.views.push(v.clone());
                    s.view = Some(v);
                }
                UiEvent::HandResult(r) => {
                    if !s.results.iter().any(|x| x.hand_no == r.hand_no) {
                        s.results.push(r);
                    }
                }
                UiEvent::MyCards { hand, cards } => s.cards = Some((hand, cards)),
                UiEvent::Error(e) => s.errors.push(e),
                UiEvent::Info(i) => s.infos.push(i),
                UiEvent::Promoted => s.promoted = true,
                UiEvent::Bundle(b) => s.bundles.push(b),
                UiEvent::Waiting(w) => s.waiting = w,
                UiEvent::Closed(r) => {
                    s.closed.get_or_insert(r);
                }
                _ => {}
            }
        }
    });
}

pub fn spawn_host(r: &Room, name: &str, tune: impl FnOnce(&mut HostConfig)) -> Node {
    let id = Arc::new(Identity::generate().unwrap());
    let mut cfg = HostConfig::new(id.clone(), r.code.clone(), r.room, name.into(), r.params.clone());
    cfg.persist = false;
    cfg.bot_think_ms = (0, 0);
    cfg.heartbeat_secs = 1;
    cfg.auto_deal_secs = 0;
    tune(&mut cfg);
    let (tx, rx) = std::sync::mpsc::channel();
    let (cmd, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let st = Arc::new(Mutex::new(NodeState::default()));
    let auto = Arc::new(AtomicBool::new(false));
    pump(rx, st.clone(), cmd.clone(), auto.clone());
    let task = if let Some(broker) = r.broker.clone() {
        cfg.broker = Some(broker);
        tokio::spawn(async move {
            let _ = pokertui::net::run_host(cfg, tx, cmd_rx).await;
        })
    } else {
        let link = r.bus.link();
        tokio::spawn(async move {
            let _ = run_host_on(link, cfg, tx, cmd_rx).await;
        })
    };
    Node { name: name.into(), id, cmd, st, auto, task }
}

pub fn spawn_client(r: &Room, name: &str, id: Option<Arc<Identity>>, tune: impl FnOnce(&mut ClientConfig)) -> Node {
    let id = id.unwrap_or_else(|| Arc::new(Identity::generate().unwrap()));
    let mut cfg = ClientConfig::new(id.clone(), r.code.clone(), r.room, name.into(), r.params.buyin);
    cfg.takeover_secs = 2;
    let mut promote = HostConfig::new(id.clone(), r.code.clone(), r.room, name.into(), r.params.clone());
    promote.persist = false;
    promote.bot_think_ms = (0, 0);
    promote.heartbeat_secs = 1;
    promote.auto_deal_secs = 0;
    cfg.promote_with = Some(promote);
    tune(&mut cfg);
    let (tx, rx) = std::sync::mpsc::channel();
    let (cmd, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let st = Arc::new(Mutex::new(NodeState::default()));
    let auto = Arc::new(AtomicBool::new(false));
    pump(rx, st.clone(), cmd.clone(), auto.clone());
    let task = if let Some(broker) = r.broker.clone() {
        cfg.broker = Some(broker);
        tokio::spawn(async move {
            let _ = pokertui::net::run_client(cfg, tx, cmd_rx).await;
        })
    } else {
        let link = r.bus.link();
        tokio::spawn(async move {
            let _ = run_client_on(link, cfg, tx, cmd_rx).await;
        })
    };
    Node { name: name.into(), id, cmd, st, auto, task }
}

/// Poll until `cond` holds or `secs` pass.
pub async fn wait_for(secs: f64, what: &str, cond: impl Fn() -> bool) {
    let t0 = Instant::now();
    while t0.elapsed().as_secs_f64() < secs {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out after {secs}s waiting for: {what}");
}

pub fn seated(n: &Node) -> usize {
    n.state()
        .view
        .as_ref()
        .map(|v| v.seats.iter().filter(|s| !s.name.is_empty()).count())
        .unwrap_or(0)
}

pub fn total_chips(n: &Node) -> u64 {
    n.state().view.as_ref().map(|v| v.seats.iter().map(|s| s.stack + s.bet).sum()).unwrap_or(0)
}

/// A stand-in for the Cloudflare relay speaking the same JSON frames
/// (see `relay/cloudflare/src/room.js`). Returns a `cf+ws://` broker URL.
pub async fn mock_relay() -> String {
    use async_tungstenite::tokio::accept_hdr_async;
    use async_tungstenite::tungstenite::Message as Ws;
    use async_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
    use futures_util::{SinkExt, StreamExt};
    use std::collections::HashMap;

    type Out = tokio::sync::mpsc::UnboundedSender<String>;
    #[derive(Default)]
    struct RoomHub {
        subs: Vec<(String, Out)>,
        retained: HashMap<String, String>,
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let rooms: Arc<Mutex<HashMap<String, RoomHub>>> = Arc::default();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            let rooms = rooms.clone();
            tokio::spawn(async move {
                let path = Arc::new(Mutex::new(String::new()));
                let p = path.clone();
                let cb = move |req: &Request, resp: Response| -> Result<Response, ErrorResponse> {
                    *p.lock().unwrap() = req.uri().path().to_string();
                    Ok(resp)
                };
                let Ok(ws) = accept_hdr_async(stream, cb).await else { return };
                let room = path.lock().unwrap().trim_start_matches("/room/").to_string();
                let (mut sink, mut stream) = ws.split();
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
                tokio::spawn(async move {
                    while let Some(f) = rx.recv().await {
                        if sink.send(Ws::Text(f.into())).await.is_err() {
                            break;
                        }
                    }
                });
                while let Some(Ok(Ws::Text(t))) = stream.next().await {
                    let Ok(v) = serde_json::from_str::<serde_json::Value>(t.as_str()) else { continue };
                    let topic = v["topic"].as_str().unwrap_or("").to_string();
                    let mut all = rooms.lock().unwrap();
                    let hub = all.entry(room.clone()).or_default();
                    match v["op"].as_str() {
                        Some("sub") => {
                            hub.subs.push((topic.clone(), tx.clone()));
                            if let Some(data) = hub.retained.get(&topic) {
                                let _ = tx.send(serde_json::json!({"op":"msg","topic":topic,"data":data,"retain":true}).to_string());
                            }
                        }
                        Some("pub") => {
                            let data = v["data"].as_str().unwrap_or("").to_string();
                            if v["retain"].as_bool() == Some(true) {
                                if data.is_empty() { hub.retained.remove(&topic); } else { hub.retained.insert(topic.clone(), data.clone()); }
                            }
                            for (t2, out) in &hub.subs {
                                if *t2 == topic {
                                    let _ = out.send(serde_json::json!({"op":"msg","topic":topic,"data":data,"retain":false}).to_string());
                                }
                            }
                        }
                        _ => {}
                    }
                }
            });
        }
    });
    format!("cf+ws://127.0.0.1:{port}")
}
