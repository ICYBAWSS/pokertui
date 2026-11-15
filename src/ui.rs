//! Ratatui rendering: ASCII table, seats, cards, action bar and log.

use crate::anim;
use crate::app::{self, App, LogKind, Mode};
use crate::art;
use crate::menu::chips;
use std::time::Duration;
use crate::game::{Card, Phase, Street, TableView};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};

pub(crate) const FELT_DARK: Color = Color::Rgb(7, 45, 30);
pub(crate) const GOLD: Color = Color::Rgb(230, 190, 90);
pub(crate) const DIM: Color = Color::Rgb(120, 130, 125);
pub(crate) const YOU: Color = Color::Cyan;
pub(crate) const RED: Color = Color::Rgb(230, 90, 90);
pub(crate) const TEXT: Color = Color::Rgb(215, 220, 215);

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    if area.width < 60 || area.height < 18 {
        let msg = Paragraph::new(vec![
            Line::from("pokertui needs at least 60×18").style(Style::default().fg(GOLD)),
            Line::from("resize the terminal or use --plain").style(Style::default().fg(DIM)),
        ])
        .alignment(Alignment::Center);
        f.render_widget(msg, area);
        return;
    }

    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(8),
        Constraint::Length(3),
        Constraint::Length(5),
        Constraint::Length(1),
    ])
    .split(area);

    draw_header(f, chunks[0], app);
    draw_body(f, chunks[1], app);
    draw_action(f, chunks[2], app);
    draw_log(f, chunks[3], app);
    draw_footer(f, chunks[4], app);

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

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let room = if app.code.is_empty() {
        "—".to_string()
    } else {
        app.code.clone()
    };
    let status = if app.connected { "●" } else { "○" };
    let status_color = if app.connected { Color::Green } else { RED };
    let hand = app
        .view
        .as_ref()
        .map(|v| {
            format!(
                "hand #{} · {}/{}",
                v.hand_no.max(1),
                v.small_blind,
                v.big_blind
            )
        })
        .unwrap_or_default();
    let line = Line::from(vec![
        Span::styled(" ♠ POKER ", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
        Span::styled(
            format!(" {room} "),
            Style::default().fg(Color::Black).bg(GOLD).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {hand}"), Style::default().fg(TEXT)),
        Span::styled(
            format!("   {} {}", status, app.mode.label()),
            Style::default().fg(status_color),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
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
                        format!("connected · waiting for the host of {}{dots:<3}", app.code)
                    } else {
                        format!("connecting to the relay{dots:<3}")
                    },
                    Style::default().fg(GOLD),
                ),
            ]),
            Line::from(""),
            Line::from(app.broker.clone()).style(Style::default().fg(DIM)),
            Line::from(Span::styled("Esc goes back to the menu", Style::default().fg(Color::Rgb(70, 80, 75)))),
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

/// "blinds 25/50 · 5,000 stacks · 6-max" from what a client can see. Stacks
/// only mean the starting stack in the lobby, so they're left out mid-hand.
fn rules_line(view: &TableView) -> String {
    let blinds = format!("blinds {}/{}", chips(view.small_blind), chips(view.big_blind));
    if view.phase == Phase::Lobby {
        let stack = view.seats.iter().map(|s| s.stack).max().unwrap_or(0);
        format!("{blinds} · {} stacks · {}-max", chips(stack), view.seats.len())
    } else {
        format!("{blinds} · {}-max", view.seats.len())
    }
}

fn draw_lobby(f: &mut Frame, area: Rect, app: &App, view: &TableView) {
    let (art_w, art_h) = art::dimensions(art::LOBBY);
    let seated: Vec<&crate::game::SeatView> = view.seats.iter().filter(|s| !s.name.is_empty()).collect();
    let deal_hint = if app.mode == Mode::Host {
        if seated.len() >= 2 { "[Enter] deal the first hand" } else { "waiting for a second player" }
    } else {
        "waiting for the host to deal"
    };
    let art_fits = app.show_art && area.width >= art_w && area.height >= art_h;
    if art_fits {
        art::draw(area, f.buffer_mut(), art::LOBBY, Style::default().fg(Color::Rgb(60, 130, 100)));
    }

    // Room card: code, rules, who's here.
    let names: Vec<Span> = seated
        .iter()
        .enumerate()
        .flat_map(|(i, s)| {
            let me = app.my_seat == Some(s.seat);
            let mut v = vec![Span::styled(
                if me { format!("{} (you)", s.name) } else { s.name.clone() },
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
            Span::styled("ROOM  ", Style::default().fg(DIM)),
            Span::styled(format!("  {}  ", app.code), Style::default().fg(Color::Black).bg(code_bg).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(Span::styled("friends: poker → Join → type this code", Style::default().fg(DIM))),
        Line::from(""),
        Line::from(Span::styled(rules_line(view), Style::default().fg(GOLD))),
        Line::from(Span::styled(format!("{} of {} seats", seated.len(), view.seats.len()), Style::default().fg(DIM))),
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
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(GOLD))
        .title(Span::styled(" ♠ TABLE LOBBY ♥ ", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)))
        .style(Style::default().bg(Color::Black));
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center).block(block), rect);
}

fn draw_table(f: &mut Frame, area: Rect, app: &App, view: &TableView) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(40, 110, 80)))
        .style(Style::default().bg(FELT_DARK))
        .title(Span::styled(format!(" {} ", street_label(view)), Style::default().fg(DIM)))
        .title(Span::styled(format!(" {} ", rules_line(view)), Style::default().fg(Color::Rgb(70, 110, 90))).into_right_aligned_line());
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mid_y = inner.y + inner.height / 2;
    let pot_y = mid_y.saturating_sub(3);
    let center = (inner.x + inner.width / 2, pot_y);

    // Pot (counts up) or the winner banner once the hand is over.
    let banner = (view.phase == Phase::HandOver)
        .then(|| view.last_result.as_ref())
        .flatten()
        .and_then(|r| r.pots.first().map(|p| (p, r)))
        .map(|(p, _)| {
            let names = p.winners.iter().map(|w| app.seat_name(*w)).collect::<Vec<_>>().join(" & ");
            format!("★ {names} wins {} ★", chips(p.amount))
        });
    let (pot_text, pot_style) = match banner {
        Some(b) => {
            let glow = anim::pulse(app.tick);
            (b, Style::default().fg(Color::Rgb(255, (190.0 + 60.0 * glow) as u8, 90)).add_modifier(Modifier::BOLD))
        }
        None => (format!("POT  {}", chips(app.pot_shown())), Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
    };
    let pot_x = inner.x + inner.width.saturating_sub(pot_text.chars().count() as u16) / 2;
    f.buffer_mut().set_string(pot_x, pot_y, &pot_text, pot_style);

    // Board: five cards, new ones flip over one after another.
    let card_w = 7u16;
    let board_w = card_w * 5 + 4;
    let board_x = inner.x + inner.width.saturating_sub(board_w) / 2;
    let board_y = mid_y.saturating_sub(2);
    for i in 0usize..5 {
        let x = board_x + i as u16 * (card_w + 1);
        let buf = f.buffer_mut();
        match view.board.get(i) {
            Some(card) => {
                let flip = app.anim.board_at.filter(|_| i >= app.anim.board_from).map(|at| {
                    let delay = (i - app.anim.board_from) as u64 * app::FLIP_STAGGER_MS;
                    at.elapsed().as_millis() as i64 - delay as i64
                });
                match flip {
                    Some(ms) if ms < 0 => draw_card_placeholder(buf, x, board_y),
                    Some(ms) if (ms as u64) < app::FLIP_MS => {
                        let t = ms as f32 / app::FLIP_MS as f32;
                        if t < 0.35 {
                            draw_card_back(buf, x, board_y)
                        } else if t < 0.65 {
                            draw_card_edge(buf, x, board_y)
                        } else {
                            draw_card(buf, x, board_y, *card, false)
                        }
                    }
                    _ => draw_card(buf, x, board_y, *card, false),
                }
            }
            None => draw_card_placeholder(buf, x, board_y),
        }
    }

    // Seats, rotated so the local player sits at the bottom center.
    let occupied: Vec<usize> = view.seats.iter().filter(|s| !s.name.is_empty()).map(|s| s.seat).collect();
    if occupied.is_empty() {
        let text = Paragraph::new("empty table").style(Style::default().fg(DIM));
        f.render_widget(text.alignment(Alignment::Center), inner);
        return;
    }
    let anchor = app.my_seat.and_then(|me| occupied.iter().position(|s| *s == me)).unwrap_or(0);
    let n = occupied.len();
    let positions = seat_positions(n);
    let panel_w = (inner.width / 4).clamp(16, 24).min(inner.width);
    let panel_h = 3u16;
    let mut rects: Vec<(usize, Rect)> = Vec::new();
    for (i, seat) in occupied.iter().enumerate() {
        let rel = (i + n - anchor) % n;
        let (fx, fy) = positions[rel.min(positions.len() - 1)];
        rects.push((*seat, at(inner, fx, fy, panel_w, panel_h)));
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
                f.buffer_mut().set_string(x, y, "▓", Style::default().fg(Color::Rgb(200, 70, 70)).bg(FELT_DARK));
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
                        let glyph = ["●", "•", "·"][trail];
                        f.buffer_mut().set_string(x, y, glyph, Style::default().fg(GOLD).bg(FELT_DARK));
                    }
                }
            }
        }
    }

    // Your hole cards as real cards above your seat, when there's room.
    if let (Some(me), false) = (app.my_seat, app.my_cards.is_empty())
        && let Some((_, r)) = rects.iter().find(|(s, _)| *s == me)
        && r.y >= board_y + 3 + 3
    {
        let x = r.x + r.width / 2 - 7;
        let y = r.y - 3;
        let shown = landed(me);
        let dealing = deal_ms.is_some_and(|ms| ms < app::DEAL_MS);
        let flipping = deal_ms.is_some_and(|ms| (app::DEAL_MS..app::DEAL_MS + app::FLIP_MS / 2).contains(&ms));
        let folded = view.seats.get(me).is_some_and(|s| s.folded);
        for (i, card) in app.my_cards.iter().enumerate().take(shown) {
            let cx = x + i as u16 * 8;
            let buf = f.buffer_mut();
            if dealing {
                draw_card_back(buf, cx, y);
            } else if flipping {
                draw_card_edge(buf, cx, y);
            } else {
                draw_card(buf, cx, y, *card, folded);
            }
        }
    }

    // Showdown reveal goes in the center under the board.
    if let Some(result) = &view.last_result
        && view.phase == Phase::HandOver
        && result.reveals.len() <= 3
    {
        let mut lines = Vec::new();
        for r in &result.reveals {
            lines.push(Line::from(vec![
                Span::styled(format!("{} ", r.name), Style::default().fg(DIM)),
                Span::styled(r.cards.iter().map(|c| c.pretty()).collect::<Vec<_>>().join(" "), Style::default().fg(TEXT)),
                Span::styled(format!("  {}", r.hand), Style::default().fg(GOLD)),
            ]));
        }
        if lines.len() as u16 + board_y + 3 < inner.y + inner.height {
            f.render_widget(
                Paragraph::new(lines).alignment(Alignment::Center),
                Rect::new(inner.x, board_y + 3, inner.width, (inner.height - (board_y - inner.y) - 4).min(4)),
            );
        }
    }

    // Fireworks when you win a pot.
    if let (Some(at), true) = (app.anim.result_at, app.celebrate_until.is_some()) {
        if app.show_art {
            let elapsed = at.elapsed().as_secs_f32();
            let buf = f.buffer_mut();
            for (x, y, glyph, color) in anim::fireworks(view.hand_no as u64 * 7919, elapsed, inner.width, inner.height) {
                buf.set_string(inner.x + x, inner.y + y, glyph.to_string(), Style::default().fg(color).bg(FELT_DARK));
            }
            // Keep the pot/winner line readable over the sparks.
            buf.set_string(pot_x, pot_y, &pot_text, pot_style);
        }
        let text = " ★  YOU WIN  ★ ";
        let glow = anim::pulse(app.tick * 2);
        let style = Style::default()
            .fg(Color::Black)
            .bg(Color::Rgb(255, (180.0 + 70.0 * glow) as u8, 60))
            .add_modifier(Modifier::BOLD);
        let x = inner.x + inner.width.saturating_sub(text.chars().count() as u16) / 2;
        f.buffer_mut().set_string(x, pot_y.saturating_sub(1).max(inner.y), text, style);
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

fn draw_seat(buf: &mut Buffer, rect: Rect, app: &App, view: &TableView, seat: usize, cards_landed: usize) {
    let s = &view.seats[seat];
    let is_actor = view.actor == Some(seat);
    let is_me = app.my_seat == Some(seat);
    let fg = if !s.connected {
        DIM
    } else if is_actor {
        GOLD
    } else if is_me {
        YOU
    } else {
        TEXT
    };
    let bg = if is_actor { Color::Rgb(40, 50, 35) } else { FELT_DARK };
    let style = Style::default().fg(fg).bg(bg);

    for y in rect.y..rect.y + rect.height {
        buf.set_string(rect.x, y, " ".repeat(rect.width as usize), style);
    }

    let tags = seat_tags(view, seat, app);
    let marker = if is_actor { "▶ " } else { "  " };
    let name = if is_me {
        format!("{} (you)", truncate(&s.name, 8))
    } else {
        truncate(&s.name, 12)
    };
    let line1 = format!("{marker}{name}");
    buf.set_string(rect.x, rect.y, pad(&line1, rect.width), style);
    // Face-down cards for opponents still in the hand.
    if !is_me && !s.folded && s.hole_count > 0 && view.phase == Phase::Betting && rect.width >= 6 {
        let backs = cards_landed.min(s.hole_count);
        for i in 0..backs as u16 {
            buf.set_string(rect.x + rect.width - 5 + i * 2, rect.y, "▓", Style::default().fg(Color::Rgb(200, 70, 70)).bg(bg));
        }
    }
    let line2 = format!(
        "  {:<7} {}",
        chips(s.stack),
        if s.bet > 0 {
            format!("● {}", chips(s.bet))
        } else {
            String::new()
        }
    );
    buf.set_string(
        rect.x,
        rect.y + 1,
        pad(&line2, rect.width),
        Style::default().fg(if s.folded { DIM } else { TEXT }).bg(bg),
    );
    let status = if !s.connected && !s.name.is_empty() {
        "  away".to_string()
    } else if s.folded {
        "  folded".to_string()
    } else if s.all_in {
        "  all-in".to_string()
    } else if is_actor {
        "  to act".to_string()
    } else {
        format!("  {tags}")
    };
    buf.set_string(
        rect.x,
        rect.y + 2,
        pad(&status, rect.width),
        Style::default().fg(if s.all_in { GOLD } else { DIM }).bg(bg),
    );
}

fn seat_tags(view: &TableView, seat: usize, app: &App) -> String {
    let mut tags = Vec::new();
    if seat == view.button {
        tags.push("BTN".to_string());
    }
    if let Some(s) = view.seats.get(seat) {
        if let Some(mv) = &s.last_move
            && !mv.is_empty()
            && view.phase == Phase::Betting
        {
            tags.push(mv.clone());
        }
        if s.hole_count == 0 && view.phase != Phase::Lobby && !s.folded {
            tags.push("sitting".to_string());
        }
    }
    let _ = app;
    tags.join(" · ")
}

fn draw_card(buf: &mut Buffer, x: u16, y: u16, card: Card, dim: bool) {
    let border = Style::default().fg(if dim { DIM } else { Color::Rgb(190, 195, 190) });
    let face = if card.is_red() { RED } else { TEXT };
    buf.set_string(x, y, "┌─────┐", border);
    buf.set_string(x, y + 1, "│     │", border);
    buf.set_string(x + 2, y + 1, &card.rank_char().to_string(), Style::default().fg(face).add_modifier(Modifier::BOLD));
    buf.set_string(x + 3, y + 1, &card.suit_symbol().to_string(), Style::default().fg(face));
    buf.set_string(x, y + 2, "└─────┘", border);
}

fn draw_card_back(buf: &mut Buffer, x: u16, y: u16) {
    let border = Style::default().fg(Color::Rgb(190, 195, 190));
    let back = Style::default().fg(Color::Rgb(200, 70, 70));
    buf.set_string(x, y, "┌─────┐", border);
    buf.set_string(x, y + 1, "│", border);
    buf.set_string(x + 1, y + 1, "▓▒▓▒▓", back);
    buf.set_string(x + 6, y + 1, "│", border);
    buf.set_string(x, y + 2, "└─────┘", border);
}

/// A card seen edge-on, halfway through a flip.
fn draw_card_edge(buf: &mut Buffer, x: u16, y: u16) {
    let style = Style::default().fg(Color::Rgb(220, 225, 220));
    for dy in 0..3 {
        buf.set_string(x + 3, y + dy, "┃", style);
    }
}

fn draw_card_placeholder(buf: &mut Buffer, x: u16, y: u16) {
    let style = Style::default().fg(Color::Rgb(45, 80, 65));
    buf.set_string(x, y, "┌─────┐", style);
    buf.set_string(x, y + 1, "│  ·  │", style);
    buf.set_string(x, y + 2, "└─────┘", style);
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

fn draw_action(f: &mut Frame, area: Rect, app: &App) {
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Length(1)]).split(area);
    let view = app.view.as_ref();

    // Row 1: your hand and stack.
    let mut hand_line = vec![Span::styled(" your hand ", Style::default().fg(DIM))];
    if app.my_cards.is_empty() {
        hand_line.push(Span::styled("—", Style::default().fg(DIM)));
    } else {
        for c in &app.my_cards {
            hand_line.push(Span::styled(
                format!(" {} ", c.pretty()),
                Style::default().fg(if c.is_red() { RED } else { Color::Black }).bg(Color::Rgb(235, 235, 230)).add_modifier(Modifier::BOLD),
            ));
            hand_line.push(Span::raw(" "));
        }
    }
    if let Some(s) = app.my_seat.and_then(|me| view.and_then(|v| v.seats.get(me))) {
        hand_line.push(Span::styled(format!("  stack {}", chips(s.stack)), Style::default().fg(GOLD)));
    }
    f.render_widget(Paragraph::new(Line::from(hand_line)), rows[0]);

    // Row 2: action buttons on your turn, otherwise what's happening.
    let line = if let Some(legal) = app.my_legal() {
        let glow = anim::pulse(app.tick * 2);
        let mut spans = vec![Span::styled(
            " YOUR TURN ",
            Style::default().fg(Color::Black).bg(Color::Rgb(255, (170.0 + 80.0 * glow) as u8, 60)).add_modifier(Modifier::BOLD),
        )];
        spans.push(Span::raw("  "));
        let middle = if legal.can_check { "Check".to_string() } else { format!("Call {}", chips(legal.to_call)) };
        let allin = view.and_then(|v| app.my_seat.and_then(|me| v.seats.get(me))).map(|s| s.stack + s.bet).unwrap_or(0);
        let buttons = [
            ("1", "Fold".to_string(), true),
            ("2", middle, legal.can_check || legal.can_call),
            ("3", "Raise…".to_string(), legal.can_raise),
            ("4", format!("All-in {}", chips(allin)), legal.can_allin),
        ];
        for (key, label, lit) in buttons {
            let (ks, ls) = if lit {
                (
                    Style::default().fg(GOLD).bg(Color::Rgb(40, 40, 30)).add_modifier(Modifier::BOLD),
                    Style::default().fg(Color::Black).bg(Color::Rgb(220, 220, 210)).add_modifier(Modifier::BOLD),
                )
            } else {
                (Style::default().fg(Color::Rgb(70, 75, 72)), Style::default().fg(Color::Rgb(70, 75, 72)))
            };
            spans.push(Span::styled(format!(" {key} "), ks));
            spans.push(Span::styled(format!(" {label} "), ls));
            spans.push(Span::raw("  "));
        }
        Line::from(spans)
    } else if view.is_some_and(|v| v.phase == Phase::HandOver) {
        Line::from(Span::styled(
            if app.mode == Mode::Host { " hand over · [Enter] deal the next hand" } else { " hand over · waiting for the host to deal" },
            Style::default().fg(if app.mode == Mode::Host { Color::Rgb(150, 230, 150) } else { DIM }),
        ))
    } else {
        let who = view.and_then(|v| v.actor).map(|a| app.seat_name(a)).unwrap_or_default();
        let dots = ".".repeat((app.tick / 6 % 4) as usize);
        Line::from(Span::styled(
            if who.is_empty() { " waiting…".to_string() } else { format!(" {who} is thinking{dots}") },
            Style::default().fg(DIM),
        ))
    };
    f.render_widget(Paragraph::new(line), rows[1]);

    // Row 3: chat box, or the turn clock.
    if app.chat_mode {
        let cursor = if app.tick / 10 % 2 == 0 { "█" } else { " " };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" chat ▸ ", Style::default().fg(Color::Black).bg(Color::Rgb(120, 200, 220))),
                Span::raw(format!(" {}{cursor}", app.input)),
            ])),
            rows[2],
        );
    } else if let (Some(frac), Some(ms)) = (app.timer_fraction(), app.deadline_left()) {
        let who = view.and_then(|v| v.actor).map(|a| if app.my_seat == Some(a) { "you".to_string() } else { app.seat_name(a) });
        let label = format!(" ⏱ {:>3}s {} ", ms.div_ceil(1000), who.unwrap_or_default());
        let width = rows[2].width.saturating_sub(label.chars().count() as u16 + 2) as usize;
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
                Span::styled("━".repeat(filled), Style::default().fg(color)),
                Span::styled("─".repeat(width - filled), Style::default().fg(Color::Rgb(45, 55, 50))),
            ])),
            rows[2],
        );
    }
}

