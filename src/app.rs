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

/// The raise-amount overlay.
#[derive(Clone, Debug, Default)]
pub struct RaisePicker {
    pub amount: u64,
    /// Digits typed so far (None = using presets / arrows).
    pub typed: Option<String>,
}

/// Timestamps that drive the table animations; see `ui`.
#[derive(Clone, Debug, Default)]
pub struct Anim {
    pub deal_at: Option<Instant>,
    pub board_from: usize,
    pub board_at: Option<Instant>,
    pub pot_from: u64,
    pub pot_at: Option<Instant>,
    pub chips_from: Vec<usize>,
    pub chips_at: Option<Instant>,
    pub deadline: u64,
    pub deadline_total: u64,
    pub result_at: Option<Instant>,
}

pub const DEAL_MS: u64 = 550;
pub const FLIP_MS: u64 = 260;
pub const FLIP_STAGGER_MS: u64 = 140;
pub const POT_MS: u64 = 500;
pub const CHIPS_MS: u64 = 420;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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
    /// Go back to the menu (table closed or player left).
    pub leave: bool,
    /// Why the table closed, shown on the menu.
    pub closed: Option<String>,
    pub confirm_leave: bool,
    pub raise: Option<RaisePicker>,
    pub anim: Anim,
    /// Ring the terminal bell: your turn just started.
    pub bell: bool,
    /// Host kick/ban picker: `Some(ban)` while waiting for a seat number.
    pub kick_mode: Option<bool>,
    /// Players waiting for you (the host) to let them in.
    pub waiting: Vec<String>,
    bot_next: crate::bot::Skill,
    /// Local rankings, updated after every hand (None disables recording).
    pub stats: Option<crate::rank::Rankings>,
    stats_seen: Option<u32>,
    tourney_recorded: bool,
    /// Where co-signed hands are queued for the global leaderboard, and the relay's key.
    pub outbox: Option<(std::path::PathBuf, x25519_dalek::PublicKey)>,
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
            leave: false,
            closed: None,
            confirm_leave: false,
            raise: None,
            anim: Anim::default(),
            bell: false,
            kick_mode: None,
            waiting: Vec::new(),
            bot_next: crate::bot::Skill::Easy,
            stats: None,
            stats_seen: None,
            tourney_recorded: false,
            outbox: None,
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
            }
            UiEvent::Disconnected(reason) => {
                self.connected = false;
                self.push_log(LogKind::Error, format!("relay: {reason}"));
                self.set_notice("reconnecting…");
            }
            UiEvent::Joined { seat } => {
                self.my_seat = Some(seat);
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
            UiEvent::Promoted => self.mode = Mode::Host,
            UiEvent::Waiting(names) => {
                if let Some(first) = names.iter().find(|n| !self.waiting.contains(n)) {
                    self.set_notice(format!("{first} wants to join: y accept · n decline"));
                    self.bell = true;
                }
                self.waiting = names;
            }
            UiEvent::Bundle(bundle) => {
                if let Some((dir, key)) = &self.outbox {
                    let now = now_ms();
                    crate::leaderboard::with_outbox(dir, |ob| {
                        let _ = ob.push(&bundle, key, now);
                    });
                }
            }
            UiEvent::Error(text) => {
                self.push_log(LogKind::Error, text.clone());
                self.set_notice(text);
            }
            UiEvent::Closed(reason) => {
                self.push_log(LogKind::Info, reason.clone());
                // The first reason is the real one; later ones are shutdown noise.
                self.closed.get_or_insert(reason);
                self.leave = true;
            }
        }
    }

    fn apply_view(&mut self, view: TableView) {
        let was_my_turn = self.my_legal().is_some();
        if !view.message.is_empty() && view.message != self.last_message {
            self.last_message = view.message.clone();
            self.push_log(LogKind::Action, view.message.clone());
        }
        if let Some(result) = &view.last_result
            && self.stats_seen != Some(result.hand_no)
        {
            self.stats_seen = Some(result.hand_no);
            if self.mode != Mode::Spectator
                && let Some(stats) = self.stats.as_mut()
            {
                stats.record_hand(&view);
                stats.save();
            }
        }
        if let Some(t) = &view.tourney
            && t.over
            && !self.tourney_recorded
        {
            self.tourney_recorded = true;
            if self.mode != Mode::Spectator
                && let Some(stats) = self.stats.as_mut()
            {
                stats.record_tourney(&view);
                stats.save();
            }
            let standings = t
                .places
                .iter()
                .map(|(_, name, place)| format!("{place}. {name}"))
                .collect::<Vec<_>>()
                .join("  ");
            self.push_log(LogKind::Info, format!("tournament over: {standings}"));
            self.set_notice("tournament over");
        }
        if let Some(result) = &view.last_result
            && self.result_seen != Some(result.hand_no)
        {
            let result = result.clone();
            self.apply_result(result);
        }
        // Keep your cards on show through the result; drop them once a
        // different hand is being played (its deal replaces them).
        if view.phase == Phase::Lobby || (view.phase == Phase::Betting && view.hand_no != self.my_cards_hand) {
            self.my_cards.clear();
        }
        self.note_animations(&view);
        self.view = Some(view);
        if !was_my_turn && self.my_legal().is_some() {
            self.bell = true;
        }
    }

    /// Compare the incoming view with the current one and start animations.
    fn note_animations(&mut self, new: &TableView) {
        let now = Instant::now();
        let shown_pot = self.pot_shown();
        let a = &mut self.anim;
        let Some(old) = &self.view else {
            // First view (e.g. joining mid-hand): show it as is.
            a.board_from = new.board.len();
            a.pot_from = new.pot;
            return;
        };
        let new_hand = old.hand_no != new.hand_no;
        if new.phase == Phase::Betting && (new_hand || old.phase != Phase::Betting) {
            a.deal_at = Some(now);
            a.board_from = 0;
            a.board_at = None;
        }
        if !new_hand && new.board.len() > old.board.len() {
            a.board_from = old.board.len();
            a.board_at = Some(now);
        } else if new.board.len() < a.board_from {
            a.board_from = new.board.len();
        }
        if new.pot != old.pot {
            a.pot_from = if new_hand { 0 } else { shown_pot };
            a.pot_at = Some(now);
        }
        if !new_hand && new.pot > old.pot {
            let swept: Vec<usize> = old
                .seats
                .iter()
                .filter(|s| s.bet > 0 && new.seats.get(s.seat).is_some_and(|n| n.bet == 0))
                .map(|s| s.seat)
                .collect();
            if !swept.is_empty() {
                a.chips_from = swept;
                a.chips_at = Some(now);
            }
        }
        if new.deadline_ms != a.deadline {
            a.deadline = new.deadline_ms;
            a.deadline_total = new.deadline_ms.saturating_sub(now_ms());
        }
    }

    /// Pot as currently displayed (counts up after a change).
    pub fn pot_shown(&self) -> u64 {
        let Some(view) = &self.view else { return 0 };
        match self.anim.pot_at {
            Some(at) => {
                let t = crate::anim::ease_out(crate::anim::progress(at, Duration::from_millis(POT_MS)));
                crate::anim::lerp_u64(self.anim.pot_from, view.pot, t)
            }
            None => view.pot,
        }
    }

    /// Fraction of the turn clock left, if a clock is running.
    pub fn timer_fraction(&self) -> Option<f32> {
        let left = self.deadline_left()?;
        if self.anim.deadline_total == 0 {
            return None;
        }
        Some((left as f32 / self.anim.deadline_total as f32).clamp(0.0, 1.0))
    }

    /// Raise presets: (label, raise-to) clamped to what's legal.
    pub fn raise_presets(&self) -> Option<[(&'static str, u64); 4]> {
        let legal = self.my_legal()?;
        let view = self.view.as_ref()?;
        if !legal.can_raise {
            return None;
        }
        let on_table: u64 = view.seats.iter().map(|s| s.bet).sum();
        let pot_after_call = view.pot + on_table + legal.to_call;
        let clamp = |v: u64| v.clamp(legal.min_raise_to, legal.max_raise_to.max(legal.min_raise_to));
        Some([
            ("Min", legal.min_raise_to),
            ("½ Pot", clamp(view.current_bet + pot_after_call / 2)),
            ("Pot", clamp(view.current_bet + pot_after_call)),
            ("Max", legal.max_raise_to.max(legal.min_raise_to)),
        ])
    }

    fn apply_result(&mut self, result: HandResult) {
        if self.result_seen == Some(result.hand_no) {
            return;
        }
        self.result_seen = Some(result.hand_no);
        let i_won = self
            .my_seat
            .is_some_and(|me| result.pots.iter().any(|p| p.winners.contains(&me)));
        if i_won {
            self.celebrate_until = Some(Instant::now() + Duration::from_millis(2400));
        }
        self.anim.result_at = Some(Instant::now());
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

    fn send(&mut self, mv: Move) {
        if self.my_turn() {
            let _ = self.net.send(NetCommand::Action(mv));
        } else if self.mode == Mode::Spectator {
            self.set_notice("you're watching");
        } else {
            self.set_notice("not your turn");
        }
    }

    fn open_raise(&mut self) {
        match self.raise_presets() {
            Some(p) => self.raise = Some(RaisePicker { amount: p[0].1, typed: None }),
            None if self.my_turn() => self.set_notice("can't raise"),
            None => self.set_notice("not your turn"),
        }
    }

    fn can_deal(&self) -> bool {
        self.mode == Mode::Host && self.view.as_ref().is_some_and(|v| v.phase != Phase::Betting)
    }

    /// Returns true if the key changed anything.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return true;
        }
        if self.help {
            self.help = false;
            return true;
        }
        if self.confirm_leave {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => self.leave = true,
                _ => self.confirm_leave = false,
            }
            return true;
        }
        if self.raise.is_some() {
            self.key_raise(key);
            return true;
        }
        if self.chat_mode {
            match key.code {
                KeyCode::Esc => {
                    self.chat_mode = false;
                    self.input.clear();
                }
                KeyCode::Enter => {
                    let text = self.input.trim().to_string();
                    self.input.clear();
                    self.chat_mode = false;
                    if !text.is_empty() {
                        let _ = self.net.send(NetCommand::Chat(text));
                    }
                }
                KeyCode::Backspace => {
                    self.input.pop();
                }
                KeyCode::Char(c) if self.input.chars().count() < 200 => self.input.push(c),
                _ => {}
            }
            return true;
        }
        if let Some(ban) = self.kick_mode {
            self.kick_mode = None;
            if let KeyCode::Char(c @ '1'..='9') = key.code {
                let _ = self.net.send(NetCommand::Kick { seat: c as usize - '1' as usize, ban });
            } else {
                self.set_notice("cancelled");
            }
            return true;
        }
        match key.code {
            KeyCode::Char(c @ ('y' | 'n')) if self.mode == Mode::Host && !self.waiting.is_empty() => {
                let _ = self.net.send(NetCommand::Approve(c == 'y'));
            }
            KeyCode::Char('+') if self.mode == Mode::Host => {
                let skill = self.bot_next;
                self.bot_next = skill.cycle();
                let _ = self.net.send(NetCommand::AddBot(skill));
            }
            KeyCode::Char(c @ ('k' | 'K')) if self.mode == Mode::Host => {
                self.kick_mode = Some(c == 'K');
                self.set_notice(format!(
                    "{} which seat? press 1-9 (anything else cancels)",
                    if c == 'K' { "ban" } else { "remove" }
                ));
            }
            KeyCode::Char('i') | KeyCode::Char('I') if self.mode != Mode::Spectator => {
                let _ = self.net.send(NetCommand::Ready(true));
                self.set_notice("sitting back in");
            }
            KeyCode::Esc | KeyCode::Char('q') => self.confirm_leave = true,
            KeyCode::Char('1') | KeyCode::Char('f') => self.send(Move::Fold),
            KeyCode::Char('2') => {
                let mv = match self.my_legal() {
                    Some(l) if l.can_check => Move::Check,
                    _ => Move::Call,
                };
                self.send(mv);
            }
            KeyCode::Char('x') => self.send(Move::Check),
            KeyCode::Char('c') => self.send(Move::Call),
            KeyCode::Char('3') | KeyCode::Char('r') | KeyCode::Char('b') => self.open_raise(),
            KeyCode::Char('4') | KeyCode::Char('a') => self.send(Move::AllIn),
            KeyCode::Char('t') | KeyCode::Char('/') => self.chat_mode = true,
            KeyCode::Char('?') | KeyCode::Char('h') => self.help = true,
            KeyCode::Enter | KeyCode::Char('s') | KeyCode::Char('d') if self.can_deal() => {
                let _ = self.net.send(NetCommand::Start);
            }
            KeyCode::Enter | KeyCode::Char('s') | KeyCode::Char('d') if self.mode == Mode::Host => {
                self.set_notice("hand in progress");
            }
            _ => return false,
        }
        true
    }

    fn key_raise(&mut self, key: KeyEvent) {
        let Some(presets) = self.raise_presets() else {
            // Turn moved on while the picker was open.
            self.raise = None;
            return;
        };
        let (min, max) = (presets[0].1, presets[3].1);
        let bb = self.view.as_ref().map(|v| v.big_blind).unwrap_or(1).max(1);
        let Some(picker) = self.raise.as_mut() else { return };
        match key.code {
            KeyCode::Esc => self.raise = None,
            KeyCode::Enter => {
                let to = picker.amount.clamp(min, max);
                self.raise = None;
                self.send(Move::Raise { to });
            }
            KeyCode::Char(c @ ('m' | 'h' | 'p' | 'a' | 'M' | 'H' | 'P' | 'A')) => {
                let idx = "mhpa".find(c.to_ascii_lowercase()).unwrap_or(0);
                picker.typed = None;
                picker.amount = presets[idx].1;
            }
            KeyCode::Char(c) if c.is_ascii_digit() => {
                let buf = picker.typed.get_or_insert_with(String::new);
                if buf.len() < 12 {
                    buf.push(c);
                }
                picker.amount = buf.parse().unwrap_or(0);
            }
            KeyCode::Backspace => {
                if let Some(buf) = picker.typed.as_mut() {
                    buf.pop();
                    picker.amount = buf.parse().unwrap_or(0);
                    if buf.is_empty() {
                        picker.typed = None;
                        picker.amount = min;
                    }
                }
            }
            KeyCode::Left | KeyCode::Down => {
                picker.typed = None;
                picker.amount = picker.amount.saturating_sub(bb).clamp(min, max);
            }
            KeyCode::Right | KeyCode::Up => {
                picker.typed = None;
                picker.amount = picker.amount.saturating_add(bb).clamp(min, max);
            }
            _ => {}
        }
    }
}
