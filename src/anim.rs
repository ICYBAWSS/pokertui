//! Time-based animation helpers. Everything is a pure function of elapsed
//! time, so a frame can be drawn at any moment and animations never block play.

use ratatui::style::Color;
use std::time::{Duration, Instant};

/// 0.0 at `start`, 1.0 once `dur` has elapsed.
pub fn progress(start: Instant, dur: Duration) -> f32 {
    (start.elapsed().as_secs_f32() / dur.as_secs_f32().max(0.001)).clamp(0.0, 1.0)
}

pub fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

pub fn lerp(a: u16, b: u16, t: f32) -> u16 {
    (a as f32 + (b as f32 - a as f32) * t).round().max(0.0) as u16
}

pub fn lerp_u64(a: u64, b: u64, t: f32) -> u64 {
    (a as f64 + (b as f64 - a as f64) * t as f64).round().max(0.0) as u64
}

/// Gold with a bright band sweeping left to right every ~2 s.
pub fn shimmer(col: u16, width: u16, tick: u64) -> Color {
    let period = width as i64 + 24;
    let band = (tick as i64 * 2) % period - 12;
    let d = (col as i64 - band).abs();
    let boost = (12 - d).max(0) as u8 * 6;
    Color::Rgb(
        230u8.saturating_add(boost / 3),
        180u8.saturating_add(boost),
        70u8.saturating_add(boost * 2),
    )
}

/// Pulsing brightness, 0.0..=1.0, period ~1.2 s at 50 ms ticks.
pub fn pulse(tick: u64) -> f32 {
    let phase = (tick % 24) as f32 / 24.0 * std::f32::consts::TAU;
    0.5 + 0.5 * phase.sin()
}

const SPARK_COLORS: [Color; 5] = [
    Color::Rgb(255, 210, 90),
    Color::Rgb(255, 110, 110),
    Color::Rgb(120, 220, 255),
    Color::Rgb(170, 255, 150),
    Color::Rgb(255, 160, 240),
];

/// Fireworks particles for `elapsed` seconds into a show seeded by `seed`,
/// inside a `w`×`h` area. Returns (x, y, glyph, color) relative to the area.
pub fn fireworks(seed: u64, elapsed: f32, w: u16, h: u16) -> Vec<(u16, u16, char, Color)> {
    let mut rng = SplitMix(seed);
    let mut out = Vec::new();
    if w < 10 || h < 6 {
        return out;
    }
    for burst in 0..5 {
        let launch = burst as f32 * 0.35;
        let cx = 4.0 + rng.unit() * (w as f32 - 8.0);
        let cy = 2.0 + rng.unit() * (h as f32 * 0.5);
        let color = SPARK_COLORS[(rng.next() % SPARK_COLORS.len() as u64) as usize];
        let age = elapsed - launch;
        if !(0.0..1.4).contains(&age) {
            // Still draw nothing for this burst, but keep the RNG stream stable.
            for _ in 0..28 {
                rng.next();
            }
            continue;
        }
        for _ in 0..28 {
            let angle = rng.unit() * std::f32::consts::TAU;
            let speed = 6.0 + rng.unit() * 10.0;
            // Terminal cells are ~2x taller than wide: stretch x.
            let x = cx + angle.cos() * speed * age * 2.0;
            let y = cy + angle.sin() * speed * age + 4.0 * age * age;
            if x < 0.0 || y < 0.0 || x >= w as f32 || y >= h as f32 {
                continue;
            }
            let glyph = match age {
                a if a < 0.3 => '*',
                a if a < 0.7 => '+',
                a if a < 1.0 => '·',
                _ => '.',
            };
            out.push((x as u16, y as u16, glyph, color));
        }
    }
    out
}

/// Tiny deterministic RNG; animations must look the same on every redraw.
struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fireworks_are_deterministic_and_in_bounds() {
        let a = fireworks(7, 0.6, 80, 20);
        let b = fireworks(7, 0.6, 80, 20);
        assert_eq!(a, b);
        assert!(!a.is_empty());
        assert!(a.iter().all(|&(x, y, _, _)| x < 80 && y < 20));
        assert!(fireworks(7, 9.0, 80, 20).is_empty(), "show ends");
    }

    #[test]
    fn lerp_and_progress_clamp() {
        assert_eq!(lerp(10, 20, 0.5), 15);
        assert_eq!(lerp(20, 10, 1.0), 10);
        assert_eq!(lerp_u64(0, 100, 0.25), 25);
        let p = progress(Instant::now() - Duration::from_secs(5), Duration::from_secs(1));
        assert_eq!(p, 1.0);
    }
}
