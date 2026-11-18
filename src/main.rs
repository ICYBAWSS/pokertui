//! pokertui — a terminal Texas Hold'em table with no server of its own.
//!
//! Rooms are just a topic on a public MQTT-over-WebSocket broker. The host's
//! client deals and adjudicates; every packet is signed; hole cards are sealed
//! to each player's key; each deck is committed before the deal and revealed
//! (and verified) after the hand.

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use pokertui::app::{App, Mode};
use pokertui::bot::Skill;
use pokertui::menu::{Launch, Menu, MenuOut, Screen};
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
    about = "Terminal Texas Hold'em over public pub/sub relays — no server required. Run `poker` for the menu."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
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
    /// Create a table and wait for players (skips the menu)
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
        /// Seat computer players, e.g. --bots easy,medium,hard
        #[arg(long, value_delimiter = ',')]
        bots: Vec<String>,
        /// Single-table tournament: blinds rise on a timer, last player wins
        #[arg(long)]
        sitngo: bool,
        /// Let spectators see hole cards, this many seconds late (0 = off)
        #[arg(long, default_value_t = 0)]
        spectate_delay: u64,
        /// New players wait until you accept them (press y / n at the table)
        #[arg(long)]
        approve: bool,
    },
    /// Join a table by room code (skips the menu)
    Join {
        code: String,
        #[arg(long)]
        name: Option<String>,
        /// Chips to sit down with (capped at the table's starting stack)
        #[arg(long, default_value_t = u64::MAX, hide_default_value = true)]
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
    /// Sit back down at the table you were last at (after a crash or lost connection)
    Rejoin,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let tui = !cli.plain && !matches!(cli.cmd, Some(Cmd::Verify { .. } | Cmd::Replay { .. } | Cmd::Art));
    init_logging(tui);

    match &cli.cmd {
        Some(Cmd::Verify { file }) => return verify(file),
        Some(Cmd::Replay { file }) => return replay(file),
        Some(Cmd::Art) => {
            print!("{}", art::LOBBY);
            return Ok(());
        }
        _ => {}
    }

    let config = store::Config::load();
    let identity = Arc::new(store::load_or_create_identity()?);
    let broker = cli.broker.clone().or(config.broker.clone());
    let default_name = config.name.clone().unwrap_or_else(store::default_name);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building async runtime")?;

    let launch = match cli.cmd {
        Some(Cmd::Host { name, seats, blinds, buyin, timer, no_rebuy, bots, sitngo, spectate_delay, approve }) => {
            let (mut small_blind, mut big_blind) = parse_blinds(&blinds)?;
            let tourney = sitngo.then(pokertui::game::Tourney::standard);
            if let Some(t) = &tourney {
                (small_blind, big_blind) = t.levels[0];
            }
            let bots = bots
                .iter()
                .map(|b| Skill::parse(b).with_context(|| format!("unknown bot skill `{b}` (easy, medium, hard)")))
                .collect::<Result<Vec<_>>>()?;
            if bots.len() + 1 > seats {
                bail!("--bots needs free seats: {} bots + you do not fit in {seats}", bots.len());
            }
            if !(2..=9).contains(&seats) {
                bail!("--seats must be between 2 and 9");
            }
            Some(Launch::Host {
                name: name.unwrap_or_else(|| default_name.clone()),
                params: TableParams {
                    max_seats: seats,
                    small_blind,
                    big_blind,
                    buyin,
                    turn_ms: timer.saturating_mul(1000),
                    tourney,
                },
                auto_rebuy: !no_rebuy,
                bots,
                spectate_secs: spectate_delay,
                approve_joins: approve,
            })
        }
        Some(Cmd::Join { code, name, buyin }) => {
            roomcode::decode(&code)?;
            Some(Launch::Join { name: name.unwrap_or_else(|| default_name.clone()), code, buyin })
        }
        Some(Cmd::Spectate { code }) => {
            roomcode::decode(&code)?;
            Some(Launch::Spectate { code })
        }
        Some(Cmd::Rejoin) => {
            let code = config.last_room.clone().context("no table to rejoin: join one first")?;
            Some(Launch::Join { name: default_name.clone(), code, buyin: u64::MAX })
        }
        _ => None,
    };

    let session = Session { runtime: &runtime, identity, broker, show_art: !cli.no_art };
    let result = if cli.plain {
        let launch = launch.context("--plain needs a subcommand: host, join or spectate")?;
        let (app, rx) = session.start(launch)?;
        run_plain(app, rx)
    } else {
        run_tui(&session, launch, config)
    };
    runtime.shutdown_background();
    result
}

