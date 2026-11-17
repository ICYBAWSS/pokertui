//! Human-friendly room codes.
//!
//! A room is a 60-bit random secret rendered as 12 Crockford base32 characters,
//! e.g. `K7Q2-9XZP-4M8T`. The code is the capability. Topic names and the key
//! that encrypts every packet are *hashed from* the code, so someone who lists
//! the public broker's topics sees opaque ids: they cannot join, read or spoof
//! a table without the code.

use crate::crypto::random_bytes;
use anyhow::{Result, bail};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};

const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
pub const CODE_LEN: usize = 12;

/// The 60-bit secret, big-endian in 8 bytes.
pub type RoomId = [u8; 8];

pub fn generate() -> Result<(RoomId, String)> {
    let v = u64::from_be_bytes(random_bytes::<8>()?) & ((1u64 << 60) - 1);
    let id = v.to_be_bytes();
    Ok((id, encode(&id)))
}

pub fn encode(id: &RoomId) -> String {
    let value = u64::from_be_bytes(*id);
    let mut out = String::with_capacity(CODE_LEN + 2);
    for i in 0..CODE_LEN {
        if i > 0 && i % 4 == 0 {
            out.push('-');
        }
        let shift = (CODE_LEN - 1 - i) * 5;
        out.push(ALPHABET[((value >> shift) & 31) as usize] as char);
    }
    out
}

pub fn decode(code: &str) -> Result<RoomId> {
    let cleaned: String = code
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            'U' => 'V',
            c => c,
        })
        .collect();
    if cleaned.len() != CODE_LEN {
        bail!("room code must have {CODE_LEN} characters, got {}", cleaned.len());
    }
    let mut value = 0u64;
    for c in cleaned.bytes() {
        let idx = ALPHABET
            .iter()
            .position(|a| *a == c)
            .ok_or_else(|| anyhow::anyhow!("`{}` is not a valid room-code character", c as char))?;
        value = (value << 5) | idx as u64;
    }
    Ok(value.to_be_bytes())
}

/// Opaque topic id: the broker (and anyone listing topics) never sees the code.
pub fn topic_id(id: &RoomId) -> String {
    let mut h = Sha256::new();
    h.update(b"pokertui-topic-v2");
    h.update(id);
    h.finalize()[..12].iter().map(|b| format!("{b:02x}")).collect()
}

/// Key that encrypts every packet in the room.
pub fn room_key(id: &RoomId) -> [u8; 32] {
    let mut okm = [0u8; 32];
    Hkdf::<Sha256>::new(Some(b"pokertui-room-v2"), id)
        .expand(b"packet-key", &mut okm)
        .expect("32-byte hkdf output");
    okm
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for _ in 0..64 {
            let (id, code) = generate().unwrap();
            assert_eq!(code.len(), CODE_LEN + 2);
            assert_eq!(decode(&code).unwrap(), id);
            assert_eq!(decode(&code.to_lowercase()).unwrap(), id);
        }
    }

    #[test]
    fn tolerant_of_confusing_characters() {
        let (id, code) = generate().unwrap();
        let ambiguous = code.replace('0', "O").replace('1', "L").replace('V', "U");
        assert_eq!(decode(&ambiguous).unwrap(), id);
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode("nope").is_err());
        assert!(decode("!!!!").is_err());
        assert!(decode("000000000000").is_ok());
    }

    #[test]
    fn topic_and_key_do_not_reveal_the_code() {
        let (a, _) = generate().unwrap();
        let (b, _) = generate().unwrap();
        assert_ne!(topic_id(&a), topic_id(&b));
        assert_ne!(room_key(&a), room_key(&b));
        assert!(!topic_id(&a).contains(&encode(&a).replace('-', "").to_lowercase()));
    }
}