fn draw_log(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().borders(Borders::TOP).border_style(Style::default().fg(Color::Rgb(40, 70, 60)));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let capacity = inner.height as usize;
    let lines: Vec<Line> = app
        .log
        .iter()
        .rev()
        .take(capacity)
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
            Line::from(Span::styled(entry.text.clone(), Style::default().fg(color)))
        })
        .collect();
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let hints = if app.chat_mode {
        "Enter send · Esc cancel"
    } else if app.mode == Mode::Spectator {
        "watching · t chat · ? help · Esc leave"
    } else if app.mode == Mode::Host {
        "1-4 act · Enter deal · t chat · ? help · Esc leave (closes the table)"
    } else {
        "1 fold · 2 check/call · 3 raise · 4 all-in · t chat · ? help · Esc leave"
    };
    f.render_widget(
        Paragraph::new(Span::styled(format!(" {hints}"), Style::default().fg(Color::Rgb(90, 100, 95)))),
        area,
    );
}

fn overlay(f: &mut Frame, area: Rect, w: u16, title: &str, color: Color, lines: Vec<Line>) {
    let rect = centered(area, w, lines.len() as u16 + 2);
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(color))
        .title(Span::styled(format!(" {title} "), Style::default().fg(color).add_modifier(Modifier::BOLD)))
        .style(Style::default().bg(Color::Black));
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center).block(block), rect);
}

