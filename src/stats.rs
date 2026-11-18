//! Your own poker statistics. Every hand you play is boiled down to one small
//! `HandStat`, appended to `hands.jsonl` in the data folder; everything shown
//! on the stats screen is recomputed from that log, so each number can be
//! checked against its formula.

use crate::game::{Card, Move, Street, TableView, eval_holdem, record_from_view, replay_steps};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::Write;

/// Seat names, as shown in tables: index = `HandStat::pos`.
pub const POSITIONS: [&str; 6] = ["Button", "Small blind", "Big blind", "Early", "Middle", "Cutoff"];
/// Gap between two hands that starts a new session.
const SESSION_GAP_MS: u64 = 30 * 60 * 1000;

/// One hand, from your seat.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HandStat {
    pub room: String,
    pub hand_no: u32,
    pub ts_ms: u64,
    /// Big blind, so results can be compared across stakes.
    pub bb: u64,
    pub players: u8,
    pub bots: u8,
    pub pos: u8,
    /// Hole cards as card ids (rank = id / 4, suit = id % 4).
    pub hole: [u8; 2],
    /// Chips won (+) or lost (-).
    pub net: i64,
    /// Chips you put in the pot.
    pub invested: u64,
    /// Total chips in all pots.
    pub pot: u64,
    pub vpip: bool,
    pub pfr: bool,
    pub bet3: bool,
    pub bet3_opp: bool,
    pub cbet: bool,
    pub cbet_opp: bool,
    /// Last street you were still in on: 0 preflop … 3 river.
    pub reached: u8,
    pub showdown: bool,
    pub allin: bool,
    /// Your final hand category (0..=8) when you reached showdown.
    pub cat: Option<u8>,
    /// Postflop actions.
    pub aggr: u8,
    pub calls: u8,
    pub checks: u8,
    pub folds: u8,
}

impl HandStat {
    pub fn net_bb(&self) -> f64 {
        self.net as f64 / self.bb.max(1) as f64
    }
    pub fn won(&self) -> bool {
        self.net > 0
    }
}

fn is_raise(t: &crate::game::Table, seat: usize, mv: &Move) -> bool {
    match mv {
        Move::Raise { .. } => true,
        Move::AllIn => t.seats[seat].as_ref().is_some_and(|s| s.bet + s.stack > t.current_bet),
        _ => false,
    }
}

/// Where `me` sat relative to the button among the seats dealt in.
fn position(dealt: &[usize], button: usize, total: usize, me: usize) -> u8 {
    let n = dealt.len();
    let mut order = dealt.to_vec();
    order.sort_by_key(|s| (s + total - button) % total);
    let d = order.iter().position(|s| *s == me).unwrap_or(0);
    match (n, d) {
        (2, 0) => 0,
        (2, _) => 2,
        (_, 0) => 0,
        (_, 1) => 1,
        (_, 2) => 2,
        (n, d) if d == n - 1 => 5,
        (n, d) if d < 3 + (n - 2) / 2 => 3,
        _ => 4,
    }
}

