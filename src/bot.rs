//! Computer opponents. A bot is an ordinary seat whose moves come from here,
//! chosen from exactly what a human in that seat could see: never the deck.

use crate::game::{Card, Legal, Move, Phase, SeatId, Street, Table, eval7, full_deck};
use rand_chacha::ChaCha20Rng;
use rand_chacha::rand_core::Rng;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skill {
    Easy,
    Medium,
    Hard,
}

impl Skill {
    pub fn label(self) -> &'static str {
        match self {
            Skill::Easy => "Easy",
            Skill::Medium => "Medium",
            Skill::Hard => "Hard",
        }
    }

    pub fn cycle(self) -> Skill {
        match self {
            Skill::Easy => Skill::Medium,
            Skill::Medium => Skill::Hard,
            Skill::Hard => Skill::Easy,
        }
    }

    pub fn parse(text: &str) -> Option<Skill> {
        match text.trim().to_ascii_lowercase().as_str() {
            "easy" | "e" => Some(Skill::Easy),
            "medium" | "med" | "m" => Some(Skill::Medium),
            "hard" | "h" => Some(Skill::Hard),
            _ => None,
        }
    }
}

pub const NAMES: [&str; 8] = [
    "Dealer Dan", "Lucky Lou", "Ace Annie", "Cool Hand", "River Rae", "Big Stack", "Bluff Bea", "Fish Finn",
];

/// What a player in this seat can see. Nothing about the deck or other hands.
#[derive(Clone, Debug)]
pub struct BotView {
    pub hole: [Card; 2],
    pub board: Vec<Card>,
    pub pot: u64,
    pub current_bet: u64,
    pub to_call: u64,
    pub stack: u64,
    pub legal: Legal,
    pub opponents: usize,
    pub street: Street,
}

impl Table {
    pub fn bot_view(&self, seat: SeatId) -> Option<BotView> {
        if self.phase != Phase::Betting {
            return None;
        }
        let s = self.seats.get(seat)?.as_ref()?;
        let legal = self.legal(seat)?;
        let hole = [*s.hole.first()?, *s.hole.get(1)?];
        Some(BotView {
            hole,
            board: self.board.clone(),
            pot: self.pot_total(),
            current_bet: self.current_bet,
            to_call: legal.to_call,
            stack: s.stack,
            legal,
            opponents: self.alive().len().saturating_sub(1).max(1),
            street: self.street,
        })
    }
}

fn unit(rng: &mut ChaCha20Rng) -> f64 {
    rng.next_u32() as f64 / u32::MAX as f64
}

/// Share of the pot we win against `opponents` random hands, by simulation.
pub fn equity(v: &BotView, samples: usize, rng: &mut ChaCha20Rng) -> f64 {
    let mut known = vec![v.hole[0], v.hole[1]];
    known.extend(&v.board);
    let rest: Vec<Card> = full_deck().into_iter().filter(|c| !known.contains(c)).collect();
    let need = 5 - v.board.len() + 2 * v.opponents;
    let mut won = 0.0;
    for _ in 0..samples {
        let mut pool = rest.clone();
        for i in 0..need {
            let j = i + (rng.next_u32() as usize) % (pool.len() - i);
            pool.swap(i, j);
        }
        let mut board = v.board.clone();
        board.extend(&pool[..5 - v.board.len()]);
        let seven = |a: Card, b: Card| {
            eval7(&[a, b, board[0], board[1], board[2], board[3], board[4]])
        };
        let mine = seven(v.hole[0], v.hole[1]);
        let mut best = None;
        let mut ties = 0;
        for o in 0..v.opponents {
            let k = 5 - v.board.len() + 2 * o;
            let theirs = seven(pool[k], pool[k + 1]);
            match best {
                None => best = Some(theirs),
                Some(b) if theirs > b => best = Some(theirs),
                _ => {}
            }
            if Some(theirs) == best {
                ties += 1;
            }
        }
        match best {
            Some(b) if mine < b => {}
            Some(b) if mine == b => won += 1.0 / (ties as f64 + 1.0),
            _ => won += 1.0,
        }
    }
    won / samples as f64
}

fn preflop_score(hole: [Card; 2]) -> f64 {
    let (a, b) = (hole[0].0 / 4, hole[1].0 / 4);
    let (hi, lo) = (a.max(b) as f64, a.min(b) as f64);
    if a == b {
        return 14.0 + hi;
    }
    let suited = if hole[0].0 % 4 == hole[1].0 % 4 { 3.0 } else { 0.0 };
    let gap = hi - lo;
    let connected = if gap == 1.0 { 2.0 } else if gap == 2.0 { 1.0 } else { 0.0 };
    hi + lo / 2.0 + suited + connected
}

/// Raise to about `frac` of the pot over the current bet, or shove if that is
/// most of the stack. Always a legal move.
fn raise(v: &BotView, frac: f64) -> Move {
    if !v.legal.can_raise {
        return if v.legal.can_check { Move::Check } else { Move::Call };
    }
    let to = v.current_bet + (v.pot as f64 * frac) as u64;
    let to = to.clamp(v.legal.min_raise_to, v.legal.max_raise_to);
    if to >= v.legal.max_raise_to || to * 5 >= (v.stack + v.current_bet - v.to_call) * 4 {
        Move::AllIn
    } else {
        Move::Raise { to }
    }
}

fn check_or_fold(v: &BotView) -> Move {
    if v.legal.can_check { Move::Check } else { Move::Fold }
}

