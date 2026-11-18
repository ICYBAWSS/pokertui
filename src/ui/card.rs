//! The ASCII playing card as a ratatui widget.

use crate::game::Card;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sz {
    pub w: u16,
    pub h: u16,
}
/// Classic 8×6 ASCII card.
pub const SMALL: Sz = Sz { w: 8, h: 6 };
/// 12×9 ASCII card with a big suit drawing (terminals ≥ ~120×40).
pub const BIG: Sz = Sz { w: 12, h: 9 };

pub fn rank_str(card: Card) -> String {
    match card.rank_char() {
        'T' => "10".into(),
        c => c.to_string(),
    }
}

/// What a card slot shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Face {
    Up(Card),
    Back,
    /// Seen edge-on, halfway through a flip.
    Edge,
    /// An empty slot (board card not dealt yet).
    Empty,
}

#[derive(Clone, Copy, Debug)]
pub struct CardView {
    pub face: Face,
    pub size: Sz,
    pub dim: bool,
}

impl CardView {
    pub fn new(face: Face, size: Sz) -> Self {
        Self { face, size, dim: false }
    }
    pub fn dim(mut self, dim: bool) -> Self {
        self.dim = dim;
        self
    }
    /// Render with the top-left corner at (x, y).
    pub fn draw(self, buf: &mut Buffer, x: u16, y: u16) {
        self.render(Rect::new(x, y, self.size.w, self.size.h), buf);
    }
}

impl Widget for CardView {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (x, y, sz) = (area.x, area.y, self.size);
        match self.face {
            Face::Up(card) => up(buf, x, y, card, self.dim, sz),
            Face::Back => back(buf, x, y, sz),
            Face::Edge => edge(buf, x, y, sz),
            Face::Empty => empty(buf, x, y, sz),
        }
    }
}

fn frame_style(dim: bool) -> Style {
    Style::default().fg(if dim { Color::Rgb(115, 125, 120) } else { Color::Rgb(205, 210, 200) })
}

/// Fill the card's rectangle with a solid background.
fn fill(buf: &mut Buffer, x: u16, y: u16, sz: Sz, style: Style) {
    let blank = " ".repeat(sz.w as usize);
    for dy in 0..sz.h {
        buf.set_string(x, y + dy, &blank, style);
    }
}

/// A solid light card: rank and suit in two corners, suit pips in the middle.
fn up(buf: &mut Buffer, x: u16, y: u16, card: Card, dim: bool, sz: Sz) {
    let (paper, ink) = match (dim, card.is_red()) {
        (true, _) => (Color::Rgb(150, 155, 150), Color::Rgb(75, 80, 78)),
        (false, true) => (Color::Rgb(242, 240, 232), Color::Rgb(200, 30, 40)),
        (false, false) => (Color::Rgb(242, 240, 232), Color::Rgb(20, 20, 25)),
    };
    let bold = Style::default().fg(ink).bg(paper).add_modifier(Modifier::BOLD);
    fill(buf, x, y, sz, Style::default().bg(paper));
    let rank = rank_str(card);
    let r = rank.chars().count() as u16;
    let suit = card.suit_symbol().to_string();
    buf.set_string(x + 1, y, &rank, bold);
    buf.set_string(x + 1, y + 1, &suit, bold);
    buf.set_string(x + sz.w - 1 - r, y + sz.h - 1, &rank, bold);
    buf.set_string(x + sz.w - 2, y + sz.h - 2, &suit, bold);
    let (cx, cy) = (x + sz.w / 2, y + sz.h / 2);
    if sz == SMALL {
        buf.set_string(cx - 1, cy, &suit, bold);
    } else {
        for (dx, dy) in [(-2i16, -2i16), (1, -2), (-1, 0), (-2, 2), (1, 2)] {
            buf.set_string((cx as i16 + dx) as u16, (cy as i16 + dy) as u16, &suit, bold);
        }
    }
}

fn back(buf: &mut Buffer, x: u16, y: u16, sz: Sz) {
    let base = Style::default().fg(Color::Rgb(235, 150, 160)).bg(Color::Rgb(150, 30, 50));
    fill(buf, x, y, sz, base);
    for dy in 1..sz.h - 1 {
        let row: String = (0..sz.w - 2).map(|i| if (i + dy) % 2 == 0 { '╱' } else { '╲' }).collect();
        buf.set_string(x + 1, y + dy, row, base);
    }
}

fn edge(buf: &mut Buffer, x: u16, y: u16, sz: Sz) {
    let style = frame_style(false);
    for dy in 0..sz.h {
        buf.set_string(x + sz.w / 2 - 1, y + dy, "||", style);
    }
}

fn empty(buf: &mut Buffer, x: u16, y: u16, sz: Sz) {
    let style = Style::default().fg(Color::Rgb(45, 90, 70));
    let dots = ". ".repeat(sz.w as usize / 2);
    buf.set_string(x, y, &dots, style);
    for dy in 1..sz.h - 1 {
        buf.set_string(x, y + dy, format!(":{}:", " ".repeat(sz.w as usize - 2)), style);
    }
    buf.set_string(x, y + sz.h - 1, &dots, style);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_art_corners() {
        for sz in [SMALL, BIG] {
            let mut buf = Buffer::empty(Rect::new(0, 0, sz.w, sz.h));
            CardView::new(Face::Up(Card::parse("As").unwrap()), sz).render(buf.area, &mut buf);
            assert_eq!(buf[(1, 0)].symbol(), "A");
            assert_eq!(buf[(1, 1)].symbol(), "♠");
            assert_eq!(buf[(sz.w - 2, sz.h - 2)].symbol(), "♠");
            CardView::new(Face::Back, sz).render(buf.area, &mut buf);
            assert_eq!(buf[(1, 1)].symbol(), "╲");
            CardView::new(Face::Empty, sz).render(buf.area, &mut buf);
            assert_eq!(buf[(0, 0)].symbol(), ".");
            assert_eq!(buf[(0, 1)].symbol(), ":");
        }
    }
}
