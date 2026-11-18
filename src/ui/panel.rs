//! Bottom of the screen: your hand and action buttons, the log, and the footer hint.

use super::card::{BIG, CardView, Face, SMALL};
use super::theme::*;
use crate::app::{self, App, LogKind, LogLine, Mode};
use crate::game::{Card, Phase, describe_hand};
use crate::menu::chips;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{LineGauge, Paragraph};

/// Bottom panel: your two cards on the left; on the right a headline, the big
/// buttons (your turn) or what's going on, and the turn clock / chat line.
pub fn draw_panel(f: &mut Frame, area: Rect, app: &App) {
    let view = app.view.as_ref();
    let legal = app.my_legal();
    let me = app.my_seat.and_then(|m| view.and_then(|v| v.seats.get(m)));
    let gold = TURN_GOLD;

    let block = ascii_block(if legal.is_some() { gold } else { PANEL_EDGE });
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
            Some(_) if dealing => CardView::new(Face::Back, sz).draw(buf, x, inner.y),
            Some(_) if flipping => CardView::new(Face::Edge, sz).draw(buf, x, inner.y),
            Some(c) => CardView::new(Face::Up(*c), sz).dim(folded).draw(buf, x, inner.y),
            None => CardView::new(Face::Empty, sz).draw(buf, x, inner.y),
        }
    }

    let [_, _, text_area] = Layout::horizontal([Constraint::Length(1), Constraint::Length(2 * sz.w + 4), Constraint::Min(0)]).areas(inner);
    let rw = text_area.width;
    // The 5 text rows sit vertically centred next to the cards.
    let [_, rows_area] = Layout::vertical([Constraint::Length(text_area.height.saturating_sub(5) / 2), Constraint::Min(0)]).areas(text_area);
    let rows: [Rect; 5] = Layout::vertical([Constraint::Length(1); 5]).areas(rows_area);
    let row = |dy: u16| rows[dy as usize];

    // What you're holding, in plain words (always shown once you have cards).
    if !folded && app.my_cards.len() == 2 {
        let board: &[Card] = view.map(|v| v.board.as_slice()).unwrap_or(&[]);
        let hand = describe_hand(&app.my_cards, board);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" You have  ", Style::default().fg(DIM)),
                Span::styled(hand, Style::default().fg(TEXT).add_modifier(Modifier::BOLD)),
            ])),
            row(1),
        );
    }

    if let Some(legal) = legal {
        let mut head = vec![Span::styled(" Your turn", Style::default().fg(gold))];
        if legal.to_call > 0 {
            head.push(Span::styled(format!("  ·  {} to call", chips(legal.to_call)), Style::default().fg(DIM)));
        }
        f.render_widget(Paragraph::new(Line::from(head)), row(0));

        let allin = me.map(|s| s.stack + s.bet).unwrap_or(0);
        let middle = if legal.can_check { "Check".to_string() } else { format!("Call {}", chips(legal.to_call)) };
        let raise = if view.is_some_and(|v| v.current_bet == 0) { "Bet" } else { "Raise" };
        let mut options = [
            ("f", "Fold".to_string(), true),
            ("c", middle, legal.can_check || legal.can_call),
            ("r", raise.to_string(), legal.can_raise),
            ("a", format!("All-in {}", chips(allin)), legal.can_allin),
        ];
        let width = |o: &[(&str, String, bool); 4]| o.iter().map(|(_, l, _)| l.chars().count() + 7).sum::<usize>() + 1;
        if width(&options) > rw as usize {
            options[3].1 = "All-in".into();
        }
        // One quiet line: gold keys, plain labels, unavailable choices dimmed.
        let mut spans = vec![Span::raw(" ")];
        for (i, (key, label, on)) in options.into_iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw("   "));
            }
            let (key_color, label_color) = if on { (gold, TEXT) } else { (DIM, DIM) };
            spans.push(Span::styled(format!("[{key}]"), Style::default().fg(key_color)));
            spans.push(Span::styled(format!(" {label}"), Style::default().fg(label_color)));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), row(3));
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
        if app.can_show() {
            let line = match app.show_choice {
                None => Line::from(vec![
                    Span::styled(" [s]", Style::default().fg(gold)),
                    Span::styled(" Show cards", Style::default().fg(TEXT)),
                    Span::raw("   "),
                    Span::styled("[m]", Style::default().fg(gold)),
                    Span::styled(" Muck", Style::default().fg(TEXT)),
                ]),
                Some(true) => Line::from(Span::styled(" you showed your cards", Style::default().fg(DIM))),
                Some(false) => Line::from(Span::styled(" you mucked", Style::default().fg(DIM))),
            };
            f.render_widget(Paragraph::new(line), row(3));
        }
    }

    // Last row: chat box, or the turn clock.
    if app.chat_mode {
        let cursor = if app.tick / 10 % 2 == 0 { "_" } else { " " };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" chat > ", Style::default().fg(Color::Black).bg(CHAT)),
                Span::raw(format!(" {}{cursor}", app.input)),
            ])),
            row(4),
        );
    } else if let (Some(frac), Some(ms)) = (app.timer_fraction(), app.deadline_left()) {
        let label = format!("{:>3}s [", ms.div_ceil(1000));
        let color = if frac > 0.5 {
            Color::Rgb(110, 210, 130)
        } else if frac > 0.2 {
            Color::Rgb(240, 190, 80)
        } else {
            Color::Rgb(240, 90, 80)
        };
        let [text, bar, end] = Layout::horizontal([Constraint::Length(label.chars().count() as u16), Constraint::Min(0), Constraint::Length(2)]).areas(row(4));
        f.render_widget(Paragraph::new(Span::styled(label, Style::default().fg(color))), text);
        f.render_widget(
            LineGauge::default()
                .ratio(frac as f64)
                .label("")
                .filled_symbol("=")
                .unfilled_symbol("-")
                .filled_style(Style::default().fg(color))
                .unfilled_style(Style::default().fg(Color::Rgb(45, 55, 50))),
            bar,
        );
        f.render_widget(Paragraph::new(Span::styled("]", Style::default().fg(color))), end);
    }
}