/// Boil a finished hand down to what happened to `me`. None if you weren't dealt in.
pub fn analyze(view: &TableView, me: usize) -> Option<HandStat> {
    let rec = record_from_view(view)?;
    let result = &rec.result;
    let (_, start, end) = *result.stacks.iter().find(|(s, _, _)| *s == me)?;
    let dealt: Vec<usize> = result.stacks.iter().map(|(s, _, _)| *s).collect();
    let bots = dealt.iter().filter(|s| view.seats.get(**s).is_some_and(|v| v.bot)).count() as u8;

    let mut h = HandStat {
        room: view.room.clone(),
        hand_no: result.hand_no,
        ts_ms: crate::net::protocol::now_ms(),
        bb: view.big_blind,
        players: dealt.len() as u8,
        bots,
        pos: position(&dealt, result.button, view.seats.len().max(2), me),
        net: end as i64 - start as i64,
        pot: result.pots.iter().map(|p| p.amount).sum(),
        ..Default::default()
    };

    // Replay the hand and read each of your decisions in context.
    let (mut street, mut raises, mut pre_raises) = (Street::Preflop, 0u32, 0u32);
    let mut folded_at: Option<u8> = None;
    let mut my_first_pre = true;
    let table = replay_steps(&rec, |t, seat, mv| {
        if t.street != street {
            street = t.street;
            raises = 0;
        }
        let raise = is_raise(t, seat, mv);
        if seat == me {
            let my_bet = t.seats[me].as_ref().map(|s| s.bet).unwrap_or(0);
            if matches!(mv, Move::AllIn) {
                h.allin = true;
            }
            if street == Street::Preflop {
                if my_first_pre && pre_raises >= 1 {
                    h.bet3_opp = true;
                }
                my_first_pre = false;
                match mv {
                    Move::Call if t.current_bet > my_bet => h.vpip = true,
                    _ if raise => {
                        h.vpip = true;
                        h.pfr = true;
                        if pre_raises >= 1 {
                            h.bet3 = true;
                        }
                    }
                    _ => {}
                }
            } else {
                if street == Street::Flop && h.pfr && raises == 0 {
                    // You raised preflop and nobody has bet yet: your continuation-bet spot.
                    h.cbet_opp = true;
                    h.cbet = raise;
                }
                match mv {
                    _ if raise => h.aggr += 1,
                    Move::Call => h.calls += 1,
                    Move::Check => h.checks += 1,
                    _ => {}
                }
            }
            if matches!(mv, Move::Fold) {
                if street != Street::Preflop {
                    h.folds += 1;
                }
                folded_at = Some(street as u8);
            }
        }
        if raise {
            raises += 1;
            if street == Street::Preflop {
                pre_raises += 1;
            }
        }
    })
    .ok()?;

    let mine = table.seats.get(me)?.as_ref()?;
    if mine.hole.len() == 2 {
        h.hole = [mine.hole[0].0, mine.hole[1].0];
    }
    h.invested = mine.contributed;
    h.allin |= mine.all_in;
    h.reached = folded_at.unwrap_or(match result.board.len() {
        0 => 0,
        3 => 1,
        4 => 2,
        _ => 3,
    });
    h.showdown = result.reveals.iter().any(|r| r.seat == me);
    if h.showdown && mine.hole.len() == 2 && result.board.len() == 5 {
        h.cat = Some(eval_holdem([mine.hole[0], mine.hole[1]], &result.board).cat);
    }
    Some(h)
}

// ---------------------------------------------------------------- storage

fn log_path() -> Option<std::path::PathBuf> {
    Some(crate::store::data_dir().ok()?.join("hands.jsonl"))
}

pub fn append(h: &HandStat) {
    let Some(path) = log_path() else { return };
    let Ok(line) = serde_json::to_string(h) else { return };
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{line}");
    }
}

/// Every saved hand, oldest first, each (room, hand) counted once.
pub fn load() -> Vec<HandStat> {
    let Some(text) = log_path().and_then(|p| std::fs::read_to_string(p).ok()) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    text.lines()
        .filter_map(|l| serde_json::from_str::<HandStat>(l).ok())
        .filter(|h| seen.insert((h.room.clone(), h.hand_no)))
        .collect()
}

// ---------------------------------------------------------------- maths

/// Standard normal CDF (Abramowitz & Stegun 7.1.26 for erf).
pub fn phi(z: f64) -> f64 {
    let x = z.abs() / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + 0.3275911 * x);
    let poly = t * (0.254829592 + t * (-0.284496736 + t * (1.421413741 + t * (-1.453152027 + t * 1.061405429))));
    let erf = 1.0 - poly * (-x * x).exp();
    0.5 * (1.0 + if z < 0.0 { -erf } else { erf })
}

fn pct(num: usize, den: usize) -> Option<f64> {
    (den > 0).then(|| 100.0 * num as f64 / den as f64)
}

