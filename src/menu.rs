//! Pre-game screens: splash, name, main menu, game types, table settings and
//! room-code entry. Pure state + key handling; `draw` renders it.

use crate::anim;
use crate::art;
use crate::game::TableParams;
use crate::net::roomcode;
use crate::store::{self, Config};
use crate::ui::{DIM, GOLD, RED, TEXT, YOU, centered};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use std::time::{Duration, Instant};

/// What the menu hands back to `main` once the player has decided.
#[derive(Clone, Debug)]
pub enum Launch {
    Host {
        name: String,
        params: TableParams,
        auto_rebuy: bool,
    },
    Join {
        name: String,
        code: String,
        buyin: u64,
    },
    Spectate {
        code: String,
    },
}

pub enum MenuOut {
    Launch(Launch),
    Quit,
}

// ---------------------------------------------------------------- settings

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub stack: u64,
    pub sb: u64,
    pub bb: u64,
    pub seats: usize,
    pub timer_s: u64,
    pub rebuy: bool,
}

pub const TIMERS: [u64; 7] = [0, 10, 15, 30, 45, 60, 120];

impl Settings {
    pub fn params(&self) -> TableParams {
        TableParams {
            max_seats: self.seats,
            small_blind: self.sb,
            big_blind: self.bb,
            buyin: self.stack,
            turn_ms: self.timer_s * 1000,
        }
    }

    /// Why these settings can't create a table, if anything.
    pub fn problem(&self) -> Option<&'static str> {
        if self.sb == 0 {
            Some("small blind must be at least 1")
        } else if self.bb < self.sb {
            Some("big blind must be at least the small blind")
        } else if self.stack < self.bb {
            Some("starting stack must be at least one big blind")
        } else if !(2..=9).contains(&self.seats) {
            Some("seats must be between 2 and 9")
        } else {
            None
        }
    }

    pub fn summary(&self) -> String {
        format!(
            "{}/{} · {} stacks · {}-max · {}",
            chips(self.sb),
            chips(self.bb),
            chips(self.stack),
            self.seats,
            timer_label(self.timer_s)
        )
    }

    pub fn encode(&self) -> String {
        format!(
            "{},{},{},{},{},{}",
            self.stack, self.sb, self.bb, self.seats, self.timer_s, self.rebuy as u8
        )
    }

    pub fn decode(text: &str) -> Option<Settings> {
        let v: Vec<u64> = text.split(',').map(|p| p.trim().parse().ok()).collect::<Option<_>>()?;
        let [stack, sb, bb, seats, timer_s, rebuy] = v[..] else {
            return None;
        };
        let s = Settings {
            stack,
            sb,
            bb,
            seats: seats as usize,
            timer_s,
            rebuy: rebuy != 0,
        };
        s.problem().is_none().then_some(s)
    }
}

pub struct Preset {
    pub name: &'static str,
    pub blurb: &'static str,
    pub settings: Settings,
}

pub const PRESETS: [Preset; 4] = [
    Preset {
        name: "Quick Game",
        blurb: "the classic: 100 big blinds deep, relaxed clock",
        settings: Settings { stack: 5_000, sb: 25, bb: 50, seats: 6, timer_s: 30, rebuy: true },
    },
    Preset {
        name: "High Roller",
        blurb: "big numbers, big pots, same 100 BB depth",
        settings: Settings { stack: 100_000, sb: 500, bb: 1_000, seats: 6, timer_s: 45, rebuy: true },
    },
    Preset {
        name: "Turbo",
        blurb: "30 BB stacks and a 15 s clock: shove-or-fold action",
        settings: Settings { stack: 3_000, sb: 50, bb: 100, seats: 6, timer_s: 15, rebuy: true },
    },
    Preset {
        name: "Heads-Up",
        blurb: "just you and one rival",
        settings: Settings { stack: 5_000, sb: 50, bb: 100, seats: 2, timer_s: 30, rebuy: true },
    },
];

const F_STACK: usize = 0;
const F_SB: usize = 1;
const F_BB: usize = 2;
const F_SEATS: usize = 3;
const F_TIMER: usize = 4;
const F_REBUY: usize = 5;
const FIELDS: usize = 6;

/// Half of the leading power of ten: 25 → 5, 5,000 → 500.
fn step(v: u64) -> u64 {
    let mut p = 1u64;
    while p.saturating_mul(10) <= v {
        p *= 10;
    }
    (p / 2).max(1)
}

