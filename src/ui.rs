//! Ratatui rendering: felt table, boxed seats, real-looking cards, the action
//! panel (your hand + big buttons) and the log.

use crate::anim;
use crate::app::{self, App, LogKind, Mode};
use crate::menu::chips;
use std::time::Duration;
use crate::game::{Card, Phase, Street, TableView};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::symbols::border;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};

/// Box-drawing in the same ASCII style as the cards.
const ASCII: border::Set = border::Set {
    top_left: ".",
    top_right: ".",
    bottom_left: "`",
    bottom_right: "'",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};
/// Whoever is acting / your turn.
const ASCII_HOT: border::Set = border::Set {
    top_left: "#",
    top_right: "#",
    bottom_left: "#",
    bottom_right: "#",
    vertical_left: "#",
    vertical_right: "#",
    horizontal_top: "=",
    horizontal_bottom: "=",
};

pub(crate) const FELT_DARK: Color = Color::Rgb(7, 45, 30);
pub(crate) const GOLD: Color = Color::Rgb(230, 190, 90);
pub(crate) const DIM: Color = Color::Rgb(165, 175, 170);
pub(crate) const YOU: Color = Color::Cyan;
pub(crate) const RED: Color = Color::Rgb(230, 90, 90);
pub(crate) const TEXT: Color = Color::Rgb(215, 220, 215);

pub const MIN_W: u16 = 80;
pub const MIN_H: u16 = 24;
/// Action panel height: room for BIG cards on tall terminals.
fn panel_h(height: u16) -> u16 {
    if height >= 28 { BIG.h + 2 } else { SMALL.h + 2 }
}
const SEAT_H: u16 = 3;

const FOLD_C: Color = Color::Rgb(225, 85, 85);
const CALL_C: Color = Color::Rgb(95, 205, 115);
const RAISE_C: Color = Color::Rgb(235, 190, 80);
const ALLIN_C: Color = Color::Rgb(185, 125, 235);

/// Gold that pulses; used for everything that means "your turn".
fn turn_gold(tick: u64) -> Color {
    let glow = anim::pulse(tick * 2);
    Color::Rgb(255, (165.0 + 85.0 * glow) as u8, 50)
}

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    if area.width < MIN_W || area.height < MIN_H {
        let msg = Paragraph::new(vec![
            Line::from(format!("pokertui needs at least {MIN_W}×{MIN_H}")).style(Style::default().fg(GOLD)),
            Line::from(format!("this terminal is {}×{} · make it bigger", area.width, area.height)).style(Style::default().fg(DIM)),
        ])
        .alignment(Alignment::Center);
        f.render_widget(msg, centered(area, area.width, 2));
        return;
    }

    let panel = panel_h(area.height);
    let log_h = area.height.saturating_sub(1 + panel + 14).clamp(1, 6);
    let chunks = Layout::vertical([
        Constraint::Min(14),
        Constraint::Length(panel),
        Constraint::Length(log_h),
        Constraint::Length(1),
    ])
    .split(area);

    draw_body(f, chunks[0], app);
    draw_panel(f, chunks[1], app);
    draw_log(f, chunks[2], app);
    draw_footer(f, chunks[3], app);

    if app.raise.is_some() {
        draw_raise(f, area, app);
    }
    if app.confirm_leave {
        draw_confirm_leave(f, area, app);
    }
    if app.help {
        draw_help(f, area);
    }
    if let Some((text, _)) = &app.notice {
        draw_notice(f, area, text);
    }
}

fn draw_body(f: &mut Frame, area: Rect, app: &App) {
    let Some(view) = &app.view else {
        const SPIN: [&str; 4] = ["♠", "♥", "♦", "♣"];
        let spin = SPIN[(app.tick / 3 % 4) as usize];
        let dots = ".".repeat((app.tick / 6 % 4) as usize);
        let text = Paragraph::new(vec![
            Line::from(vec![
                Span::styled(format!("{spin}  "), Style::default().fg(if matches!(spin, "♥" | "♦") { RED } else { TEXT })),
                Span::styled(
                    if app.connected {
                        format!("waiting for host{dots:<3}")
                    } else {
                        format!("connecting{dots:<3}")
                    },
                    Style::default().fg(GOLD),
                ),
            ]),
        ])
        .alignment(Alignment::Center);
        f.render_widget(text, centered(area, 64, 4));
        return;
    };

    if view.phase == Phase::Lobby {
        draw_lobby(f, area, app, view);
    } else {
        draw_table(f, area, app, view);
    }
}

