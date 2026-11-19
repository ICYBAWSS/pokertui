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
    /// Game-theory-optimal style: balanced ranges, mixed strategies.
    Gto,
}

impl Skill {
    pub fn label(self) -> &'static str {
        match self {
            Skill::Easy => "Easy",
            Skill::Medium => "Medium",
            Skill::Hard => "Hard",
            Skill::Gto => "GTO",
        }
    }

    pub fn cycle(self) -> Skill {
        match self {
            Skill::Easy => Skill::Medium,
            Skill::Medium => Skill::Hard,
            Skill::Hard => Skill::Gto,
            Skill::Gto => Skill::Easy,
        }
    }

    pub fn parse(text: &str) -> Option<Skill> {
        match text.trim().to_ascii_lowercase().as_str() {
            "easy" | "e" => Some(Skill::Easy),
            "medium" | "med" | "m" => Some(Skill::Medium),
            "hard" | "h" => Some(Skill::Hard),
            "gto" | "g" => Some(Skill::Gto),
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
    /// On the button or one seat before it (acts late after the flop).
    pub late: bool,
    /// Per live opponent: (calls, raises) made this hand. Drives range narrowing.
    pub opp_actions: Vec<(u32, u32)>,
}

impl Table {
    pub fn bot_view(&self, seat: SeatId) -> Option<BotView> {
        if self.phase != Phase::Betting {
            return None;
        }
        let s = self.seats.get(seat)?.as_ref()?;
        let legal = self.legal(seat)?;
        let hole = [*s.hole.first()?, *s.hole.get(1)?];
        let n = self.seats.len();
        let late = (self.button + n - seat) % n <= 1;
        let opp_actions = self
            .alive()
            .into_iter()
            .filter(|&o| o != seat)
            .map(|o| {
                let mine = self.hand_moves().iter().filter(|(who, _)| *who == o);
                mine.fold((0, 0), |(c, r), (_, m)| match m {
                    Move::Call => (c + 1, r),
                    Move::Raise { .. } | Move::AllIn => (c, r + 1),
                    _ => (c, r),
                })
            })
            .collect();
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
            late,
            opp_actions,
        })
    }
}

fn unit(rng: &mut ChaCha20Rng) -> f64 {
    rng.next_u32() as f64 / u32::MAX as f64
}

/// Chen formula: a rough points value for a starting hand.
fn chen(hole: [Card; 2]) -> f64 {
    let pts = |r: u8| match r {
        12 => 10.0,
        11 => 8.0,
        10 => 7.0,
        9 => 6.0,
        r => (r as f64 + 2.0) / 2.0,
    };
    let (a, b) = (hole[0].0 / 4, hole[1].0 / 4);
    let (hi, lo) = (a.max(b), a.min(b));
    let mut p = if hi == lo { (pts(hi) * 2.0).max(5.0) } else { pts(hi) };
    if hole[0].0 % 4 == hole[1].0 % 4 {
        p += 2.0;
    }
    if hi != lo {
        p += match hi - lo - 1 {
            0 => 0.0,
            1 => -1.0,
            2 => -2.0,
            3 => -4.0,
            _ => -5.0,
        };
        if hi - lo <= 2 && hi < 10 {
            p += 1.0;
        }
    }
    p.ceil()
}

/// Chen score needed to be in the top `frac` of all starting hands.
fn range_cutoff(frac: f64) -> f64 {
    let all = sorted_chen();
    all[((1.0 - frac.clamp(0.0, 1.0)) * (all.len() - 1) as f64) as usize]
}

/// Share of all starting hands this one beats or ties (1.0 = best).
fn percentile(hole: [Card; 2]) -> f64 {
    let all = sorted_chen();
    all.partition_point(|&x| x <= chen(hole)) as f64 / all.len() as f64
}

fn sorted_chen() -> &'static [f64] {
    static SORTED: std::sync::OnceLock<Vec<f64>> = std::sync::OnceLock::new();
    SORTED.get_or_init(|| {
        let deck = full_deck();
        let mut v = Vec::with_capacity(1326);
        for i in 0..deck.len() {
            for j in i + 1..deck.len() {
                v.push(chen([deck[i], deck[j]]));
            }
        }
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v
    })
}

