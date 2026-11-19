//! The table: lobby, felt, board, seats and the deal / chip / firework animations.

use super::card::{BIG, CardView, Face, SMALL, rank_str};
use super::chips;
use super::theme::*;
use super::{popup, truncate};
use crate::anim;
use crate::app::{self, App, Mode, Spot};
use crate::game::{Card, Phase, Street, TableView};
use crate::menu::chips;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};


pub fn draw_lobby(f: &mut Frame, area: Rect, app: &App, view: &TableView) {
    let seated: Vec<&crate::game::SeatView> = view.seats.iter().filter(|s| !s.name.is_empty()).collect();
    let deal_hint = if app.mode == Mode::Host {
        if seated.len() >= 2 { "Enter  deal" } else { "waiting for players" }
    } else {
        "waiting for host"
    };
    // Room card: code, who's here (one per line so nobody is cut off), what happens next.
    let glow = anim::pulse(app.tick);
    let code_bg = Color::Rgb(200 + (55.0 * glow) as u8, 165 + (40.0 * glow) as u8, 70);
    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!("  {}  ", app.code), Style::default().fg(Color::Black).bg(code_bg).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(""),
    ];
    for s in &seated {
        let me = app.my_seat == Some(s.seat);
        lines.push(Line::from(Span::styled(
            s.name.clone(),
            Style::default().fg(if me { YOU } else { TEXT }).add_modifier(Modifier::BOLD),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        deal_hint,
        if app.mode == Mode::Host && seated.len() >= 2 {
            Style::default().fg(Color::Rgb((150.0 + 105.0 * glow) as u8, 255, (150.0 + 105.0 * glow) as u8)).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(DIM)
        },
    )));
    let h = (lines.len() as u16 + 2).min(area.height);
    let w = 56.min(area.width);

    // A royal flush fanned across the top, as big as fits above the room card.
    let sz = [BIG, SMALL].into_iter().find(|sz| area.height >= sz.h + 2 + h + 2 && area.width >= 5 * (sz.w + 2));
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

/// "* bob wins 1,200 · Two Pair *", or "*** YOU WIN … ***" when it's you. Amounts are what each
/// winner actually collected, so split and side pots add up.
fn winner_banner(app: &App, view: &TableView) -> Option<(String, bool)> {
    if view.phase != Phase::HandOver {
        return None;
    }
    let result = view.last_result.as_ref()?;
    let p = result.pots.first()?;
    let won = result.collected();
    let mine = app.my_seat.and_then(|me| won.iter().find(|(s, _)| *s == me)).map(|(_, a)| *a);
    // "alice wins uncontested" would just repeat the name.
    let desc = if p.desc.is_empty() || p.desc.ends_with("uncontested") { String::new() } else { format!(" · {}", p.desc) };
    Some(if let Some(amount) = mine {
        (format!(" *** YOU WIN {}{desc} *** ", chips(amount)), true)
    } else if won.len() == 1 {
        (format!("* {} wins {}{desc} *", app.seat_name(won[0].0), chips(won[0].1)), false)
    } else {
        let list = won.iter().map(|(s, a)| format!("{} {}", app.seat_name(*s), chips(*a))).collect::<Vec<_>>().join(" & ");
        (format!("* {list}{desc} *"), false)
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
    // Seats grow a row for their chips when the table is tall enough.
    let seat_h: u16 = if inner.height >= 2 * 4 + SMALL.h + 1 { 4 } else { 3 };
    let sz = if inner.height >= 2 * seat_h + BIG.h + 2 && inner.width >= 5 * BIG.w + 4 + 2 * 20 { BIG } else { SMALL };
    let board_w = sz.w * 5 + 4;
    let board_x = inner.x + inner.width.saturating_sub(board_w) / 2;
    // Pot sits on the line above the board; on a short table there's no such
    // line, so it goes on the table's bottom border instead.
    let roomy = inner.height > 2 * seat_h + sz.h;
    let two_rows = inner.height >= 2 * seat_h + sz.h + 2;
    let board_y = if two_rows {
        (inner.y + inner.height.saturating_sub(sz.h) / 2).max(inner.y + seat_h + 2)
    } else if roomy { (inner.y + inner.height.saturating_sub(sz.h) / 2).max(inner.y + seat_h + 1) } else { inner.y + seat_h };
    // With a spare row the pot's chips sit centred on top of its label; otherwise they go to the
    // right of the label.
    let pot_y = if roomy { board_y - 1 } else { inner.y + inner.height };
    let center = (inner.x + inner.width / 2, pot_y);

    let (pot_text, pot_style) = match winner_banner(app, view) {
        Some((b, true)) => (b, Style::default().fg(Color::Black).bg(TURN_GOLD).add_modifier(Modifier::BOLD)),
        Some((b, false)) => (b, Style::default().fg(TURN_GOLD).add_modifier(Modifier::BOLD)),
        None => (format!("POT  {}", chips(app.pot_shown() + view.seats.iter().map(|s| s.bet).sum::<u64>())), Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
    };
    let pot_x = inner.x + inner.width.saturating_sub(pot_text.chars().count() as u16) / 2;
    // The pot as chips either side of its label (while it pays out, the amount being paid).
    let blinds = (view.small_blind, view.big_blind);
    let paying_total: u64 = app.anim.payout.iter().map(|(_, a)| *a).sum();
    let pot_chips = if app.anim.payout_at.is_some_and(|at| at.elapsed().as_millis() < app::PAYOUT_MS as u128) {
        paying_total
    } else {
        app.pot_shown() + view.seats.iter().map(|s| s.bet).sum::<u64>()
    };
    let run = chips::chip_run(pot_chips, blinds, if two_rows { (inner.width as usize / 2).min(30) } else { 14 });
    let text_w = pot_text.chars().count() as u16;
    let (chips_y, chips_x) = if two_rows {
        (pot_y - 1, inner.x + inner.width.saturating_sub(run.len() as u16) / 2)
    } else {
        (pot_y, pot_x + text_w + 1)
    };
    let pot_cells: Vec<(u16, Color)> = run.iter().enumerate().filter_map(|(i, c)| c.map(|c| (chips_x + i as u16, c))).collect();

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
        rects.push((*seat, at(inner, fx, fy, panel_w, seat_h)));
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

    // While the pot is paying out, a winner's chips only appear as the flying ones land.
    let paying = app.anim.payout_at.is_some_and(|at| at.elapsed().as_millis() < app::PAYOUT_MS as u128);
    for (seat, rect) in &rects {
        let owed = if paying { app.anim.payout.iter().find(|(w, _)| w == seat).map_or(0, |(_, a)| *a) } else { 0 };
        let chip_stack = view.seats.get(*seat).map_or(0, |s| s.stack).saturating_sub(owed);
        f.render_widget(SeatWidget { chip_stack, app, view, seat: *seat, cards_landed: landed(*seat) }, *rect);
    }
    f.buffer_mut().set_string(pot_x, pot_y, &pot_text, pot_style);
    if pot_y < inner.y + inner.height {
        for (x, c) in &pot_cells {
            f.buffer_mut().set_string(*x, chips_y, "●", Style::default().fg(*c).bg(FELT_DARK));
        }
    }

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

    // Chips in flight: bets into the pot, the pot out to the winners. Seats' chips sit at the right
    // end of their box, so that is where chips leave from and land.
    let spot = |s: Spot| -> (i32, i32) {
        match s {
            Spot::Pot => (center.0 as i32 - 1, center.1 as i32),
            Spot::Seat(seat) => rects
                .iter()
                .find(|(r, _)| *r == seat)
                .map_or((center.0 as i32, center.1 as i32), |(_, r)| ((r.x + r.width) as i32 - 5, (r.y + 1) as i32)),
        }
    };
    for fl in &app.anim.flights {
        let ms = fl.at.elapsed().as_millis() as i64 - fl.delay_ms as i64;
        if ms < 0 || ms >= app::FLIGHT_MS as i64 {
            continue;
        }
        let t = anim::ease_out(ms as f32 / app::FLIGHT_MS as f32);
        let ((x0, y0), (x1, y1)) = (spot(fl.from), spot(fl.to));
        let x = x0 as f32 + (x1 - x0) as f32 * t;
        let y = y0 as f32 + (y1 - y0) as f32 * t - (std::f32::consts::PI * t).sin() * 2.0;
        chips::draw_chip(f.buffer_mut(), x.round() as i32, y.round() as i32, fl.color, FELT_DARK);
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
    /// Chips to draw in the box: the stack, less what a winner is still waiting to receive.
    chip_stack: u64,
    app: &'a App,
    view: &'a TableView,
    seat: usize,
    cards_landed: usize,
}

impl Widget for SeatWidget<'_> {
    fn render(self, rect: Rect, buf: &mut Buffer) {
        let SeatWidget { chip_stack, app, view, seat, cards_landed } = self;
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

    // The player's chips: runs of each denomination. On a tall table they get their own row in
    // the box; on a short one they share the stack's line.
    let bg = if is_actor { Color::Rgb(28, 62, 42) } else { FELT_DARK };
    let (row, x0, room) = if rect.height >= 4 {
        (rect.y + 2, rect.x + 2, rect.width.saturating_sub(4))
    } else {
        let room = rect.width.saturating_sub(2 + used + 2 + 2);
        (rect.y + 1, rect.x + rect.width - 3 - room, room)
    };
    for (i, chip) in chips::chip_run(chip_stack, (view.small_blind, view.big_blind), room as usize).into_iter().enumerate() {
        if let Some(c) = chip {
            buf.set_string(x0 + i as u16, row, "●", Style::default().fg(c).bg(bg));
        }
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

