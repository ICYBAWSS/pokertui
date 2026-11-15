//! pokertui — a terminal Texas Hold'em table with no server of its own.
//!
//! Rooms are just a topic on a public MQTT-over-WebSocket broker. The host's
//! client deals and adjudicates; every packet is signed; hole cards are sealed
//! to each player's key; each deck is committed before the deal and revealed
//! (and verified) after the hand.

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use pokertui::app::App;
use pokertui::game::{TableParams, verify_record};
use pokertui::net::{ClientConfig, HostConfig, NetCommand, UiEvent, roomcode, run_client, run_host};
use pokertui::{art, store, ui};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "poker",
    version,
    about = "Terminal Texas Hold'em over public pub/sub relays — no server required"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
    /// Relay endpoint, e.g. wss://broker.emqx.io:8084/mqtt
    #[arg(long, global = true, env = "POKER_BROKER")]
    broker: Option<String>,
    /// Line mode: print events and read commands from stdin (no TUI)
    #[arg(long, global = true)]
    plain: bool,
    /// Do not render the embedded ASCII art
    #[arg(long, global = true)]
    no_art: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a table and wait for players
    Host {
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value_t = 6)]
        seats: usize,
        /// Small/big blind, e.g. 25/50
        #[arg(long, default_value = "25/50")]
        blinds: String,
        #[arg(long, default_value_t = 5_000)]
        buyin: u64,
        /// Seconds per turn (0 disables the clock)
        #[arg(long, default_value_t = 60)]
        timer: u64,
        /// Do not top busted players back up automatically
        #[arg(long)]
        no_rebuy: bool,
    },
    /// Join a table by room code
    Join {
        code: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value_t = 5_000)]
        buyin: u64,
    },
    /// Watch a table without sitting down
    Spectate { code: String },
    /// Check a saved hand's deck commitment
    Verify { file: PathBuf },
    /// Print a saved hand as text
    Replay { file: PathBuf },
    /// Print the embedded lobby art (handy for a README or MOTD)
    Art,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "pokertui=warn,rumqttc=error".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();

    match &cli.cmd {
        Cmd::Verify { file } => return verify(file),
        Cmd::Replay { file } => return replay(file),
        Cmd::Art => {
            print!("{}", art::LOBBY);
            return Ok(());
        }
        _ => {}
    }

    let config = store::Config::load();
    let identity = Arc::new(store::load_or_create_identity()?);
    let broker = cli.broker.clone().or(config.broker.clone());
    let default_name = config
        .name
        .clone()
        .unwrap_or_else(store::default_name);

    let (ui_tx, ui_rx) = std::sync::mpsc::channel::<UiEvent>();
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<NetCommand>();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building async runtime")?;

    let (mode, code, app_broker) = match cli.cmd {
        Cmd::Host {
            name,
            seats,
            blinds,
            buyin,
            timer,
            no_rebuy,
        } => {
            let (small_blind, big_blind) = parse_blinds(&blinds)?;
            if seats < 2 || seats > 9 {
                bail!("--seats must be between 2 and 9");
            }
            let (room, code) = roomcode::generate()?;
            let cfg = HostConfig {
                identity: identity.clone(),
                code: code.clone(),
                room,
                name: name.unwrap_or(default_name),
                params: TableParams {
                    max_seats: seats,
                    small_blind,
                    big_blind,
                    buyin,
                    turn_ms: timer.saturating_mul(1000),
                },
                broker: broker.clone(),
                auto_rebuy: !no_rebuy,
            };
            let tx = ui_tx.clone();
            runtime.spawn(async move {
                if let Err(e) = run_host(cfg, tx, cmd_rx).await {
                    eprintln!("host stopped: {e:#}");
                }
            });
            (pokertui::app::Mode::Host, code, broker_display(&broker))
        }
        Cmd::Join { code, name, buyin } => {
            let room = roomcode::decode(&code)?;
            let cfg = ClientConfig {
                identity: identity.clone(),
                code: code.clone(),
                room,
                name: name.unwrap_or(default_name),
                buyin,
                broker: broker.clone(),
                spectate: false,
            };
            let tx = ui_tx.clone();
            runtime.spawn(async move {
                if let Err(e) = run_client(cfg, tx, cmd_rx).await {
                    eprintln!("client stopped: {e:#}");
                }
            });
            (pokertui::app::Mode::Client, code, broker_display(&broker))
        }
        Cmd::Spectate { code } => {
            let room = roomcode::decode(&code)?;
            let cfg = ClientConfig {
                identity: identity.clone(),
                code: code.clone(),
                room,
                name: "spectator".into(),
                buyin: 0,
                broker: broker.clone(),
                spectate: true,
            };
            let tx = ui_tx.clone();
            runtime.spawn(async move {
                if let Err(e) = run_client(cfg, tx, cmd_rx).await {
                    eprintln!("spectator stopped: {e:#}");
                }
            });
            (pokertui::app::Mode::Spectator, code, broker_display(&broker))
        }
        _ => unreachable!(),
    };

    let app = App::new(mode, code, app_broker, cmd_tx, !cli.no_art);
    let result = if cli.plain {
        run_plain(app, ui_rx)
    } else {
        run_tui(app, ui_rx)
    };
    runtime.shutdown_background();
    result
}

