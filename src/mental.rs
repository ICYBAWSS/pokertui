//! Mental poker: a deck that nobody, the host included, can read until a card
//! is meant to be seen.
//!
//! Think of 52 boxes with padlocks. Each player in turn puts their own padlock on
//! every box and shuffles them. Padlocks can come off in any order, so to show a
//! card to one player everybody else unlocks that single box, and only that
//! player's lock is left. Board cards are unlocked by everyone, in the open.
//!
//! The maths is Ristretto points: card `i` is a fixed point `C_i`, a lock with
//! secret `k` is multiplication by `k` and unlocking is multiplication by `k⁻¹`,
//! and `a·(b·P) = b·(a·P)`. Everything a player does is deterministic from one
//! 32-byte seed, so after the hand each player reveals the seed and anyone can
//! replay their steps (`verify_transcript`) and name whoever cheated.
//!
//! What this does and does not give you (also in docs/EXPERIMENTAL_FEATURES.md):
//! cheating is detected after the hand, not prevented during it, and everyone must
//! stay online until the end.

use crate::game::{Card, DECK_LEN};
use curve25519_dalek::ristretto::{CompressedRistretto, RistrettoPoint};
use curve25519_dalek::scalar::Scalar;
use rand_chacha::ChaCha20Rng;
use rand_chacha::rand_core::{Rng, SeedableRng};
use sha2::{Digest, Sha512};

pub type Locked = [u8; 32];

