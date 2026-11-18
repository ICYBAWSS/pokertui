//! Five- and seven-card hand evaluation for Texas Hold'em.
//!
//! `HandScore` derives `Ord`, so comparing two scores is comparing category
//! first and then kickers lexicographically — exactly poker hand ranking.

use super::cards::Card;
use serde::{Deserialize, Serialize};

pub const CAT_HIGH: u8 = 0;
pub const CAT_PAIR: u8 = 1;
pub const CAT_TWO_PAIR: u8 = 2;
pub const CAT_TRIPS: u8 = 3;
pub const CAT_STRAIGHT: u8 = 4;
pub const CAT_FLUSH: u8 = 5;
pub const CAT_FULL_HOUSE: u8 = 6;
pub const CAT_QUADS: u8 = 7;
pub const CAT_STRAIGHT_FLUSH: u8 = 8;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct HandScore {
    pub cat: u8,
    /// Up to five rank values that break ties within the category, padded with 0.
    pub kickers: [u8; 5],
}

impl HandScore {
    fn new(cat: u8, kickers: &[u8]) -> Self {
        let mut k = [0u8; 5];
        for (dst, src) in k.iter_mut().zip(kickers.iter()) {
            *dst = *src;
        }
        Self { cat, kickers: k }
    }

    pub fn describe(&self) -> String {
        let one = |i: usize| rank_name(self.kickers[i]);
        let many = |i: usize| rank_plural(self.kickers[i]);
        match self.cat {
            CAT_STRAIGHT_FLUSH => {
                if self.kickers[0] == 12 {
                    "Royal Flush".into()
                } else {
                    format!("Straight Flush, {}-high", one(0))
                }
            }
            CAT_QUADS => format!("Four of a Kind, {}", many(0)),
            CAT_FULL_HOUSE => format!("Full House, {} over {}", many(0), many(1)),
            CAT_FLUSH => format!("Flush, {}-high", one(0)),
            CAT_STRAIGHT => format!("Straight, {}-high", one(0)),
            CAT_TRIPS => format!("Three of a Kind, {}", many(0)),
            CAT_TWO_PAIR => format!("Two Pair, {} and {}", many(0), many(1)),
            CAT_PAIR => format!("Pair of {}", many(0)),
            _ => format!("High Card, {}", one(0)),
        }
    }
}

pub fn rank_name(v: u8) -> &'static str {
    ["Two", "Three", "Four", "Five", "Six", "Seven", "Eight", "Nine", "Ten", "Jack", "Queen", "King", "Ace"][v as usize % 13]
}

pub fn rank_plural(v: u8) -> &'static str {
    ["Twos", "Threes", "Fours", "Fives", "Sixes", "Sevens", "Eights", "Nines", "Tens", "Jacks", "Queens", "Kings", "Aces"][v as usize % 13]
}

/// Best five cards out of five to seven.
fn best_of(cards: &[Card]) -> HandScore {
    let n = cards.len();
    let mut best: Option<HandScore> = None;
    for mask in 0u32..(1 << n) {
        if mask.count_ones() != 5 {
            continue;
        }
        let pick: Vec<Card> = (0..n).filter(|i| mask >> i & 1 == 1).map(|i| cards[i]).collect();
        let score = eval5(&[pick[0], pick[1], pick[2], pick[3], pick[4]]);
        if best.is_none_or(|b| score > b) {
            best = Some(score);
        }
    }
    best.expect("at least five cards")
}

/// Plain-English read of your hand right now, for the player: before the flop the
/// two cards on their own, after it the best made hand, plus any draw still live.
pub fn describe_hand(hole: &[Card], board: &[Card]) -> String {
    let [a, b] = hole else {
        return String::new();
    };
    if board.is_empty() {
        let (hi, lo) = (a.rank().max(b.rank()), a.rank().min(b.rank()));
        return if hi == lo {
            format!("Pocket {}", rank_plural(hi))
        } else {
            let kind = if a.suit() == b.suit() { "suited" } else { "offsuit" };
            format!("{}-{} {kind}", rank_name(hi), rank_name(lo))
        };
    }
    let all: Vec<Card> = hole.iter().chain(board).copied().collect();
    let score = best_of(&all);
    let mut text = score.describe();

    // A pair or trips that is entirely on the board isn't really yours.
    let in_hand = |r: u8| a.rank() == r || b.rank() == r;
    let on_board_only = match score.cat {
        CAT_PAIR | CAT_TRIPS => !in_hand(score.kickers[0]),
        CAT_TWO_PAIR => !in_hand(score.kickers[0]) && !in_hand(score.kickers[1]),
        _ => false,
    };
    if on_board_only {
        text.push_str(" (on the board)");
    }

    if board.len() < 5 {
        let mut notes = Vec::new();
        if score.cat < CAT_FLUSH {
            let suited = (0..4u8).any(|s| {
                all.iter().filter(|c| c.suit() == s).count() == 4 && hole.iter().any(|c| c.suit() == s)
            });
            if suited {
                notes.push("flush draw");
            }
        }
        if score.cat < CAT_STRAIGHT {
            let mut have = [false; 14]; // index 0 is the ace playing low
            for c in &all {
                have[c.rank() as usize + 1] = true;
                if c.rank() == 12 {
                    have[0] = true;
                }
            }
            let draw = (0..=9).any(|start| (start..start + 5).filter(|i| have[*i]).count() == 4);
            if draw {
                notes.push("straight draw");
            }
        }
        for n in notes {
            text.push_str(" · ");
            text.push_str(n);
        }
    }
    text
}