/// Logs go to stderr in plain mode and to a file under the TUI, where stderr
/// would scribble over the screen.
fn init_logging(tui: bool) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "pokertui=warn,rumqttc=error".into());
    let file = tui
        .then(|| store::data_dir().ok())
        .flatten()
        .and_then(|dir| std::fs::OpenOptions::new().create(true).append(true).open(dir.join("poker.log")).ok());
    match file {
        Some(file) => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(file))
            .init(),
        None => tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).init(),
    }
}

struct Session<'a> {
    runtime: &'a tokio::runtime::Runtime,
    identity: Arc<pokertui::crypto::Identity>,
    broker: Option<String>,
    show_art: bool,
}

impl Session<'_> {
    /// Spawn the host or client task for one table and build its UI state.
    fn start(&self, launch: Launch) -> Result<(App, std::sync::mpsc::Receiver<UiEvent>)> {
        let (ui_tx, ui_rx) = std::sync::mpsc::channel::<UiEvent>();
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<NetCommand>();
        let tx = ui_tx.clone();
        let (mode, code) = match launch {
            Launch::Host { name, params, auto_rebuy, bots, spectate_secs, approve_joins } => {
                let (room, code) = roomcode::generate()?;
                let mut cfg = HostConfig::new(self.identity.clone(), code.clone(), room, name, params);
                cfg.broker = self.broker.clone();
                cfg.auto_rebuy = auto_rebuy;
                cfg.bots = bots;
                cfg.spectate_secs = spectate_secs;
                cfg.approve_joins = approve_joins;
                self.runtime.spawn(async move {
                    if let Err(e) = run_host(cfg, tx.clone(), cmd_rx).await {
                        let _ = tx.send(UiEvent::Closed(format!("host stopped: {e:#}")));
                    }
                });
                (Mode::Host, code)
            }
            Launch::Join { name, code, buyin } => {
                let mut cfg = ClientConfig::new(
                    self.identity.clone(),
                    code.clone(),
                    roomcode::decode(&code)?,
                    name,
                    buyin,
                );
                cfg.broker = self.broker.clone();
                self.runtime.spawn(async move {
                    if let Err(e) = run_client(cfg, tx.clone(), cmd_rx).await {
                        let _ = tx.send(UiEvent::Closed(format!("could not join: {e:#}")));
                    }
                });
                (Mode::Client, code)
            }
            Launch::Spectate { code } => {
                let mut cfg = ClientConfig::new(
                    self.identity.clone(),
                    code.clone(),
                    roomcode::decode(&code)?,
                    "spectator".into(),
                    0,
                );
                cfg.broker = self.broker.clone();
                cfg.spectate = true;
                self.runtime.spawn(async move {
                    if let Err(e) = run_client(cfg, tx.clone(), cmd_rx).await {
                        let _ = tx.send(UiEvent::Closed(format!("could not watch: {e:#}")));
                    }
                });
                (Mode::Spectator, code)
            }
        };
        let mut app = App::new(mode, code, broker_display(&self.broker), cmd_tx, self.show_art);
        app.record_stats = true;
        Ok((app, ui_rx))
    }
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

/// Menu → table → menu … in one terminal session.
fn run_tui(session: &Session, mut pending: Option<Launch>, config: store::Config) -> Result<()> {
    let mut terminal = ratatui::init();
    let _ = ratatui::crossterm::execute!(std::io::stdout(), ratatui::crossterm::event::EnableMouseCapture);
    let mut menu = Menu::new(config, session.show_art);
    let result = (|| -> Result<()> {
        loop {
            let launch = match pending.take() {
                Some(launch) => launch,
                None => match run_menu(&mut terminal, &mut menu)? {
                    Some(launch) => launch,
                    None => return Ok(()),
                },
            };
            let seated = !matches!(launch, Launch::Spectate { .. });
            let (app, rx) = match session.start(launch) {
                Ok(started) => started,
                Err(e) => {
                    menu.back_to_main(Some(format!("{e:#}")));
                    continue;
                }
            };
            // Remember the table so a crash can be undone with "Rejoin".
            if seated {
                menu.config.last_room = Some(app.code.clone());
                let _ = store::save_config(&menu.config);
            }
            let exit = run_table(&mut terminal, app, rx)?;
            // Leaving on purpose frees the seat, so there is nothing to rejoin.
            menu.config.last_room = None;
            let _ = store::save_config(&menu.config);
            match exit {
                TableExit::Quit => return Ok(()),
                TableExit::Menu(reason) => menu.back_to_main(reason),
            }
        }
    })();
    let _ = ratatui::crossterm::execute!(std::io::stdout(), ratatui::crossterm::event::DisableMouseCapture);
    ratatui::restore();
    result
}

