//! Player identity, sealed (encrypted) per-player payloads, and deck commitments.
//!
//! - Signing is ed25519. The seed lives in the config dir, so a player keeps their
//!   name and seat across restarts.
//! - Hole cards travel over a *public* MQTT topic, so they are encrypted to the
//!   recipient's X25519 key with an ephemeral-static ECDH + ChaCha20-Poly1305.
//!   Anyone who knows the room code can subscribe; only the addressee can decrypt.
//! - The deck commitment is `sha256("pokertui-deck-v1" || nonce || deck)`, so the
//!   host cannot swap cards mid-hand without it being detectable.

use anyhow::{Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey as XPublicKey, StaticSecret};
use zeroize::Zeroizing;

pub fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf).context("operating system random source failed")?;
    Ok(buf)
}

fn derive_key(ikm: &[u8], salt: &[u8], info: &[u8]) -> Result<[u8; 32]> {
    let hk = Hkdf::<Sha256>::new(Some(salt), ikm);
    let mut okm = [0u8; 32];
    hk.expand(info, &mut okm)
        .map_err(|_| anyhow::anyhow!("hkdf expansion failed"))?;
    Ok(okm)
}

#[derive(Clone)]
pub struct Identity {
    signing: SigningKey,
    enc: StaticSecret,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("pubkey", &self.pubkey_b64())
            .finish()
    }
}

impl Identity {
    pub fn generate() -> Result<Self> {
        Ok(Self::from_seed(random_bytes::<32>()?))
    }

    pub fn from_seed(seed: [u8; 32]) -> Self {
        let signing = SigningKey::from_bytes(&seed);
        let okm = derive_key(&seed, b"pokertui-identity", b"x25519-encryption-key")
            .expect("32-byte hkdf output");
        Self {
            signing,
            enc: StaticSecret::from(okm),
        }
    }

    pub fn seed_b64(&self) -> String {
        B64.encode(self.signing.to_bytes())
    }

    pub fn from_seed_b64(s: &str) -> Result<Self> {
        let bytes = B64.decode(s).context("identity seed is not valid base64url")?;
        let seed: [u8; 32] = bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("identity seed must be 32 bytes"))?;
        Ok(Self::from_seed(seed))
    }

    pub fn pubkey(&self) -> VerifyingKey {
        self.signing.verifying_key()
    }

    pub fn pubkey_b64(&self) -> String {
        B64.encode(self.signing.verifying_key().to_bytes())
    }

    pub fn enc_pub_b64(&self) -> String {
        B64.encode(self.enc_pub().as_bytes())
    }

    pub fn enc_pub(&self) -> XPublicKey {
        XPublicKey::from(&self.enc)
    }

    pub fn sign_b64(&self, msg: &[u8]) -> String {
        B64.encode(self.signing.sign(msg).to_bytes())
    }

    /// Decrypt a payload sealed to this identity.
    pub fn open(&self, sealed: &Sealed) -> Result<Vec<u8>> {
        open_sealed(&self.enc, sealed)
    }
}

pub fn parse_pubkey(s: &str) -> Result<VerifyingKey> {
    let bytes = B64.decode(s).context("bad public key encoding")?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("public key must be 32 bytes"))?;
    VerifyingKey::from_bytes(&arr).context("invalid ed25519 public key")
}

pub fn parse_enc_pubkey(s: &str) -> Result<XPublicKey> {
    let bytes = B64.decode(s).context("bad encryption key encoding")?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("encryption key must be 32 bytes"))?;
    Ok(XPublicKey::from(arr))
}

pub fn verify_b64(pubkey_b64: &str, msg: &[u8], sig_b64: &str) -> Result<()> {
    let key = parse_pubkey(pubkey_b64)?;
    let sig_bytes = B64.decode(sig_b64).context("bad signature encoding")?;
    let arr: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("signature must be 64 bytes"))?;
    let sig = Signature::from_bytes(&arr);
    key.verify(msg, &sig).map_err(|_| anyhow::anyhow!("bad signature"))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sealed {
    /// Ephemeral public key of the sender.
    pub epk: String,
    pub nonce: String,
    pub ct: String,
}

