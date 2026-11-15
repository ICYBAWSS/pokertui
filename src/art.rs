//! Embedded ASCII art.
//!
//! The pieces in `assets/art/` are stills generated offline with
//! `npx ascii-magic-art` (see `scripts/gen-art.sh`) and embedded at compile
//! time, so the shipped binary has no Node, network or asset-directory
//! dependency. Regenerate them whenever you want a different look.

pub const LOBBY: &str = include_str!("../assets/art/lobby.txt");
pub const WIN: &str = include_str!("../assets/art/win.txt");

/// Title wordmark for the splash and menus (41×6).
pub const WORDMARK: &str = "\
██████╗  ██████╗ ██╗  ██╗███████╗██████╗
██╔══██╗██╔═══██╗██║ ██╔╝██╔════╝██╔══██╗
██████╔╝██║   ██║█████╔╝ █████╗  ██████╔╝
██╔═══╝ ██║   ██║██╔═██╗ ██╔══╝  ██╔══██╗
██║     ╚██████╔╝██║  ██╗███████╗██║  ██║
╚═╝      ╚═════╝ ╚═╝  ╚═╝╚══════╝╚═╝  ╚═╝";

/// Trimmed lines of the art.
pub fn lines(art: &str) -> Vec<&str> {
    let mut out: Vec<&str> = art.lines().map(|l| l.trim_end()).collect();
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out
}

pub fn dimensions(art: &str) -> (u16, u16) {
    let lines = lines(art);
    let width = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
    (width, lines.len() as u16)
}

/// Draw the art clipped and centered inside `area`.
pub fn draw(area: ratatui::layout::Rect, buf: &mut ratatui::buffer::Buffer, art: &str, style: ratatui::style::Style) {
    let lines = lines(art);
    if lines.is_empty() || area.width == 0 || area.height == 0 {
        return;
    }
    let art_w = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let art_h = lines.len();
    let top = area.y + area.height.saturating_sub(art_h as u16) / 2;
    for (row, line) in lines.iter().enumerate() {
        if row as u16 >= area.height {
            break;
        }
        let len = line.chars().count();
        let indent = art_w.saturating_sub(len) / 2;
        let x = area.x + area.width.saturating_sub(art_w as u16) / 2 + indent as u16;
        if x >= area.x + area.width {
            continue;
        }
        let clipped: String = line
            .chars()
            .take((area.x + area.width - x) as usize)
            .collect();
        buf.set_string(x, top + row as u16, clipped, style);
    }
}