fn draw_lobby(f: &mut Frame, area: Rect, app: &App, view: &TableView) {
    let seated: Vec<&crate::game::SeatView> = view.seats.iter().filter(|s| !s.name.is_empty()).collect();
    let deal_hint = if app.mode == Mode::Host {
        if seated.len() >= 2 { "Enter  deal" } else { "waiting for players" }
    } else {
        "waiting for host"
    };
    // A royal flush fanned across the top, as big as fits above the room card.
    let sz = [BIG, SMALL].into_iter().find(|sz| area.height >= sz.h + 13 && area.width >= 5 * (sz.w + 2));
    let art_fits = app.show_art && sz.is_some();
    if let (true, Some(sz)) = (art_fits, sz) {
        let step = sz.w + 2;
        let x0 = area.x + area.width.saturating_sub(5 * step - 2) / 2;
        let lift = [2u16, 1, 0, 1, 2];
        for (i, code) in ["As", "Ks", "Qs", "Js", "Ts"].iter().enumerate() {
            if let Some(card) = Card::parse(code) {
                draw_card(f.buffer_mut(), x0 + i as u16 * step, area.y + lift[i], card, false, sz);
            }
        }
    }

    // Room card: code, rules, who's here.
    let names: Vec<Span> = seated
        .iter()
        .enumerate()
        .flat_map(|(i, s)| {
            let me = app.my_seat == Some(s.seat);
            let mut v = vec![Span::styled(
                s.name.clone(),
                Style::default().fg(if me { YOU } else { TEXT }).add_modifier(Modifier::BOLD),
            )];
            if i + 1 < seated.len() {
                v.push(Span::styled("  ·  ", Style::default().fg(DIM)));
            }
            v
        })
        .collect();
    let glow = anim::pulse(app.tick);
    let code_bg = Color::Rgb(200 + (55.0 * glow) as u8, 165 + (40.0 * glow) as u8, 70);
    let lines = vec![
        Line::from(vec![
            Span::styled(format!("  {}  ", app.code), Style::default().fg(Color::Black).bg(code_bg).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(""),
        Line::from(names),
        Line::from(""),
        Line::from(Span::styled(
            deal_hint,
            if app.mode == Mode::Host && seated.len() >= 2 {
                Style::default().fg(Color::Rgb((150.0 + 105.0 * glow) as u8, 255, (150.0 + 105.0 * glow) as u8)).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(DIM)
            },
        )),
    ];
    let h = lines.len() as u16 + 2;
    let w = 56.min(area.width);
    let rect = if art_fits {
        Rect::new(area.x + area.width.saturating_sub(w) / 2, area.y + area.height.saturating_sub(h + 1), w, h)
    } else {
        centered(area, w, h)
    };
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(ASCII)
        .border_style(Style::default().fg(GOLD))
        .title(Span::styled(" LOBBY ", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)))
        .style(Style::default().bg(Color::Black));
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center).block(block), rect);
}

/// "★ bob wins 1,200 · Two Pair ★", or "★ YOU WIN …" when it's you.
fn winner_banner(app: &App, view: &TableView) -> Option<(String, bool)> {
    if view.phase != Phase::HandOver {
        return None;
    }
    let p = view.last_result.as_ref()?.pots.first()?;
    let me = app.my_seat.is_some_and(|m| p.winners.contains(&m));
    // "alice wins uncontested" would just repeat the name.
    let desc = if p.desc.is_empty() || p.desc.ends_with("uncontested") { String::new() } else { format!(" · {}", p.desc) };
    Some(if me {
        (format!(" *** YOU WIN {}{desc} *** ", chips(p.amount)), true)
    } else {
        let names = p.winners.iter().map(|w| app.seat_name(*w)).collect::<Vec<_>>().join(" & ");
        (format!("* {names} wins {}{desc} *", chips(p.amount)), false)
    })
}