pub fn chips(v: u64) -> String {
    let s = v.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn timer_label(s: u64) -> String {
    if s == 0 { "no clock".into() } else { format!("{s} s clock") }
}

// ---------------------------------------------------------------- menu state

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Splash,
    Name,
    Main,
    GameType,
    Settings,
    JoinCode,
    WatchCode,
}

pub struct Menu {
    pub screen: Screen,
    pub screen_at: Instant,
    pub started: Instant,
    pub tick: u64,
    pub sel: usize,
    pub input: String,
    pub settings: Settings,
    pub field: usize,
    /// Digits typed into the active settings field (None = not typing).
    pub typed: Option<String>,
    pub hint: Option<String>,
    pub notice: Option<String>,
    pub show_art: bool,
    pub config: Config,
}

impl Menu {
    pub fn new(config: Config, show_art: bool) -> Self {
        let now = Instant::now();
        Self {
            screen: Screen::Splash,
            screen_at: now,
            started: now,
            tick: 0,
            sel: 0,
            input: String::new(),
            settings: PRESETS[0].settings,
            field: 0,
            typed: None,
            hint: None,
            notice: None,
            show_art,
            config,
        }
    }

    pub fn name(&self) -> String {
        self.config.name.clone().unwrap_or_else(store::default_name)
    }

    fn go(&mut self, screen: Screen) {
        self.screen = screen;
        self.screen_at = Instant::now();
        self.sel = 0;
        self.hint = None;
        self.typed = None;
        if matches!(screen, Screen::JoinCode | Screen::WatchCode) {
            self.input.clear();
        }
        if screen == Screen::Name {
            self.input = self.config.name.clone().unwrap_or_default();
        }
    }

    /// Return here after a table closes, optionally explaining why.
    pub fn back_to_main(&mut self, notice: Option<String>) {
        self.go(Screen::Main);
        self.notice = notice;
    }

    fn saved_custom(&self) -> Settings {
        self.config
            .custom
            .as_deref()
            .and_then(Settings::decode)
            .unwrap_or(PRESETS[0].settings)
    }