/// Everything computed from a set of hands.
#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub n: usize,
    pub net_bb: f64,
    pub bb100: f64,
    pub sd: f64,
    pub ci: f64,
    /// P(true win rate > 0) under a normal approximation.
    pub p_winner: f64,
    pub win_pct: f64,
    pub vpip: Option<f64>,
    pub pfr: Option<f64>,
    pub bet3: Option<f64>,
    pub af: Option<f64>,
    pub afq: Option<f64>,
    pub cbet: Option<f64>,
    pub saw_flop: Option<f64>,
    pub saw_turn: Option<f64>,
    pub saw_river: Option<f64>,
    pub wtsd: Option<f64>,
    pub wsd: Option<f64>,
    pub wwsf: Option<f64>,
    pub allin_pct: Option<f64>,
    pub allin_win: Option<f64>,
    pub avg_pot: f64,
    pub avg_invested: f64,
    pub biggest_win: f64,
    pub biggest_loss: f64,
    pub biggest_pot: f64,
    pub profit_factor: Option<f64>,
    pub drawdown: f64,
    pub win_streak: usize,
    pub lose_streak: usize,
    pub sessions: Vec<(usize, f64, u64)>, // (hands, net bb, minutes)
    pub per_hour: Option<f64>,
}

pub fn summarize(hands: &[&HandStat]) -> Summary {
    let n = hands.len();
    let mut s = Summary { n, ..Default::default() };
    if n == 0 {
        return s;
    }
    let x: Vec<f64> = hands.iter().map(|h| h.net_bb()).collect();
    s.net_bb = x.iter().sum();
    let mean = s.net_bb / n as f64;
    s.bb100 = 100.0 * mean;
    s.sd = if n > 1 { (x.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n as f64 - 1.0)).sqrt() } else { 0.0 };
    s.ci = if n > 1 { 100.0 * 1.96 * s.sd / (n as f64).sqrt() } else { 0.0 };
    s.p_winner = if s.sd > 0.0 { phi(mean * (n as f64).sqrt() / s.sd) } else { 0.5 };
    s.win_pct = 100.0 * hands.iter().filter(|h| h.won()).count() as f64 / n as f64;

    let count = |f: &dyn Fn(&HandStat) -> bool| hands.iter().filter(|h| f(h)).count();
    s.vpip = pct(count(&|h| h.vpip), n);
    s.pfr = pct(count(&|h| h.pfr), n);
    s.bet3 = pct(count(&|h| h.bet3), count(&|h| h.bet3_opp));
    let (aggr, calls, checks, folds) = hands.iter().fold((0usize, 0usize, 0usize, 0usize), |a, h| {
        (a.0 + h.aggr as usize, a.1 + h.calls as usize, a.2 + h.checks as usize, a.3 + h.folds as usize)
    });
    let _ = checks;
    s.af = (calls > 0).then(|| aggr as f64 / calls as f64);
    s.afq = pct(aggr, aggr + calls + folds);
    s.cbet = pct(count(&|h| h.cbet), count(&|h| h.cbet_opp));
    let flop = count(&|h| h.reached >= 1);
    s.saw_flop = pct(flop, n);
    s.saw_turn = pct(count(&|h| h.reached >= 2), n);
    s.saw_river = pct(count(&|h| h.reached >= 3), n);
    let sd_hands = count(&|h| h.showdown);
    s.wtsd = pct(sd_hands, flop);
    s.wsd = pct(count(&|h| h.showdown && h.won()), sd_hands);
    s.wwsf = pct(count(&|h| h.reached >= 1 && h.won()), flop);
    let ai = count(&|h| h.allin);
    s.allin_pct = pct(ai, n);
    s.allin_win = pct(count(&|h| h.allin && h.won()), ai);
    let bb = |h: &HandStat, v: u64| v as f64 / h.bb.max(1) as f64;
    s.avg_pot = hands.iter().map(|h| bb(h, h.pot)).sum::<f64>() / n as f64;
    s.avg_invested = hands.iter().map(|h| bb(h, h.invested)).sum::<f64>() / n as f64;
    s.biggest_pot = hands.iter().map(|h| bb(h, h.pot)).fold(0.0, f64::max);
    s.biggest_win = x.iter().cloned().fold(0.0, f64::max);
    s.biggest_loss = x.iter().cloned().fold(0.0, f64::min);
    let (gain, loss) = x.iter().fold((0.0, 0.0), |(g, l), v| if *v > 0.0 { (g + v, l) } else { (g, l - v) });
    s.profit_factor = (loss > 0.0).then(|| gain / loss);

    let (mut cum, mut peak, mut cur_w, mut cur_l) = (0.0f64, 0.0f64, 0usize, 0usize);
    for v in &x {
        cum += v;
        peak = peak.max(cum);
        s.drawdown = s.drawdown.max(peak - cum);
        if *v > 0.0 {
            cur_w += 1;
            cur_l = 0;
        } else if *v < 0.0 {
            cur_l += 1;
            cur_w = 0;
        }
        s.win_streak = s.win_streak.max(cur_w);
        s.lose_streak = s.lose_streak.max(cur_l);
    }

    // Sessions: a gap of 30 minutes or more starts a new one.
    let mut start = 0;
    for i in 1..=n {
        if i == n || hands[i].ts_ms.saturating_sub(hands[i - 1].ts_ms) >= SESSION_GAP_MS {
            let mins = (hands[i - 1].ts_ms.saturating_sub(hands[start].ts_ms) / 60_000).max(1);
            s.sessions.push((i - start, x[start..i].iter().sum(), mins));
            start = i;
        }
    }
    let total_min: u64 = s.sessions.iter().map(|s| s.2).sum();
    s.per_hour = (total_min > 0).then(|| n as f64 * 60.0 / total_min as f64);
    s
}

