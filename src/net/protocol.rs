//! Signed wire envelopes and the message types exchanged over MQTT.
//!
//! Every packet is self-authenticating: the sender's ed25519 public key is in
//! `from`, and `sig` covers room, sender, sequence number, timestamp and the
//! canonical JSON of the payload. A public broker can drop or reorder packets
//! but cannot forge or silently rewrite them.

use crate::crypto::{Identity, Sealed, verify_b64};
use crate::game::{Card, Move, TableParams, TableView};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

pub const PROTOCOL_VERSION: u8 = 1;

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "body", rename_all = "snake_case")]
pub enum Payload {
    JoinRequest {
        name: String,
        buyin: u64,
        enc_pub: String,
    },
    JoinAccept {
        to: String,
        seat: usize,
        params: TableParams,
        view: TableView,
    },
    JoinReject {
        to: String,
        reason: String,
    },
    State {
        view: TableView,
    },
    Deal {
        hand: u32,
        cards: Vec<Card>,
    },
    Action {
        hand: u32,
        version: u64,
        mv: Move,
    },
    Chat {
        from: String,
        text: String,
    },
    Ready {
        ready: bool,
    },
    RequestState {
        version: u64,
    },
    /// Heartbeat. Carries what the client holds so the host can resend
    /// anything a lossy broker dropped.
    Ping {
        #[serde(default)]
        version: u64,
        #[serde(default)]
        cards_hand: u32,
    },
    Leave {},
    Sealed {
        sealed: Sealed,
    },
    Error {
        to: String,
        message: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Envelope {
    pub v: u8,
    pub room: String,
    pub from: String,
    pub seq: u64,
    pub ts_ms: u64,
    pub payload: Payload,
    pub sig: String,
}

fn signing_bytes(
    room: &str,
    from: &str,
    seq: u64,
    ts_ms: u64,
    payload: &Payload,
) -> Result<Vec<u8>> {
    // JSON of a derived struct with fixed field order is stable across versions
    // of this crate, which is all that matters: both ends run the same code.
    let body = serde_json::to_string(payload)?;
    Ok(format!("pokertui-v1|{room}|{from}|{seq}|{ts_ms}|{body}").into_bytes())
}

impl Envelope {
    pub fn new(id: &Identity, room: &str, seq: u64, payload: Payload) -> Result<Self> {
        let from = id.pubkey_b64();
        let ts_ms = now_ms();
        let sig = id.sign_b64(&signing_bytes(room, &from, seq, ts_ms, &payload)?);
        Ok(Self {
            v: PROTOCOL_VERSION,
            room: room.to_string(),
            from,
            seq,
            ts_ms,
            payload,
            sig,
        })
    }

    pub fn verify(&self) -> Result<()> {
        if self.v != PROTOCOL_VERSION {
            bail!("unsupported protocol version {}", self.v);
        }
        let bytes = signing_bytes(&self.room, &self.from, self.seq, self.ts_ms, &self.payload)?;
        verify_b64(&self.from, &bytes, &self.sig)
            .map_err(|_| anyhow::anyhow!("message from {} failed signature check", self.from))
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let env: Envelope = serde_json::from_slice(bytes).map_err(|e| anyhow::anyhow!("bad packet: {e}"))?;
        env.verify()?;
        Ok(env)
    }
}

/// MQTT topics for one room.
#[derive(Clone, Debug)]
pub struct Topics {
    pub room: String,
}

impl Topics {
    pub fn new(id: &[u8; 5]) -> Self {
        Self {
            room: crate::net::roomcode::hex(id),
        }
    }

    pub fn base(&self) -> String {
        format!("pokertui/v1/{}", self.room)
    }

    pub fn events(&self) -> String {
        format!("{}/events", self.base())
    }

    /// Retained: the newest signed table snapshot, for anyone who joins later.
    pub fn snapshot(&self) -> String {
        format!("{}/snapshot", self.base())
    }

    /// Per-player sealed payloads. Anyone may subscribe; only the holder of the
    /// matching X25519 key can decrypt.
    pub fn secrets(&self, pubkey: &str) -> String {
        format!("{}/secrets/{}", self.base(), pubkey)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_round_trip_and_tamper() {
        let id = Identity::generate().unwrap();
        let payload = Payload::Chat {
            from: "alice".into(),
            text: "gl hf".into(),
        };
        let env = Envelope::new(&id, "aabbccddee", 7, payload).unwrap();
        let bytes = env.to_bytes().unwrap();
        let parsed = Envelope::parse(&bytes).unwrap();
        assert_eq!(parsed.seq, 7);
        match parsed.payload {
            Payload::Chat { from, text } => {
                assert_eq!(from, "alice");
                assert_eq!(text, "gl hf");
            }
            _ => panic!("wrong payload"),
        }

        // A byte flip in the payload must be caught by the signature.
        let mut tampered = bytes.clone();
        let idx = tampered
            .windows(3)
            .position(|w| w == b"gl ")
            .expect("text present");
        tampered[idx] = b'G';
        assert!(Envelope::parse(&tampered).is_err());
    }

    /// `verify` re-serializes the parsed payload, so every view the host can
    /// produce must survive a JSON round-trip byte-for-byte or clients drop it.
    #[test]
    fn every_table_view_survives_envelope_round_trip() {
        use crate::game::{Move, Phase, Table, TableParams};
        let id = Identity::generate().unwrap();
        let mut table = Table::new(TableParams {
            max_seats: 3,
            small_blind: 25,
            big_blind: 50,
            buyin: 2_000,
            turn_ms: 20_000,
        });
        for name in ["host", "alice", "bob"] {
            let seat = table.join(name, &format!("pk-{name}"), "enc", 2_000).unwrap();
            table.set_ready(seat, true);
        }
        let check = |table: &Table| {
            let view = table.view("aabbccddee");
            let (v, actor) = (view.version, view.actor);
            let env = Envelope::new(&id, "aabbccddee", 1, Payload::State { view }).unwrap();
            if let Err(e) = Envelope::parse(&env.to_bytes().unwrap()) {
                panic!("v{v} actor {actor:?} dropped: {e}");
            }
        };
        for hand in 0..20u8 {
            table.start_hand_seeded(1, [hand; 32]).unwrap();
            check(&table);
            while table.phase == Phase::Betting {
                let seat = table.actor.unwrap();
                let legal = table.legal(seat).unwrap();
                let mv = if legal.can_check { Move::Check } else { Move::Call };
                table.apply(seat, mv, 2).unwrap();
                table.set_deadline(1_700_000_000_000);
                check(&table);
            }
        }
    }

    #[test]
    fn topics_are_room_scoped() {
        let t = Topics::new(&[0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
        assert_eq!(t.events(), "pokertui/v1/aabbccddee/events");
        assert_eq!(t.snapshot(), "pokertui/v1/aabbccddee/snapshot");
        assert_eq!(
            t.secrets("abc"),
            "pokertui/v1/aabbccddee/secrets/abc"
        );
    }
}