fn draw_table(f: &mut Frame, area: Rect, app: &App, view: &TableView) {
    let my_turn = app.my_legal().is_some();
    let (border, border_type) = if my_turn {
        (turn_gold(app.tick), ASCII_HOT)
    } else {
        (Color::Rgb(40, 110, 80), ASCII)
    };
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_set(border_type)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(FELT_DARK))
        .title(Span::styled(
            format!(" {} ", street_label(view).to_uppercase()),
            Style::default().fg(GOLD),
        ));
    if !my_turn {
        block = block.title(Line::from(Span::styled(format!(" {}/{} ", chips(view.small_blind), chips(view.big_blind)), Style::default().fg(DIM))).right_aligned());
    } else {
        block = block.title(
            Line::from(Span::styled(" >> YOUR TURN << ", Style::default().fg(Color::Black).bg(border).add_modifier(Modifier::BOLD))).centered(),
        );
    }
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Board in the middle, pot line just above it, seats around.
    let sz = if inner.height >= 2 * SEAT_H + BIG.h + 2 && inner.width >= 5 * BIG.w + 4 + 2 * 20 { BIG } else { SMALL };
    let board_w = sz.w * 5 + 4;
    let board_x = inner.x + inner.width.saturating_sub(board_w) / 2;
    // Pot sits on the line above the board; on a short table there's no such
    // line, so it goes on the table's bottom border instead.
    let roomy = inner.height > 2 * SEAT_H + sz.h;
    let board_y = if roomy { (inner.y + inner.height.saturating_sub(sz.h) / 2).max(inner.y + SEAT_H + 1) } else { inner.y + SEAT_H };
    let pot_y = if roomy { board_y - 1 } else { inner.y + inner.height };
    let center = (inner.x + inner.width / 2, pot_y);

    let (pot_text, pot_style) = match winner_banner(app, view) {
        Some((b, true)) => (b, Style::default().fg(Color::Black).bg(turn_gold(app.tick)).add_modifier(Modifier::BOLD)),
        Some((b, false)) => (b, Style::default().fg(turn_gold(app.tick)).add_modifier(Modifier::BOLD)),
        None => (format!("POT  {}", chips(app.pot_shown() + view.seats.iter().map(|s| s.bet).sum::<u64>())), Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
    };
    let pot_x = inner.x + inner.width.saturating_sub(pot_text.chars().count() as u16) / 2;

    for i in 0usize..5 {
        let x = board_x + i as u16 * (sz.w + 1);
        let buf = f.buffer_mut();
        match view.board.get(i) {
            Some(card) => {
                let flip = app.anim.board_at.filter(|_| i >= app.anim.board_from).map(|at| {
                    let delay = (i - app.anim.board_from) as u64 * app::FLIP_STAGGER_MS;
                    at.elapsed().as_millis() as i64 - delay as i64
                });
                match flip {
                    Some(ms) if ms < 0 => draw_card_placeholder(buf, x, board_y, sz),
                    Some(ms) if (ms as u64) < app::FLIP_MS => {
                        let t = ms as f32 / app::FLIP_MS as f32;
                        if t < 0.35 {
                            draw_card_back(buf, x, board_y, sz)
                        } else if t < 0.65 {
                            draw_card_edge(buf, x, board_y, sz)
                        } else {
                            draw_card(buf, x, board_y, *card, false, sz)
                        }
                    }
                    _ => draw_card(buf, x, board_y, *card, false, sz),
                }
            }
            None => draw_card_placeholder(buf, x, board_y, sz),
        }
    }

    // Seats, rotated so the local player sits at the bottom center.
    let occupied: Vec<usize> = view.seats.iter().filter(|s| !s.name.is_empty()).map(|s| s.seat).collect();
    if occupied.is_empty() {
        f.buffer_mut().set_string(pot_x, pot_y, &pot_text, pot_style);
        return;
    }
    let anchor = app.my_seat.and_then(|me| occupied.iter().position(|s| *s == me)).unwrap_or(0);
    let n = occupied.len();
    let positions = seat_positions(n);
    let panel_w = (inner.width / 4).clamp(18, 26).min(inner.width);
    let mut rects: Vec<(usize, Rect)> = Vec::new();
    for (i, seat) in occupied.iter().enumerate() {
        let rel = (i + n - anchor) % n;
        let (fx, fy) = positions[rel.min(positions.len() - 1)];
        rects.push((*seat, at(inner, fx, fy, panel_w, SEAT_H)));
    }
    rects.sort_by_key(|(seat, _)| (*seat + n - occupied[anchor]) % view.seats.len().max(1));

    // Deal: card backs fly from the middle to each player, two rounds.
    let dealt: Vec<usize> = rects
        .iter()
        .filter(|(s, _)| view.seats.get(*s).is_some_and(|v| v.hole_count > 0 || v.in_hand))
        .map(|(s, _)| *s)
        .collect();
    let deal_ms = app.anim.deal_at.map(|at| at.elapsed().as_millis() as u64);
    let total_cards = (dealt.len() * 2).max(1) as u64;
    let step = app::DEAL_MS / total_cards;
    let landed = |seat: usize| -> usize {
        let Some(ms) = deal_ms else { return 2 };
        let Some(k) = dealt.iter().position(|s| *s == seat) else { return 0 };
        (0..2).filter(|round| ms >= (round * dealt.len() + k) as u64 * step + step).count()
    };

    for (seat, rect) in &rects {
        draw_seat(f.buffer_mut(), *rect, app, view, *seat, landed(*seat));
    }
    f.buffer_mut().set_string(pot_x, pot_y, &pot_text, pot_style);

    if let Some(ms) = deal_ms.filter(|ms| *ms < app::DEAL_MS + step) {
        for c in 0..total_cards {
            let start = c * step;
            if ms < start || ms >= start + step {
                continue;
            }
            let seat = dealt[(c as usize) % dealt.len().max(1)];
            if let Some((_, r)) = rects.iter().find(|(s, _)| *s == seat) {
                let t = anim::ease_out((ms - start) as f32 / step.max(1) as f32);
                let x = anim::lerp(center.0, r.x + r.width / 2, t);
                let y = anim::lerp(center.1, r.y + 1, t);
                f.buffer_mut().set_string(x, y, "#", Style::default().fg(Color::Rgb(170, 40, 55)).bg(FELT_DARK));
            }
        }
    }

    // Chips sweep into the pot when a betting round ends.
    if let Some(at) = app.anim.chips_at {
        let t = anim::progress(at, Duration::from_millis(app::CHIPS_MS));
        if t < 1.0 {
            for seat in &app.anim.chips_from {
                if let Some((_, r)) = rects.iter().find(|(s, _)| s == seat) {
                    for trail in 0..3 {
                        let tt = anim::ease_out((t - trail as f32 * 0.08).max(0.0));
                        let x = anim::lerp(r.x + r.width / 2, center.0, tt);
                        let y = anim::lerp(r.y + 1, center.1, tt);
                        let glyph = ["$", "o", "."][trail];
                        f.buffer_mut().set_string(x, y, glyph, Style::default().fg(GOLD).bg(FELT_DARK));
                    }
                }
            }
        }
    }

    // Fireworks when you win a pot.
    if let (Some(at), true, true) = (app.anim.result_at, app.celebrate_until.is_some(), app.show_art) {
        let elapsed = at.elapsed().as_secs_f32();
        let buf = f.buffer_mut();
        for (x, y, glyph, color) in anim::fireworks(view.hand_no as u64 * 7919, elapsed, inner.width, inner.height) {
            buf.set_string(inner.x + x, inner.y + y, glyph.to_string(), Style::default().fg(color).bg(FELT_DARK));
        }
        buf.set_string(pot_x, pot_y, &pot_text, pot_style);
    }
}

