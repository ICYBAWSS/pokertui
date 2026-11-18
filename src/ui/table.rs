//! The table: lobby, felt, board, seats and the deal / chip / firework animations.

use super::card::{BIG, CardView, Face, SMALL, rank_str};
use super::theme::*;
use super::{popup, truncate};
use crate::anim;
use crate::app::{self, App, Mode};
use crate::game::{Card, Phase, Street, TableView};
use crate::menu::chips;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};
use std::time::Duration;

const SEAT_H: u16 = 3;

pub fn draw_lobby(f: &mut Frame, area: Rect, app: &App, view: &TableView) {
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
                CardView::new(Face::Up(card), sz).draw(f.buffer_mut(), x0 + i as u16 * step, area.y + lift[i]);
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
        popup(area, w, h)
    };
    f.render_widget(Clear, rect);
    let block = ascii_block(GOLD)
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

pub fn draw_table(f: &mut Frame, area: Rect, app: &App, view: &TableView) {
    let my_turn = app.my_legal().is_some();
    let (border, border_type) = if my_turn {
        (TURN_GOLD, ASCII)
    } else {
        (FELT_EDGE, ASCII)
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
    block = block.title(Line::from(Span::styled(blinds_label(view), Style::default().fg(DIM))).right_aligned());
    if my_turn {
        block = block.title(Line::from(Span::styled(" your turn ", Style::default().fg(border))).centered());
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
        Some((b, true)) => (b, Style::default().fg(Color::Black).bg(TURN_GOLD).add_modifier(Modifier::BOLD)),
        Some((b, false)) => (b, Style::default().fg(TURN_GOLD).add_modifier(Modifier::BOLD)),
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
                    Some(ms) if ms < 0 => CardView::new(Face::Empty, sz).draw(buf, x, board_y),
                    Some(ms) if (ms as u64) < app::FLIP_MS => {
                        let t = ms as f32 / app::FLIP_MS as f32;
                        if t < 0.35 {
                            CardView::new(Face::Back, sz).draw(buf, x, board_y)
                        } else if t < 0.65 {
                            CardView::new(Face::Edge, sz).draw(buf, x, board_y)
                        } else {
                            CardView::new(Face::Up(*card), sz).draw(buf, x, board_y)
                        }
                    }
                    _ => CardView::new(Face::Up(*card), sz).draw(buf, x, board_y),
                }
            }
            None => CardView::new(Face::Empty, sz).draw(buf, x, board_y),
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
        f.render_widget(SeatWidget { app, view, seat: *seat, cards_landed: landed(*seat) }, *rect);
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

/// Blinds, or the tournament level and countdown to the next one.
fn blinds_label(view: &TableView) -> String {
    let blinds = format!("{}/{}", chips(view.small_blind), chips(view.big_blind));
    let Some(t) = &view.tourney else {
        return format!(" {blinds} ");
    };
    if t.over {
        return " TOURNAMENT OVER ".into();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let next = if t.next_level_at_ms > now {
        let s = (t.next_level_at_ms - now) / 1000;
        format!(" · next {}:{:02}", s / 60, s % 60)
    } else {
        String::new()
    };
    format!(" L{} · {blinds}{next} ", t.level + 1)
}

fn ordinal(n: usize) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// One player's box: name, badges, stack, bet, status and their cards.
struct SeatWidget<'a> {
    app: &'a App,
    view: &'a TableView,
    seat: usize,
    cards_landed: usize,
}

impl Widget for SeatWidget<'_> {
    fn render(self, rect: Rect, buf: &mut Buffer) {
        let SeatWidget { app, view, seat, cards_landed } = self;
    let s = &view.seats[seat];
    let is_actor = view.actor == Some(seat) && view.phase == Phase::Betting;
    let is_me = app.my_seat == Some(seat);
    let out = s.folded || (view.phase != Phase::Lobby && s.hole_count == 0 && !s.in_hand);
    let border = if !s.connected {
        DIM
    } else if is_actor && is_me {
        TURN_GOLD
    } else if is_actor {
        GOLD
    } else if is_me {
        YOU
    } else if out {
        Color::Rgb(45, 75, 60)
    } else {
        BOX_EDGE
    };
    let bg = if is_actor { Color::Rgb(28, 62, 42) } else { FELT_DARK };
    let name_fg = if out || !s.connected { DIM } else if is_me { YOU } else { TEXT };
    let name_room = rect.width.saturating_sub(8) as usize;
    let name = truncate(&s.name, name_room);

    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_set(if is_actor && !is_me { ASCII_HOT } else { ASCII })
        .border_style(Style::default().fg(border).bg(bg))
        .style(Style::default().bg(bg))
        .title(Span::styled(
            format!(" {}{name} ", if is_actor { "> " } else { "" }),
            Style::default().fg(if is_actor { border } else { name_fg }).add_modifier(Modifier::BOLD),
        ));
    if view.phase != Phase::Lobby {
        let chip = badge;
        let mut badges = vec![];
        if view.sb_seat == Some(seat) {
            badges.push(chip("SB", Color::Rgb(100, 165, 235)));
        }
        if view.bb_seat == Some(seat) {
            badges.push(chip("BB", Color::Rgb(235, 155, 65)));
        }
        if seat == view.button {
            badges.push(chip("D", Color::Rgb(235, 235, 228)));
        }
        if !badges.is_empty() {
            let spaced: Vec<Span> = badges.into_iter().flat_map(|b| [b, Span::raw(" ")]).collect();
            block = block.title(Line::from(spaced).right_aligned());
        }
    }

    let (status, status_fg) = if let Some(p) = s.place.filter(|_| s.stack == 0) {
        (format!("out · {}", ordinal(p)), DIM)
    } else if s.place == Some(1) {
        ("WINNER".to_string(), GOLD)
    } else if !s.connected {
        ("away".to_string(), DIM)
    } else if s.sitting_out {
        ("sitting out".to_string(), DIM)
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

    // Cards the player chose to show after the hand, otherwise face-down backs for opponents in the hand.
    let shown: Option<&[Card]> = (!s.reveal.is_empty()).then_some(s.reveal.as_slice());
    if let Some(cards) = shown {
        let mut spans = vec![Span::raw(" ")];
        for c in cards {
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
    let line = Line::from(line);
    let used = line.width() as u16;
    buf.set_line(rect.x + 2, rect.y + 1, &line, rect.width.saturating_sub(3));

    // A few small chips on the right: how much you have, next to the biggest stack at the table.
    let biggest = view.seats.iter().map(|x| x.stack).max().unwrap_or(0).max(1);
    let n = if s.stack == 0 { 0 } else { (s.stack as f64 / biggest as f64 * 6.0).ceil().clamp(1.0, 6.0) as u16 };
    let tint = if out {
        DIM
    } else {
        match s.stack / view.big_blind.max(1) {
            0..=19 => Color::Rgb(135, 135, 145),
            20..=49 => Color::Rgb(75, 120, 185),
            50..=99 => Color::Rgb(70, 150, 95),
            100..=199 => Color::Rgb(175, 80, 80),
            _ => Color::Rgb(185, 155, 70),
        }
    };
    let start = (rect.x + rect.width).saturating_sub(2 + n);
    if n > 0 && start > rect.x + 2 + used + 1 {
        buf.set_string(start, rect.y + 1, "●".repeat(n as usize), Style::default().fg(tint));
    }
}

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