/// Colour and weight for a log line, by what it says.
fn log_style(entry: &LogLine) -> Style {
    let t = entry.text.as_str();
    let plain = |c: Color| Style::default().fg(c);
    let bold = |c: Color| Style::default().fg(c).add_modifier(Modifier::BOLD);
    match entry.kind {
        LogKind::Chat => plain(CHAT),
        LogKind::Error => bold(RED),
        LogKind::Info => plain(DIM),
        LogKind::Action if t.contains("all-in") => bold(RED),
        LogKind::Action if t.contains(" wins ") || t.contains(" win ") => bold(Color::Rgb(120, 230, 140)),
        LogKind::Action if t.starts_with("showdown") => plain(Color::Rgb(190, 150, 235)),
        LogKind::Action if t.contains(" raises") || t.contains(" bets") => bold(GOLD),
        LogKind::Action if t.contains(" calls") => plain(CALL_C),
        LogKind::Action if t.contains(" checks") => plain(Color::Rgb(130, 170, 200)),
        LogKind::Action if t.contains(" folds") => plain(Color::Rgb(135, 135, 145)),
        LogKind::Action if t.starts_with("Flop") || t.starts_with("Turn") || t.starts_with("River") => plain(Color::Rgb(110, 170, 235)),
        LogKind::Action if t.starts_with("hand #") => plain(DIM),
        LogKind::Action => plain(TEXT),
    }
}

pub fn draw_log(f: &mut Frame, area: Rect, app: &App) {
    let inner = if area.height >= 3 {
        let block = ascii_block(BOX_EDGE)
            .title(Span::styled(" Log ", Style::default().fg(DIM)));
        let inner = block.inner(area);
        f.render_widget(block, area);
        inner
    } else {
        area
    };
    let lines: Vec<Line> = app
        .log
        .iter()
        .rev()
        .take(inner.height as usize)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|entry| Line::from(Span::styled(format!(" {}", entry.text), log_style(entry))))
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

pub fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let mut spans = vec![];
    if !app.connected {
        spans.push(badge("offline", RED));
    }
    let hint = if app.chat_mode {
        " Enter send · Esc cancel"
    } else if app.mode == Mode::Host {
        " [+] add bot   [k] remove   [?] help"
    } else {
        " [?] help"
    };
    spans.push(Span::styled(hint, Style::default().fg(DIM)));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

