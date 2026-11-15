//! Ratatui rendering: ASCII table, seats, cards, action bar and log.

use crate::app::{App, LogKind, Mode};
use crate::art;
use crate::game::{Card, Phase, Street, TableView};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};

const FELT: Color = Color::Rgb(10, 60, 40);
const FELT_DARK: Color = Color::Rgb(7, 45, 30);
const GOLD: Color = Color::Rgb(230, 190, 90);
const DIM: Color = Color::Rgb(120, 130, 125);
const YOU: Color = Color::Cyan;
const RED: Color = Color::Rgb(230, 90, 90);
const TEXT: Color = Color::Rgb(215, 220, 215);

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
        let text = Paragraph::new(vec![
            Line::from("connecting to the relay…").style(Style::default().fg(GOLD)),
            Line::from(app.broker.clone()).style(Style::default().fg(DIM)),
        ])
        .alignment(Alignment::Center);
        f.render_widget(text, centered(area, 60, 3));
        return;
    };

    if view.phase == Phase::Lobby {
        draw_lobby(f, area, app, view);
    } else {
        draw_table(f, area, app, view);
    }
}

fn draw_lobby(f: &mut Frame, area: Rect, app: &App, view: &TableView) {
    let (art_w, art_h) = art::dimensions(art::LOBBY);
    let seats = view.seats.iter().filter(|s| !s.name.is_empty()).count();
    if app.show_art && area.width >= art_w && area.height >= art_h + 2 {
        art::draw(
            Rect::new(area.x, area.y, area.width, area.height - 1),
            f.buffer_mut(),
            art::LOBBY,
            Style::default().fg(Color::Rgb(120, 200, 160)),
        );
        let hint = Line::from(vec![
            Span::styled(
                format!("  room {}  ", app.code),
                Style::default().fg(Color::Black).bg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    " {} of {} seats · {} ",
                    seats,
                    view.seats.len(),
                    if app.mode == Mode::Host {
                        "press s to deal"
                    } else {
                        "waiting for the host"
                    }
                ),
                Style::default().fg(TEXT),
            ),
        ]);
        f.render_widget(
            Paragraph::new(hint).alignment(Alignment::Center),
            Rect::new(area.x, area.y + area.height - 1, area.width, 1),
        );
        return;
    }

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "♠  P O K E R  ♥",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!("room  {}", app.code),
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!("{seats} / {} seats", view.seats.len()),
            Style::default().fg(DIM),
        )),
        Line::from(""),
    ];
    for s in view.seats.iter().filter(|s| !s.name.is_empty()) {
        let tags = seat_tags(view, s.seat, app);
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<12}", truncate(&s.name, 12)), Style::default().fg(TEXT)),
            Span::styled(format!("{:>7}", s.stack), Style::default().fg(GOLD)),
            Span::styled(format!("  {tags}"), Style::default().fg(DIM)),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        if app.mode == Mode::Host {
            "press s to deal the first hand"
        } else {
            "waiting for the host to deal"
        },
        Style::default().fg(DIM),
    )));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(FELT))
        .style(Style::default().bg(Color::Black));
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center).block(block), area);
}

