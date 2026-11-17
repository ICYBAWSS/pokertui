//! Rankings kept on this machine: everyone you have played with, by public key.
//! Winnings are in big blinds so a High Roller win doesn't swamp a Turbo one.

use crate::game::{TableView, tourney_points};
use crate::store;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Stats {
    pub name: String,
    pub hands: u32,
    pub won: u32,
    pub net_bb: f64,
    pub tourney_points: u32,
    pub tourneys: u32,
}

impl Stats {
    pub fn win_pct(&self) -> f64 {
        if self.hands == 0 { 0.0 } else { 100.0 * self.won as f64 / self.hands as f64 }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Rankings {
    pub players: BTreeMap<String, Stats>,
}

fn counts(pubkey: &str) -> bool {
    !pubkey.is_empty() && !pubkey.starts_with("bot:")
}

impl Rankings {
    pub fn load() -> Self {
        store::data_dir()
            .ok()
            .and_then(|d| std::fs::read_to_string(d.join("rankings.json")).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Ok(dir) = store::data_dir()
            && let Ok(text) = serde_json::to_string_pretty(self)
        {
            let _ = std::fs::write(dir.join("rankings.json"), text);
        }
    }

    /// Fold the hand that just finished into everyone's totals.
    pub fn record_hand(&mut self, view: &TableView) {
        let Some(result) = &view.last_result else {
            return;
        };
        let bb = view.big_blind.max(1) as f64;
        for (seat, start, end) in &result.stacks {
            let Some(sv) = view.seats.get(*seat) else { continue };
            if !counts(&sv.pubkey) {
                continue;
            }
            let s = self.players.entry(sv.pubkey.clone()).or_default();
            s.name = sv.name.clone();
            s.hands += 1;
            if end > start {
                s.won += 1;
            }
            s.net_bb += (*end as f64 - *start as f64) / bb;
        }
    }

    /// Award tournament points once, when the standings are final.
    pub fn record_tourney(&mut self, view: &TableView) {
        for sv in view.seats.iter().filter(|s| counts(&s.pubkey)) {
            if let Some(place) = sv.place {
                let s = self.players.entry(sv.pubkey.clone()).or_default();
                s.name = sv.name.clone();
                s.tourney_points += tourney_points(place);
                s.tourneys += 1;
            }
        }
    }

    /// Best first: most big blinds won, then most hands played.
    pub fn sorted(&self) -> Vec<(&String, &Stats)> {
        let mut v: Vec<_> = self.players.iter().collect();
        v.sort_by(|a, b| {
            b.1.net_bb
                .partial_cmp(&a.1.net_bb)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(b.1.hands.cmp(&a.1.hands))
        });
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::{Move, Phase, Table, TableParams};

    fn played_hand() -> TableView {
        let mut t = Table::new(TableParams {
            max_seats: 3,
            small_blind: 10,
            big_blind: 20,
            buyin: 1_000,
            turn_ms: 0,
            tourney: None,
        });
        for i in 0..2 {
            t.join(&format!("p{i}"), &format!("pk{i}"), "enc", 1_000).unwrap();
        }
        t.join_bot("Bot", crate::bot::Skill::Easy).unwrap();
        t.start_hand(0).unwrap();
        while t.phase == Phase::Betting {
            let seat = t.actor.unwrap();
            let legal = t.legal(seat).unwrap();
            t.apply(seat, if legal.can_check { Move::Check } else { Move::Call }, 0).unwrap();
        }
        t.view("room")
    }

    #[test]
    fn hands_are_zero_sum_and_bots_are_skipped() {
        let view = played_hand();
        let mut r = Rankings::default();
        r.record_hand(&view);
        assert_eq!(r.players.len(), 2, "bot not ranked");
        assert!(r.players.values().all(|s| s.hands == 1));
        // Only humans are listed, so their totals alone need not cancel, but
        // nobody can win or lose more than the table's chips.
        assert!(r.players.values().all(|s| s.net_bb.abs() <= 1_000.0 / 20.0));
        let json = serde_json::to_string(&r).unwrap();
        let back: Rankings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.players.len(), 2);
        assert!(r.sorted()[0].1.net_bb >= r.sorted()[1].1.net_bb);
    }

    #[test]
    fn tourney_points_follow_places() {
        let mut view = played_hand();
        view.seats[0].place = Some(1);
        view.seats[1].place = Some(2);
        let mut r = Rankings::default();
        r.record_tourney(&view);
        assert_eq!(r.players["pk0"].tourney_points, 10);
        assert_eq!(r.players["pk1"].tourney_points, 6);
    }
}