// ---------------------------------------------------------------- series

/// Running total in big blinds, plus the same split into hands that reached showdown and those that didn't.
pub fn cumulative(hands: &[&HandStat]) -> (Vec<(f64, f64)>, Vec<(f64, f64)>, Vec<(f64, f64)>) {
    let (mut all, mut sd, mut nsd) = (vec![(0.0, 0.0)], vec![(0.0, 0.0)], vec![(0.0, 0.0)]);
    let (mut a, mut b, mut c) = (0.0, 0.0, 0.0);
    for (i, h) in hands.iter().enumerate() {
        let v = h.net_bb();
        a += v;
        if h.showdown {
            b += v;
        } else {
            c += v;
        }
        let x = (i + 1) as f64;
        all.push((x, a));
        sd.push((x, b));
        nsd.push((x, c));
    }
    (all, sd, nsd)
}

/// Rolling average of `f` over the last `w` hands.
pub fn rolling(hands: &[&HandStat], w: usize, f: impl Fn(&HandStat) -> f64) -> Vec<(f64, f64)> {
    let vals: Vec<f64> = hands.iter().map(|h| f(h)).collect();
    (w.min(vals.len()).max(1)..=vals.len())
        .map(|end| {
            let from = end.saturating_sub(w);
            (end as f64, vals[from..end].iter().sum::<f64>() / (end - from) as f64)
        })
        .collect()
}

/// (hands, bb/100) for each seat position.
pub fn by_position(hands: &[&HandStat]) -> Vec<(usize, f64)> {
    (0..POSITIONS.len() as u8)
        .map(|p| {
            let hs: Vec<_> = hands.iter().filter(|h| h.pos == p).collect();
            let net: f64 = hs.iter().map(|h| h.net_bb()).sum();
            (hs.len(), if hs.is_empty() { 0.0 } else { 100.0 * net / hs.len() as f64 })
        })
        .collect()
}

