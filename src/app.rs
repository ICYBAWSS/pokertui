//! UI-facing application state and input handling. Rendering lives in `ui`.

use crate::game::{Card, HandResult, Legal, Move, Phase, TableView};
use crate::net::{NetCommand, UiEvent};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Host,
    Client,
    Spectator,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Host => "hosting",
            Mode::Client => "playing",
            Mode::Spectator => "spectating",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LogKind {
    Info,
    Action,
    Chat,
    Error,
}

#[derive(Clone)]
pub struct LogLine {
    pub text: String,
    pub kind: LogKind,
}

pub struct App {
    pub mode: Mode,
    pub code: String,
    pub broker: String,
    pub view: Option<TableView>,
    pub my_seat: Option<usize>,
    pub my_cards: Vec<Card>,
    pub my_cards_hand: u32,
    pub log: Vec<LogLine>,
    pub input: String,
    pub chat_mode: bool,
    pub connected: bool,
    pub notice: Option<(String, Instant)>,
    pub celebrate_until: Option<Instant>,
    pub quit: bool,
    pub help: bool,
    pub show_art: bool,
    pub last_message: String,
    pub result_seen: Option<u32>,
    pub net: UnboundedSender<NetCommand>,
    pub started: Instant,
    pub tick: u64,
}

impl App {
    pub fn new(
        mode: Mode,
        code: String,
        broker: String,
        net: UnboundedSender<NetCommand>,
        show_art: bool,
    ) -> Self {
        Self {
            mode,
            code,
            broker,
            view: None,
            my_seat: None,
            my_cards: Vec::new(),
            my_cards_hand: 0,
            log: Vec::new(),
            input: String::new(),
            chat_mode: false,
            connected: false,
            notice: None,
            celebrate_until: None,
            quit: false,
            help: false,
            show_art,
            last_message: String::new(),
            result_seen: None,
            net,
            started: Instant::now(),
            tick: 0,
        }
    }

    pub fn push_log(&mut self, kind: LogKind, text: impl Into<String>) {
        let text = text.into();
        if text.trim().is_empty() {
            return;
        }
        self.log.push(LogLine { text, kind });
        if self.log.len() > 400 {
            self.log.remove(0);
        }
    }

    pub fn set_notice(&mut self, text: impl Into<String>) {
        self.notice = Some((text.into(), Instant::now() + Duration::from_secs(4)));
    }

    pub fn apply_event(&mut self, event: UiEvent) {
        match event {
            UiEvent::Connected { broker } => {
                self.connected = true;
                self.broker = broker;
                self.push_log(LogKind::Info, "connected to relay");
            }
            UiEvent::Disconnected(reason) => {
                self.connected = false;
                self.push_log(LogKind::Error, format!("relay: {reason}"));
                self.set_notice("relay disconnected, reconnecting…");
            }
            UiEvent::Joined { seat } => {
                self.my_seat = Some(seat);
                self.push_log(LogKind::Info, format!("you are in seat {seat}"));
            }
            UiEvent::View(view) => self.apply_view(view),
            UiEvent::MyCards { hand, cards } => {
                self.my_cards_hand = hand;
                self.my_cards = cards;
            }
            UiEvent::HandResult(result) => self.apply_result(result),
            UiEvent::Chat { from, text } => {
                self.push_log(LogKind::Chat, format!("{from}: {text}"));
            }
            UiEvent::Info(text) => self.push_log(LogKind::Info, text),
            UiEvent::Error(text) => {
                self.push_log(LogKind::Error, text.clone());
                self.set_notice(text);
            }
            UiEvent::Closed(reason) => {
                self.push_log(LogKind::Info, reason);
                self.quit = true;
            }
        }
    }

    fn apply_view(&mut self, view: TableView) {
        if !view.message.is_empty() && view.message != self.last_message {
            self.last_message = view.message.clone();
            self.push_log(LogKind::Action, view.message.clone());
        }
        if let Some(result) = &view.last_result
            && self.result_seen != Some(result.hand_no)
        {
            let result = result.clone();
            self.apply_result(result);
        }
        // Clear hole cards that belong to a hand that has ended.
        if view.phase == Phase::Lobby {
            self.my_cards.clear();
        }
        self.view = Some(view);
    }

    fn apply_result(&mut self, result: HandResult) {
        if self.result_seen == Some(result.hand_no) {
            return;
        }
        self.result_seen = Some(result.hand_no);
        for pot in &result.pots {
            let names = pot
                .winners
                .iter()
                .map(|&w| self.seat_name(w))
                .collect::<Vec<_>>()
                .join(", ");
            self.push_log(
                LogKind::Action,
                format!("{} wins {} ({})", names, pot.amount, pot.desc),
            );
        }
        let i_won = self
            .my_seat
            .is_some_and(|me| result.pots.iter().any(|p| p.winners.contains(&me)));
        if i_won {
            self.celebrate_until = Some(Instant::now() + Duration::from_millis(1600));
        }
        // The next hand re-deals; drop stale cards until the deal arrives.
        self.my_cards.clear();
    }