fn draw_raise(f: &mut Frame, area: Rect, app: &App) {
    let (Some(picker), Some(p)) = (&app.raise, app.raise_presets()) else { return };
    let (min, max) = (p[0].1, p[3].1);
    let amount = picker.amount;
    let cursor = if picker.typed.is_some() && app.tick / 10 % 2 == 0 { "█" } else { " " };
    let valid = (min..=max).contains(&amount);
    // Slider between min and max.
    let width = 30usize;
    let pos = if max > min { ((amount.clamp(min, max) - min) as f32 / (max - min) as f32 * width as f32) as usize } else { width };
    let slider = format!("{}●{}", "━".repeat(pos), "─".repeat(width - pos.min(width)));
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
            Span::styled("◂  ", Style::default().fg(DIM)),
            Span::styled(
                format!(" {}{cursor}", chips(amount)),
                Style::default().fg(if valid { Color::Black } else { Color::White }).bg(if valid { GOLD } else { RED }).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  ▸", Style::default().fg(DIM)),
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
            if valid { "←→ ± big blind · type an amount · Enter raise · Esc cancel".to_string() } else { format!("must be between {} and {}", chips(min), chips(max)) },
            Style::default().fg(if valid { DIM } else { RED }),
        )),
    ];
    overlay(f, area, 70.min(area.width), "RAISE TO", GOLD, lines);
}