fn wide(parts: &[&[u8]]) -> [u8; 64] {
    let mut h = Sha512::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

/// The fixed point standing for card `i` (0..52).
pub fn card_point(i: u8) -> RistrettoPoint {
    RistrettoPoint::from_uniform_bytes(&wide(&[b"pokertui-mental-card-v1", &[i]]))
}

fn master(seed: &[u8; 32]) -> Scalar {
    Scalar::from_bytes_mod_order_wide(&wide(&[b"pokertui-mental-master", seed]))
}

fn card_key(seed: &[u8; 32], position: usize) -> Scalar {
    Scalar::from_bytes_mod_order_wide(&wide(&[b"pokertui-mental-card-key", seed, &(position as u32).to_le_bytes()]))
}

fn permutation(seed: &[u8; 32]) -> Vec<usize> {
    let mut rng = ChaCha20Rng::from_seed(wide(&[b"pokertui-mental-perm", seed])[..32].try_into().expect("32 bytes"));
    let mut p: Vec<usize> = (0..DECK_LEN).collect();
    for i in (1..p.len()).rev() {
        let j = (rng.next_u32() as usize) % (i + 1);
        p.swap(i, j);
    }
    p
}

fn point(c: &Locked) -> Option<RistrettoPoint> {
    CompressedRistretto(*c).decompress()
}

/// The unshuffled, unlocked deck every game starts from.
pub fn fresh_deck() -> Vec<Locked> {
    (0..DECK_LEN as u8).map(|i| card_point(i).compress().to_bytes()).collect()
}

/// Step 1 (in seat order): put my lock on every card and shuffle.
pub fn shuffle_step(deck: &[Locked], seed: &[u8; 32]) -> Option<Vec<Locked>> {
    let k = master(seed);
    let locked: Vec<Locked> = deck
        .iter()
        .map(|c| point(c).map(|p| (p * k).compress().to_bytes()))
        .collect::<Option<_>>()?;
    Some(permutation(seed).into_iter().map(|from| locked[from]).collect())
}

/// Step 2 (in seat order): swap my one master lock for a separate lock per position,
/// so single cards can be unlocked later without opening the rest.
pub fn relock_step(deck: &[Locked], seed: &[u8; 32]) -> Option<Vec<Locked>> {
    let inv = master(seed).invert();
    deck.iter()
        .enumerate()
        .map(|(j, c)| point(c).map(|p| (p * (inv * card_key(seed, j))).compress().to_bytes()))
        .collect()
}

/// What a player hands over so others can open position `j` (sealed to one player for
/// hole cards, published for board cards). It is that player's lock key for `j`.
pub fn release_key(seed: &[u8; 32], position: usize) -> Scalar {
    card_key(seed, position)
}

/// Take the given locks off a card. Remove all but the viewer's own for a private
/// card, or every lock for a public one.
pub fn unlock(card: &Locked, keys: &[Scalar]) -> Option<Locked> {
    let mut p = point(card)?;
    for k in keys {
        p *= k.invert();
    }
    Some(p.compress().to_bytes())
}

/// Which card is this plain point, if any? `None` means it is still locked.
pub fn identify(plain: &Locked) -> Option<Card> {
    (0..DECK_LEN as u8).find(|i| card_point(*i).compress().to_bytes() == *plain).map(Card)
}

/// Everything public that happened: the deck after every shuffle and relock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transcript {
    pub shuffles: Vec<Vec<Locked>>,
    pub relocks: Vec<Vec<Locked>>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Cheat {
    /// This player's published deck is not what their revealed seed produces.
    Shuffle(usize),
    Relock(usize),
    /// The final deck is not 52 distinct real cards.
    NotADeck,
    /// A player never revealed a seed.
    Missing(usize),
}

/// After the hand: replay every player's steps from their revealed seeds, compare with
/// what they published, and return the whole deck order (position → card).
pub fn verify_transcript(t: &Transcript, seeds: &[Option<[u8; 32]>]) -> Result<Vec<Card>, Cheat> {
    let n = seeds.len();
    let mut deck = fresh_deck();
    for (p, seed) in seeds.iter().enumerate() {
        let seed = seed.ok_or(Cheat::Missing(p))?;
        deck = shuffle_step(&deck, &seed).ok_or(Cheat::Shuffle(p))?;
        if t.shuffles.get(p) != Some(&deck) {
            return Err(Cheat::Shuffle(p));
        }
    }
    for (p, seed) in seeds.iter().enumerate() {
        deck = relock_step(&deck, &seed.expect("checked above")).ok_or(Cheat::Relock(p))?;
        if t.relocks.get(p) != Some(&deck) {
            return Err(Cheat::Relock(p));
        }
    }
    let all: Vec<Scalar> = (0..DECK_LEN).map(|j| (0..n).fold(Scalar::ONE, |acc, p| acc * card_key(&seeds[p].expect("checked"), j))).collect();
    let mut cards = Vec::new();
    for (j, c) in deck.iter().enumerate() {
        let plain = point(c).map(|p| (p * all[j].invert()).compress().to_bytes()).ok_or(Cheat::NotADeck)?;
        cards.push(identify(&plain).ok_or(Cheat::NotADeck)?);
    }
    let mut sorted = cards.clone();
    sorted.sort();
    sorted.dedup();
    if sorted.len() != DECK_LEN {
        return Err(Cheat::NotADeck);
    }
    Ok(cards)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeds(n: usize) -> Vec<[u8; 32]> {
        (0..n).map(|i| [i as u8 + 1; 32]).collect()
    }

    /// Run the whole protocol for `n` players; returns the transcript.
    fn run(seeds: &[[u8; 32]]) -> Transcript {
        let (mut shuffles, mut relocks) = (Vec::new(), Vec::new());
        let mut deck = fresh_deck();
        for s in seeds {
            deck = shuffle_step(&deck, s).unwrap();
            shuffles.push(deck.clone());
        }
        for s in seeds {
            deck = relock_step(&deck, s).unwrap();
            relocks.push(deck.clone());
        }
        Transcript { shuffles, relocks }
    }

    fn reveal(seeds: &[[u8; 32]]) -> Vec<Option<[u8; 32]>> {
        seeds.iter().map(|s| Some(*s)).collect()
    }

    /// Open position `j` for player `viewer`: every other player releases their key,
    /// the viewer removes theirs last.
    fn deal(t: &Transcript, seeds: &[[u8; 32]], j: usize, viewer: usize) -> Option<Card> {
        let deck = t.relocks.last().unwrap();
        let others: Vec<Scalar> = (0..seeds.len()).filter(|p| *p != viewer).map(|p| release_key(&seeds[p], j)).collect();
        let mine_still_on = unlock(&deck[j], &others)?;
        identify(&unlock(&mine_still_on, &[release_key(&seeds[viewer], j)])?)
    }

    #[test]
    fn a_full_hand_deals_private_and_public_cards() {
        let s = seeds(3);
        let t = run(&s);
        let order = verify_transcript(&t, &reveal(&s)).expect("honest hand verifies");
        assert_eq!(order.len(), 52);

        // Each player's private cards match what the full reveal says is at that position.
        for (viewer, positions) in [(0usize, [0, 1]), (1, [2, 3]), (2, [4, 5])] {
            for j in positions {
                assert_eq!(deal(&t, &s, j, viewer), Some(order[j]), "player {viewer} position {j}");
            }
        }
        // Board cards: everyone releases, anyone can read them.
        let deck = t.relocks.last().unwrap();
        let all: Vec<Scalar> = (0..3).map(|p| release_key(&s[p], 6)).collect();
        assert_eq!(identify(&unlock(&deck[6], &all).unwrap()), Some(order[6]));
    }

    #[test]
    fn nobody_can_read_a_card_early() {
        let s = seeds(3);
        let t = run(&s);
        // Anyone watching sees only locked points: none is a card, at any stage.
        for deck in t.shuffles.iter().chain(&t.relocks) {
            assert!(deck.iter().all(|c| identify(c).is_none()), "a published deck leaked a card");
        }
        // Even the other two players together cannot open player 0's card at position 0.
        let deck = t.relocks.last().unwrap();
        let others = [release_key(&s[1], 0), release_key(&s[2], 0)];
        let still_locked = unlock(&deck[0], &others).unwrap();
        assert!(identify(&still_locked).is_none(), "player 0's own lock still protects it");
        // And a single player's keys alone open nothing.
        assert!(identify(&unlock(&deck[0], &[release_key(&s[0], 0)]).unwrap()).is_none());
    }

    #[test]
    fn deals_are_random_and_depend_on_every_player() {
        let a = verify_transcript(&run(&seeds(3)), &reveal(&seeds(3))).unwrap();
        let mut other = seeds(3);
        other[2] = [99; 32];
        let b = verify_transcript(&run(&other), &reveal(&other)).unwrap();
        assert_ne!(a, b, "changing one player's seed changes the whole deck");
        assert_ne!(a, (0..52u8).map(Card).collect::<Vec<_>>(), "not left in order");
    }

    #[test]
    fn a_player_who_swaps_a_card_is_named() {
        let s = seeds(3);
        let honest = run(&s);
        // Player 1 publishes a shuffle where one card is replaced by a copy of another.
        let mut t = honest.clone();
        t.shuffles[1][5] = t.shuffles[1][9];
        assert_eq!(verify_transcript(&t, &reveal(&s)), Err(Cheat::Shuffle(1)));
        // Player 2 tampers while relocking.
        let mut t = honest.clone();
        t.relocks[2][0] = t.relocks[2][1];
        assert_eq!(verify_transcript(&t, &reveal(&s)), Err(Cheat::Relock(2)));
        // A player who will not reveal their seed cannot be verified.
        let mut seeds_shown = reveal(&s);
        seeds_shown[0] = None;
        assert_eq!(verify_transcript(&honest, &seeds_shown), Err(Cheat::Missing(0)));
    }

    #[test]
    fn works_heads_up_and_full_ring() {
        for n in [2usize, 6, 9] {
            let s = seeds(n);
            let t = run(&s);
            let order = verify_transcript(&t, &reveal(&s)).unwrap();
            assert_eq!(deal(&t, &s, 0, 0), Some(order[0]));
            assert_eq!(deal(&t, &s, 1, n - 1), Some(order[1]));
        }
    }
}