fn draw_seat(buf: &mut Buffer, rect: Rect, app: &App, view: &TableView, seat: usize, cards_landed: usize) {
    let s = &view.seats[seat];
    let is_actor = view.actor == Some(seat) && view.phase == Phase::Betting;
    let is_me = app.my_seat == Some(seat);
    let out = s.folded || (view.phase != Phase::Lobby && s.hole_count == 0 && !s.in_hand);
    let border = if !s.connected {
        DIM
    } else if is_actor && is_me {
        turn_gold(app.tick)
    } else if is_actor {
        GOLD
    } else if is_me {
        YOU
    } else if out {
        Color::Rgb(45, 75, 60)
    } else {
        Color::Rgb(80, 140, 110)
    };
    let bg = if is_actor { Color::Rgb(28, 62, 42) } else { FELT_DARK };
    let name_fg = if out || !s.connected { DIM } else if is_me { YOU } else { TEXT };
    let name_room = rect.width.saturating_sub(8) as usize;
    let name = truncate(&s.name, name_room);

    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_set(if is_actor { ASCII_HOT } else { ASCII })
        .border_style(Style::default().fg(border).bg(bg))
        .style(Style::default().bg(bg))
        .title(Span::styled(
            format!(" {}{name} ", if is_actor { "> " } else { "" }),
            Style::default().fg(if is_actor { border } else { name_fg }).add_modifier(Modifier::BOLD),
        ));
    if seat == view.button && view.phase != Phase::Lobby {
        block = block.title(Line::from(Span::styled(" D ", Style::default().fg(Color::Black).bg(Color::Rgb(235, 235, 228)).add_modifier(Modifier::BOLD))).right_aligned());
    }

    let (status, status_fg) = if !s.connected {
        ("away".to_string(), DIM)
    } else if s.folded {
        ("folded".to_string(), DIM)
    } else if s.all_in {
        ("ALL-IN".to_string(), GOLD)
    } else if let Some(mv) = s.last_move.as_ref().filter(|m| !m.is_empty() && view.phase == Phase::Betting) {
        (mv.clone(), TEXT)
    } else if view.phase != Phase::Lobby && s.hole_count == 0 && !s.in_hand {
        ("sitting out".to_string(), DIM)
    } else {
        (String::new(), DIM)
    };
    if !status.is_empty() {
        block = block.title_bottom(Span::styled(format!(" {status} "), Style::default().fg(status_fg)));
    }

    // Shown cards at showdown, otherwise face-down backs for opponents in the hand.
    let reveal = view
        .last_result
        .as_ref()
        .filter(|_| view.phase == Phase::HandOver)
        .and_then(|r| r.reveals.iter().find(|r| r.seat == seat));
    if let Some(r) = reveal {
        let mut spans = vec![Span::raw(" ")];
        for c in &r.cards {
            spans.push(Span::styled(
                format!("{}{}", rank_str(*c), c.suit_symbol()),
                Style::default().fg(if c.is_red() { Color::Rgb(200, 30, 40) } else { Color::Black }).bg(Color::Rgb(240, 240, 232)).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::raw(" "));
        }
        block = block.title_bottom(Line::from(spans).right_aligned());
    } else if !is_me && !s.folded && s.hole_count > 0 && view.phase == Phase::Betting {
        let backs = cards_landed.min(s.hole_count);
        let mut spans = vec![Span::raw(" ")];
        for _ in 0..backs {
            spans.push(Span::styled("[#]", Style::default().fg(Color::Rgb(210, 70, 85))));
            spans.push(Span::raw(" "));
        }
        block = block.title_bottom(Line::from(spans).right_aligned());
    }
    block.render(rect, buf);

    let mut line = vec![Span::styled(chips(s.stack), Style::default().fg(if out { DIM } else { TEXT }).add_modifier(Modifier::BOLD))];
    if s.bet > 0 {
        line.push(Span::styled(format!("  bet ${}", chips(s.bet)), Style::default().fg(GOLD)));
    }
    buf.set_line(rect.x + 2, rect.y + 1, &Line::from(line), rect.width.saturating_sub(3));
}

fn rank_str(card: Card) -> String {
    match card.rank_char() {
        'T' => "10".into(),
        c => c.to_string(),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Sz {
    w: u16,
    h: u16,
}
/// Classic 8×6 ASCII card.
const SMALL: Sz = Sz { w: 8, h: 6 };
/// 12×9 ASCII card with a big suit drawing (terminals ≥ ~120×40).
const BIG: Sz = Sz { w: 12, h: 9 };

/// Two-row suit drawing that sits inside the `.--.` of a small card.
fn suit_small(card: Card) -> [&'static str; 2] {
    match card.suit_char() {
        's' => [":/\\:", "(__)"],
        'h' => ["(\\/)", ":\\/:"],
        'd' => [":/\\:", ":\\/:"],
        _ => [":():", "()()"],
    }
}

/// Five-row drawing for the middle of a big card.
fn suit_big(card: Card) -> [&'static str; 5] {
    let face = |top: &'static str, mouth: &'static str| -> [&'static str; 5] {
        let suit = match card.suit_char() {
            's' => " :/\\: ",
            'h' => " (\\/) ",
            'd' => " :/\\: ",
            _ => " :(): ",
        };
        [top, " |o o| ", mouth, " \\___/ ", suit]
    };
    match card.rank_char() {
        'K' => face(" /vVv\\ ", " | ^ | "),
        'Q' => face(" .oOo. ", " | v | "),
        'J' => face(" .---. ", " | - | "),
        _ => match card.suit_char() {
            's' => ["   .   ", "  / \\  ", " /   \\ ", "(_. ._)", "  /_\\  "],
            'h' => [" _   _ ", "( `v' )", " \\   / ", "  \\ /  ", "   v   "],
            'd' => ["   ^   ", "  / \\  ", " <   > ", "  \\ /  ", "   v   "],
            _ => ["  ( )  ", " (   ) ", "( )_( )", "   |   ", "  /_\\  "],
        },
    }
}

fn frame_style(dim: bool) -> Style {
    Style::default().fg(if dim { Color::Rgb(115, 125, 120) } else { Color::Rgb(205, 210, 200) })
}

/// Outline: `.---.` top, `|` sides (interior blanked), `` `---' `` bottom.
fn draw_outline(buf: &mut Buffer, x: u16, y: u16, sz: Sz, style: Style) {
    let bar = "-".repeat(sz.w as usize - 2);
    buf.set_string(x, y, format!(".{bar}."), style);
    for dy in 1..sz.h - 1 {
        buf.set_string(x, y + dy, format!("|{}|", " ".repeat(sz.w as usize - 2)), style);
    }
    buf.set_string(x, y + sz.h - 1, format!("`{bar}'"), style);
}

/// An ASCII-art playing card:
/// ```text
/// .------.
/// |A.--. |
/// | :/\: |
/// | (__) |
/// | '--'A|
/// `------'
/// ```
fn draw_card(buf: &mut Buffer, x: u16, y: u16, card: Card, dim: bool, sz: Sz) {
    let frame = frame_style(dim);
    let ink = if dim {
        Color::Rgb(130, 140, 135)
    } else if card.is_red() {
        Color::Rgb(235, 80, 80)
    } else {
        Color::Rgb(235, 235, 230)
    };
    let ink = Style::default().fg(ink);
    let bold = ink.add_modifier(Modifier::BOLD);
    let rank = rank_str(card);
    let r = rank.len() as u16;
    draw_outline(buf, x, y, sz, frame);
    if sz == SMALL {
        let art = suit_small(card);
        buf.set_string(x + 1, y + 1, &rank, bold);
        buf.set_string(x + 1 + r, y + 1, ".--.", frame);
        buf.set_string(x + 2, y + 2, art[0], ink);
        buf.set_string(x + 2, y + 3, art[1], ink);
        buf.set_string(x + 7 - r - 4, y + 4, "'--'", frame);
        buf.set_string(x + 7 - r, y + 4, &rank, bold);
        return;
    }
    buf.set_string(x + 2, y + 1, &rank, bold);
    buf.set_string(x + sz.w - 2 - r, y + sz.h - 2, &rank, bold);
    for (dy, line) in suit_big(card).iter().enumerate() {
        let lx = x + (sz.w - line.chars().count() as u16) / 2;
        buf.set_string(lx, y + 2 + dy as u16, line, ink);
    }
}

fn draw_card_back(buf: &mut Buffer, x: u16, y: u16, sz: Sz) {
    draw_outline(buf, x, y, sz, frame_style(false));
    let pat = Style::default().fg(Color::Rgb(210, 70, 85));
    for dy in 1..sz.h - 1 {
        let row: String = (0..sz.w - 2).map(|i| if (i + dy) % 2 == 0 { '/' } else { '\\' }).collect();
        buf.set_string(x + 1, y + dy, row, pat);
    }
}

/// A card seen edge-on, halfway through a flip.
fn draw_card_edge(buf: &mut Buffer, x: u16, y: u16, sz: Sz) {
    let style = frame_style(false);
    for dy in 0..sz.h {
        buf.set_string(x + sz.w / 2 - 1, y + dy, "||", style);
    }
}

fn draw_card_placeholder(buf: &mut Buffer, x: u16, y: u16, sz: Sz) {
    let style = Style::default().fg(Color::Rgb(45, 90, 70));
    let dots = ". ".repeat(sz.w as usize / 2);
    buf.set_string(x, y, &dots, style);
    for dy in 1..sz.h - 1 {
        buf.set_string(x, y + dy, format!(":{}:", " ".repeat(sz.w as usize - 2)), style);
    }
    buf.set_string(x, y + sz.h - 1, &dots, style);
}

/// Bottom panel: your two cards on the left; on the right a headline, the big
/// buttons (your turn) or what's going on, and the turn clock / chat line.
fn draw_panel(f: &mut Frame, area: Rect, app: &App) {
    let view = app.view.as_ref();
    let legal = app.my_legal();
    let me = app.my_seat.and_then(|m| view.and_then(|v| v.seats.get(m)));
    let gold = turn_gold(app.tick);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(if legal.is_some() { ASCII_HOT } else { ASCII })
        .border_style(Style::default().fg(if legal.is_some() { gold } else { Color::Rgb(90, 130, 110) }));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Your cards.
    let folded = me.is_some_and(|s| s.folded);
    let deal_ms = app.anim.deal_at.map(|at| at.elapsed().as_millis() as u64);
    let dealing = deal_ms.is_some_and(|ms| ms < app::DEAL_MS);
    let flipping = deal_ms.is_some_and(|ms| (app::DEAL_MS..app::DEAL_MS + app::FLIP_MS / 2).contains(&ms));
    let sz = if inner.height >= BIG.h { BIG } else { SMALL };
    for i in 0..2u16 {
        let x = inner.x + 1 + i * (sz.w + 1);
        let buf = f.buffer_mut();
        match app.my_cards.get(i as usize) {
            Some(_) if dealing => draw_card_back(buf, x, inner.y, sz),
            Some(_) if flipping => draw_card_edge(buf, x, inner.y, sz),
            Some(c) => draw_card(buf, x, inner.y, *c, folded, sz),
            None => draw_card_placeholder(buf, x, inner.y, sz),
        }
    }

    let rx = inner.x + 1 + 2 * (sz.w + 1) + 2;
    let rw = (inner.x + inner.width).saturating_sub(rx);
    // The 5 text rows sit vertically centred next to the cards.
    let top = inner.y + inner.height.saturating_sub(5) / 2;
    let row = |dy: u16| Rect::new(rx, top + dy, rw, 1);

    if let Some(legal) = legal {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" YOUR TURN ", Style::default().fg(Color::Black).bg(gold).add_modifier(Modifier::BOLD)),
            ])),
            row(0),
        );
        let allin = me.map(|s| s.stack + s.bet).unwrap_or(0);
        let middle = if legal.can_check { "CHECK".to_string() } else { format!("CALL {}", chips(legal.to_call)) };
        let raise = if view.is_some_and(|v| v.current_bet == 0) { "BET" } else { "RAISE" };
        let mut buttons = [
            ("1", "FOLD".to_string(), FOLD_C, true),
            ("2", middle, CALL_C, legal.can_check || legal.can_call),
            ("3", raise.to_string(), RAISE_C, legal.can_raise),
            ("4", format!("ALL-IN {}", chips(allin)), ALLIN_C, legal.can_allin),
        ];
        // ASCII boxes: ".------." / "| 1 FOLD |" / "`------'", 1 col apart.
        let total: u16 = buttons.iter().map(|b| b.1.chars().count() as u16 + 7).sum();
        if total > rw {
            buttons[3].1 = "ALL-IN".into();
        }
        let mut x = rx;
        for (key, label, color, lit) in buttons {
            let text = format!(" {key} {label} ");
            let w = text.chars().count() as u16 + 2;
            if x + w > rx + rw {
                break;
            }
            let ink = if lit { color } else { Color::Rgb(105, 112, 108) };
            let st = Style::default().fg(ink).add_modifier(if lit { Modifier::BOLD } else { Modifier::empty() });
            let bar = "-".repeat(w as usize - 2);
            let buf = f.buffer_mut();
            buf.set_string(x, top + 1, format!(".{bar}."), st);
            buf.set_string(x, top + 2, format!("|{text}|"), st);
            buf.set_string(x, top + 3, format!("`{bar}'"), st);
            // The key in reverse video so "press 2" reads at a glance.
            if lit {
                buf.set_string(x + 2, top + 2, key, Style::default().fg(Color::Black).bg(color).add_modifier(Modifier::BOLD));
            }
            x += w + 1;
        }
    } else if let Some(view) = view {
        let enter = |what: &str| Line::from(vec![
            Span::styled(" Enter ", Style::default().fg(Color::Black).bg(CALL_C).add_modifier(Modifier::BOLD)),
            Span::styled(format!(" {what}"), Style::default().fg(CALL_C).add_modifier(Modifier::BOLD)),
        ]);
        let line = match view.phase {
            Phase::HandOver if app.mode == Mode::Host => enter("next hand"),
            Phase::Betting if folded => Line::from(Span::styled("folded", Style::default().fg(DIM))),
            Phase::Betting => match view.actor {
                Some(a) => Line::from(Span::styled(format!("{}'s turn", app.seat_name(a)), Style::default().fg(TEXT).add_modifier(Modifier::BOLD))),
                None => Line::default(),
            },
            _ => Line::default(),
        };
        f.render_widget(Paragraph::new(line), row(2));
    }

    // Last row: chat box, or the turn clock.
    if app.chat_mode {
        let cursor = if app.tick / 10 % 2 == 0 { "_" } else { " " };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" chat > ", Style::default().fg(Color::Black).bg(Color::Rgb(120, 200, 220))),
                Span::raw(format!(" {}{cursor}", app.input)),
            ])),
            row(4),
        );
    } else if let (Some(frac), Some(ms)) = (app.timer_fraction(), app.deadline_left()) {
        let label = format!("{:>3}s [", ms.div_ceil(1000));
        let width = rw.saturating_sub(label.chars().count() as u16 + 2) as usize;
        let filled = (width as f32 * frac).round() as usize;
        let color = if frac > 0.5 {
            Color::Rgb(110, 210, 130)
        } else if frac > 0.2 {
            Color::Rgb(240, 190, 80)
        } else {
            Color::Rgb(240, 90, 80)
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(label, Style::default().fg(color)),
                Span::styled("=".repeat(filled), Style::default().fg(color)),
                Span::styled("-".repeat(width - filled), Style::default().fg(Color::Rgb(45, 55, 50))),
                Span::styled("]", Style::default().fg(color)),
            ])),
            row(4),
        );
    }
}