    fn host(&mut self, s: Settings) -> Option<MenuOut> {
        Some(MenuOut::Launch(Launch::Host {
            name: self.name(),
            params: s.params(),
            auto_rebuy: s.rebuy,
        }))
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<MenuOut> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(MenuOut::Quit);
        }
        if self.screen != Screen::Splash {
            self.notice = None;
        }
        match self.screen {
            Screen::Splash => {
                if self.config.name.is_some() {
                    self.go(Screen::Main);
                } else {
                    self.go(Screen::Name);
                }
                None
            }
            Screen::Name => self.key_name(key),
            Screen::Main => self.key_main(key),
            Screen::GameType => self.key_game_type(key),
            Screen::Settings => self.key_settings(key),
            Screen::JoinCode | Screen::WatchCode => self.key_code(key),
        }
    }

    fn key_name(&mut self, key: KeyEvent) -> Option<MenuOut> {
        match key.code {
            KeyCode::Enter => {
                let name = self.input.trim().to_string();
                if name.is_empty() {
                    self.hint = Some("type a name first".into());
                } else {
                    self.config.name = Some(name);
                    let _ = store::save_config(&self.config);
                    self.go(Screen::Main);
                }
            }
            KeyCode::Esc if self.config.name.is_some() => self.go(Screen::Main),
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(c)
                if (c.is_alphanumeric() || " _-.".contains(c)) && self.input.chars().count() < 16 =>
            {
                self.input.push(c);
            }
            _ => {}
        }
        None
    }

    fn key_main(&mut self, key: KeyEvent) -> Option<MenuOut> {
        const ITEMS: usize = 5;
        let pick = match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.sel = (self.sel + ITEMS - 1) % ITEMS;
                None
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.sel = (self.sel + 1) % ITEMS;
                None
            }
            KeyCode::Enter => Some(self.sel),
            KeyCode::Char(c @ '1'..='4') => Some(c as usize - '1' as usize),
            KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => Some(4),
            _ => None,
        };
        match pick {
            Some(0) => self.go(Screen::GameType),
            Some(1) => self.go(Screen::JoinCode),
            Some(2) => self.go(Screen::WatchCode),
            Some(3) => self.go(Screen::Name),
            Some(4) => return Some(MenuOut::Quit),
            _ => {}
        }
        None
    }

    fn key_game_type(&mut self, key: KeyEvent) -> Option<MenuOut> {
        let items = PRESETS.len() + 1; // + Custom
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.sel = (self.sel + items - 1) % items,
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => self.sel = (self.sel + 1) % items,
            KeyCode::Esc => self.go(Screen::Main),
            KeyCode::Char('e') | KeyCode::Char('E') => {
                let s = PRESETS.get(self.sel).map(|p| p.settings).unwrap_or_else(|| self.saved_custom());
                self.go(Screen::Settings);
                self.settings = s;
            }
            KeyCode::Enter | KeyCode::Char('1'..='5') => {
                let idx = match key.code {
                    KeyCode::Char(c) => c as usize - '1' as usize,
                    _ => self.sel,
                };
                if let Some(p) = PRESETS.get(idx) {
                    return self.host(p.settings);
                }
                let s = self.saved_custom();
                self.go(Screen::Settings);
                self.settings = s;
            }
            _ => {}
        }
        None
    }

    fn key_settings(&mut self, key: KeyEvent) -> Option<MenuOut> {
        match key.code {
            KeyCode::Esc => {
                self.go(Screen::GameType);
                self.sel = PRESETS.len();
            }
            KeyCode::Up | KeyCode::BackTab => {
                self.field = (self.field + FIELDS - 1) % FIELDS;
                self.typed = None;
            }
            KeyCode::Down | KeyCode::Tab => {
                self.field = (self.field + 1) % FIELDS;
                self.typed = None;
            }
            KeyCode::Left => self.adjust(-1),
            KeyCode::Right => self.adjust(1),
            KeyCode::Char(' ') if self.field == F_REBUY => self.adjust(1),
            KeyCode::Char(c @ '0'..='9') => self.type_digit(c),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Enter => match self.settings.problem() {
                Some(p) => self.hint = Some(p.into()),
                None => {
                    self.config.custom = Some(self.settings.encode());
                    let _ = store::save_config(&self.config);
                    let s = self.settings;
                    return self.host(s);
                }
            },
            _ => {}
        }
        self.hint = self.settings.problem().map(str::to_string);
        None
    }

    fn adjust(&mut self, dir: i64) {
        self.typed = None;
        let s = &mut self.settings;
        let bump = |v: u64| -> u64 {
            if dir > 0 {
                v.saturating_add(step(v))
            } else {
                v.saturating_sub(step(v.saturating_sub(1))).max(1)
            }
        };
        match self.field {
            F_STACK => s.stack = bump(s.stack).max(s.bb),
            F_SB => {
                s.sb = bump(s.sb);
                s.bb = s.bb.max(s.sb);
            }
            F_BB => s.bb = bump(s.bb).max(s.sb),
            F_SEATS => s.seats = (s.seats as i64 + dir).clamp(2, 9) as usize,
            F_TIMER => {
                let i = TIMERS.iter().position(|t| *t >= s.timer_s).unwrap_or(TIMERS.len() - 1) as i64;
                s.timer_s = TIMERS[(i + dir).clamp(0, TIMERS.len() as i64 - 1) as usize];
            }
            F_REBUY => s.rebuy = !s.rebuy,
            _ => {}
        }
    }

    fn type_digit(&mut self, c: char) {
        if !matches!(self.field, F_STACK | F_SB | F_BB | F_SEATS | F_TIMER) {
            return;
        }
        let mut buf = self.typed.take().unwrap_or_default();
        if buf.len() < 12 {
            buf.push(c);
        }
        self.set_typed(buf);
    }

    fn backspace(&mut self) {
        if let Some(mut buf) = self.typed.take() {
            buf.pop();
            self.set_typed(buf);
        } else if matches!(self.field, F_STACK | F_SB | F_BB | F_SEATS | F_TIMER) {
            self.set_typed(String::new());
        }
    }

    fn set_typed(&mut self, buf: String) {
        let v: u64 = buf.parse().unwrap_or(0);
        let s = &mut self.settings;
        match self.field {
            F_STACK => s.stack = v,
            F_SB => s.sb = v,
            F_BB => s.bb = v,
            F_SEATS => s.seats = v as usize,
            F_TIMER => s.timer_s = v.min(600),
            _ => {}
        }
        self.typed = Some(buf);
    }

    fn key_code(&mut self, key: KeyEvent) -> Option<MenuOut> {
        match key.code {
            KeyCode::Esc => self.go(Screen::Main),
            KeyCode::Backspace => {
                self.input.pop();
                self.hint = None;
            }
            KeyCode::Char(c) if c.is_ascii_alphanumeric() && self.input.len() < 8 => {
                self.input.push(c.to_ascii_uppercase());
                self.hint = None;
            }
            KeyCode::Enter => match roomcode::decode(&self.input) {
                Ok(_) => {
                    let code = format!("{}-{}", &self.input[..4], &self.input[4..]);
                    return Some(MenuOut::Launch(if self.screen == Screen::JoinCode {
                        // The host clamps the buy-in to its table's starting stack.
                        Launch::Join { name: self.name(), code, buyin: u64::MAX }
                    } else {
                        Launch::Spectate { code }
                    }));
                }
                Err(e) => self.hint = Some(e.to_string()),
            },
            _ => {}
        }
        None
    }
}

