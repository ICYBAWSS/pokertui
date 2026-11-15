//! Human-friendly room codes.
//!
//! A room is a 40-bit random id rendered as 8 Crockford base32 characters, e.g.
//! `K7Q2-9XZP`. The code is the capability: topics are derived from the id, so
//! there is no directory to scan and no way to enumerate tables.

use crate::crypto::random_bytes;
use anyhow::{Result, bail};

const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

pub type RoomId = [u8; 5];

pub fn generate() -> Result<(RoomId, String)> {
    let id = random_bytes::<5>()?;
    Ok((id, encode(&id)))
}

pub fn encode(id: &RoomId) -> String {
    let value = id.iter().fold(0u64, |acc, b| (acc << 8) | *b as u64);
    let mut out = String::with_capacity(9);
    for i in 0..8 {
        if i == 4 {
            out.push('-');
        }
        let shift = (7 - i) * 5;
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
    if cleaned.len() != 8 {
        bail!("room code must have 8 characters, got {}", cleaned.len());
    }
    let mut value = 0u64;
    for c in cleaned.bytes() {
        let idx = ALPHABET
            .iter()
            .position(|a| *a == c)
            .ok_or_else(|| anyhow::anyhow!("`{}` is not a valid room-code character", c as char))?;
        value = (value << 5) | idx as u64;
    }
    let mut id = [0u8; 5];
    for (i, slot) in id.iter_mut().enumerate() {
        *slot = ((value >> ((4 - i) * 8)) & 0xff) as u8;
    }
    Ok(id)
}

pub fn hex(id: &RoomId) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for _ in 0..64 {
            let (id, code) = generate().unwrap();
            assert_eq!(code.len(), 9);
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
        assert!(decode("00000000").is_ok());
    }
}