/// Starting-hand class: (row, col) in the 13×13 grid, A high at 0. Pairs on the
/// diagonal, suited above it, offsuit below.
pub fn grid_cell(hole: [u8; 2]) -> (usize, usize) {
    let (a, b) = (Card(hole[0]), Card(hole[1]));
    let (ra, rb) = (a.0 / 4, b.0 / 4);
    let (hi, lo) = (12 - ra.max(rb) as usize, 12 - ra.min(rb) as usize);
    if a.0 % 4 == b.0 % 4 { (hi, lo) } else { (lo, hi) }
}

/// (hands, total bb) per grid cell.
pub fn grid(hands: &[&HandStat]) -> [[(usize, f64); 13]; 13] {
    let mut g = [[(0usize, 0.0f64); 13]; 13];
    for h in hands.iter().filter(|h| h.hole != [0, 0]) {
        let (r, c) = grid_cell(h.hole);
        g[r][c].0 += 1;
        g[r][c].1 += h.net_bb();
    }
    g
}

pub fn grid_label(r: usize, c: usize) -> String {
    const R: [char; 13] = ['A', 'K', 'Q', 'J', 'T', '9', '8', '7', '6', '5', '4', '3', '2'];
    let suffix = if r == c { "" } else if c > r { "s" } else { "o" };
    let (hi, lo) = (r.min(c), r.max(c));
    format!("{}{}{}", R[hi], R[lo], suffix)
}

// ---------------------------------------------------------------- rows

pub struct Row {
    /// Long name, used in the formulas list.
    pub name: &'static str,
    /// Short label for the cards.
    pub short: &'static str,
    pub value: String,
    /// One plain sentence.
    pub meaning: &'static str,
    pub formula: &'static str,
    pub typical: &'static str,
}

fn p(v: Option<f64>) -> String {
    v.map(|v| format!("{v:.0}%")).unwrap_or_else(|| "—".into())
}