// ---------------------------------------------------------------- drawing

pub fn draw(f: &mut Frame, m: &Menu) {
    let area = f.area();
    f.render_widget(Block::default().style(Style::default().bg(Color::Black)), area);
    if area.width < 44 || area.height < 16 {
        f.render_widget(
            Paragraph::new("♠ POKER · make the terminal bigger").alignment(Alignment::Center).style(Style::default().fg(GOLD)),
            area,
        );
        return;
    }
    draw_marquee(f, area, m.tick);
    let inner = Rect::new(area.x, area.y + 1, area.width, area.height.saturating_sub(2));

    if m.screen == Screen::Splash {
        draw_splash(f, inner, m);
        return;
    }

    // Title: wordmark when there's room, compact otherwise.
    let (ww, wh) = art::dimensions(art::WORDMARK);
    let big = m.show_art && inner.width >= ww + 2 && inner.height >= wh + 16;
    let title_h = if big { wh + 1 } else { 2 };
    if big {
        draw_wordmark(f, Rect::new(inner.x, inner.y + 1, inner.width, wh), m.tick);
    } else {
        let line = Line::from(Span::styled("♠  P O K E R  ♥", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)));
        f.render_widget(Paragraph::new(line).alignment(Alignment::Center), Rect::new(inner.x, inner.y, inner.width, 1));
    }
    let body = Rect::new(inner.x, inner.y + title_h + 1, inner.width, inner.height.saturating_sub(title_h + 1));

    let (title, lines, footer) = match m.screen {
        Screen::Name => name_panel(m),
        Screen::Main => main_panel(m),
        Screen::GameType => game_type_panel(m),
        Screen::Settings => settings_panel(m),
        Screen::JoinCode | Screen::WatchCode => code_panel(m),
        Screen::Splash => unreachable!(),
    };
    // Panels slide up a couple of rows when they open.
    let rise = 2 - (anim::ease_out(anim::progress(m.screen_at, Duration::from_millis(180))) * 2.0) as u16;
    let h = (lines.len() as u16 + 2).min(body.height);
    let w = lines.iter().map(|l| l.width() as u16).max().unwrap_or(20).max(title.len() as u16 + 4) + 6;
    let mut rect = centered(Rect::new(body.x, body.y, body.width, body.height.saturating_sub(2)), w, h);
    rect.y = (rect.y + rise).min(body.y + body.height.saturating_sub(h));
    rect.y = rect.y.min(body.y + 1).max(body.y);
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(GOLD))
        .title(Span::styled(format!(" {title} "), Style::default().fg(GOLD).add_modifier(Modifier::BOLD)))
        .style(Style::default().bg(Color::Black));
    f.render_widget(Paragraph::new(lines).block(block), rect);

    let below = rect.y + rect.height;
    if let Some(n) = &m.notice
        && below + 1 < body.y + body.height
    {
        f.render_widget(
            Paragraph::new(Span::styled(format!(" {n} "), Style::default().fg(Color::Black).bg(GOLD)))
                .alignment(Alignment::Center),
            Rect::new(body.x, below + 1, body.width, 1),
        );
    }
    f.render_widget(
        Paragraph::new(Span::styled(footer, Style::default().fg(DIM))).alignment(Alignment::Center),
        Rect::new(inner.x, inner.y + inner.height.saturating_sub(1), inner.width, 1),
    );
}

