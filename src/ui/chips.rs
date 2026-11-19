//! Poker chips. Values scale with the blinds so a stack reads at a glance:
//! white = small blind, red = big blind, green = 5 big blinds, blue = 20 big blinds.

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Style};

pub const COLORS: [Color; 4] = [
    Color::Rgb(225, 225, 220), // white
    Color::Rgb(220, 80, 80),   // red
    Color::Rgb(70, 170, 100),  // green
    Color::Rgb(80, 135, 230),  // blue
];

/// (small blind, big blind)
pub type Blinds = (u64, u64);

/// Chip values for these blinds, smallest first, each paired with its colour.
pub fn values(b: Blinds) -> Vec<(u64, Color)> {
    let (sb, bb) = (b.0.max(1), b.1.max(1));
    let mut v: Vec<u64> = vec![sb, bb, bb * 5, bb * 20];
    v.sort_unstable();
    v.dedup();
    v.into_iter().zip(COLORS).collect()
}

/// `amount` as (colour, value, chip count) per denomination, biggest first. Anything smaller
/// than the smallest chip is ignored.
pub fn denominations(amount: u64, b: Blinds) -> Vec<(Color, u64, u64)> {
    let mut left = amount;
    values(b)
        .into_iter()
        .rev()
        .filter_map(|(v, c)| {
            let n = left / v;
            left %= v;
            (n > 0).then_some((c, v, n))
        })
        .collect()
}

/// The chips for `amount` laid out in a row at most `room` cells wide: runs of each denomination
/// (biggest first) with a gap between runs. Every denomination keeps several chips where there is
/// room; runs are shortened evenly (and the smallest denominations dropped) when there is not.
pub fn chip_run(amount: u64, b: Blinds, room: usize) -> Vec<Option<Color>> {
    let mut d = denominations(amount, b);
    while !d.is_empty() {
        let width = |cap: u64| d.iter().map(|(_, _, n)| (*n).min(cap) as usize).sum::<usize>() + d.len() - 1;
        let max = d.iter().map(|(_, _, n)| *n).max().unwrap_or(1);
        if let Some(cap) = (1..=max).rev().find(|cap| width(*cap) <= room) {
            let mut out = Vec::new();
            for (i, (c, _, n)) in d.iter().enumerate() {
                if i > 0 {
                    out.push(None);
                }
                out.extend(std::iter::repeat_n(Some(*c), (*n).min(cap) as usize));
            }
            return out;
        }
        d.pop(); // not even one chip each fits: drop the smallest denomination
    }
    Vec::new()
}

/// Up to `n` chip colours to fly for `amount`, covering every denomination in it.
pub fn flight_colors(amount: u64, b: Blinds, n: usize) -> Vec<Color> {
    chip_run(amount, b, n * 2).into_iter().flatten().take(n).collect()
}

/// Draw one flying chip (a round dot, like the ones in the stacks), clipped to the buffer.
pub fn draw_chip(buf: &mut Buffer, x: i32, y: i32, color: Color, bg: Color) {
    let a = buf.area;
    if (a.x as i32..(a.x + a.width) as i32).contains(&x) && (a.y as i32..(a.y + a.height) as i32).contains(&y) {
        buf.set_string(x as u16, y as u16, "●", Style::default().fg(color).bg(bg));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: Blinds = (25, 50);

    #[test]
    fn values_scale_with_the_blinds() {
        assert_eq!(values(B).iter().map(|v| v.0).collect::<Vec<_>>(), vec![25, 50, 250, 1_000]);
        assert_eq!(values((5, 10)).iter().map(|v| v.0).collect::<Vec<_>>(), vec![5, 10, 50, 200]);
        assert_eq!(values((10, 10)).len(), 3, "equal blinds don't duplicate a chip");
    }

    #[test]
    fn a_starting_stack_reads_as_many_chips() {
        // 5,000 at 25/50 = 5 blue chips; 1,275 = 1 blue + 1 green + 0 red + 1 white.
        assert_eq!(denominations(5_000, B).iter().map(|d| d.2).collect::<Vec<_>>(), vec![5]);
        assert_eq!(denominations(1_275, B).iter().map(|d| (d.1, d.2)).collect::<Vec<_>>(), vec![(1_000, 1), (250, 1), (25, 1)]);
    }

    #[test]
    fn runs_fit_the_room_and_keep_every_denomination() {
        let run = chip_run(1_875, B, 12); // 1×blue, 3×green, 2×red, 1×white
        assert!(run.len() <= 12);
        assert_eq!(run.iter().flatten().count(), 7);
        let tight = chip_run(1_875, B, 5);
        assert!(tight.len() <= 5 && !tight.is_empty());
        assert!(chip_run(0, B, 10).is_empty());
        assert_eq!(flight_colors(4_975, B, 4).len(), 4);
    }
}