pub fn decide(skill: Skill, v: &BotView, rng: &mut ChaCha20Rng) -> Move {
    let r = unit(rng);
    match skill {
        Skill::Easy => {
            if v.legal.can_check {
                if r < 0.12 { raise(v, 0.4) } else { Move::Check }
            } else if r < 0.15 && v.to_call * 3 > v.stack {
                Move::Fold
            } else if r < 0.90 {
                Move::Call
            } else {
                raise(v, 0.5)
            }
        }
        Skill::Medium => {
            let needed = v.to_call as f64 / (v.pot + v.to_call).max(1) as f64;
            if v.board.is_empty() {
                let s = preflop_score(v.hole);
                return if s >= 20.0 {
                    raise(v, 0.6)
                } else if s >= 12.5 || (v.legal.can_check && s >= 9.0) {
                    if v.legal.can_check { Move::Check } else if needed < 0.4 { Move::Call } else { Move::Fold }
                } else {
                    check_or_fold(v)
                };
            }
            let e = equity(v, 80, rng);
            if e > 0.78 {
                raise(v, 0.6)
            } else if v.to_call == 0 {
                if e > 0.6 { raise(v, 0.4) } else { Move::Check }
            } else if e > needed + 0.05 {
                Move::Call
            } else {
                Move::Fold
            }
        }
        Skill::Hard => {
            let needed = v.to_call as f64 / (v.pot + v.to_call).max(1) as f64;
            let e = equity(v, 500, rng);
            let fair = 1.0 / (v.opponents as f64 + 1.0);
            if e > fair + 0.3 {
                raise(v, if e > 0.85 { 0.9 } else { 0.6 })
            } else if v.to_call == 0 {
                if e > fair + 0.12 {
                    raise(v, 0.4)
                } else if e < fair && v.legal.can_raise && r < 0.12 {
                    raise(v, 0.5) // occasional bluff so we are not readable
                } else {
                    Move::Check
                }
            } else if e > needed + 0.02 {
                Move::Call
            } else if v.legal.can_raise && r < 0.05 && v.to_call * 6 < v.stack {
                raise(v, 0.7)
            } else {
                Move::Fold
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::TableParams;
    use rand_chacha::rand_core::SeedableRng;

    fn view(hole: &str, board: &str, pot: u64, to_call: u64, opponents: usize) -> BotView {
        let cards = |t: &str| -> Vec<Card> {
            t.split_whitespace().map(|c| Card::parse(c).unwrap()).collect()
        };
        let h = cards(hole);
        BotView {
            hole: [h[0], h[1]],
            board: cards(board),
            pot,
            current_bet: to_call,
            to_call,
            stack: 5_000,
            legal: Legal {
                to_call,
                can_check: to_call == 0,
                can_call: to_call > 0,
                can_raise: true,
                can_allin: true,
                min_raise_to: to_call * 2 + 50,
                max_raise_to: 5_000,
            },
            opponents,
            street: Street::Preflop,
        }
    }

    #[test]
    fn hard_folds_trash_to_a_big_raise_but_not_aces() {
        let mut rng = ChaCha20Rng::from_seed([3; 32]);
        let mut folds = 0;
        for _ in 0..100 {
            // Hard bluffs now and then on purpose, so "almost always folds".
            let trash = view("7c 2d", "", 300, 800, 1);
            folds += (decide(Skill::Hard, &trash, &mut rng) == Move::Fold) as u32;
            let aces = view("As Ah", "", 300, 200, 1);
            assert_ne!(decide(Skill::Hard, &aces, &mut rng), Move::Fold);
        }
        assert!(folds >= 90, "folded trash only {folds}/100 times");
    }

    #[test]
    fn equity_orders_hands_sensibly() {
        let mut rng = ChaCha20Rng::from_seed([5; 32]);
        let aces = equity(&view("As Ah", "", 100, 0, 1), 600, &mut rng);
        let trash = equity(&view("7c 2d", "", 100, 0, 1), 600, &mut rng);
        assert!(aces > 0.75 && trash < 0.45, "aces {aces} trash {trash}");
    }

    /// Bots never make an illegal move, and chips are conserved over many hands.
    #[test]
    fn bots_play_legal_poker_and_conserve_chips() {
        let mut rng = ChaCha20Rng::from_seed([9; 32]);
        let mut t = Table::new(TableParams {
            max_seats: 4,
            small_blind: 10,
            big_blind: 20,
            buyin: 1_000,
            turn_ms: 0,
            tourney: None,
        });
        for (i, skill) in [Skill::Easy, Skill::Medium, Skill::Hard, Skill::Easy].into_iter().enumerate() {
            t.join_bot(NAMES[i], skill).unwrap();
        }
        let total = t.total_chips();
        for hand in 0..120 {
            if t.start_hand(hand).is_err() {
                for (_, s) in t.players_mut() {
                    s.stack = 1_000;
                }
                continue;
            }
            let mut guard = 0;
            while t.phase == Phase::Betting {
                guard += 1;
                assert!(guard < 400, "hand never finished");
                let seat = t.actor.unwrap();
                let skill = t.seats[seat].as_ref().unwrap().bot.unwrap();
                let v = t.bot_view(seat).unwrap();
                let mv = decide(skill, &v, &mut rng);
                t.apply(seat, mv.clone(), 0)
                    .unwrap_or_else(|e| panic!("{skill:?} made illegal move {mv:?}: {e}"));
            }
            assert_eq!(t.total_chips(), total, "chips changed");
        }
    }
}