fn draw_marquee(f: &mut Frame, area: Rect, tick: u64) {
    const SUITS: [(char, Color); 4] = [
        ('♠', Color::Rgb(200, 205, 200)),
        ('♥', Color::Rgb(230, 90, 90)),
        ('♦', Color::Rgb(230, 90, 90)),
        ('♣', Color::Rgb(200, 205, 200)),
    ];
    let buf = f.buffer_mut();
    let shift = (tick / 3) as usize;
    for x in 0..area.width {
        for (y, dir) in [(area.y, 1usize), (area.y + area.height - 1, 0usize)] {
            let pos = if dir == 1 { x as usize + shift } else { (x as usize + 10_000).wrapping_sub(shift) };
            if pos % 3 != 0 {
                continue;
            }
            let (c, color) = SUITS[(pos / 3) % 4];
            let dim = Color::Rgb(
                if let Color::Rgb(r, _, _) = color { r / 2 } else { 90 },
                if let Color::Rgb(_, g, _) = color { g / 2 } else { 90 },
                if let Color::Rgb(_, _, b) = color { b / 2 } else { 90 },
            );
            buf.set_string(area.x + x, y, c.to_string(), Style::default().fg(dim));
        }
    }
}

fn draw_wordmark(f: &mut Frame, area: Rect, tick: u64) {
    let lines = art::lines(art::WORDMARK);
    let (w, _) = art::dimensions(art::WORDMARK);
    let x0 = area.x + area.width.saturating_sub(w) / 2;
    let buf = f.buffer_mut();
    for (row, line) in lines.iter().enumerate() {
        if row as u16 >= area.height {
            break;
        }
        for (col, ch) in line.chars().enumerate() {
            if ch == ' ' {
                continue;
            }
            let x = x0 + col as u16;
            if x >= area.x + area.width {
                break;
            }
            let color = if ch == '█' { anim::shimmer(col as u16 + row as u16, w, tick) } else { Color::Rgb(120, 90, 40) };
            buf.set_string(x, area.y + row as u16, ch.to_string(), Style::default().fg(color));
        }
    }
}

fn draw_splash(f: &mut Frame, area: Rect, m: &Menu) {
    let (ww, wh) = art::dimensions(art::WORDMARK);
    let big = m.show_art && area.width >= ww + 2 && area.height >= wh + 12;
    let block_h = if big { wh + 10 } else { 10 };
    let top = area.y + area.height.saturating_sub(block_h) / 2;
    if big {
        draw_wordmark(f, Rect::new(area.x, top, area.width, wh), m.tick);
    } else {
        let line = Line::from(Span::styled("♠  P O K E R  ♥", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)));
        f.render_widget(Paragraph::new(line).alignment(Alignment::Center), Rect::new(area.x, top + 2, area.width, 1));
    }
    let sub_y = top + if big { wh + 1 } else { 4 };
    f.render_widget(
        Paragraph::new(Span::styled("texas hold'em · no server · every deal signed & sealed", Style::default().fg(DIM)))
            .alignment(Alignment::Center),
        Rect::new(area.x, sub_y, area.width, 1),
    );

    // Riffle: seven cards fan out from the middle.
    const CARDS: [(&str, bool); 7] =
        [("A♠", false), ("K♥", true), ("Q♣", false), ("J♦", true), ("T♠", false), ("9♥", true), ("8♣", false)];
    let p = anim::ease_out(anim::progress(m.started, Duration::from_millis(900)));
    let cy = sub_y + 2;
    let cx = area.x + area.width / 2;
    let buf = f.buffer_mut();
    for (i, (label, red)) in CARDS.iter().enumerate() {
        let offset = (i as f32 - 3.0) * 8.0 * p;
        let x = (cx as f32 + offset - 3.0).max(area.x as f32) as u16;
        // Lift the middle cards a touch for an arc.
        let lift = ((3.0 - (i as f32 - 3.0).abs()) * p * 0.4) as u16;
        let y = cy + 1 - lift.min(1);
        if x + 7 > area.x + area.width {
            continue;
        }
        let border = Style::default().fg(Color::Rgb(190, 195, 190)).bg(Color::Black);
        let face = Style::default().fg(if *red { RED } else { TEXT }).bg(Color::Black).add_modifier(Modifier::BOLD);
        buf.set_string(x, y, "┌─────┐", border);
        buf.set_string(x, y + 1, "│     │", border);
        buf.set_string(x + 2, y + 1, *label, face);
        buf.set_string(x, y + 2, "│     │", border);
        buf.set_string(x, y + 3, "└─────┘", border);
    }

    if p >= 1.0 {
        let glow = anim::pulse(m.tick);
        let v = (120.0 + 135.0 * glow) as u8;
        f.render_widget(
            Paragraph::new(Span::styled("press any key", Style::default().fg(Color::Rgb(v, v, v))))
                .alignment(Alignment::Center),
            Rect::new(area.x, cy + 6, area.width, 1),
        );
    }
}

