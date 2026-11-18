//! Shared look: palette, ASCII border sets and the small helpers every screen uses.

use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders};

/// Box-drawing in the same ASCII style as the cards.
pub const ASCII: border::Set = border::Set {
    top_left: ".",
    top_right: ".",
    bottom_left: "`",
    bottom_right: "'",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};
/// Whoever is acting.
pub const ASCII_HOT: border::Set = border::Set {
    top_left: "#",
    top_right: "#",
    bottom_left: "#",
    bottom_right: "#",
    vertical_left: "#",
    vertical_right: "#",
    horizontal_top: "=",
    horizontal_bottom: "=",
};

pub const FELT_DARK: Color = Color::Rgb(7, 45, 30);
pub const GOLD: Color = Color::Rgb(230, 190, 90);
pub const DIM: Color = Color::Rgb(165, 175, 170);
pub const YOU: Color = Color::Cyan;
pub const RED: Color = Color::Rgb(230, 90, 90);
pub const TEXT: Color = Color::Rgb(215, 220, 215);
pub const CALL_C: Color = Color::Rgb(95, 205, 115);
pub const GREEN: Color = Color::Rgb(90, 190, 120);
pub const BAD: Color = Color::Rgb(220, 90, 90);
pub const CHAT: Color = Color::Rgb(120, 200, 220);
/// Table border when it isn't your turn, and the log/panel border.
pub const FELT_EDGE: Color = Color::Rgb(40, 110, 80);
pub const PANEL_EDGE: Color = Color::Rgb(90, 130, 110);
pub const BOX_EDGE: Color = Color::Rgb(80, 140, 110);

/// A steady, soft gold for "your turn": noticeable, but nothing flashes.
pub const TURN_GOLD: Color = Color::Rgb(214, 178, 96);

pub fn bold(c: Color) -> Style {
    Style::default().fg(c).add_modifier(Modifier::BOLD)
}

/// A black-on-colour key badge: ` Y `.
pub fn badge(label: &str, bg: Color) -> Span<'static> {
    Span::styled(format!(" {label} "), Style::default().fg(Color::Black).bg(bg).add_modifier(Modifier::BOLD))
}

/// A bordered box in the ASCII style.
pub fn ascii_block(color: Color) -> Block<'static> {
    Block::default().borders(Borders::ALL).border_set(ASCII).border_style(Style::default().fg(color))
}

/// A `w`×`h` rect centred in `area` (clamped to fit).
pub fn popup(area: Rect, w: u16, h: u16) -> Rect {
    let [row] = Layout::vertical([Constraint::Length(h.min(area.height))]).flex(Flex::Center).areas(area);
    let [rect] = Layout::horizontal([Constraint::Length(w.min(area.width))]).flex(Flex::Center).areas(row);
    rect
}