fn draw_log(f: &mut Frame, area: Rect, app: &App) {
    let lines: Vec<Line> = app
        .log
        .iter()
        .rev()
        .take(area.height as usize)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|entry| {
            let color = match entry.kind {
                LogKind::Chat => Color::Rgb(120, 200, 220),
                LogKind::Error => RED,
                LogKind::Action => TEXT,
                LogKind::Info => DIM,
            };
            Line::from(Span::styled(format!(" {}", entry.text), Style::default().fg(color)))
        })
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let mut spans = vec![];
    if !app.connected {
        spans.push(Span::styled(" offline ", Style::default().fg(Color::Black).bg(RED).add_modifier(Modifier::BOLD)));
    }
    spans.push(Span::styled(if app.chat_mode { " Enter send · Esc cancel" } else { " ? help" }, Style::default().fg(DIM)));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn seat_positions(n: usize) -> &'static [(u16, u16)] {
    const P2: &[(u16, u16)] = &[(50, 100), (50, 0)];
    const P3: &[(u16, u16)] = &[(50, 100), (0, 0), (100, 0)];
    const P4: &[(u16, u16)] = &[(50, 100), (0, 0), (50, 0), (100, 0)];
    const P5: &[(u16, u16)] = &[(50, 100), (0, 55), (0, 0), (50, 0), (100, 0)];
    const P6: &[(u16, u16)] = &[(50, 100), (0, 60), (0, 0), (50, 0), (100, 0), (100, 60)];
    match n {
        0 | 1 => P2,
        2 => P2,
        3 => P3,
        4 => P4,
        5 => P5,
        _ => P6,
    }
}