fn draw_confirm_leave(f: &mut Frame, area: Rect, app: &App) {
    let warn = match app.mode {
        Mode::Host => "you're the host: leaving closes the table for everyone",
        Mode::Client => "your seat will be given up",
        Mode::Spectator => "stop watching this table",
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
            Span::styled(format!("  {d:<34}"), Style::default().fg(TEXT)),
        ])
    };
    let lines = vec![
        Line::from(""),
        row("1  f", "fold"),
        row("2  c x", "check, or call the bet"),
        row("3  r", "raise… (M min · H ½ pot · P pot · A max)"),
        row("4  a", "all-in"),
        row("Enter", "deal the next hand (host)"),
        row("t  /", "chat"),
        row("Esc  q", "leave the table"),
        Line::from(""),
        Line::from(Span::styled("the host deals; every deck is committed before the", Style::default().fg(DIM))),
        Line::from(Span::styled("deal and revealed after it (poker verify <file>)", Style::default().fg(DIM))),
        Line::from(Span::styled("any key closes this", Style::default().fg(Color::Rgb(70, 80, 75)))),
    ];
    overlay(f, area, 56.min(area.width), "KEYS", GOLD, lines);
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

fn pad(s: &str, width: u16) -> String {
    let mut out = truncate(s, width as usize);
    let len = out.chars().count() as u16;
    if len < width {
        out.push_str(&" ".repeat((width - len) as usize));
    }
    out
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
        for (w, h) in [(80, 24), (120, 40), (60, 18)] {
            let (mut app, _rx) = app_on_turn();
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| super::draw(f, &app)).unwrap();
            let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
            assert!(text.contains("YOUR TURN"), "{w}x{h}: no turn banner");
            assert!(text.contains("Fold"), "{w}x{h}: no buttons");
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