fn broker_display(broker: &Option<String>) -> String {
    broker
        .clone()
        .unwrap_or_else(|| pokertui::net::mqtt::DEFAULT_BROKERS[0].to_string())
}

fn parse_blinds(text: &str) -> Result<(u64, u64)> {
    let (small, big) = text
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("blinds look like 25/50"))?;
    let small: u64 = small.trim().parse().context("bad small blind")?;
    let big: u64 = big.trim().parse().context("bad big blind")?;
    if small == 0 || big < small {
        bail!("blinds must be positive with big blind >= small blind");
    }
    Ok((small, big))
}

fn verify(file: &PathBuf) -> Result<()> {
    let record = store::load_hand(file)?;
    verify_record(&record)?;
    println!("✔ hand #{} in room {}", record.hand_no, record.room);
    println!("  deck commitment matches the reveal (52 unique cards)");
    println!(
        "  board: {}",
        record.board.iter().map(|c| c.pretty()).collect::<Vec<_>>().join(" ")
    );
    for pot in &record.result.pots {
        let winners: Vec<String> = pot
            .winners
            .iter()
            .map(|w| {
                record
                    .players
                    .iter()
                    .find(|p| p.seat == *w)
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| format!("seat {w}"))
            })
            .collect();
        println!("  {} → {} ({})", pot.amount, winners.join(", "), pot.desc);
    }
    println!("  every board and revealed hole card is inside the committed deck");
    Ok(())
}

fn replay(file: &PathBuf) -> Result<()> {
    let record = store::load_hand(file)?;
    println!(
        "hand #{} · room {} · blinds {}/{}",
        record.hand_no, record.room, record.stakes.0, record.stakes.1
    );
    for p in &record.players {
        println!(
            "  seat {} {:<12} start {}{}{}",
            p.seat,
            p.name,
            p.starting_stack,
            if p.hole.len() == 2 {
                format!("  hole {}", p.hole.iter().map(|c| c.pretty()).collect::<Vec<_>>().join(" "))
            } else {
                String::new()
            },
            ""
        );
    }
    println!("  board: {}", record.board.iter().map(|c| c.pretty()).collect::<Vec<_>>().join(" "));
    for action in &record.actions {
        println!("  · {action}");
    }
    for pot in &record.result.pots {
        println!("  {} → {:?} ({})", pot.amount, pot.winners, pot.desc);
    }
    match verify_record(&record) {
        Ok(()) => println!("  ✔ deck commitment verified"),
        Err(e) => println!("  ✘ verification failed: {e}"),
    }
    Ok(())
}

fn run_tui(mut app: App, mut ui_rx: std::sync::mpsc::Receiver<UiEvent>) -> Result<()> {
    use ratatui::crossterm::event::{self, Event, KeyEventKind};

    let mut terminal = ratatui::init();
    let result = (|| -> Result<()> {
        loop {
            app.pump(&mut ui_rx);
            app.on_tick();
            terminal.draw(|frame| ui::draw(frame, &app))?;
            if app.quit {
                break;
            }
            if event::poll(Duration::from_millis(50))? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        app.handle_key(key);
                    }
                    Event::Resize(_, _) => {}
                    _ => {}
                }
            }
        }
        Ok(())
    })();
    ratatui::restore();
    let _ = app.net.send(NetCommand::Quit);
    result
}

fn run_plain(mut app: App, ui_rx: std::sync::mpsc::Receiver<UiEvent>) -> Result<()> {
    use std::io::{BufRead, Write};

    println!(
        "pokertui plain mode · {} · room {}",
        app.mode.label(),
        app.code
    );
    println!("commands: f c x a r <n> s(start) /chat :q");
    let stdin = std::io::stdin();
    loop {
        while let Ok(event) = ui_rx.try_recv() {
            app.apply_event(event);
        }
        if app.quit {
            break;
        }
        if let Some(view) = app.view.clone() {
            print_state(&app, &view);
        }
        print!("> ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            break;
        }
        app.submit_line(line.trim());
        while let Ok(event) = ui_rx.try_recv() {
            app.apply_event(event);
        }
    }
    let _ = app.net.send(NetCommand::Quit);
    Ok(())
}

fn print_state(app: &App, view: &pokertui::game::TableView) {
    let board = if view.board.is_empty() {
        "—".to_string()
    } else {
        view.board.iter().map(|c| c.pretty()).collect::<Vec<_>>().join(" ")
    };
    let me = app
        .my_seat
        .and_then(|s| view.seats.get(s))
        .map(|s| format!("{} (stack {})", s.name, s.stack))
        .unwrap_or_else(|| "spectator".into());
    println!(
        "[{}] hand #{} pot {} board {} | you: {} | {}",
        app.code, view.hand_no, view.pot, board, me, view.message
    );
    let seats = view
        .seats
        .iter()
        .filter(|s| !s.name.is_empty())
        .map(|s| format!("{}{}:{}", s.name, if s.folded { "(fold)" } else { "" }, s.stack))
        .collect::<Vec<_>>()
        .join("  ");
    println!("    {seats}");
    if !app.my_cards.is_empty() {
        println!(
            "    your cards: {}",
            app.my_cards.iter().map(|c| c.pretty()).collect::<Vec<_>>().join(" ")
        );
    }
}