fn at(inner: Rect, fx: u16, fy: u16, w: u16, h: u16) -> Rect {
    let x = inner.x + inner.width.saturating_sub(w) * fx / 100;
    let y = inner.y + inner.height.saturating_sub(h) * fy / 100;
    Rect::new(
        x.min(inner.x + inner.width.saturating_sub(w)),
        y.min(inner.y + inner.height.saturating_sub(h)),
        w.min(inner.width),
        h.min(inner.height),
    )
}

fn street_label(view: &TableView) -> String {
    if view.phase == Phase::Lobby {
        return "lobby".into();
    }
    match view.street {
        Street::Preflop => "preflop",
        Street::Flop => "flop",
        Street::Turn => "turn",
        Street::River => "river",
    }
    .to_string()
}

fn overlay(f: &mut Frame, area: Rect, w: u16, title: &str, color: Color, lines: Vec<Line>) {
    let rect = centered(area, w, lines.len() as u16 + 2);
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(ASCII)
        .border_style(Style::default().fg(color))
        .title(Span::styled(format!(" {title} "), Style::default().fg(color).add_modifier(Modifier::BOLD)))
        .style(Style::default().bg(Color::Black));
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center).block(block), rect);
}

fn draw_raise(f: &mut Frame, area: Rect, app: &App) {
    let (Some(picker), Some(p)) = (&app.raise, app.raise_presets()) else { return };
    let (min, max) = (p[0].1, p[3].1);
    let amount = picker.amount;
    let cursor = if picker.typed.is_some() && app.tick / 10 % 2 == 0 { "_" } else { " " };
    let valid = (min..=max).contains(&amount);
    // Slider between min and max.
    let width = 30usize;
    let pos = if max > min { ((amount.clamp(min, max) - min) as f32 / (max - min) as f32 * width as f32) as usize } else { width };
    let slider = format!("[{}O{}]", "=".repeat(pos), "-".repeat(width - pos.min(width)));
    let mut presets = Vec::new();
    for (key, (label, v)) in ["M", "H", "P", "A"].iter().zip(p.iter()) {
        let active = *v == amount && picker.typed.is_none();
        presets.push(Span::styled(format!(" {key} "), Style::default().fg(GOLD).bg(Color::Rgb(40, 40, 30)).add_modifier(Modifier::BOLD)));
        presets.push(Span::styled(
            format!(" {label} {} ", chips(*v)),
            if active { Style::default().fg(Color::Black).bg(GOLD) } else { Style::default().fg(TEXT) },
        ));
        presets.push(Span::raw(" "));
    }
    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("<  ", Style::default().fg(DIM)),
            Span::styled(
                format!(" {}{cursor}", chips(amount)),
                Style::default().fg(if valid { Color::Black } else { Color::White }).bg(if valid { GOLD } else { RED }).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  >", Style::default().fg(DIM)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(format!("{} ", chips(min)), Style::default().fg(DIM)),
            Span::styled(slider, Style::default().fg(GOLD)),
            Span::styled(format!(" {}", chips(max)), Style::default().fg(DIM)),
        ]),
        Line::from(""),
        Line::from(presets),
        Line::from(""),
        Line::from(Span::styled(
            if valid { "←→ adjust · Enter raise · Esc cancel".to_string() } else { format!("must be between {} and {}", chips(min), chips(max)) },
            Style::default().fg(if valid { DIM } else { RED }),
        )),
    ];
    overlay(f, area, 70.min(area.width), "RAISE TO", GOLD, lines);
}