fn item(m: &Menu, idx: usize, key: &str, label: &str, desc: &str) -> Line<'static> {
    let selected = m.sel == idx;
    if selected {
        let glow = anim::pulse(m.tick);
        let bg = Color::Rgb(200 + (55.0 * glow) as u8, 160 + (50.0 * glow) as u8, 60);
        Line::from(vec![
            Span::styled(" ▸ ", Style::default().fg(GOLD)),
            Span::styled(format!(" [{key}] {label:<14}"), Style::default().fg(Color::Black).bg(bg).add_modifier(Modifier::BOLD)),
            Span::styled(format!("  {desc}"), Style::default().fg(TEXT)),
        ])
    } else {
        Line::from(vec![
            Span::raw("   "),
            Span::styled(format!(" [{key}] "), Style::default().fg(GOLD)),
            Span::styled(format!("{label:<14}"), Style::default().fg(TEXT)),
            Span::styled(format!("  {desc}"), Style::default().fg(DIM)),
        ])
    }
}

type Panel = (&'static str, Vec<Line<'static>>, &'static str);

fn name_panel(m: &Menu) -> Panel {
    let cursor = if m.tick / 10 % 2 == 0 { "█" } else { " " };
    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled("  What should the table call you?", Style::default().fg(TEXT))),
        Line::from(""),
        Line::from(vec![
            Span::styled("   ▸ ", Style::default().fg(GOLD)),
            Span::styled(format!("{:<16}", format!("{}{cursor}", m.input)), Style::default().fg(YOU).add_modifier(Modifier::BOLD).bg(Color::Rgb(25, 30, 28))),
        ]),
        Line::from(""),
    ];
    if let Some(h) = &m.hint {
        lines.push(Line::from(Span::styled(format!("  {h}"), Style::default().fg(RED))));
    }
    ("YOUR NAME", lines, "type your name · Enter save · Esc back")
}