pub fn seal(recipient: &XPublicKey, plaintext: &[u8]) -> Result<Sealed> {
    let eph = StaticSecret::from(random_bytes::<32>()?);
    let epk = XPublicKey::from(&eph);
    let shared = eph.diffie_hellman(recipient);
    let mut salt = Vec::with_capacity(64);
    salt.extend_from_slice(epk.as_bytes());
    salt.extend_from_slice(recipient.as_bytes());
    let key = Zeroizing::new(derive_key(shared.as_bytes(), &salt, b"pokertui-seal-v1")?);
    let cipher = ChaCha20Poly1305::new_from_slice(&key[..]).expect("32-byte key");
    let nonce_bytes = random_bytes::<12>()?;
    let ct = cipher
        .encrypt(&Nonce::from(nonce_bytes), plaintext)
        .map_err(|_| anyhow::anyhow!("encryption failed"))?;
    Ok(Sealed {
        epk: B64.encode(epk.as_bytes()),
        nonce: B64.encode(nonce_bytes),
        ct: B64.encode(ct),
    })
}

pub fn open_sealed(secret: &StaticSecret, sealed: &Sealed) -> Result<Vec<u8>> {
    let epk = parse_enc_pubkey(&sealed.epk)?;
    let shared = secret.diffie_hellman(&epk);
    let recipient = XPublicKey::from(secret);
    let mut salt = Vec::with_capacity(64);
    salt.extend_from_slice(epk.as_bytes());
    salt.extend_from_slice(recipient.as_bytes());
    let key = Zeroizing::new(derive_key(shared.as_bytes(), &salt, b"pokertui-seal-v1")?);
    let cipher = ChaCha20Poly1305::new_from_slice(&key[..]).expect("32-byte key");
    let nonce_bytes = B64.decode(&sealed.nonce).context("bad nonce")?;
    let ct = B64.decode(&sealed.ct).context("bad ciphertext")?;
    let nonce: [u8; 12] = nonce_bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("nonce must be 12 bytes"))?;
    cipher
        .decrypt(&Nonce::from(nonce), ct.as_ref())
        .map_err(|_| anyhow::anyhow!("decryption failed (wrong recipient or tampered payload)"))
}

pub const DECK_DOMAIN: &[u8] = b"pokertui-deck-v1";

pub fn deck_commit(nonce: &[u8; 16], deck: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(DECK_DOMAIN);
    h.update(nonce);
    h.update(deck);
    h.finalize().into()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn b64(bytes: &[u8]) -> String {
    B64.encode(bytes)
}

pub fn b64_decode(s: &str) -> Result<Vec<u8>> {
    B64.decode(s).context("invalid base64url")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify() {
        let id = Identity::generate().unwrap();
        let sig = id.sign_b64(b"hello");
        verify_b64(&id.pubkey_b64(), b"hello", &sig).unwrap();
        assert!(verify_b64(&id.pubkey_b64(), b"hellp", &sig).is_err());
    }

    #[test]
    fn seal_round_trip_only_for_recipient() {
        let alice = Identity::generate().unwrap();
        let bob = Identity::generate().unwrap();
        let sealed = seal(&bob.enc_pub(), b"Ah Kh").unwrap();
        assert_eq!(bob.open(&sealed).unwrap(), b"Ah Kh");
        assert!(alice.open(&sealed).is_err());
    }

    #[test]
    fn identity_survives_seed_round_trip() {
        let id = Identity::generate().unwrap();
        let same = Identity::from_seed_b64(&id.seed_b64()).unwrap();
        assert_eq!(id.pubkey_b64(), same.pubkey_b64());
        assert_eq!(id.enc_pub_b64(), same.enc_pub_b64());
    }

    #[test]
    fn commit_changes_with_deck() {
        let a = deck_commit(&[1u8; 16], &[0, 1, 2]);
        let b = deck_commit(&[1u8; 16], &[0, 1, 3]);
        assert_ne!(a, b);
    }
}