fn draw_confirm_leave(f: &mut Frame, area: Rect, app: &App) {
    let warn = match app.mode {
        Mode::Host => "this closes the table for everyone",
        Mode::Client | Mode::Spectator => "",
    };
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled("Leave the table?", Style::default().fg(TEXT).add_modifier(Modifier::BOLD))),
        Line::from(Span::styled(warn, Style::default().fg(DIM))),
        Line::from(""),
        Line::from(vec![
            Span::styled(" Y ", Style::default().fg(Color::Black).bg(RED).add_modifier(Modifier::BOLD)),
            Span::styled(" leave     ", Style::default().fg(TEXT)),
            Span::styled(" N ", Style::default().fg(Color::Black).bg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled(" stay", Style::default().fg(TEXT)),
        ]),
        Line::from(""),
    ];
    overlay(f, area, 60.min(area.width), "LEAVE", RED, lines);
}

fn draw_help(f: &mut Frame, area: Rect) {
    let row = |k: &str, d: &str| {
        Line::from(vec![
            Span::styled(format!(" {k:^9} "), Style::default().fg(Color::Black).bg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled(format!("  {d:<14}"), Style::default().fg(TEXT)),
        ])
    };
    let lines = vec![
        Line::from(""),
        row("1  f", "fold"),
        row("2  c x", "check / call"),
        row("3  r", "raise"),
        row("4  a", "all-in"),
        row("Enter", "deal (host)"),
        row("t  /", "chat"),
        row("Esc  q", "leave"),
        Line::from(""),
    ];
    overlay(f, area, 34.min(area.width), "KEYS", GOLD, lines);
}

fn draw_notice(f: &mut Frame, area: Rect, text: &str) {
    let width = (text.len() as u16 + 4).min(area.width);
    let rect = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(3),
        width,
        1,
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {text} "),
            Style::default().fg(Color::Black).bg(GOLD),
        ))),
        rect,
    );
}

pub(crate) fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
    }
}

