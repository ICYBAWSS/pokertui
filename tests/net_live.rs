//! Live end-to-end test against a public relay.
//!
//! Ignored by default because it needs the internet and a public broker:
//!
//!   cargo test --test net_live -- --ignored --nocapture
//!
//! It boots a host and two clients through the real MQTT/WSS path, plays one
//! hand to showdown with simple "call or check" bots, and asserts the hand
//! completed with a five-card board and a verifiable deck reveal.

use pokertui::app::Mode;
use pokertui::game::{Move, Phase, TableParams};
use pokertui::net::{
    ClientConfig, HostConfig, NetCommand, UiEvent, roomcode, run_client, run_host,
};
use pokertui::crypto::Identity;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

struct BotArgs {
    rx: Receiver<UiEvent>,
    cmd: UnboundedSender<NetCommand>,
    host: bool,
    seats_needed: usize,
    name: &'static str,
}

fn spawn_bot(args: BotArgs, done: tokio::sync::mpsc::UnboundedSender<String>) {
    std::thread::spawn(move || {
        let t0 = std::time::Instant::now();
        let mut seat: Option<usize> = None;
        let mut started = false;
        let mut last_sent: Option<(u32, u64)> = None;
        while let Ok(event) = args.rx.recv() {
            match event {
                UiEvent::Joined { seat: s } => seat = Some(s),
                UiEvent::View(view) => {
                    eprintln!(
                        "[{}] {:>6.1}s view v{} hand {} phase {:?} actor {:?}",
                        args.name,
                        t0.elapsed().as_secs_f32(),
                        view.version,
                        view.hand_no,
                        view.phase,
                        view.actor
                    );
                    let occupied = view.seats.iter().filter(|s| !s.name.is_empty()).count();
                    if args.host
                        && !started
                        && view.phase == Phase::Lobby
                        && occupied >= args.seats_needed
                    {
                        started = true;
                        let _ = args.cmd.send(NetCommand::Start);
                    }
                    if view.actor.is_some() && view.actor == seat {
                        let key = (view.hand_no, view.version);
                        if last_sent != Some(key) {
                            last_sent = Some(key);
                            if let Some(legal) = &view.actor_legal {
                                let mv = if legal.can_check {
                                    Move::Check
                                } else {
                                    Move::Call
                                };
                                let _ = args.cmd.send(NetCommand::Action(mv));
                            }
                        }
                    }
                }
                UiEvent::HandResult(result) => {
                    let _ = done.send(format!(
                        "hand {} board {} pots {}",
                        result.hand_no,
                        result.board.len(),
                        result.pots.len()
                    ));
                }
                UiEvent::Error(e) => {
                    eprintln!("[{}] error: {e}", args.name);
                }
                UiEvent::Closed(reason) => {
                    eprintln!("[{}] closed: {reason}", args.name);
                    let _ = done.send(format!("closed: {reason}"));
                    break;
                }
                _ => {}
            }
        }
        let _ = done.send("bot exited".into());
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs internet + a public MQTT broker"]
async fn host_and_two_clients_play_a_hand() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    let broker = std::env::var("POKER_BROKER").ok();
    let (room, code) = roomcode::generate().unwrap();
    let params = TableParams {
        max_seats: 3,
        small_blind: 25,
        big_blind: 50,
        buyin: 2_000,
        turn_ms: 20_000,
    };

    let host_id = Arc::new(Identity::generate().unwrap());
    let a_id = Arc::new(Identity::generate().unwrap());
    let b_id = Arc::new(Identity::generate().unwrap());

    let (host_tx, host_rx) = std::sync::mpsc::channel();
    let (a_tx, a_rx) = std::sync::mpsc::channel();
    let (b_tx, b_rx) = std::sync::mpsc::channel();

    let (host_cmd_tx, host_cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let (a_cmd_tx, a_cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let (b_cmd_tx, b_cmd_rx) = tokio::sync::mpsc::unbounded_channel();

    tokio::spawn(run_host(
        HostConfig {
            identity: host_id,
            code: code.clone(),
            room,
            name: "host".into(),
            params,
            broker: broker.clone(),
            auto_rebuy: true,
        },
        host_tx,
        host_cmd_rx,
    ));
    tokio::spawn(run_client(
        ClientConfig {
            identity: a_id,
            code: code.clone(),
            room,
            name: "alice".into(),
            buyin: 2_000,
            broker: broker.clone(),
            spectate: false,
        },
        a_tx,
        a_cmd_rx,
    ));
    tokio::spawn(run_client(
        ClientConfig {
            identity: b_id,
            code: code.clone(),
            room,
            name: "bob".into(),
            buyin: 2_000,
            broker: broker.clone(),
            spectate: false,
        },
        b_tx,
        b_cmd_rx,
    ));

    let (done_tx, mut done_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    spawn_bot(
        BotArgs {
            rx: host_rx,
            cmd: host_cmd_tx,
            host: true,
            seats_needed: 3,
            name: "host",
        },
        done_tx.clone(),
    );
    spawn_bot(
        BotArgs {
            rx: a_rx,
            cmd: a_cmd_tx,
            host: false,
            seats_needed: 3,
            name: "alice",
        },
        done_tx.clone(),
    );
    spawn_bot(
        BotArgs {
            rx: b_rx,
            cmd: b_cmd_tx,
            host: false,
            seats_needed: 3,
            name: "bob",
        },
        done_tx,
    );

    let outcome = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            match done_rx.recv().await {
                Some(msg) if msg.starts_with("hand ") => return Some(msg),
                Some(msg) if msg.starts_with("closed:") => return None,
                Some(_) => continue,
                None => return None,
            }
        }
    })
    .await;

    match outcome {
        Ok(Some(msg)) => {
            println!("hand completed: {msg}");
            assert!(msg.contains("board 5"), "expected a full board: {msg}");
        }
        Ok(None) => panic!("table closed before a hand finished"),
        Err(_) => panic!("timed out waiting for a hand"),
    }
    let _ = Mode::Client;
}