/// The statistics worth showing, in plain words, each with its formula.
/// The order matters: the stats screen lays the first three out as headline cards.
pub fn rows(s: &Summary) -> Vec<(&'static str, Vec<Row>)> {
    let r = |name, short, value: String, meaning, formula, typical| Row { name, short, value, meaning, formula, typical };
    vec![
        (
            "RESULTS",
            vec![
                r("Total won or lost", "Total", format!("{:+.1} bb", s.net_bb), "all you've won or lost", "Σ (chips won or lost ÷ big blind) over all your hands", ""),
                r(
                    "Win rate",
                    "Win rate",
                    format!("{:+.1} bb/100", s.bb100),
                    "big blinds won per 100 hands",
                    "100 × your average result per hand;  ± is the 95% range: 100 × 1.96 × σ ÷ √n",
                    "+2 to +10 is strong",
                ),
                r("Chance you're really a winner", "Winner odds", format!("{:.0}%", 100.0 * s.p_winner), "how sure we can be that you beat the game", "Φ( average × √n ÷ σ ),  Φ = the normal curve", "50% = too early to tell"),
                r("Biggest win", "Biggest win", format!("{:+.1} bb", s.biggest_win), "your best single hand", "your best single hand, in big blinds", ""),
                r("Worst losing stretch", "Worst slump", format!("{:.1} bb", s.drawdown), "biggest drop from your peak", "largest drop from a peak in your running total", ""),
            ],
        ),
        (
            "HOW YOU PLAY",
            vec![
                r("How loose you play (VPIP)", "Loose (VPIP)", p(s.vpip), "hands you play before the flop", "hands where you put chips in before the flop ÷ all hands", "tight 15-22%, loose 30%+"),
                r("How often you raise (PFR)", "Raises (PFR)", p(s.pfr), "hands you raise before the flop", "hands where you raised before the flop ÷ all hands", "12-18%"),
                r("How aggressive (AF)", "Aggression", s.af.map(|v| format!("{v:.1}")).unwrap_or_else(|| "—".into()), "bets and raises per call, after the flop", "(bets + raises) ÷ calls, after the flop", "2 to 3"),
            ],
        ),
        (
            "SHOWDOWNS",
            vec![
                r("Hands that reach showdown (WTSD)", "Go to showdown", p(s.wtsd), "of the hands that see a flop", "showdowns ÷ hands that saw the flop", "25-30%"),
                r("Showdowns you win (W$SD)", "Showdown wins", p(s.wsd), "of the showdowns you reach", "showdowns won ÷ showdowns", "50-55%"),
            ],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot::Skill;
    use crate::game::{Phase, Table, TableParams};

    fn hs(net: i64, bb: u64) -> HandStat {
        HandStat { bb, net, players: 2, ..Default::default() }
    }

    #[test]
    fn win_rate_and_interval_follow_the_formulas() {
        let hands = [hs(100, 10), hs(-50, 10), hs(20, 10)]; // +10, -5, +2 bb
        let refs: Vec<&HandStat> = hands.iter().collect();
        let s = summarize(&refs);
        assert!((s.net_bb - 7.0).abs() < 1e-9);
        assert!((s.bb100 - 100.0 * 7.0 / 3.0).abs() < 1e-9);
        let mean = 7.0 / 3.0;
        let var = ((10.0f64 - mean).powi(2) + (-5.0f64 - mean).powi(2) + (2.0f64 - mean).powi(2)) / 2.0;
        assert!((s.sd - var.sqrt()).abs() < 1e-9);
        assert!((s.ci - 100.0 * 1.96 * var.sqrt() / 3f64.sqrt()).abs() < 1e-9);
        assert!((s.profit_factor.unwrap() - 12.0 / 5.0).abs() < 1e-9);
        assert!((s.drawdown - 5.0).abs() < 1e-9);
    }

    #[test]
    fn normal_cdf_is_right() {
        assert!((phi(0.0) - 0.5).abs() < 1e-6);
        assert!((phi(1.96) - 0.975).abs() < 1e-3);
        assert!((phi(-1.0) - 0.1587).abs() < 1e-3);
    }

    #[test]
    fn grid_puts_pairs_on_the_diagonal_and_suited_above() {
        let c = |r: u8, s: u8| Card::new(r, s).0;
        assert_eq!(grid_cell([c(12, 0), c(12, 1)]), (0, 0)); // AA
        assert_eq!(grid_cell([c(12, 0), c(11, 0)]), (0, 1)); // AKs
        assert_eq!(grid_cell([c(12, 0), c(11, 1)]), (1, 0)); // AKo
        assert_eq!(grid_label(0, 1), "AKs");
        assert_eq!(grid_label(1, 0), "AKo");
        assert_eq!(grid_label(2, 2), "QQ");
    }

    /// Play a real hand and read the flags back from the finished view.
    #[test]
    fn analyze_reads_preflop_actions() {
        let mut t = Table::new(TableParams { max_seats: 2, small_blind: 5, big_blind: 10, buyin: 1_000, turn_ms: 0, tourney: None });
        t.join_bot("a", Skill::Easy).unwrap();
        t.join_bot("b", Skill::Easy).unwrap();
        t.start_hand(0).unwrap();
        let opener = t.actor.unwrap(); // heads-up: the button acts first
        t.apply(opener, Move::Raise { to: 30 }, 0).unwrap();
        let bb = t.actor.unwrap();
        t.apply(bb, Move::Fold, 0).unwrap();
        assert_eq!(t.phase, Phase::HandOver);
        let view = t.view("room");

        let me = analyze(&view, opener).unwrap();
        assert!(me.vpip && me.pfr && !me.bet3);
        assert_eq!(me.net, 10);
        assert_eq!(me.pos, 0);
        assert_eq!(me.reached, 0);
        assert!(!me.showdown);
        assert_ne!(me.hole, [0, 0]);

        let other = analyze(&view, bb).unwrap();
        assert!(!other.vpip && !other.pfr);
        assert_eq!(other.net, -10);
        assert_eq!(other.pos, 2);
    }
}