/// Evaluate exactly five cards.
pub fn eval5(cards: &[Card; 5]) -> HandScore {
    let mut ranks: Vec<u8> = cards.iter().map(|c| c.rank()).collect();
    ranks.sort_unstable_by(|a, b| b.cmp(a)); // descending

    let flush = cards.iter().all(|c| c.suit() == cards[0].suit());

    // Straight detection over unique ranks (A can play low as 1).
    let mut unique: Vec<u8> = ranks.clone();
    unique.dedup();
    let straight_high = if unique.len() == 5 {
        if unique[0] - unique[4] == 4 {
            Some(unique[0])
        } else if unique == [12, 3, 2, 1, 0] {
            Some(3) // wheel: 5-high straight
        } else {
            None
        }
    } else {
        None
    };

    if flush && let Some(high) = straight_high {
        return HandScore::new(CAT_STRAIGHT_FLUSH, &[high]);
    }

    // Group ranks by count, highest count first and highest rank first within a count.
    let mut groups: Vec<(u8, u8)> = Vec::new(); // (count, rank)
    for &r in &ranks {
        match groups.iter_mut().find(|(_, rr)| *rr == r) {
            Some((count, _)) => *count += 1,
            None => groups.push((1, r)),
        }
    }
    groups.sort_unstable_by(|a, b| b.cmp(a)); // (count desc, rank desc)

    let counts: Vec<u8> = groups.iter().map(|(c, _)| *c).collect();
    let g = |i: usize| groups[i].1;

    match counts.as_slice() {
        [4, 1] => HandScore::new(CAT_QUADS, &[g(0), g(1)]),
        [3, 2] => HandScore::new(CAT_FULL_HOUSE, &[g(0), g(1)]),
        [3, 1, 1] => HandScore::new(CAT_TRIPS, &[g(0), g(1), g(2)]),
        [2, 2, 1] => HandScore::new(CAT_TWO_PAIR, &[g(0), g(1), g(2)]),
        [2, 1, 1, 1] => HandScore::new(CAT_PAIR, &[g(0), g(1), g(2), g(3)]),
        _ => {
            if flush {
                HandScore::new(CAT_FLUSH, &ranks)
            } else if let Some(high) = straight_high {
                HandScore::new(CAT_STRAIGHT, &[high])
            } else {
                HandScore::new(CAT_HIGH, &ranks)
            }
        }
    }
}

/// Evaluate the best five of seven cards (21 combinations).
pub fn eval7(cards: &[Card; 7]) -> HandScore {
    const C: [[usize; 5]; 21] = [
        [0, 1, 2, 3, 4],
        [0, 1, 2, 3, 5],
        [0, 1, 2, 3, 6],
        [0, 1, 2, 4, 5],
        [0, 1, 2, 4, 6],
        [0, 1, 2, 5, 6],
        [0, 1, 3, 4, 5],
        [0, 1, 3, 4, 6],
        [0, 1, 3, 5, 6],
        [0, 1, 4, 5, 6],
        [0, 2, 3, 4, 5],
        [0, 2, 3, 4, 6],
        [0, 2, 3, 5, 6],
        [0, 2, 4, 5, 6],
        [0, 3, 4, 5, 6],
        [1, 2, 3, 4, 5],
        [1, 2, 3, 4, 6],
        [1, 2, 3, 5, 6],
        [1, 2, 4, 5, 6],
        [1, 3, 4, 5, 6],
        [2, 3, 4, 5, 6],
    ];
    let mut best: Option<HandScore> = None;
    for idx in C {
        let hand = [
            cards[idx[0]],
            cards[idx[1]],
            cards[idx[2]],
            cards[idx[3]],
            cards[idx[4]],
        ];
        let score = eval5(&hand);
        if best.is_none_or(|b| score > b) {
            best = Some(score);
        }
    }
    best.expect("21 combinations")
}