fn draw_table(f: &mut Frame, area: Rect, app: &App, view: &TableView) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(40, 110, 80)))
        .style(Style::default().bg(FELT_DARK))
        .title(Span::styled(
            format!(" {} ", street_label(view)),
            Style::default().fg(DIM),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let buf = f.buffer_mut();
    let mid_y = inner.y + inner.height / 2;

    // Pot.
    let pot = format!("POT  {}", view.pot);
    let pot_x = inner.x + inner.width.saturating_sub(pot.len() as u16) / 2;
    buf.set_string(pot_x, mid_y.saturating_sub(3), pot, Style::default().fg(GOLD).add_modifier(Modifier::BOLD));

    // Board: five compact cards, centered.
    let card_w = 7u16;
    let board_w = card_w * 5 + 4;
    let board_x = inner.x + inner.width.saturating_sub(board_w) / 2;
    let board_y = mid_y.saturating_sub(2);
    for i in 0usize..5 {
        let x = board_x + i as u16 * (card_w + 1);
        match view.board.get(i) {
            Some(card) => draw_card(buf, x, board_y, *card, false),
            None => draw_card_placeholder(buf, x, board_y),
        }
    }

    // Seats, rotated so the local player sits at the bottom center.
    let occupied: Vec<usize> = view
        .seats
        .iter()
        .filter(|s| !s.name.is_empty())
        .map(|s| s.seat)
        .collect();
    if occupied.is_empty() {
        let text = Paragraph::new("empty table").style(Style::default().fg(DIM));
        f.render_widget(text.alignment(Alignment::Center), inner);
        return;
    }
    let anchor = app
        .my_seat
        .and_then(|me| occupied.iter().position(|s| *s == me))
        .unwrap_or(0);
    let n = occupied.len();
    let positions = seat_positions(n);
    let panel_w = (inner.width / 4).clamp(14, 22).min(inner.width);
    let panel_h = 3u16;

    for (rel, seat) in occupied.iter().enumerate() {
        let rel = (rel + n - anchor) % n;
        let (fx, fy) = positions[rel.min(positions.len() - 1)];
        let rect = at(inner, fx, fy, panel_w, panel_h);
        draw_seat(f.buffer_mut(), rect, app, view, *seat);
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
                Span::styled(
                    r.cards.iter().map(|c| c.pretty()).collect::<Vec<_>>().join(" "),
                    Style::default().fg(TEXT),
                ),
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

    // Celebration overlay for a pot won by the local player.
    if app.celebrate_until.is_some() && inner.width >= 80 && inner.height >= 16 {
        f.render_widget(Clear, inner);
        art::draw(
            inner,
            f.buffer_mut(),
            art::WIN,
            Style::default().fg(Color::Rgb(240, 200, 90)),
        );
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

fn draw_seat(buf: &mut Buffer, rect: Rect, app: &App, view: &TableView, seat: usize) {
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
    let line2 = format!(
        "  {:<6} {}",
        s.stack,
        if s.bet > 0 {
            format!("bet {}", s.bet)
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
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);
    let view = app.view.as_ref();
    let my_cards = if app.my_cards.is_empty() {
        String::new()
    } else {
        app.my_cards.iter().map(|c| c.pretty()).collect::<Vec<_>>().join(" ")
    };
    let mut hand_line = vec![Span::styled(" your hand ", Style::default().fg(DIM))];
    if my_cards.is_empty() {
        hand_line.push(Span::styled("—", Style::default().fg(DIM)));
    } else {
        for c in &app.my_cards {
            hand_line.push(Span::styled(
                format!(" {} ", c.pretty()),
                Style::default()
                    .fg(if c.is_red() { RED } else { TEXT })
                    .add_modifier(Modifier::BOLD),
            ));
        }
    }
    if let Some(s) = app.my_seat.and_then(|me| view.and_then(|v| v.seats.get(me))) {
        hand_line.push(Span::styled(
            format!("   stack {}  ", s.stack),
            Style::default().fg(GOLD),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(hand_line)), rows[0]);

    let prompt = if let Some(legal) = app.my_legal() {
        let mut parts = vec![format!(" YOUR TURN ")];
        if legal.to_call > 0 {
            parts.push(format!("call {} ", legal.to_call));
        }
        if legal.can_raise {
            parts.push(format!("· min raise {} ", legal.min_raise_to));
        }
        if let Some(ms) = app.deadline_left() {
            parts.push(format!("· {}s ", ms / 1000));
        }
        parts.push("  [f]old [c]all [x]heck [r]aise [a]ll-in ".into());
        Line::from(Span::styled(
            parts.concat(),
            Style::default().fg(Color::Black).bg(GOLD).add_modifier(Modifier::BOLD),
        ))
    } else if view.is_some_and(|v| v.phase == Phase::HandOver) {
        Line::from(Span::styled(
            if app.mode == Mode::Host {
                " hand over · press s to deal the next hand"
            } else {
                " hand over · waiting for the host"
            },
            Style::default().fg(DIM),
        ))
    } else {
        let who = view
            .and_then(|v| v.actor)
            .map(|a| app.seat_name(a))
            .unwrap_or_default();
        Line::from(Span::styled(
            if who.is_empty() {
                " waiting…".to_string()
            } else {
                format!(" {who} is thinking…")
            },
            Style::default().fg(DIM),
        ))
    };
    f.render_widget(Paragraph::new(prompt), rows[1]);

    let (prefix, text) = if app.chat_mode {
        (" chat> ", app.input.clone())
    } else {
        ("> ", app.input.clone())
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(prefix, Style::default().fg(GOLD)),
            Span::raw(text),
        ])),
        rows[2],
    );
}

fn draw_log(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::Rgb(40, 70, 60)));
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
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: true }),
        inner,
    );
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let hints = if app.chat_mode {
        "enter send · esc cancel"
    } else {
        "f fold · c call · x check · r raise · a all-in · / chat · ? help · ^C quit"
    };
    let line = Line::from(Span::styled(
        format!(" {hints}"),
        Style::default().fg(Color::Rgb(90, 100, 95)),
    ));
    f.render_widget(Paragraph::new(line), area);
}

fn draw_help(f: &mut Frame, area: Rect) {
    let rect = centered(area, 52, 14);
    f.render_widget(Clear, rect);
    let lines = vec![
        Line::from(Span::styled(" pokertui help ", Style::default().fg(GOLD).add_modifier(Modifier::BOLD))),
        Line::from(""),
        Line::from(" f / fold      fold"),
        Line::from(" c / call      call the current bet"),
        Line::from(" x / check     check when there is no bet"),
        Line::from(" r <amount>    raise to <amount>"),
        Line::from(" a / all-in    shove"),
        Line::from(" /text         chat"),
        Line::from(" :q            quit"),
        Line::from(""),
        Line::from(Span::styled(" trust: the host deals; the deck is committed", Style::default().fg(DIM))),
        Line::from(Span::styled(" and revealed each hand (poker verify <file>)", Style::default().fg(DIM))),
        Line::from(Span::styled(" any key closes this", Style::default().fg(DIM))),
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(GOLD))
        .style(Style::default().bg(Color::Black));
    f.render_widget(Paragraph::new(lines).block(block), rect);
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

fn centered(area: Rect, w: u16, h: u16) -> Rect {
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