fn main_panel(m: &Menu) -> Panel {
    let lines = vec![
        Line::from(vec![
            Span::styled("  playing as ", Style::default().fg(DIM)),
            Span::styled(m.name(), Style::default().fg(YOU).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(""),
        item(m, 0, "1", "Host a table", "pick a game, share the code"),
        item(m, 1, "2", "Join a table", "enter a friend's room code"),
        item(m, 2, "3", "Watch a table", "spectate without sitting down"),
        item(m, 3, "4", "Change name", ""),
        item(m, 4, "Q", "Quit", ""),
        Line::from(""),
    ];
    ("MAIN MENU", lines, "↑↓ move · Enter select · or press a number")
}

fn game_type_panel(m: &Menu) -> Panel {
    let mut lines = vec![Line::from("")];
    for (i, p) in PRESETS.iter().enumerate() {
        lines.push(item(m, i, &(i + 1).to_string(), p.name, &p.settings.summary()));
        if m.sel == i {
            lines.push(Line::from(Span::styled(format!("        {}", p.blurb), Style::default().fg(DIM).add_modifier(Modifier::ITALIC))));
        }
    }
    let custom = PRESETS.len();
    lines.push(item(m, custom, "5", "Custom", "set stacks, blinds, seats and clock yourself"));
    if m.sel == custom {
        lines.push(Line::from(Span::styled(
            format!("        last used: {}", m.saved_custom().summary()),
            Style::default().fg(DIM).add_modifier(Modifier::ITALIC),
        )));
    }
    lines.push(Line::from(""));
    ("CHOOSE A GAME", lines, "number or Enter create table · E edit settings first · Esc back")
}

fn settings_panel(m: &Menu) -> Panel {
    let s = &m.settings;
    let cursor = if m.tick / 10 % 2 == 0 { "█" } else { " " };
    let value = |field: usize, text: String| -> String {
        if m.field == field
            && let Some(t) = &m.typed
        {
            format!("{t}{cursor}")
        } else {
            text
        }
    };
    let rows: [(&str, String, &str); FIELDS] = [
        ("Starting stack (everyone)", value(F_STACK, chips(s.stack)), "←→ step · or type chips"),
        ("Small blind", value(F_SB, chips(s.sb)), "←→ step · or type"),
        ("Big blind", value(F_BB, chips(s.bb)), "never below the small blind"),
        ("Seats", value(F_SEATS, s.seats.to_string()), "2 – 9"),
        ("Turn timer", value(F_TIMER, if s.timer_s == 0 { "off".into() } else { format!("{} s", s.timer_s) }), "off · 10 … 120 s"),
        ("Auto top-up busted players", if s.rebuy { "on".into() } else { "off".into() }, "←→ / space toggles"),
    ];
    let mut lines = vec![Line::from("")];
    for (i, (label, val, help)) in rows.into_iter().enumerate() {
        let active = m.field == i;
        lines.push(Line::from(vec![
            Span::styled(if active { " ▸ " } else { "   " }, Style::default().fg(GOLD)),
            Span::styled(format!("{label:<28}"), Style::default().fg(if active { TEXT } else { DIM })),
            Span::styled(
                format!(" {}{val:>10} {}", if active { "◂" } else { " " }, if active { "▸" } else { " " }),
                if active {
                    Style::default().fg(Color::Black).bg(GOLD).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(GOLD)
                },
            ),
            Span::styled(format!("  {help}"), Style::default().fg(if active { DIM } else { Color::Rgb(60, 66, 62) })),
        ]));
    }
    lines.push(Line::from(""));
    let depth = if s.bb > 0 { format!("{} big blinds deep", s.stack / s.bb) } else { "—".into() };
    lines.push(Line::from(vec![
        Span::styled("   ── ", Style::default().fg(DIM)),
        Span::styled(format!("{depth} · {}-max · {}", s.seats, timer_label(s.timer_s)), Style::default().fg(TEXT)),
    ]));
    if let Some(h) = &m.hint {
        lines.push(Line::from(Span::styled(format!("   ✘ {h}"), Style::default().fg(RED))));
    } else {
        lines.push(Line::from(Span::styled("   ✔ ready", Style::default().fg(Color::Rgb(120, 210, 140)))));
    }
    lines.push(Line::from(""));
    ("TABLE SETTINGS", lines, "↑↓ field · ←→ adjust · type a number · Enter create · Esc back")
}

fn code_panel(m: &Menu) -> Panel {
    let mut slots = Vec::new();
    for i in 0..8 {
        if i == 4 {
            slots.push(Span::styled(" ─ ", Style::default().fg(DIM)));
        }
        let ch = m.input.chars().nth(i);
        let active = i == m.input.len() && m.tick / 10 % 2 == 0;
        slots.push(Span::styled(
            format!(" {} ", ch.map(String::from).unwrap_or_else(|| if active { "▁".into() } else { "·".into() })),
            Style::default()
                .fg(if ch.is_some() { Color::Black } else { DIM })
                .bg(if ch.is_some() { GOLD } else { Color::Rgb(25, 30, 28) })
                .add_modifier(Modifier::BOLD),
        ));
        slots.push(Span::raw(" "));
    }
    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            if m.screen == Screen::JoinCode { "  Enter the room code your host shared" } else { "  Enter the room code to watch" },
            Style::default().fg(TEXT),
        )),
        Line::from(""),
        Line::from([vec![Span::raw("   ")], slots].concat()),
        Line::from(""),
    ];
    if let Some(h) = &m.hint {
        lines.push(Line::from(Span::styled(format!("   ✘ {h}"), Style::default().fg(RED))));
        lines.push(Line::from(""));
    }
    let title = if m.screen == Screen::JoinCode { "JOIN A TABLE" } else { "WATCH A TABLE" };
    (title, lines, "type the code · Enter go · Backspace fix · Esc back")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn menu() -> Menu {
        let mut m = Menu::new(Config { name: Some("tester".into()), ..Config::default() }, true);
        m.screen = Screen::Main;
        m
    }

    #[test]
    fn presets_are_valid_and_map_to_params() {
        for p in &PRESETS {
            assert_eq!(p.settings.problem(), None, "{}", p.name);
        }
        let hr = PRESETS[1].settings.params();
        assert_eq!((hr.small_blind, hr.big_blind, hr.buyin, hr.turn_ms), (500, 1_000, 100_000, 45_000));
        assert_eq!(PRESETS[3].settings.params().max_seats, 2);
    }

    #[test]
    fn host_flow_picks_a_preset() {
        let mut m = menu();
        assert!(m.handle_key(key(KeyCode::Char('1'))).is_none());
        assert_eq!(m.screen, Screen::GameType);
        match m.handle_key(key(KeyCode::Char('2'))) {
            Some(MenuOut::Launch(Launch::Host { params, name, .. })) => {
                assert_eq!(params.big_blind, 1_000);
                assert_eq!(name, "tester");
            }
            _ => panic!("expected a host launch"),
        }
    }

    #[test]
    fn settings_editor_types_steps_and_guards() {
        let mut m = menu();
        m.screen = Screen::Settings;
        m.settings = PRESETS[0].settings; // 5,000 · 25/50
        // Type an exact stack.
        for c in "12000".chars() {
            m.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(m.settings.stack, 12_000);
        // Small blind: step up, and raising it past the big blind bumps the big blind.
        m.handle_key(key(KeyCode::Down));
        for c in "80".chars() {
            m.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(m.hint.as_deref(), Some("big blind must be at least the small blind"));
        m.handle_key(key(KeyCode::Right)); // 80 → 85, bb bumped to 85
        assert_eq!((m.settings.sb, m.settings.bb), (85, 85));
        assert_eq!(m.hint, None);
        // Stack below a big blind is refused on Enter.
        m.handle_key(key(KeyCode::Up));
        m.handle_key(key(KeyCode::Char('9')));
        assert!(m.handle_key(key(KeyCode::Enter)).is_none());
        assert_eq!(m.hint.as_deref(), Some("starting stack must be at least one big blind"));
        // Timer cycles through the fixed list.
        m.field = F_TIMER;
        m.settings.timer_s = 30;
        m.handle_key(key(KeyCode::Right));
        assert_eq!(m.settings.timer_s, 45);
        m.handle_key(key(KeyCode::Left));
        m.handle_key(key(KeyCode::Left));
        assert_eq!(m.settings.timer_s, 15);
    }

    #[test]
    fn settings_round_trip_through_config() {
        let s = Settings { stack: 7_500, sb: 10, bb: 25, seats: 4, timer_s: 0, rebuy: false };
        assert_eq!(Settings::decode(&s.encode()), Some(s));
        assert_eq!(Settings::decode("garbage"), None);
        assert_eq!(Settings::decode("1,50,25,6,30,1"), None, "invalid settings rejected");
    }

    #[test]
    fn code_entry_validates_and_formats() {
        let mut m = menu();
        m.handle_key(key(KeyCode::Char('2')));
        assert_eq!(m.screen, Screen::JoinCode);
        for c in "k7q2".chars() {
            m.handle_key(key(KeyCode::Char(c)));
        }
        assert!(m.handle_key(key(KeyCode::Enter)).is_none());
        assert!(m.hint.is_some(), "short code rejected");
        for c in "9xzp!".chars() {
            m.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(m.input, "K7Q29XZP");
        match m.handle_key(key(KeyCode::Enter)) {
            Some(MenuOut::Launch(Launch::Join { code, buyin, .. })) => {
                assert_eq!(code, "K7Q2-9XZP");
                assert_eq!(buyin, u64::MAX);
            }
            _ => panic!("expected a join launch"),
        }
    }

    #[test]
    fn chips_format() {
        assert_eq!(chips(0), "0");
        assert_eq!(chips(950), "950");
        assert_eq!(chips(5_000), "5,000");
        assert_eq!(chips(1_234_567), "1,234,567");
    }

    #[test]
    fn every_screen_renders_at_small_and_large_sizes() {
        use ratatui::{Terminal, backend::TestBackend};
        for (w, h) in [(80, 24), (120, 40), (44, 16)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            let mut m = menu();
            for screen in [Screen::Splash, Screen::Name, Screen::Main, Screen::GameType, Screen::Settings, Screen::JoinCode] {
                m.screen = screen;
                term.draw(|f| draw(f, &m)).unwrap();
            }
            m.screen = Screen::Main;
            term.draw(|f| draw(f, &m)).unwrap();
            let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
            if w >= 80 {
                assert!(text.contains("Host a table"), "{w}x{h}: main menu text missing");
            }
        }
    }
}