/// Convenience: evaluate 2 hole cards plus a 5-card board.
pub fn eval_holdem(hole: [Card; 2], board: &[Card]) -> HandScore {
    let mut seven = [hole[0], hole[1], board[0], board[1], board[2], board[3], board[4]];
    let _ = &mut seven;
    eval7(&seven)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cards(s: &str) -> Vec<Card> {
        s.split_whitespace()
            .map(|c| Card::parse(c).unwrap_or_else(|| panic!("bad card {c}")))
            .collect()
    }

    fn five(s: &str) -> HandScore {
        let v = cards(s);
        eval5(&v[..5].try_into().unwrap())
    }

    #[test]
    fn categories() {
        assert_eq!(five("As Ks Qs Js Ts").cat, CAT_STRAIGHT_FLUSH);
        assert_eq!(five("9s 8s 7s 6s 5s").cat, CAT_STRAIGHT_FLUSH);
        assert_eq!(five("Ah Ad Ac As 2d").cat, CAT_QUADS);
        assert_eq!(five("Ah Ad Ac Ks Kd").cat, CAT_FULL_HOUSE);
        assert_eq!(five("Ah 9h 7h 4h 2h").cat, CAT_FLUSH);
        assert_eq!(five("9h 8s 7d 6c 5h").cat, CAT_STRAIGHT);
        assert_eq!(five("Ah Ad Ac Ks Qd").cat, CAT_TRIPS);
        assert_eq!(five("Ah Ad Ks Kd Qc").cat, CAT_TWO_PAIR);
        assert_eq!(five("Ah Ad Ks Qd Jc").cat, CAT_PAIR);
        assert_eq!(five("Ah Ks Qd Jc 9h").cat, CAT_HIGH);
    }

    #[test]
    fn wheel_is_five_high() {
        let wheel = five("Ah 2s 3d 4c 5h");
        assert_eq!(wheel.cat, CAT_STRAIGHT);
        assert_eq!(wheel.kickers[0], 3); // rank index of the 5
        let six_high = five("2h 3s 4d 5c 6h");
        assert!(six_high > wheel);
    }

    #[test]
    fn kickers_break_ties() {
        assert!(five("Ah Ad Ks Qd Jc") > five("Ah Ad Ks Qd Tc"));
        assert!(five("Ah Ad Ks Qd Jc") > five("Ah Ad Qs Jd Tc"));
        assert_eq!(five("Ah Ad Ks Qd Jc"), five("Ac As Kd Qh Js"));
    }

    #[test]
    fn two_pair_ordering() {
        assert!(five("Ah Ad Ks Kd 2c") > five("Ah Ad Qs Qd Kc"));
        assert!(five("Ah Ad Ks Kd Qc") > five("Ah Ad Ks Kd Jc"));
    }

    #[test]
    fn full_house_ordering() {
        assert!(five("Ah Ad Ac Ks Kd") > five("Kh Kd Kc As Ad"));
    }

    #[test]
    fn seven_card_best_five() {
        // Board plays: straight flush on board beats three aces.
        let v = cards("As Ks Qs Js Ts 2h 2d");
        let seven: [Card; 7] = v[..7].try_into().unwrap();
        assert_eq!(eval7(&seven).cat, CAT_STRAIGHT_FLUSH);

        // Quads on board with ace kicker in hand.
        let v = cards("9h 9d 9c 9s Ah 2d 3c");
        let seven: [Card; 7] = v[..7].try_into().unwrap();
        let score = eval7(&seven);
        assert_eq!(score.cat, CAT_QUADS);
        assert_eq!(score.kickers[1], 12);
    }

    #[test]
    fn board_plays_and_split() {
        let v = cards("Ah Kh Qh Jh Th 2c 3d");
        let board = &v[2..7];
        let a = eval_holdem([v[0], v[1]], board);
        let b = eval_holdem([v[0], v[1]], board);
        assert_eq!(a, b);
        assert_eq!(a.cat, CAT_STRAIGHT_FLUSH);
    }

    #[test]
    fn royal_beats_king_high_straight_flush() {
        assert!(five("As Ks Qs Js Ts") > five("Ks Qs Js Ts 9s"));
    }

    #[test]
    fn describes_a_hand_in_plain_english() {
        let say = |hole: &str, board: &str| describe_hand(&cards(hole), &cards(board));
        assert_eq!(say("Ks Kd", ""), "Pocket Kings");
        assert_eq!(say("As Ks", ""), "Ace-King suited");
        assert_eq!(say("7c 2d", ""), "Seven-Two offsuit");
        assert_eq!(say("Ks 9d", "Kd 5c 2h"), "Pair of Kings");
        assert_eq!(say("Ks 5d", "Kd 5c 2h"), "Two Pair, Kings and Fives");
        assert_eq!(say("As 3d", "Kd 5c 2h"), "High Card, Ace · straight draw");
        assert_eq!(say("9s 8s", "2s 5s Kd"), "High Card, King · flush draw");
        assert_eq!(say("9s 2d", "Kd Kc 5h"), "Pair of Kings (on the board)");
        assert_eq!(say("9s 8d", "7h 6c 2s Kd Kc"), "Pair of Kings (on the board)");
        assert_eq!(say("Ah Kh", "Qh Jh 2h"), "Flush, Ace-high");
        assert_eq!(say("9h 8d", "7s 6c Ah"), "High Card, Ace · straight draw");
        assert_eq!(say("9h 8d", "7s 6c 5h"), "Straight, Nine-high");
        assert_eq!(say("5h 4d", "As 3c Kh"), "High Card, Ace · straight draw", "wheel draw");
    }
}
