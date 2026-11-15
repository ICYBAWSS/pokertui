//! Card, deck and shuffle primitives.
//!
//! A card is packed into a `u8`: `rank * 4 + suit` with rank 0..=12 meaning
//! 2..=A and suit 0..=3 meaning clubs, diamonds, hearts, spades.

use rand_chacha::ChaCha20Rng;
use rand_chacha::rand_core::Rng;
use serde::de::Error as DeError;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub const RANKS: [char; 13] = [
    '2', '3', '4', '5', '6', '7', '8', '9', 'T', 'J', 'Q', 'K', 'A',
];
pub const SUIT_CHARS: [char; 4] = ['c', 'd', 'h', 's'];
pub const SUIT_SYMBOLS: [char; 4] = ['♣', '♦', '♥', '♠'];
pub const DECK_LEN: usize = 52;

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Card(pub u8);

impl Card {
    pub const fn new(rank: u8, suit: u8) -> Self {
        Card(rank * 4 + suit)
    }

    pub const fn rank(self) -> u8 {
        self.0 / 4
    }

    pub const fn suit(self) -> u8 {
        self.0 % 4
    }

    pub fn rank_char(self) -> char {
        RANKS[self.rank() as usize]
    }

    pub fn suit_char(self) -> char {
        SUIT_CHARS[self.suit() as usize]
    }

    pub fn suit_symbol(self) -> char {
        SUIT_SYMBOLS[self.suit() as usize]
    }

    pub fn is_red(self) -> bool {
        matches!(self.suit(), 1 | 2)
    }

    /// `As`, `Td`, `9h`, ... the compact form used on the wire and in history.
    pub fn name(self) -> String {
        format!("{}{}", self.rank_char(), self.suit_char())
    }

    /// `A♠`, `9♥`, ... for the terminal.
    pub fn pretty(self) -> String {
        format!("{}{}", self.rank_char(), self.suit_symbol())
    }

    pub fn parse(s: &str) -> Option<Card> {
        let mut chars = s.chars();
        let rank = chars.next()?;
        let suit = chars.next()?;
        let rank = RANKS
            .iter()
            .position(|c| c.eq_ignore_ascii_case(&rank))? as u8;
        let suit = match suit.to_ascii_lowercase() {
            'c' | '♣' => 0,
            'd' | '♦' => 1,
            'h' | '♥' => 2,
            's' | '♠' => 3,
            _ => return None,
        };
        if chars.next().is_some() {
            return None;
        }
        Some(Card::new(rank, suit))
    }
}

impl std::fmt::Debug for Card {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name())
    }
}

impl std::fmt::Display for Card {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name())
    }
}

impl Serialize for Card {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.name())
    }
}

impl<'de> Deserialize<'de> for Card {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Card::parse(&s).ok_or_else(|| D::Error::custom(format!("bad card `{s}`")))
    }
}

/// A fresh 52-card deck in deterministic order (rank-major).
pub fn full_deck() -> Vec<Card> {
    (0..DECK_LEN as u8).map(Card).collect()
}

/// Fisher-Yates over a ChaCha20 CSPRNG. `rng` must be seeded from OS entropy.
pub fn shuffle(deck: &mut [Card], rng: &mut ChaCha20Rng) {
    for i in (1..deck.len()).rev() {
        let j = (rng.next_u32() as usize) % (i + 1);
        deck.swap(i, j);
    }
}

pub fn deck_bytes(deck: &[Card]) -> Vec<u8> {
    deck.iter().map(|c| c.0).collect()
}

pub fn cards_from_bytes(bytes: &[u8]) -> Option<Vec<Card>> {
    if bytes.len() != DECK_LEN || bytes.iter().any(|b| *b >= DECK_LEN as u8) {
        return None;
    }
    let mut seen = [false; DECK_LEN];
    let mut out = Vec::with_capacity(DECK_LEN);
    for b in bytes {
        if seen[*b as usize] {
            return None;
        }
        seen[*b as usize] = true;
        out.push(Card(*b));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::random_bytes;
    use rand_chacha::rand_core::SeedableRng;

    #[test]
    fn parse_round_trip() {
        for c in full_deck() {
            assert_eq!(Card::parse(&c.name()), Some(c));
            assert_eq!(Card::parse(&c.pretty()), Some(c));
        }
        assert_eq!(Card::parse("as"), Some(Card::new(12, 3)));
        assert_eq!(Card::parse("nope"), None);
    }

    #[test]
    fn shuffle_is_a_permutation() {
        let mut deck = full_deck();
        let seed = random_bytes::<32>().unwrap();
        shuffle(&mut deck, &mut ChaCha20Rng::from_seed(seed));
        let mut sorted = deck.clone();
        sorted.sort();
        assert_eq!(sorted, full_deck());
        // Sanity: not the identity order.
        assert_ne!(deck, full_deck());
    }

    #[test]
    fn bytes_round_trip() {
        let deck = full_deck();
        let bytes = deck_bytes(&deck);
        assert_eq!(cards_from_bytes(&bytes).unwrap(), deck);
        assert!(cards_from_bytes(&[0u8; 52]).is_none()); // duplicates
    }
}