    pub fn seat_name(&self, seat: usize) -> String {
        self.view
            .as_ref()
            .and_then(|v| v.seats.get(seat))
            .map(|s| s.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| format!("seat {seat}"))
    }

    pub fn my_legal(&self) -> Option<Legal> {
        let view = self.view.as_ref()?;
        let me = self.my_seat?;
        if view.actor != Some(me) {
            return None;
        }
        view.actor_legal.clone()
    }

    pub fn my_turn(&self) -> bool {
        self.view
            .as_ref()
            .is_some_and(|v| v.actor.is_some() && v.actor == self.my_seat)
    }

    pub fn deadline_left(&self) -> Option<u64> {
        let view = self.view.as_ref()?;
        if view.deadline_ms == 0 {
            return None;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        Some(view.deadline_ms.saturating_sub(now))
    }

    /// Drain network events. Returns true if anything changed.
    pub fn pump(&mut self, rx: &mut std::sync::mpsc::Receiver<UiEvent>) -> bool {
        let mut changed = false;
        while let Ok(event) = rx.try_recv() {
            self.apply_event(event);
            changed = true;
        }
        changed
    }

    pub fn on_tick(&mut self) {
        self.tick += 1;
        if let Some((_, until)) = &self.notice
            && Instant::now() > *until
        {
            self.notice = None;
        }
        if let Some(until) = self.celebrate_until
            && Instant::now() > until
        {
            self.celebrate_until = None;
        }
    }

    /// Submit a command line from plain mode.
    pub fn submit_line(&mut self, text: &str) {
        self.input = text.to_string();
        self.submit();
    }

    fn submit(&mut self) {
        let text = self.input.trim().to_string();
        self.input.clear();
        if text.is_empty() {
            return;
        }
        if self.chat_mode {
            self.chat_mode = false;
            if !text.is_empty() {
                let _ = self.net.send(NetCommand::Chat(text));
            }
            return;
        }
        if let Some(rest) = text.strip_prefix('/') {
            let _ = self.net.send(NetCommand::Chat(rest.to_string()));
            return;
        }
        let mut parts = text.splitn(2, ' ');
        let cmd = parts.next().unwrap_or("").to_lowercase();
        let arg = parts.next().unwrap_or("").trim();
        match cmd.as_str() {
            "f" | "fold" => {
                let _ = self.net.send(NetCommand::Action(Move::Fold));
            }
            "x" | "check" => {
                let _ = self.net.send(NetCommand::Action(Move::Check));
            }
            "c" | "call" => {
                let _ = self.net.send(NetCommand::Action(Move::Call));
            }
            "a" | "allin" | "all-in" => {
                let _ = self.net.send(NetCommand::Action(Move::AllIn));
            }
            "b" | "bet" | "r" | "raise" => match arg.parse::<u64>() {
                Ok(to) => {
                    let _ = self.net.send(NetCommand::Action(Move::Raise { to }));
                }
                Err(_) => self.set_notice("usage: r <amount>  e.g. `r 300`"),
            },
            "s" | "start" => {
                let _ = self.net.send(NetCommand::Start);
            }
            "q" | "quit" => self.quit = true,
            "h" | "help" | "?" => self.help = true,
            ":h" => self.help = true,
            ":q" => self.quit = true,
            other => self.set_notice(format!("unknown command `{other}` · try :help")),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return true;
        }
        if self.help {
            self.help = false;
            return true;
        }
        match key.code {
            KeyCode::Esc => {
                if self.input.is_empty() {
                    self.quit = true;
                } else {
                    self.input.clear();
                    self.chat_mode = false;
                }
                true
            }
            KeyCode::Enter => {
                self.submit();
                true
            }
            KeyCode::Backspace => {
                self.input.pop();
                true
            }
            KeyCode::Char(c) => {
                if self.input.is_empty() && !self.chat_mode {
                    match c {
                        'f' => {
                            let _ = self.net.send(NetCommand::Action(Move::Fold));
                            return true;
                        }
                        'x' => {
                            let _ = self.net.send(NetCommand::Action(Move::Check));
                            return true;
                        }
                        'c' => {
                            let _ = self.net.send(NetCommand::Action(Move::Call));
                            return true;
                        }
                        'a' => {
                            let _ = self.net.send(NetCommand::Action(Move::AllIn));
                            return true;
                        }
                        'r' | 'b' => {
                            self.input.push(c);
                            self.input.push(' ');
                            return true;
                        }
                        '/' => {
                            self.chat_mode = true;
                            return true;
                        }
                        '?' => {
                            self.help = true;
                            return true;
                        }
                        's' if self.mode == Mode::Host => {
                            let _ = self.net.send(NetCommand::Start);
                            return true;
                        }
                        _ => {}
                    }
                }
                if c.is_ascii_graphic() || c == ' ' {
                    self.input.push(c);
                }
                true
            }
            _ => false,
        }
    }
}
