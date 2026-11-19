//! Popups: raise picker, confirmations, help, bot picker and the toast.

use super::theme::*;
use crate::app::{App, Mode};
use crate::menu::chips;
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

pub fn draw_bot_pick(f: &mut Frame, area: Rect) {
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled("Add a bot. Pick its level:", bold(TEXT))),
        Line::from(""),
        Line::from(vec![
            Span::raw(" [e]"), Span::styled(" Easy", Style::default().fg(TEXT)), Span::raw("   [m]"),
            Span::styled(" Medium", Style::default().fg(TEXT)), Span::raw("   [h]"),
            Span::styled(" Hard", Style::default().fg(TEXT)), Span::raw("   [g]"),
            Span::styled(" GTO", Style::default().fg(TEXT)), Span::styled("     Esc cancel", Style::default().fg(DIM)),
        ]),
        Line::from(""),
    ];
    overlay(f, area, 60.min(area.width), "ADD BOT", GOLD, lines);
}

/// A centred, cleared ASCII box with a title and centred text lines.
fn overlay(f: &mut Frame, area: Rect, w: u16, title: &str, color: Color, lines: Vec<Line>) {
    let rect = popup(area, w, lines.len() as u16 + 2);
    f.render_widget(Clear, rect);
    let block = ascii_block(color)
        .title(Span::styled(format!(" {title} "), bold(color)))
        .style(Style::default().bg(Color::Black));
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center).block(block), rect);
}

pub fn draw_raise(f: &mut Frame, area: Rect, app: &App) {
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
            if valid { "type an amount · ←→ adjust · Enter · Esc".to_string() } else { format!("must be between {} and {}", chips(min), chips(max)) },
            Style::default().fg(if valid { DIM } else { RED }),
        )),
    ];
    overlay(f, area, 70.min(area.width), "RAISE TO", GOLD, lines);
}

pub fn draw_confirm_leave(f: &mut Frame, area: Rect, app: &App) {
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
            badge("Y", RED),
            Span::styled(" leave     ", Style::default().fg(TEXT)),
            badge("N", GOLD),
            Span::styled(" stay", Style::default().fg(TEXT)),
        ]),
        Line::from(""),
    ];
    overlay(f, area, 60.min(area.width), "LEAVE", RED, lines);
}

pub fn draw_confirm_move(f: &mut Frame, area: Rect, app: &App, mv: &crate::game::Move) {
    let bold = Style::default().fg(TEXT).add_modifier(Modifier::BOLD);
    let (title, head, warn) = match mv {
        crate::game::Move::AllIn => {
            let me = app.my_seat.and_then(|m| app.view.as_ref().and_then(|v| v.seats.get(m)));
            let amount = me.map(|s| s.stack + s.bet).unwrap_or(0);
            ("ALL-IN", format!("Go all-in for {}?", chips(amount)), String::new())
        }
        _ if app.my_legal().is_some_and(|l| l.can_check) => {
            ("FOLD", "Fold?".to_string(), "You can check for free. Folding gives up the hand for nothing.".to_string())
        }
        _ => ("FOLD", "Fold this hand?".to_string(), String::new()),
    };
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(head, bold)),
        Line::from(Span::styled(warn, Style::default().fg(RED))),
        Line::from(""),
        Line::from(vec![
            badge("Y", RED),
            Span::styled(" yes     ", Style::default().fg(TEXT)),
            badge("N", GOLD),
            Span::styled(" cancel", Style::default().fg(TEXT)),
        ]),
        Line::from(""),
    ];
    overlay(f, area, 66.min(area.width), title, RED, lines);
}

pub fn draw_help(f: &mut Frame, area: Rect) {
    let row = |k: &str, d: &str| {
        Line::from(vec![
            Span::styled(format!(" {k:^9} "), Style::default().fg(Color::Black).bg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled(format!("  {d:<14}"), Style::default().fg(TEXT)),
        ])
    };
    let lines = vec![
        Line::from(""),
        row("f", "fold"),
        row("c  x", "check / call"),
        row("r  0-9", "raise / type a bet"),
        row("a", "all-in"),
        row("Enter", "deal (host)"),
        row("s  m", "show / muck after a hand"),
        row("+  e m h", "add a bot, pick level"),
        row("k  K", "kick / ban (host)"),
        row("y  n", "accept / decline joiner"),
        row("i", "sit back in"),
        row("t  /", "chat"),
        row("Esc  q", "leave"),
        Line::from(""),
    ];
    overlay(f, area, 40.min(area.width), "KEYS", GOLD, lines);
}

pub fn draw_notice(f: &mut Frame, area: Rect, text: &str) {
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