#[cfg(test)]
mod tests {
    use crate::app::{App, Mode};
    use crate::game::{Phase, Table, TableParams};
    use crate::net::{NetCommand, UiEvent};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};

    /// An App seated at a real 3-handed table, on the clock.
    fn app_on_turn() -> (App, tokio::sync::mpsc::UnboundedReceiver<NetCommand>) {
        let mut table = Table::new(TableParams { max_seats: 6, small_blind: 25, big_blind: 50, buyin: 5_000, turn_ms: 30_000 });
        for name in ["alice", "bob", "carol"] {
            let seat = table.join(name, &format!("pk-{name}"), "enc", 5_000).unwrap();
            table.set_ready(seat, true);
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(Mode::Client, "K7Q2-9XZP".into(), "wss://x".into(), tx, true);
        app.apply_event(UiEvent::View(table.view("r")));
        table.start_hand_seeded(1, [3; 32]).unwrap();
        table.set_deadline(u64::MAX / 2);
        let view = table.view("r");
        let me = view.actor.unwrap();
        app.apply_event(UiEvent::Joined { seat: me });
        app.apply_event(UiEvent::MyCards { hand: view.hand_no, cards: table.seats[me].as_ref().unwrap().hole.clone() });
        app.apply_event(UiEvent::View(view));
        assert_eq!(app.view.as_ref().unwrap().phase, Phase::Betting);
        (app, rx)
    }

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn screen(app: &App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| super::draw(f, app)).unwrap();
        let buf = term.backend().buffer();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A whole 3-handed hand, one App per seat: at every step exactly the
    /// player to act sees YOUR TURN and the buttons, everyone else sees who
    /// they're waiting for. Set POKER_DUMP=1 to print the screens.
    #[test]
    fn full_hand_only_the_actor_sees_controls() {
        let names = ["alice", "bob", "carol"];
        let mut table = Table::new(TableParams { max_seats: 6, small_blind: 25, big_blind: 50, buyin: 5_000, turn_ms: 30_000 });
        for name in names {
            let seat = table.join(name, &format!("pk-{name}"), "enc", 5_000).unwrap();
            table.set_ready(seat, true);
        }
        let mut apps = Vec::new();
        let mut rxs = Vec::new();
        for seat in 0..names.len() {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            let mut app = App::new(Mode::Client, "K7Q2-9XZP".into(), "wss://x".into(), tx, false);
            app.apply_event(UiEvent::Joined { seat });
            app.apply_event(UiEvent::View(table.view("r")));
            apps.push(app);
            rxs.push(rx);
        }
        table.start_hand_seeded(1, [7; 32]).unwrap();
        let dump = std::env::var("POKER_DUMP").is_ok();
        let mut steps = 0;
        loop {
            table.set_deadline(u64::MAX / 2);
            let view = table.view("r");
            for (seat, app) in apps.iter_mut().enumerate() {
                app.apply_event(UiEvent::MyCards { hand: view.hand_no, cards: table.seats[seat].as_ref().unwrap().hole.clone() });
                app.apply_event(UiEvent::View(view.clone()));
                app.anim = Default::default(); // render the settled frame
            }
            if view.phase != Phase::Betting {
                break;
            }
            let actor = view.actor.expect("someone acts while betting");
            for (seat, app) in apps.iter().enumerate().flat_map(|a| [(80, 24), (130, 42)].map(move |sz| (a, sz))).map(|((seat, app), (w, h))| (seat, (app, w, h))) {
                let (app, w, h) = app;
                let text = screen(app, w, h);
                if dump && seat == actor {
                    println!("--- step {steps}: {} to act ---\n{text}", names[seat]);
                }
                if seat == actor {
                    assert!(text.contains("YOUR TURN") && text.contains("FOLD"), "step {steps}: actor sees no controls\n{text}");
                } else {
                    assert!(!text.contains("YOUR TURN"), "step {steps}: seat {seat} wrongly sees controls");
                    assert!(text.contains("'s turn"), "step {steps}: seat {seat} doesn't say whose turn\n{text}");
                }
            }
            assert!(apps[actor].bell, "turn start rings the bell");
            apps[actor].bell = false;
            apps[actor].handle_key(key(KeyCode::Char('2')));
            let Ok(NetCommand::Action(mv)) = rxs[actor].try_recv() else { panic!("key 2 sent no action") };
            table.apply(actor, mv, 2).unwrap();
            steps += 1;
            assert!(steps < 50, "hand never finished");
        }
        assert_eq!(table.view("r").phase, Phase::HandOver);
        for app in &apps {
            let text = screen(app, 80, 24);
            if dump {
                println!("--- hand over ---\n{text}");
            }
            assert!(text.contains("wins") || text.contains("YOU WIN"), "no winner banner\n{text}");
        }
    }

    #[test]
    fn raise_presets_are_legal_and_picker_clamps() {
        let (mut app, mut rx) = app_on_turn();
        let legal = app.my_legal().unwrap();
        let presets = app.raise_presets().unwrap();
        for (_, v) in presets {
            assert!((legal.min_raise_to..=legal.max_raise_to).contains(&v));
        }
        assert!(presets[1].1 <= presets[2].1, "half pot <= pot");
        // Open the picker, type an over-large amount, confirm: clamps to max.
        app.handle_key(key(KeyCode::Char('3')));
        assert!(app.raise.is_some());
        for c in "999999".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        app.handle_key(key(KeyCode::Enter));
        assert!(app.raise.is_none());
        match rx.try_recv() {
            Ok(NetCommand::Action(crate::game::Move::Raise { to })) => assert_eq!(to, legal.max_raise_to),
            other => panic!("expected a raise, got {other:?}"),
        }
        // Key 2 calls when there's a bet to call.
        app.handle_key(key(KeyCode::Char('2')));
        assert!(matches!(rx.try_recv(), Ok(NetCommand::Action(crate::game::Move::Call))));
    }

    #[test]
    fn esc_asks_before_leaving() {
        let (mut app, _rx) = app_on_turn();
        app.handle_key(key(KeyCode::Esc));
        assert!(app.confirm_leave && !app.leave);
        app.handle_key(key(KeyCode::Char('n')));
        assert!(!app.confirm_leave && !app.leave);
        app.handle_key(key(KeyCode::Esc));
        app.handle_key(key(KeyCode::Char('y')));
        assert!(app.leave);
    }

    #[test]
    fn table_and_overlays_render_at_common_sizes() {
        for (w, h) in [(80, 24), (120, 40), (200, 60)] {
            let (mut app, _rx) = app_on_turn();
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| super::draw(f, &app)).unwrap();
            let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
            assert!(text.contains("YOUR TURN"), "{w}x{h}: no turn banner");
            assert!(text.contains("FOLD"), "{w}x{h}: no buttons");
            app.handle_key(key(KeyCode::Char('3')));
            term.draw(|f| super::draw(f, &app)).unwrap();
            app.raise = None;
            app.confirm_leave = true;
            term.draw(|f| super::draw(f, &app)).unwrap();
            app.confirm_leave = false;
            app.help = true;
            term.draw(|f| super::draw(f, &app)).unwrap();
        }
    }
}