/// Turn a mouse event into the key press it stands for: clicking a `[k]` label
/// presses `k`, the wheel moves the selection. `splash` makes any click a key.
fn mouse_key(m: ratatui::crossterm::event::MouseEvent, buf: &ratatui::buffer::Buffer, splash: bool) -> Option<ratatui::crossterm::event::KeyEvent> {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
    let code = match m.kind {
        MouseEventKind::ScrollUp => KeyCode::Up,
        MouseEventKind::ScrollDown => KeyCode::Down,
        MouseEventKind::Down(MouseButton::Left) if splash => KeyCode::Enter,
        MouseEventKind::Down(MouseButton::Left) => KeyCode::Char(click_key(buf, m.column, m.row)?),
        _ => return None,
    };
    Some(KeyEvent::new(code, KeyModifiers::NONE))
}

/// The `k` of the `[k]` label under (x, y). A label's hit area runs from its
/// `[` to the first run of three spaces after its text.
fn click_key(buf: &ratatui::buffer::Buffer, x: u16, y: u16) -> Option<char> {
    let area = buf.area;
    if y < area.y || y >= area.y + area.height {
        return None;
    }
    let cells: Vec<&str> = (area.x..area.x + area.width).map(|cx| buf[(cx, y)].symbol()).collect();
    let x = x.checked_sub(area.x)? as usize;
    let mut i = 0;
    while i + 2 < cells.len() {
        if cells[i] == "[" && cells[i + 2] == "]" && cells[i + 1].chars().count() == 1 {
            let mut end = i + 3;
            while end < cells.len() && !(cells[end..].iter().take(3).all(|c| *c == " ") && end + 3 <= cells.len()) {
                end += 1;
            }
            if (i..end).contains(&x) {
                return cells[i + 1].chars().next();
            }
            i = end;
        } else {
            i += 1;
        }
    }
    None
}

fn run_menu(terminal: &mut ratatui::DefaultTerminal, menu: &mut Menu) -> Result<Option<Launch>> {
    use ratatui::crossterm::event::{self, Event, KeyEventKind};
    loop {
        menu.tick += 1;
        let buf = terminal.draw(|frame| pokertui::menu::draw(frame, menu))?.buffer.clone();
        if event::poll(Duration::from_millis(50))?
            && let Some(key) = match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => Some(key),
                Event::Mouse(m) => mouse_key(m, &buf, menu.screen == Screen::Splash),
                _ => None,
            }
        {
            match menu.handle_key(key) {
                Some(MenuOut::Launch(launch)) => return Ok(Some(launch)),
                Some(MenuOut::Quit) => return Ok(None),
                None => {}
            }
        }
    }
}

enum TableExit {
    Quit,
    Menu(Option<String>),
}

fn run_table(
    terminal: &mut ratatui::DefaultTerminal,
    mut app: App,
    mut ui_rx: std::sync::mpsc::Receiver<UiEvent>,
) -> Result<TableExit> {
    use ratatui::crossterm::event::{self, Event, KeyEventKind};
    let exit = loop {
        app.pump(&mut ui_rx);
        app.on_tick();
        if std::mem::take(&mut app.bell) {
            use std::io::Write;
            let _ = std::io::stdout().write_all(b"\x07").and_then(|_| std::io::stdout().flush());
        }
        let buf = terminal.draw(|frame| ui::draw(frame, &app))?.buffer.clone();
        if app.quit {
            break TableExit::Quit;
        }
        if app.leave {
            break TableExit::Menu(app.closed.clone());
        }
        if event::poll(Duration::from_millis(50))?
            && let Some(key) = match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => Some(key),
                Event::Mouse(m) => mouse_key(m, &buf, false),
                _ => None,
            }
        {
            app.handle_key(key);
        }
    };
    // Free our seat, then stop the network task for this table.
    let _ = app.net.send(NetCommand::Leave);
    let _ = app.net.send(NetCommand::Quit);
    Ok(exit)
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

#[cfg(test)]
mod mouse_tests {
    use super::click_key;
    use ratatui::{buffer::Buffer, layout::Rect};

    #[test]
    fn click_hits_labels_only() {
        let mut b = Buffer::empty(Rect::new(0, 0, 40, 1));
        b.set_string(1, 0, "[1] Fold   [2] Call 50   [3] Raise", ratatui::style::Style::default());
        assert_eq!(click_key(&b, 2, 0), Some('1'));
        assert_eq!(click_key(&b, 6, 0), Some('1'));
        assert_eq!(click_key(&b, 16, 0), Some('2'));
        assert_eq!(click_key(&b, 22, 0), Some('2'));
        assert_eq!(click_key(&b, 9, 0), None, "gap between labels");
        assert_eq!(click_key(&b, 30, 0), Some('3'));
        assert_eq!(click_key(&b, 39, 0), None);
    }
}
