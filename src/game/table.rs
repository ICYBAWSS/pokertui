//! No-limit Texas Hold'em table state machine.
//!
//! The host owns a `Table` and is the only writer. Every public message the host
//! sends carries `view.version`, so clients can drop stale/duplicated frames.

use super::cards::{Card, DECK_LEN, deck_bytes, full_deck, shuffle};
use super::eval::{HandScore, eval_holdem};
use crate::crypto::{deck_commit, random_bytes};
use anyhow::{Result, bail};
use rand_chacha::ChaCha20Rng;
use rand_chacha::rand_core::SeedableRng;
use serde::{Deserialize, Serialize};

pub type SeatId = usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Street {
    Preflop,
    Flop,
    Turn,
    River,
}

impl Street {
    pub fn next(self) -> Option<Street> {
        match self {
            Street::Preflop => Some(Street::Flop),
            Street::Flop => Some(Street::Turn),
            Street::Turn => Some(Street::River),
            Street::River => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Street::Preflop => "preflop",
            Street::Flop => "flop",
            Street::Turn => "turn",
            Street::River => "river",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Lobby,
    Betting,
    HandOver,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "move", rename_all = "snake_case")]
pub enum Move {
    Fold,
    Check,
    Call,
    Raise { to: u64 },
    AllIn,
}

impl Move {
    pub fn label(&self) -> String {
        match self {
            Move::Fold => "fold".into(),
            Move::Check => "check".into(),
            Move::Call => "call".into(),
            Move::Raise { to } => format!("raise to {to}"),
            Move::AllIn => "all-in".into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TableParams {
    pub max_seats: usize,
    pub small_blind: u64,
    pub big_blind: u64,
    pub buyin: u64,
    pub turn_ms: u64,
}

impl Default for TableParams {
    fn default() -> Self {
        Self {
            max_seats: 6,
            small_blind: 25,
            big_blind: 50,
            buyin: 5_000,
            turn_ms: 60_000,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Seat {
    pub name: String,
    pub pubkey: String,
    pub enc_pub: String,
    pub stack: u64,
    pub bet: u64,
    pub contributed: u64,
    pub folded: bool,
    pub all_in: bool,
    pub sitting_out: bool,
    pub connected: bool,
    pub ready: bool,
    pub hole: Vec<Card>,
    pub timeout_streak: u8,
    pub acted: bool,
    pub can_raise: bool,
    pub last_move: Option<String>,
}

impl Seat {
    pub fn new(name: String, pubkey: String, enc_pub: String, stack: u64) -> Self {
        Self {
            name,
            pubkey,
            enc_pub,
            stack,
            bet: 0,
            contributed: 0,
            folded: false,
            all_in: false,
            sitting_out: false,
            connected: true,
            ready: true,
            hole: Vec::new(),
            timeout_streak: 0,
            acted: false,
            can_raise: true,
            last_move: None,
        }
    }

    pub fn in_hand(&self) -> bool {
        !self.folded && !self.hole.is_empty()
    }

    pub fn can_act(&self) -> bool {
        !self.folded && !self.all_in && self.stack > 0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SeatView {
    pub seat: SeatId,
    pub name: String,
    pub stack: u64,
    pub bet: u64,
    pub folded: bool,
    pub all_in: bool,
    pub sitting_out: bool,
    pub connected: bool,
    pub ready: bool,
    pub in_hand: bool,
    pub hole_count: usize,
    pub last_move: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TableView {
    pub room: String,
    /// Public key of the host that authored this view; clients pin it.
    pub host: String,
    pub version: u64,
    pub hand_no: u32,
    pub phase: Phase,
    pub street: Street,
    pub button: usize,
    pub small_blind: u64,
    pub big_blind: u64,
    pub board: Vec<Card>,
    pub pot: u64,
    pub current_bet: u64,
    pub min_raise: u64,
    pub actor: Option<SeatId>,
    /// Legal actions for whoever is to act; lets a client render an exact
    /// action bar without duplicating betting rules.
    pub actor_legal: Option<Legal>,
    pub deadline_ms: u64,
    pub seats: Vec<SeatView>,
    pub last_result: Option<HandResult>,
    pub commit: Option<String>,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HandResult {
    pub hand_no: u32,
    pub board: Vec<Card>,
    pub reveals: Vec<SeatReveal>,
    pub pots: Vec<PotAward>,
    pub deck_nonce: String,
    pub deck: Vec<Card>,
    pub commit: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SeatReveal {
    pub seat: SeatId,
    pub name: String,
    pub cards: Vec<Card>,
    pub hand: String,
    pub won: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PotAward {
    pub amount: u64,
    pub winners: Vec<SeatId>,
    pub desc: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Legal {
    pub to_call: u64,
    pub can_check: bool,
    pub can_call: bool,
    pub can_raise: bool,
    pub can_allin: bool,
    pub min_raise_to: u64,
    pub max_raise_to: u64,
}

#[derive(Clone, Debug)]
pub struct Applied {
    pub note: String,
    pub hand_finished: bool,
    pub street_changed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HandRecord {
    pub room: String,
    pub hand_no: u32,
    pub board: Vec<Card>,
    pub stakes: (u64, u64),
    pub players: Vec<RecordPlayer>,
    pub actions: Vec<String>,
    pub result: HandResult,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecordPlayer {
    pub seat: SeatId,
    pub name: String,
    pub starting_stack: u64,
    pub hole: Vec<Card>,
}

pub struct Table {
    pub params: TableParams,
    pub seats: Vec<Option<Seat>>,
    pub button: usize,
    pub hand_no: u32,
    pub version: u64,
    pub phase: Phase,
    pub street: Street,
    pub board: Vec<Card>,
    deck: Vec<Card>,
    nonce: [u8; 16],
    commit: Option<[u8; 32]>,
    pub actor: Option<SeatId>,
    pub current_bet: u64,
    pub min_raise: u64,
    pub deadline_ms: u64,
    pub last_result: Option<HandResult>,
    pub message: String,
    pub actions: Vec<String>,
    start_stacks: Vec<u64>,
    started_at_ms: u64,
    /// Cursor into `deck`; `deck` always holds the full committed 52 cards so the
    /// reveal can be verified against the commitment.
    draw_idx: usize,
}

impl Table {
    pub fn new(params: TableParams) -> Self {
        let n = params.max_seats.max(2);
        Self {
            params: TableParams {
                max_seats: n,
                ..params
            },
            seats: (0..n).map(|_| None).collect(),
            button: 0,
            hand_no: 0,
            version: 0,
            phase: Phase::Lobby,
            street: Street::Preflop,
            board: Vec::new(),
            deck: Vec::new(),
            nonce: [0u8; 16],
            commit: None,
            actor: None,
            current_bet: 0,
            min_raise: 0,
            deadline_ms: 0,
            last_result: None,
            message: "waiting for players".into(),
            actions: Vec::new(),
            start_stacks: vec![0; n],
            started_at_ms: 0,
            draw_idx: 0,
        }
    }

    pub fn bump(&mut self) {
        self.version += 1;
    }

    pub fn set_message(&mut self, msg: impl Into<String>) {
        self.message = msg.into();
    }

    pub fn note(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        self.message = msg.clone();
        self.actions.push(msg);
    }

    pub fn seated(&self) -> usize {
        self.seats.iter().filter(|s| s.is_some()).count()
    }

    pub fn players(&self) -> impl Iterator<Item = (SeatId, &Seat)> {
        self.seats
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.as_ref().map(|s| (i, s)))
    }

    pub fn players_mut(&mut self) -> impl Iterator<Item = (SeatId, &mut Seat)> {
        self.seats
            .iter_mut()
            .enumerate()
            .filter_map(|(i, s)| s.as_mut().map(|s| (i, s)))
    }

    pub fn seat_of(&self, pubkey: &str) -> Option<SeatId> {
        self.seats
            .iter()
            .position(|s| s.as_ref().is_some_and(|s| s.pubkey == pubkey))
    }

    pub fn join(
        &mut self,
        name: &str,
        pubkey: &str,
        enc_pub: &str,
        buyin: u64,
    ) -> Result<SeatId> {
        if self.phase == Phase::Betting {
            bail!("a hand is in progress");
        }
        if let Some(existing) = self.seat_of(pubkey) {
            return Ok(existing);
        }
        let seat = self
            .seats
            .iter()
            .position(|s| s.is_none())
            .ok_or_else(|| anyhow::anyhow!("table is full"))?;
        let buyin = buyin.min(self.params.buyin.max(1));
        self.seats[seat] = Some(Seat::new(
            name.to_string(),
            pubkey.to_string(),
            enc_pub.to_string(),
            buyin,
        ));
        self.bump();
        Ok(seat)
    }

    pub fn leave(&mut self, seat: SeatId) -> Result<()> {
        let Some(s) = self.seats.get(seat).and_then(|s| s.as_ref()) else {
            bail!("no such seat");
        };
        if self.phase == Phase::Betting && s.in_hand() {
            bail!("cannot leave mid-hand; fold or wait");
        }
        self.seats[seat] = None;
        self.bump();
        Ok(())
    }

    pub fn set_connected(&mut self, seat: SeatId, connected: bool) {
        if let Some(Some(s)) = self.seats.get_mut(seat)
            && s.connected != connected
        {
            s.connected = connected;
            self.bump();
        }
    }

    pub fn set_ready(&mut self, seat: SeatId, ready: bool) {
        if let Some(Some(s)) = self.seats.get_mut(seat) {
            s.ready = ready;
            self.bump();
        }
    }

    pub fn set_deadline(&mut self, deadline_ms: u64) {
        self.deadline_ms = deadline_ms;
    }

    fn eligible(&self) -> Vec<SeatId> {
        self.players()
            .filter(|(_, s)| s.stack > 0 && !s.sitting_out)
            .map(|(i, _)| i)
            .collect()
    }

    fn next_eligible_after(&self, seat: SeatId) -> Option<SeatId> {
        let n = self.seats.len();
        (1..=n)
            .map(|off| (seat + off) % n)
            .find(|i| self.seats[*i].as_ref().is_some_and(|s| s.stack > 0 && !s.sitting_out))
    }

    pub fn start_hand(&mut self, now_ms: u64) -> Result<()> {
        self.start_hand_seeded(now_ms, random_bytes::<32>()?)
    }

    /// Deterministic variant for tests and reproducible replays.
    pub fn start_hand_seeded(&mut self, now_ms: u64, seed: [u8; 32]) -> Result<()> {
        if self.phase == Phase::Betting {
            bail!("hand already in progress");
        }
        let eligible = self.eligible();
        if eligible.len() < 2 {
            bail!("need at least two players with chips");
        }

        // Move the button to the next eligible seat, or to the first eligible seat
        // when the previous button seat is empty.
        self.button = if self.seats[self.button].is_some() && eligible.contains(&self.button) {
            self.next_eligible_after(self.button).unwrap_or(self.button)
        } else {
            *eligible.first().unwrap()
        };

        self.hand_no += 1;
        self.phase = Phase::Betting;
        self.street = Street::Preflop;
        self.board.clear();
        self.actions.clear();
        self.last_result = None;
        self.started_at_ms = now_ms;
        self.commit = None;

        self.start_stacks = self
            .seats
            .iter()
            .map(|s| s.as_ref().map(|s| s.stack).unwrap_or(0))
            .collect();

        for (_, s) in self.players_mut() {
            s.bet = 0;
            s.contributed = 0;
            s.folded = false;
            s.all_in = false;
            s.hole.clear();
            s.acted = false;
            s.can_raise = true;
            s.last_move = None;
            s.timeout_streak = 0;
        }

        let mut deck = full_deck();
        shuffle(&mut deck, &mut ChaCha20Rng::from_seed(seed));
        let nonce = random_bytes::<16>()?;
        self.commit = Some(deck_commit(&nonce, &deck_bytes(&deck)));
        self.nonce = nonce;
        self.deck = deck;
        self.draw_idx = 0;

        // Deal two cards to each player, starting left of the button.
        let order = self.deal_order();
        for _round in 0..2 {
            for &seat in &order {
                let card = self.draw_card().expect("52 cards");
                if let Some(Some(s)) = self.seats.get_mut(seat) {
                    s.hole.push(card);
                }
            }
        }

        let heads_up = eligible.len() == 2;
        let sb = if heads_up {
            self.button
        } else {
            self.next_eligible_after(self.button).expect("eligible seat")
        };
        let bb = self.next_eligible_after(sb).expect("eligible seat");

        let sb_amount = self.params.small_blind;
        let bb_amount = self.params.big_blind;
        self.post_blind(sb, sb_amount);
        self.post_blind(bb, bb_amount);

        self.current_bet = self
            .players()
            .map(|(_, s)| s.bet)
            .max()
            .unwrap_or(0)
            .max(bb_amount);
        self.min_raise = bb_amount;
        self.deadline_ms = 0;

        let first = if heads_up {
            self.button
        } else {
            self.next_eligible_after(bb).unwrap_or(self.button)
        };
        self.actor = self.next_actor_from(first);
        if self.actor.is_none() {
            // Nobody can act (everyone is all-in from the blinds): run the board.
            self.actor = Some(bb);
            self.advance();
        }

        let sb_name = self
            .seats
            .get(sb)
            .and_then(|s| s.as_ref())
            .map(|s| s.name.clone())
            .unwrap_or_default();
        let bb_name = self
            .seats
            .get(bb)
            .and_then(|s| s.as_ref())
            .map(|s| s.name.clone())
            .unwrap_or_default();
        self.note(format!(
            "hand #{} · blinds {}/{} · {} posts SB, {} posts BB",
            self.hand_no, sb_amount, bb_amount, sb_name, bb_name
        ));
        self.bump();
        Ok(())
    }

    fn deal_order(&self) -> Vec<SeatId> {
        let mut order = Vec::new();
        let n = self.seats.len();
        for off in 1..=n {
            let i = (self.button + off) % n;
            if self.seats[i].as_ref().is_some_and(|s| s.stack > 0 && !s.sitting_out) {
                order.push(i);
            }
        }
        order
    }

    fn draw_card(&mut self) -> Option<Card> {
        let card = self.deck.get(self.draw_idx).copied();
        if card.is_some() {
            self.draw_idx += 1;
        }
        card
    }

    fn post_blind(&mut self, seat: SeatId, amount: u64) {
        if let Some(Some(s)) = self.seats.get_mut(seat) {
            let pay = amount.min(s.stack);
            s.stack -= pay;
            s.bet += pay;
            s.contributed += pay;
            if s.stack == 0 {
                s.all_in = true;
            }
            s.last_move = Some(if amount == self.params.small_blind {
                "small blind".into()
            } else {
                "big blind".into()
            });
        }
    }

    pub fn alive(&self) -> Vec<SeatId> {
        self.players()
            .filter(|(_, s)| s.in_hand() && !s.folded)
            .map(|(i, _)| i)
            .collect()
    }

    pub fn pot_total(&self) -> u64 {
        self.players().map(|(_, s)| s.contributed).sum()
    }

    /// Every chip on the table: stacks plus everything committed to the pot.
    /// This must stay constant for the lifetime of a hand.
    pub fn total_chips(&self) -> u64 {
        self.players().map(|(_, s)| s.stack).sum::<u64>() + self.pot_total()
    }

    pub fn legal(&self, seat: SeatId) -> Option<Legal> {
        if self.phase != Phase::Betting || self.actor != Some(seat) {
            return None;
        }
        let s = self.seats.get(seat)?.as_ref()?;
        if !s.can_act() {
            return None;
        }
        let to_call = self.current_bet.saturating_sub(s.bet);
        let max_raise_to = s.bet + s.stack;
        let min_raise_to = self.current_bet + self.min_raise;
        let can_raise = s.can_raise && s.stack > to_call && min_raise_to <= max_raise_to;
        Some(Legal {
            to_call,
            can_check: to_call == 0,
            can_call: to_call > 0,
            can_raise,
            can_allin: s.stack > 0,
            min_raise_to,
            max_raise_to,
        })
    }

    fn needs_action(&self, seat: SeatId) -> bool {
        let Some(s) = self.seats.get(seat).and_then(|s| s.as_ref()) else {
            return false;
        };
        if !s.can_act() {
            return false;
        }
        if s.bet < self.current_bet {
            return true; // owes chips, must call or fold
        }
        if s.acted {
            return false;
        }
        // With at most one player able to act and no bet outstanding there is
        // nobody to bet against: the remaining streets just get dealt.
        let actors = self
            .players()
            .filter(|(_, p)| p.can_act())
            .count();
        actors > 1
    }

    fn next_actor_from(&self, start: SeatId) -> Option<SeatId> {
        let n = self.seats.len();
        (0..n)
            .map(|off| (start + off) % n)
            .find(|i| self.needs_action(*i))
    }

    pub fn apply(&mut self, seat: SeatId, mv: Move, now_ms: u64) -> Result<Applied> {
        if self.phase != Phase::Betting {
            bail!("no hand in progress");
        }
        if self.actor != Some(seat) {
            bail!("not your turn");
        }
        let Some(legal) = self.legal(seat) else {
            bail!("seat cannot act");
        };
        let name = self
            .seats
            .get(seat)
            .and_then(|s| s.as_ref())
            .map(|s| s.name.clone())
            .unwrap_or_default();

        match mv {
            Move::Fold => {
                if let Some(Some(s)) = self.seats.get_mut(seat) {
                    s.folded = true;
                    s.acted = true;
                    s.last_move = Some("fold".into());
                }
                self.note(format!("{name} folds"));
            }
            Move::Check => {
                if legal.to_call > 0 {
                    bail!("cannot check facing a bet of {}", legal.to_call);
                }
                if let Some(Some(s)) = self.seats.get_mut(seat) {
                    s.acted = true;
                    s.last_move = Some("check".into());
                }
                self.note(format!("{name} checks"));
            }
            Move::Call => {
                if legal.to_call == 0 {
                    return self.apply(seat, Move::Check, now_ms);
                }
                let pay = legal.to_call.min(self.stack_of(seat));
                if let Some(Some(s)) = self.seats.get_mut(seat) {
                    s.stack -= pay;
                    s.bet += pay;
                    s.contributed += pay;
                    s.acted = true;
                    if s.stack == 0 {
                        s.all_in = true;
                    }
                    s.last_move = Some(if s.all_in {
                        format!("call {pay} (all-in)")
                    } else {
                        format!("call {pay}")
                    });
                }
                self.note(format!("{name} calls {pay}"));
            }
            Move::Raise { to } => {
                if !legal.can_raise {
                    bail!("raising is not available");
                }
                if to < legal.min_raise_to {
                    bail!("minimum raise is to {}", legal.min_raise_to);
                }
                if to > legal.max_raise_to {
                    bail!("you only have {}", legal.max_raise_to);
                }
                self.raise_to(seat, to);
                self.note(format!("{name} raises to {to}"));
            }
            Move::AllIn => {
                let stack = self.stack_of(seat);
                if stack == 0 {
                    bail!("no chips left");
                }
                let to = self.seats[seat].as_ref().map(|s| s.bet + stack).unwrap_or(stack);
                self.raise_to(seat, to);
                if to > self.current_bet || legal.to_call > 0 {
                    self.note(format!("{name} is all-in for {to}"));
                } else {
                    self.note(format!("{name} is all-in"));
                }
            }
        }

        let before = self.street;
        self.advance();
        let street_changed = self.street != before;
        let hand_finished = self.phase == Phase::HandOver;
        self.bump();
        Ok(Applied {
            note: self.message.clone(),
            hand_finished,
            street_changed,
        })
    }

    fn stack_of(&self, seat: SeatId) -> u64 {
        self.seats
            .get(seat)
            .and_then(|s| s.as_ref())
            .map(|s| s.stack)
            .unwrap_or(0)
    }

    fn raise_to(&mut self, seat: SeatId, to: u64) {
        let (old_bet, stack) = {
            let s = self.seats[seat].as_ref().expect("seat");
            (s.bet, s.stack)
        };
        let target = to.min(old_bet + stack);
        let pay = target - old_bet;
        let raise_size = target.saturating_sub(self.current_bet);
        let full_raise = raise_size >= self.min_raise && raise_size > 0;

        if let Some(Some(s)) = self.seats.get_mut(seat) {
            s.stack -= pay;
            s.bet = target;
            s.contributed += pay;
            s.acted = true;
            s.can_raise = false;
            if s.stack == 0 {
                s.all_in = true;
            }
            s.last_move = Some(if s.all_in {
                format!("all-in {target}")
            } else {
                format!("raise to {target}")
            });
        }

        if target > self.current_bet {
            self.current_bet = target;
            if full_raise {
                self.min_raise = raise_size;
                for (i, s) in self.players_mut() {
                    if i != seat && s.can_act() {
                        s.acted = false;
                        s.can_raise = true;
                    }
                }
            }
            // A short all-in raises the amount to call but does not reopen the
            // betting for players who already acted (their `can_raise` stays false).
        }
    }

    fn advance(&mut self) {
        // After an action, action continues with the next seat. After a street
        // change, action starts with the first active seat left of the button
        // (which is the small blind postflop, or the button itself heads-up).
        let mut start = match self.actor {
            Some(a) => (a + 1) % self.seats.len(),
            None => (self.button + 1) % self.seats.len(),
        };
        loop {
            if self.alive().len() <= 1 {
                self.finish_uncontested();
                return;
            }
            if let Some(next) = self.next_actor_from(start) {
                self.actor = Some(next);
                return;
            }
            if self.street == Street::River {
                self.showdown();
                return;
            }
            self.next_street();
            start = (self.button + 1) % self.seats.len();
        }
    }

    fn next_street(&mut self) {
        let Some(next) = self.street.next() else {
            return;
        };
        self.street = next;
        for (_, s) in self.players_mut() {
            s.bet = 0;
            s.acted = false;
            s.can_raise = true;
            s.last_move = None;
        }
        self.current_bet = 0;
        self.min_raise = self.params.big_blind;
        self.actor = None;

        // Burn one, then deal the board.
        self.draw_card();
        let count = match next {
            Street::Flop => 3,
            Street::Turn | Street::River => 1,
            Street::Preflop => 0,
        };
        for _ in 0..count {
            if let Some(c) = self.draw_card() {
                self.board.push(c);
            }
        }
        self.note(format!("{}: {}", next.label(), self.board_pretty()));
        self.bump();
    }

    pub fn board_pretty(&self) -> String {
        self.board
            .iter()
            .map(|c| c.pretty())
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn finish_uncontested(&mut self) {
        let Some(winner) = self.alive().first().copied() else {
            return;
        };
        let amount = self.pot_total();
        let name = self
            .seats
            .get(winner)
            .and_then(|s| s.as_ref())
            .map(|s| s.name.clone())
            .unwrap_or_default();
        if let Some(Some(s)) = self.seats.get_mut(winner) {
            s.stack += amount;
        }
        let commit = self.commit.map(|c| crate::crypto::b64(&c)).unwrap_or_default();
        let result = HandResult {
            hand_no: self.hand_no,
            board: self.board.clone(),
            reveals: Vec::new(),
            pots: vec![PotAward {
                amount,
                winners: vec![winner],
                desc: format!("{name} wins uncontested"),
            }],
            deck_nonce: crate::crypto::b64(&self.nonce),
            deck: self.deck.clone(),
            commit,
        };
        for (_, s) in self.players_mut() {
            s.bet = 0;
            s.contributed = 0;
        }
        self.last_result = Some(result);
        self.phase = Phase::HandOver;
        self.actor = None;
        self.deadline_ms = 0;
        self.note(format!("{name} wins {amount} uncontested"));
        self.bump();
    }

    fn side_pots(&self) -> Vec<(u64, Vec<SeatId>)> {
        let mut levels: Vec<u64> = self
            .players()
            .filter(|(_, s)| s.contributed > 0)
            .map(|(_, s)| s.contributed)
            .collect();
        levels.sort_unstable();
        levels.dedup();

        let mut pots = Vec::new();
        let mut prev = 0u64;
        for level in levels {
            let amount: u64 = self
                .players()
                .map(|(_, s)| s.contributed.min(level) - s.contributed.min(prev))
                .sum();
            if amount == 0 {
                continue;
            }
            let eligible: Vec<SeatId> = self
                .players()
                .filter(|(_, s)| s.contributed >= level && !s.folded && s.in_hand())
                .map(|(i, _)| i)
                .collect();
            pots.push((amount, eligible));
            prev = level;
        }
        pots
    }

    fn showdown(&mut self) {
        let players = self.alive();
        let mut scores: Vec<(SeatId, HandScore)> = Vec::new();
        for &seat in &players {
            let Some(s) = self.seats[seat].as_ref() else {
                continue;
            };
            if s.hole.len() == 2 && self.board.len() == 5 {
                let score = eval_holdem(
                    [s.hole[0], s.hole[1]],
                    &self.board[..5],
                );
                scores.push((seat, score));
            }
        }

        let mut totals: Vec<(SeatId, u64)> = players.iter().map(|s| (*s, 0)).collect();
        let mut awards = Vec::new();
        for (amount, eligible) in self.side_pots() {
            let contenders: Vec<(SeatId, HandScore)> = scores
                .iter()
                .filter(|(s, _)| eligible.contains(s))
                .cloned()
                .collect();
            let Some((_, best)) = contenders.iter().cloned().max_by_key(|(_, h)| *h) else {
                continue;
            };
            let mut winners: Vec<SeatId> = contenders
                .iter()
                .filter(|(_, h)| *h == best)
                .map(|(s, _)| *s)
                .collect();
            winners.sort_by_key(|w| self.seat_distance_from_button(*w));
            let share = amount / winners.len() as u64;
            let mut remainder = amount % winners.len() as u64;
            for &w in &winners {
                let mut take = share;
                if remainder > 0 {
                    take += 1;
                    remainder -= 1;
                }
                if let Some((_, total)) = totals.iter_mut().find(|(s, _)| *s == w) {
                    *total += take;
                }
            }
            awards.push(PotAward {
                amount,
                winners: winners.clone(),
                desc: best.describe(),
            });
        }

        for (seat, amount) in &totals {
            if let Some(Some(s)) = self.seats.get_mut(*seat) {
                s.stack += *amount;
            }
        }

        let reveals: Vec<SeatReveal> = players
            .iter()
            .filter_map(|&seat| {
                let s = self.seats[seat].as_ref()?;
                let score = scores.iter().find(|(id, _)| *id == seat).map(|(_, h)| *h);
                let won = totals
                    .iter()
                    .find(|(id, _)| *id == seat)
                    .map(|(_, v)| *v)
                    .unwrap_or(0);
                Some(SeatReveal {
                    seat,
                    name: s.name.clone(),
                    cards: s.hole.clone(),
                    hand: score.map(|h| h.describe()).unwrap_or_default(),
                    won,
                })
            })
            .collect();

        let commit = self.commit.map(|c| crate::crypto::b64(&c)).unwrap_or_default();
        let result = HandResult {
            hand_no: self.hand_no,
            board: self.board.clone(),
            reveals,
            pots: awards.clone(),
            deck_nonce: crate::crypto::b64(&self.nonce),
            deck: self.deck.clone(),
            commit,
        };

        for (_, s) in self.players_mut() {
            s.bet = 0;
            s.contributed = 0;
        }
        self.last_result = Some(result);
        self.phase = Phase::HandOver;
        self.actor = None;
        self.deadline_ms = 0;
        let summary = awards
            .iter()
            .map(|a| {
                let names: Vec<String> = a
                    .winners
                    .iter()
                    .filter_map(|w| self.seats[*w].as_ref().map(|s| s.name.clone()))
                    .collect();
                format!("{} → {} ({})", a.amount, names.join(", "), a.desc)
            })
            .collect::<Vec<_>>()
            .join(" · ");
        self.note(format!("showdown: {summary}"));
        self.bump();
    }

    fn seat_distance_from_button(&self, seat: SeatId) -> usize {
        let n = self.seats.len();
        (seat + n - self.button) % n
    }

    pub fn view(&self, room: &str) -> TableView {
        let pot = self.pot_total().saturating_sub(
            self.players().map(|(_, s)| s.bet).sum::<u64>(),
        );
        TableView {
            room: room.to_string(),
            host: String::new(),
            version: self.version,
            hand_no: self.hand_no,
            phase: self.phase,
            street: self.street,
            button: self.button,
            small_blind: self.params.small_blind,
            big_blind: self.params.big_blind,
            board: self.board.clone(),
            pot,
            current_bet: self.current_bet,
            min_raise: self.min_raise,
            actor: self.actor,
            actor_legal: self.actor.and_then(|a| self.legal(a)),
            deadline_ms: self.deadline_ms,
            seats: self
                .seats
                .iter()
                .enumerate()
                .map(|(i, s)| match s {
                    Some(s) => SeatView {
                        seat: i,
                        name: s.name.clone(),
                        stack: s.stack,
                        bet: s.bet,
                        folded: s.folded,
                        all_in: s.all_in,
                        sitting_out: s.sitting_out,
                        connected: s.connected,
                        ready: s.ready,
                        in_hand: s.in_hand(),
                        hole_count: s.hole.len(),
                        last_move: s.last_move.clone(),
                    },
                    None => SeatView {
                        seat: i,
                        name: String::new(),
                        stack: 0,
                        bet: 0,
                        folded: false,
                        all_in: false,
                        sitting_out: false,
                        connected: false,
                        ready: false,
                        in_hand: false,
                        hole_count: 0,
                        last_move: None,
                    },
                })
                .collect(),
            last_result: self.last_result.clone(),
            commit: self.commit.map(|c| crate::crypto::b64(&c)),
            message: self.message.clone(),
        }
    }

    pub fn hand_record(&self, room: &str) -> Option<HandRecord> {
        let result = self.last_result.clone()?;
        Some(HandRecord {
            room: room.to_string(),
            hand_no: self.hand_no,
            board: self.board.clone(),
            stakes: (self.params.small_blind, self.params.big_blind),
            players: self
                .players()
                .map(|(i, s)| RecordPlayer {
                    seat: i,
                    name: s.name.clone(),
                    starting_stack: self.start_stacks.get(i).copied().unwrap_or(0),
                    hole: s.hole.clone(),
                })
                .collect(),
            actions: self.actions.clone(),
            result,
        })
    }

    pub fn started_at_ms(&self) -> u64 {
        self.started_at_ms
    }
}

/// Verify a recorded hand's deck commitment.
pub fn verify_record(record: &HandRecord) -> Result<()> {
    use crate::crypto::{b64_decode, deck_commit};

    let nonce_bytes = b64_decode(&record.result.deck_nonce)?;
    let nonce: [u8; 16] = nonce_bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("nonce must be 16 bytes"))?;
    let deck = &record.result.deck;
    if deck.len() != DECK_LEN {
        bail!("revealed deck has {} cards", deck.len());
    }
    let mut sorted = deck.clone();
    sorted.sort();
    if sorted != full_deck() {
        bail!("revealed deck is not a permutation of 52 cards");
    }
    let commit = deck_commit(&nonce, &deck_bytes(deck));
    if crate::crypto::b64(&commit) != record.result.commit {
        bail!("deck reveal does not match the commitment");
    }
    for card in record.board.iter().chain(
        record
            .result
            .reveals
            .iter()
            .flat_map(|r| r.cards.iter()),
    ) {
        if !deck.contains(card) {
            bail!("card {card} shown on the table is not in the committed deck");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rig(seats: usize, stack: u64) -> Table {
        let mut t = Table::new(TableParams {
            max_seats: seats,
            small_blind: 5,
            big_blind: 10,
            buyin: stack,
            turn_ms: 60_000,
        });
        for i in 0..seats {
            t.join(&format!("p{i}"), &format!("key{i}"), &format!("enc{i}"), stack)
                .unwrap();
        }
        t.start_hand(0).unwrap();
        t
    }

    fn seat(t: &Table, i: usize) -> &Seat {
        t.seats[i].as_ref().unwrap()
    }

    fn actor(t: &Table) -> usize {
        t.actor.expect("someone should be to act")
    }

    #[test]
    fn heads_up_blinds_and_order() {
        let t = rig(2, 1000);
        assert_eq!(t.alive().len(), 2);
        let sb = t.button;
        let bb = (t.button + 1) % 2;
        assert_eq!(seat(&t, sb).bet, 5, "button posts the small blind heads-up");
        assert_eq!(seat(&t, bb).bet, 10);
        assert_eq!(t.actor, Some(sb), "button acts first preflopheads-up");
        assert_eq!(t.current_bet, 10);

        // Everyone calls/checks: heads-up the button acts last postflop.
        let mut t2 = rig(2, 1000);
        t2.apply(sb, Move::Call, 0).unwrap();
        t2.apply(bb, Move::Check, 0).unwrap();
        assert_eq!(t2.street, Street::Flop);
        assert_eq!(t2.actor, Some(bb), "big blind acts first postflop heads-up");
    }

    #[test]
    fn three_handed_button_acts_first_preflop_then_sb_postflop() {
        let t = rig(3, 1000);
        let button = t.button;
        let sb = (button + 1) % 3;
        assert_eq!(t.actor, Some(button), "button is UTG three-handed");
        assert_eq!(seat(&t, sb).bet, 5);

        let mut t = t;
        t.apply(button, Move::Call, 0).unwrap();
        t.apply(sb, Move::Call, 0).unwrap();
        let bb = (button + 2) % 3;
        t.apply(bb, Move::Check, 0).unwrap();
        assert_eq!(t.street, Street::Flop);
        assert_eq!(t.actor, Some(sb), "small blind acts first postflop");
    }

    #[test]
    fn folding_heads_up_awards_the_blinds() {
        let mut t = rig(2, 1000);
        let sb = t.button;
        let bb = (sb + 1) % 2;
        t.apply(sb, Move::Fold, 0).unwrap();
        assert_eq!(t.phase, Phase::HandOver);
        let result = t.last_result.as_ref().unwrap();
        assert_eq!(result.pots.len(), 1);
        assert_eq!(result.pots[0].winners, vec![bb]);
        assert_eq!(result.pots[0].amount, 15);
        // The big blind gets its own 10 back plus the small blind's 5.
        assert_eq!(seat(&t, bb).stack, 1005);
        assert_eq!(t.total_chips(), 2000);
    }

    #[test]
    fn check_down_reaches_showdown_and_conserves_chips() {
        let mut t = rig(2, 1000);
        let sb = t.button;
        let bb = (sb + 1) % 2;
        let mut steps = 0;
        while t.phase == Phase::Betting {
            steps += 1;
            assert!(steps < 20, "hand did not end");
            let a = actor(&t);
            let legal = t.legal(a).unwrap();
            let mv = if legal.to_call > 0 {
                Move::Call
            } else {
                Move::Check
            };
            t.apply(a, mv, 0).unwrap();
        }
        assert_eq!(t.phase, Phase::HandOver);
        assert_eq!(t.board.len(), 5);
        let result = t.last_result.as_ref().unwrap();
        assert_eq!(result.reveals.len(), 2);
        assert_eq!(result.pots.iter().map(|p| p.amount).sum::<u64>(), 20);
        assert_eq!(t.total_chips(), 2000);
    }

    #[test]
    fn side_pots_respect_all_in_levels() {
        let mut t = rig(3, 1000);
        for (i, contributed) in [(0usize, 100u64), (1, 50), (2, 100)] {
            t.seats[i].as_mut().unwrap().contributed = contributed;
        }
        let pots = t.side_pots();
        assert_eq!(pots.len(), 2);
        assert_eq!(pots[0], (150, vec![0, 1, 2]));
        assert_eq!(pots[1], (100, vec![0, 2]));

        // A folded player's chips stay in the pot but they cannot win it.
        t.seats[1].as_mut().unwrap().folded = true;
        let pots = t.side_pots();
        assert_eq!(pots[0], (150, vec![0, 2]));
    }

    #[test]
    fn all_in_for_less_cannot_reopen_betting() {
        let mut t = rig(3, 1000);
        let a = actor(&t);
        // Simulate a bet of 100 and a 40-chip all-in raise: not a full raise.
        t.current_bet = 100;
        t.min_raise = 100;
        let seat_under = t.seats[a].as_mut().unwrap();
        seat_under.bet = 60;
        seat_under.stack = 40;
        seat_under.acted = true;
        seat_under.can_raise = false;
        // 40 more would be a raise of only 40 < min_raise 100, so no re-raise.
        assert!(!t.legal(a).map(|l| l.can_raise).unwrap_or(true));
    }

    #[test]
    fn out_of_turn_actions_are_rejected() {
        let mut t = rig(3, 1000);
        let a = actor(&t);
        let other = (a + 1) % 3;
        assert!(t.apply(other, Move::Fold, 0).is_err());
        assert!(t.apply(a, Move::Raise { to: 1 }, 0).is_err()); // below min raise
    }

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }
    }

    #[test]
    fn random_hands_conserve_chips_and_terminate() {
        let seats = 4;
        let buyin = 2_000;
        let total = (seats as u64) * buyin;
        for seed_i in 0..250u64 {
            let mut t = Table::new(TableParams {
                max_seats: seats,
                small_blind: 5,
                big_blind: 10,
                buyin,
                turn_ms: 60_000,
            });
            for i in 0..seats {
                t.join(&format!("p{i}"), &format!("key{i}"), &format!("enc{i}"), buyin)
                    .unwrap();
            }
            let mut rng = Lcg(0xC0FFEE ^ seed_i);
            for hand in 0..25 {
                if t.eligible().len() < 2 {
                    break;
                }
                let mut seed = [0u8; 32];
                seed[..8].copy_from_slice(&seed_i.to_le_bytes());
                seed[8..16].copy_from_slice(&(hand as u64).to_le_bytes());
                t.start_hand_seeded(hand, seed).unwrap();
                let mut steps = 0;
                let mut trace: Vec<String> = Vec::new();
                while t.phase == Phase::Betting {
                    steps += 1;
                    assert!(steps < 5_000, "seed {seed_i} hand {hand}: no termination\n{trace:?}");
                    let a = actor(&t);
                    let legal = t.legal(a).unwrap();
                    let choice = rng.next() % 12;
                    let mv = if choice == 0 {
                        Move::Fold
                    } else if choice <= 3 && legal.can_raise {
                        let lo = legal.min_raise_to;
                        let hi = legal.max_raise_to;
                        Move::Raise {
                            to: lo + rng.next() % (hi - lo + 1),
                        }
                    } else if choice == 4 {
                        Move::AllIn
                    } else if legal.to_call > 0 {
                        Move::Call
                    } else {
                        Move::Check
                    };
                    let label = mv.label();
                    trace.push(format!(
                        "seat{a} {label} (street {:?}, bet {}, actor {:?})",
                        t.street, t.current_bet, t.actor
                    ));
                    if let Err(e) = t.apply(a, mv, 0) {
                        panic!("seed {seed_i} hand {hand}: rejected: {e}\n{trace:?}");
                    }
                    if t.total_chips() != total {
                        let dump = t
                            .players()
                            .map(|(i, s)| {
                                format!(
                                    "s{i} stack={} bet={} contrib={} folded={} allin={} hole={:?}",
                                    s.stack, s.bet, s.contributed, s.folded, s.all_in, s.hole
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        panic!(
                            "seed {seed_i} hand {hand}: chip conservation broken ({}/{})\n{dump}\nlast={:?}\n{trace:?}",
                            t.total_chips(),
                            total,
                            t.last_result
                        );
                    }
                }
                let result = t.last_result.clone().unwrap_or_else(|| {
                    panic!("seed {seed_i} hand {hand}: finished without a result\n{trace:?}")
                });
                assert!(
                    matches!(t.board.len(), 0 | 3 | 4 | 5),
                    "seed {seed_i} hand {hand}: odd board {}\n{trace:?}",
                    t.board.len()
                );
                let paid: u64 = result.pots.iter().map(|p| p.amount).sum();
                assert!(paid > 0, "seed {seed_i} hand {hand}: empty pot");
                assert_eq!(
                    t.total_chips(),
                    total,
                    "seed {seed_i} hand {hand}: chips vanished after payout\n{trace:?}"
                );
            }
        }
    }

    #[test]
    fn hand_record_verifies_and_detects_tampering() {
        let mut t = rig(2, 1000);
        while t.phase == Phase::Betting {
            let a = actor(&t);
            let legal = t.legal(a).unwrap();
            t.apply(
                a,
                if legal.to_call > 0 { Move::Call } else { Move::Check },
                0,
            )
            .unwrap();
        }
        let record = t.hand_record("room").unwrap();
        verify_record(&record).unwrap();

        let mut tampered = record.clone();
        tampered.result.deck.swap(0, 1);
        assert!(verify_record(&tampered).is_err());

        // Removing the commit is also detected.
        let mut tampered = record.clone();
        tampered.result.commit = "AAAA".into();
        assert!(verify_record(&tampered).is_err());
    }
}
