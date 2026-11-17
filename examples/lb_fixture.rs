//! Prints a deterministic leaderboard fixture so the Worker's tests can prove it
//! reads exactly what the Rust client writes (signatures, hash, sealed box).
//!   cargo run --example lb_fixture > relay/cloudflare/test/fixture.json

use pokertui::crypto::{Identity, seal};
use pokertui::leaderboard::{Bundle, PlayerResult, hand_hash};
use std::collections::BTreeMap;

fn main() {
    let server = Identity::from_seed([9; 32]);
    let (a, b) = (Identity::from_seed([1; 32]), Identity::from_seed([2; 32]));
    let mut players = vec![
        PlayerResult { pubkey: a.pubkey_b64(), delta: 150 },
        PlayerResult { pubkey: b.pubkey_b64(), delta: -150 },
    ];
    players.sort_by(|x, y| x.pubkey.cmp(&y.pubkey));
    let hash = hand_hash("roomfixture", 7, "commitfixture", &players);
    let mut sigs = BTreeMap::new();
    sigs.insert(a.pubkey_b64(), a.sign_b64(hash.as_bytes()));
    sigs.insert(b.pubkey_b64(), b.sign_b64(hash.as_bytes()));
    let good = Bundle {
        hand_hash: hash,
        room: "roomfixture".into(),
        hand_no: 7,
        commit: "commitfixture".into(),
        ts_ms: 1_700_000_000_000,
        bb: 50,
        players,
        sigs,
    };
    let mut unsigned = good.clone();
    unsigned.sigs.remove(&b.pubkey_b64());
    let sealed = |x: &Bundle| seal(&server.enc_pub(), &serde_json::to_vec(x).unwrap()).unwrap();
    let username = "river_rat";
    let line = format!("pokertui-profile-v1|{username}|1|1");
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "server_secret": server.enc_secret_b64(),
            "server_public": server.enc_pub_b64(),
            "hand_hash": good.hand_hash,
            "alice": a.pubkey_b64(),
            "bob": b.pubkey_b64(),
            "good": sealed(&good),
            "unsigned": sealed(&unsigned),
            "profile": { "pubkey": a.pubkey_b64(), "username": username, "listed": true, "show_state": true, "sig": a.sign_b64(line.as_bytes()) },
        }))
        .unwrap()
    );
}