/// Share of the pot we win against `opponents` random hands, by simulation.
pub fn equity(v: &BotView, samples: usize, rng: &mut ChaCha20Rng) -> f64 {
    equity_vs(v, samples, rng, &[])
}

/// Like `equity`, but opponent `o` holds a hand from the top `fracs[o]` of
/// all starting hands (missing entries = any hand).
pub fn equity_vs(v: &BotView, samples: usize, rng: &mut ChaCha20Rng, fracs: &[f64]) -> f64 {
    let mut known = vec![v.hole[0], v.hole[1]];
    known.extend(&v.board);
    let rest: Vec<Card> = full_deck().into_iter().filter(|c| !known.contains(c)).collect();
    let cuts: Vec<f64> = (0..v.opponents).map(|o| range_cutoff(fracs.get(o).copied().unwrap_or(1.0))).collect();
    let need = 5 - v.board.len();
    let mut won = 0.0;
    for _ in 0..samples {
        let mut pool = rest.clone();
        let mut k = 0;
        // Deal each opponent a hand from their range (rejection sampling).
        for cut in &cuts {
            for t in 0..24 {
                let span = pool.len() - k;
                let i = k + rng.next_u32() as usize % span;
                let j = k + ((i - k) + 1 + rng.next_u32() as usize % (span - 1)) % span;
                if t == 23 || chen([pool[i], pool[j]]) >= *cut {
                    pool.swap(k, i);
                    let j = if j == k { i } else { j };
                    pool.swap(k + 1, j);
                    break;
                }
            }
            k += 2;
        }
        for i in 0..need {
            let j = k + i + rng.next_u32() as usize % (pool.len() - k - i);
            pool.swap(k + i, j);
        }
        let mut board = v.board.clone();
        board.extend(&pool[k..k + need]);
        let seven = |a: Card, b: Card| eval7(&[a, b, board[0], board[1], board[2], board[3], board[4]]);
        let mine = seven(v.hole[0], v.hole[1]);
        let mut best = None;
        let mut ties = 0;
        for o in 0..v.opponents {
            let theirs = seven(pool[2 * o], pool[2 * o + 1]);
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
        Skill::Medium | Skill::Hard => model(skill, v, rng, r),
        Skill::Gto => gto(v, rng, r),
    }
}

/// Loki-style play (Billings et al., "The challenge of poker"): every
/// opponent's range narrows with the calls and raises they made this hand, our
/// hand strength is simulated against those ranges, and we mix value bets,
/// calls and a few bluffs at ONE bet size so the size gives nothing away.
fn model(skill: Skill, v: &BotView, rng: &mut ChaCha20Rng, r: f64) -> Move {
    // (samples, range kept per call, per raise, value margin, call margin, bluff rate)
    let (samples, call_f, raise_f, value_m, call_m, bluff) = match skill {
        Skill::Hard => (400, 0.6f64, 0.25f64, 0.22, 0.02, 0.10),
        _ => (150, 0.8, 0.45, 0.28, 0.05, 0.03),
    };
    let fracs: Vec<f64> = (0..v.opponents)
        .map(|o| {
            let (c, rr) = v.opp_actions.get(o).copied().unwrap_or((0, 0));
            (call_f.powi(c as i32) * raise_f.powi(rr as i32)).max(0.04)
        })
        .collect();
    let e = equity_vs(v, samples, rng, &fracs);
    let fair = 1.0 / (v.opponents as f64 + 1.0);
    let needed = v.to_call as f64 / (v.pot + v.to_call).max(1) as f64;
    let pos = if v.late { 0.03 } else { 0.0 };
    let value = e > fair + value_m - pos;
    const SIZE: f64 = 0.66;
    if v.to_call == 0 {
        if value {
            if r < 0.85 { raise(v, SIZE) } else { Move::Check }
        } else if e > fair + 0.08 - pos {
            if r < 0.5 { raise(v, SIZE) } else { Move::Check }
        } else if e < fair && r < bluff + pos && v.legal.can_raise {
            raise(v, SIZE)
        } else {
            Move::Check
        }
    } else if value {
        if r < 0.8 { raise(v, SIZE) } else { Move::Call }
    } else if e > needed + call_m - pos {
        Move::Call
    } else if v.legal.can_raise && r < bluff * 0.5 && v.to_call * 6 < v.stack {
        raise(v, SIZE)
    } else {
        Move::Fold
    }
}

/// Game-theory-optimal style. Not a solved strategy (heads-up hold'em alone is
/// far too big to solve here) but the same principles a solver converges to:
/// position-based preflop ranges, defending at least the minimum defence
/// frequency, and bluffing at the ratio that makes calls indifferent. Every
/// close spot is mixed randomly so there is no pattern to exploit.
fn gto(v: &BotView, rng: &mut ChaCha20Rng, r: f64) -> Move {
    let pos = if v.late { 1.0 } else { 0.0 };
    let facing_raise = v.opp_actions.iter().any(|&(_, raises)| raises > 0);
    let pot_before = v.pot.saturating_sub(v.to_call).max(1) as f64;
    // Fraction of the pot we must defend to stop a pure bluff from profiting.
    let mdf = pot_before / v.pot.max(1) as f64;
    let needed = v.to_call as f64 / (v.pot + v.to_call).max(1) as f64;
    let cheap = (v.to_call as f64) * 6.0 < v.stack as f64;

    if v.board.is_empty() {
        let p = percentile(v.hole);
        // Wider with the button and when fewer players are left to act behind.
        let table = 1.0 / (1.0 + 0.12 * v.opponents.saturating_sub(1) as f64);
        if !facing_raise {
            let open = (0.28 + 0.2 * pos) * table;
            return if p >= 1.0 - open {
                raise(v, 2.0)
            } else if v.to_call == 0 {
                Move::Check
            } else if p >= 1.0 - open - 0.25 && cheap && v.to_call * 3 <= v.pot {
                Move::Call
            } else {
                Move::Fold
            };
        }
        // Polarised 3-bet: premiums for value plus a few suited/connected bluffs.
        let value = 0.06 * table;
        if p >= 1.0 - value || (p > 0.6 && p < 0.78 && r < 0.07 && cheap) {
            return raise(v, 1.0);
        }
        let keep = (mdf * 0.55 * table).clamp(0.05, 0.5);
        return if p >= 1.0 - keep { Move::Call } else { Move::Fold };
    }

    let fracs: Vec<f64> = (0..v.opponents)
        .map(|o| {
            let (c, rr) = v.opp_actions.get(o).copied().unwrap_or((0, 0));
            (0.65f64.powi(c as i32) * 0.3f64.powi(rr as i32)).max(0.05)
        })
        .collect();
    let e = equity_vs(v, 700, rng, &fracs);
    let fair = 1.0 / (v.opponents as f64 + 1.0);
    let river = v.street == Street::River;

    if v.to_call == 0 {
        return if e > fair + 0.38 {
            if r < 0.8 { raise(v, 1.0) } else { Move::Check }
        } else if e > fair + 0.22 {
            if r < 0.85 { raise(v, 0.75) } else { Move::Check }
        } else if e > fair + 0.08 {
            if r < 0.55 { raise(v, 0.33) } else { Move::Check }
        } else if river && e < fair - 0.15 && r < 0.22 {
            // Pure bluff with the worst hands: bet/(pot+2*bet) of the range at 2/3 pot.
            raise(v, 0.66)
        } else if !river && e < fair && e > fair - 0.25 && r < 0.2 + 0.05 * pos {
            // Semi-bluff with hands that can still improve.
            raise(v, 0.66)
        } else {
            Move::Check
        };
    }
    if e > fair + 0.3 {
        if r < 0.6 { raise(v, 0.75) } else { Move::Call }
    } else if e > needed + 0.02 {
        Move::Call
    } else if e > needed * 0.7 && r < mdf * 0.5 {
        // Bluff-catch some marginal hands so a bluff cannot print money.
        Move::Call
    } else if !river && e > needed * 0.5 && v.legal.can_raise && cheap && r < 0.07 {
        raise(v, 0.75)
    } else {
        Move::Fold
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
            late: false,
            opp_actions: vec![],
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
        for (i, skill) in [Skill::Gto, Skill::Medium, Skill::Hard, Skill::Easy].into_iter().enumerate() {
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

    #[test]
    fn narrow_range_lowers_equity() {
        let mut rng = ChaCha20Rng::from_seed([6; 32]);
        let v = view("Ks Qs", "", 100, 0, 1);
        let any = equity_vs(&v, 800, &mut rng, &[]);
        let tight = equity_vs(&v, 800, &mut rng, &[0.05]);
        assert!(tight + 0.1 < any, "any {any} vs top-5% {tight}");
    }

    #[test]
    fn chen_orders_starting_hands() {
        let c = |t: &str| {
            let h: Vec<Card> = t.split_whitespace().map(|c| Card::parse(c).unwrap()).collect();
            chen([h[0], h[1]])
        };
        assert_eq!(c("As Ah"), 20.0);
        assert_eq!(c("As Ks"), 12.0);
        assert!(c("7c 2d") < 0.0 && c("Ts 9s") > c("Ts 4d"));
    }

    /// Hard, playing against Easy and Medium with fresh stacks every hand,
    /// wins chips overall.
    #[test]
    fn hard_beats_the_others() {
        let mut rng = ChaCha20Rng::from_seed([11; 32]);
        let mut t = Table::new(TableParams {
            max_seats: 4,
            small_blind: 10,
            big_blind: 20,
            buyin: 1_000,
            turn_ms: 0,
            tourney: None,
        });
        for (i, skill) in [Skill::Hard, Skill::Easy, Skill::Medium, Skill::Easy].into_iter().enumerate() {
            t.join_bot(NAMES[i], skill).unwrap();
        }
        let mut profit = std::collections::HashMap::<&str, i64>::new();
        for hand in 0..300 {
            for (_, s) in t.players_mut() {
                s.stack = 1_000;
            }
            t.start_hand(hand).unwrap();
            while t.phase == Phase::Betting {
                let seat = t.actor.unwrap();
                let skill = t.seats[seat].as_ref().unwrap().bot.unwrap();
                let v = t.bot_view(seat).unwrap();
                t.apply(seat, decide(skill, &v, &mut rng), 0).unwrap();
            }
            for (_, s) in t.players_mut() {
                let skill = s.bot.unwrap();
                *profit.entry(skill.label()).or_default() += s.stack as i64 - 1_000;
            }
        }
        assert!(profit["Hard"] > profit["Easy"] / 2 && profit["Hard"] > 0, "{profit:?}");
    }

    #[test]
    fn gto_folds_trash_to_a_raise_and_opens_aces() {
        let mut rng = ChaCha20Rng::from_seed([4; 32]);
        let mut folds = 0;
        for _ in 0..100 {
            let mut trash = view("7c 2d", "", 300, 800, 1);
            trash.opp_actions = vec![(0, 1)];
            folds += (decide(Skill::Gto, &trash, &mut rng) == Move::Fold) as u32;
            let aces = view("As Ah", "", 30, 0, 1);
            assert!(matches!(decide(Skill::Gto, &aces, &mut rng), Move::Raise { .. } | Move::AllIn));
        }
        assert!(folds >= 85, "folded trash only {folds}/100 times");
    }

    #[test]
    fn gto_beats_easy() {
        let mut rng = ChaCha20Rng::from_seed([12; 32]);
        let mut t = Table::new(TableParams {
            max_seats: 3,
            small_blind: 10,
            big_blind: 20,
            buyin: 1_000,
            turn_ms: 0,
            tourney: None,
        });
        for (i, skill) in [Skill::Gto, Skill::Easy, Skill::Easy].into_iter().enumerate() {
            t.join_bot(NAMES[i], skill).unwrap();
        }
        let mut gto = 0i64;
        for hand in 0..200 {
            for (_, s) in t.players_mut() {
                s.stack = 1_000;
            }
            t.start_hand(hand).unwrap();
            while t.phase == Phase::Betting {
                let seat = t.actor.unwrap();
                let skill = t.seats[seat].as_ref().unwrap().bot.unwrap();
                let v = t.bot_view(seat).unwrap();
                t.apply(seat, decide(skill, &v, &mut rng), 0).unwrap();
            }
            gto += t.seats[0].as_ref().unwrap().stack as i64 - 1_000;
        }
        assert!(gto > 0, "GTO bot lost {gto} to two Easy bots");
    }
}
