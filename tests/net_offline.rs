//! End-to-end tests over the in-memory relay: no internet, no flakiness.

mod common;

use common::*;
use pokertui::bot::Skill;
use pokertui::game::{Phase, Tourney};
use pokertui::net::{MemoryBus, NetCommand};
use std::sync::Arc;

/// Host + two clients, everyone checks/calls: the hand finishes, the joint
/// shuffle is used, every client's audit passes and chips are conserved.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn full_hand_with_joint_shuffle_and_audit() {
    let mut r = room(MemoryBus::new(), 3, 2_000);
    r.params.turn_ms = 5_000; // an honest host never trips the turn-clock check
    let host = spawn_host(&r, "host", |_| {});
    let a = spawn_client(&r, "alice", None, |_| {});
    let b = spawn_client(&r, "bob", None, |_| {});
    for n in [&host, &a, &b] {
        n.set_auto(true);
    }
    wait_for(5.0, "three seated", || seated(&host) == 3).await;
    host.send(NetCommand::Start);
    wait_for(10.0, "hand result everywhere", || {
        [&host, &a, &b].iter().all(|n| !n.state().results.is_empty())
    })
    .await;

    let res = host.state().results[0].clone();
    assert_eq!(res.board.len(), 5);
    let seed = res.seed.expect("joint shuffle seed revealed");
    assert_eq!(seed.contribs.len(), 2, "both clients contributed randomness");
    for n in [&host, &a, &b] {
        assert!(n.state().errors.is_empty(), "{} saw errors: {:?}", n.name, n.state().errors);
    }
    assert_eq!(total_chips(&a), 6_000);
}

/// A broker that drops every 4th packet: pings and retries heal the table.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lossy_relay_still_finishes_a_hand() {
    let mut r = room(MemoryBus::lossy(4), 3, 2_000);
    r.params.turn_ms = 1_500; // a lost action times out instead of stalling
    let host = spawn_host(&r, "host", |_| {});
    let a = spawn_client(&r, "alice", None, |_| {});
    let b = spawn_client(&r, "bob", None, |_| {});
    for n in [&host, &a, &b] {
        n.set_auto(true);
    }
    wait_for(20.0, "three seated", || seated(&host) == 3).await;
    host.send(NetCommand::Start);
    wait_for(40.0, "a hand to finish", || !host.state().results.is_empty()).await;
}

/// Quitting without a goodbye (crash) and coming back with the same identity
/// returns the same seat, the same chips and the live hole cards.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_player_reclaims_seat_and_cards() {
    let r = room(MemoryBus::new(), 3, 2_000);
    let host = spawn_host(&r, "host", |_| {});
    let a = spawn_client(&r, "alice", None, |_| {});
    let b = spawn_client(&r, "bob", None, |_| {});
    wait_for(5.0, "three seated", || seated(&host) == 3).await;
    host.send(NetCommand::Start);
    wait_for(5.0, "alice dealt in", || {
        a.phase() == Some(Phase::Betting) && a.state().cards.is_some()
    })
    .await;
    let seat = a.state().seat.unwrap();
    let hand = a.hand_no();
    let id = a.id.clone();
    a.send(NetCommand::Quit); // crash: no Leave is sent
    wait_for(2.0, "alice gone", || a.state().closed.is_some()).await;

    let a2 = spawn_client(&r, "alice", Some(id), |_| {});
    wait_for(5.0, "alice back in her seat", || a2.state().seat == Some(seat)).await;
    wait_for(5.0, "hole cards re-sent", || {
        a2.state().cards.as_ref().is_some_and(|(h, c)| *h == hand && c.len() == 2)
    })
    .await;
    assert_eq!(seated(&host), 3, "no duplicate seat was created");
    drop(b);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kick_and_ban_block_the_player() {
    let r = room(MemoryBus::new(), 4, 2_000);
    let host = spawn_host(&r, "host", |_| {});
    let a = spawn_client(&r, "alice", None, |_| {});
    let b = spawn_client(&r, "bob", None, |_| {});
    wait_for(5.0, "three seated", || seated(&host) == 3).await;

    wait_for(5.0, "seats assigned", || a.state().seat.is_some() && b.state().seat.is_some()).await;
    let a_seat = a.state().seat.unwrap();
    host.send(NetCommand::Kick { seat: a_seat, ban: true });
    wait_for(5.0, "alice told", || a.state().closed.is_some()).await;
    assert!(a.state().closed.clone().unwrap().contains("banned"));
    wait_for(5.0, "seat freed", || seated(&host) == 2).await;

    // Same key tries to come back and is turned away.
    let a2 = spawn_client(&r, "alice", Some(a.id.clone()), |_| {});
    wait_for(5.0, "rejected", || a2.state().closed.is_some()).await;
    assert!(a2.state().closed.clone().unwrap().contains("banned"));

    // A plain kick (no ban) lets a player return.
    let b_seat = b.state().seat.unwrap();
    host.send(NetCommand::Kick { seat: b_seat, ban: false });
    wait_for(5.0, "bob told", || b.state().closed.is_some()).await;
    let b2 = spawn_client(&r, "bob", Some(b.id.clone()), |_| {});
    wait_for(5.0, "bob back", || b2.state().seat.is_some()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_names_are_rejected() {
    let r = room(MemoryBus::new(), 4, 2_000);
    let host = spawn_host(&r, "host", |_| {});
    let a = spawn_client(&r, "alice", None, |_| {});
    wait_for(5.0, "alice seated", || seated(&host) == 2).await;
    let imposter = spawn_client(&r, "ALICE", None, |_| {});
    wait_for(5.0, "rejected", || imposter.state().closed.is_some()).await;
    assert!(imposter.state().closed.clone().unwrap().contains("taken"));
    drop(a);
}

/// The host dies mid-hand: the next player in line becomes host, the hand is
/// cancelled with stacks restored, and the table keeps playing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_migration_keeps_the_table_alive() {
    let r = room(MemoryBus::new(), 3, 2_000);
    let host = spawn_host(&r, "host", |_| {});
    let a = spawn_client(&r, "alice", None, |_| {});
    let b = spawn_client(&r, "bob", None, |_| {});
    wait_for(5.0, "three seated", || seated(&host) == 3).await;
    host.send(NetCommand::Start);
    wait_for(5.0, "betting", || a.phase() == Some(Phase::Betting)).await;

    host.task.abort(); // crash: nothing is announced

    // Seats follow join order, so whoever sits lowest is next in line.
    let (new_host, other) = if a.state().seat < b.state().seat { (&a, &b) } else { (&b, &a) };
    wait_for(15.0, "successor promoted", || new_host.state().promoted).await;
    wait_for(5.0, "the other player accepts the new host", || {
        other.state().view.as_ref().is_some_and(|v| v.epoch == 1)
    })
    .await;
    assert!(!other.state().promoted, "only one successor");
    let v = new_host.state().view.clone().unwrap();
    assert_eq!(v.phase, Phase::Lobby, "interrupted hand is cancelled");
    for s in v.seats.iter().filter(|s| !s.name.is_empty()) {
        assert_eq!(s.stack, 2_000, "{} got the pre-hand stack back", s.name);
    }

    // And the new host can deal.
    new_host.set_auto(true);
    other.set_auto(true);
    let before = new_host.state().results.len();
    new_host.send(NetCommand::Start);
    wait_for(10.0, "a hand under the new host", || new_host.state().results.len() > before).await;
    wait_for(5.0, "the other player audits it", || !other.state().results.is_empty()).await;
    assert!(other.state().errors.is_empty(), "{:?}", other.state().errors);
}

/// An impostor cannot take over: only a seated player who is next in line may
/// host, and only with a newer epoch.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_the_successor_can_replace_the_host() {
    let r = room(MemoryBus::new(), 3, 2_000);
    let host = spawn_host(&r, "host", |_| {});
    let a = spawn_client(&r, "alice", None, |_| {});
    wait_for(5.0, "seated", || seated(&host) == 2).await;
    // A second "host" on the same room (right code, wrong key) is ignored.
    let fake = spawn_host(&r, "fake", |_| {});
    tokio::time::sleep(std::time::Duration::from_millis(1_500)).await;
    let view = a.state().view.clone().unwrap();
    assert_eq!(view.host, host.id.pubkey_b64());
    assert_eq!(view.epoch, 0);
    drop(fake);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bots_play_a_table_with_the_host() {
    let r = room(MemoryBus::new(), 4, 2_000);
    let host = spawn_host(&r, "host", |c| c.bots = vec![Skill::Easy, Skill::Medium, Skill::Hard]);
    host.set_auto(true);
    wait_for(5.0, "bots seated", || seated(&host) == 4).await;
    for played in 0..3 {
        wait_for(30.0, "table ready", || host.phase() != Some(Phase::Betting)).await;
        host.send(NetCommand::Start);
        wait_for(60.0, "hand to finish", || host.state().results.len() > played).await;
    }
    assert!(host.state().errors.is_empty(), "{:?}", host.state().errors);
    // The host can add and remove bots from the lobby too.
    let seat = host
        .state()
        .view
        .as_ref()
        .unwrap()
        .seats
        .iter()
        .find(|s| s.bot)
        .map(|s| s.seat)
        .unwrap();
    wait_for(20.0, "between hands", || host.phase() == Some(Phase::HandOver)).await;
    host.send(NetCommand::RemoveBot(seat));
    wait_for(5.0, "bot removed", || seated(&host) == 3).await;
    host.send(NetCommand::AddBot(Skill::Hard));
    wait_for(5.0, "bot added", || seated(&host) == 4).await;
}

/// A sit-and-go plays to a single winner with distinct finishing places.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sit_and_go_finishes_with_one_winner() {
    let mut r = room(MemoryBus::new(), 3, 400);
    r.params.small_blind = 50;
    r.params.big_blind = 100;
    r.params.tourney = Some(Tourney { levels: vec![(50, 100), (100, 200), (200, 400)], level_secs: 2 });
    let host = spawn_host(&r, "host", |c| c.bots = vec![Skill::Easy, Skill::Easy]);
    host.set_auto(true);
    wait_for(5.0, "seated", || seated(&host) == 3).await;
    host.send(NetCommand::Start);
    wait_for(90.0, "tournament over", || {
        host.state().view.as_ref().and_then(|v| v.tourney.as_ref()).is_some_and(|t| t.over)
    })
    .await;
    let v = host.state().view.clone().unwrap();
    let mut places: Vec<usize> = v.seats.iter().filter_map(|s| s.place).collect();
    places.sort();
    assert_eq!(places, vec![1, 2, 3]);
    assert_eq!(total_chips(&host), 1_200);
}

/// Spectators with the delayed feed see hole cards, but only for hands that
/// have already finished.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delayed_spectator_sees_cards_only_after_the_hand() {
    let r = room(MemoryBus::new(), 3, 2_000);
    let host = spawn_host(&r, "host", |c| c.spectate_secs = 1);
    let a = spawn_client(&r, "alice", None, |_| {});
    let spec = spawn_client(&r, "watcher", None, |c| c.spectate = true);
    for n in [&host, &a] {
        n.set_auto(true);
    }
    wait_for(5.0, "seated", || seated(&host) == 2).await;
    host.send(NetCommand::Start);
    wait_for(10.0, "hand over", || !host.state().results.is_empty()).await;
    wait_for(10.0, "spectator sees revealed cards", || {
        spec.state().views.iter().any(|v| v.seats.iter().any(|s| s.reveal.len() == 2))
    })
    .await;
    let finished: Vec<u32> = host.state().results.iter().map(|r| r.hand_no).collect();
    for v in spec.state().views.iter().filter(|v| v.seats.iter().any(|s| !s.reveal.is_empty())) {
        assert!(finished.contains(&v.hand_no), "hole cards leaked for unfinished hand {}", v.hand_no);
    }
    // Live views never carry hole cards.
    assert!(host.state().views.iter().all(|v| v.seats.iter().all(|s| s.reveal.is_empty())));
    drop(Arc::new(0));
}

/// The whole game over the Cloudflare-relay websocket protocol (against a
/// stand-in relay that speaks the same frames as the Worker).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn full_hand_over_the_cloudflare_relay_protocol() {
    let mut r = room(MemoryBus::new(), 3, 2_000);
    r.broker = Some(mock_relay().await);
    let host = spawn_host(&r, "host", |_| {});
    let a = spawn_client(&r, "alice", None, |_| {});
    let b = spawn_client(&r, "bob", None, |_| {});
    for n in [&host, &a, &b] {
        n.set_auto(true);
    }
    wait_for(10.0, "three seated", || seated(&host) == 3).await;
    host.send(NetCommand::Start);
    wait_for(15.0, "hand result everywhere", || {
        [&host, &a, &b].iter().all(|n| !n.state().results.is_empty())
    })
    .await;
    for n in [&host, &a, &b] {
        assert!(n.state().errors.is_empty(), "{} saw {:?}", n.name, n.state().errors);
    }
}

/// Every human signs the hand; each node ends up with the same complete,
/// verifiable bundle for the leaderboard queue.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_player_ends_up_with_the_same_cosigned_bundle() {
    use pokertui::crypto::verify_b64;
    let r = room(MemoryBus::new(), 3, 2_000);
    let host = spawn_host(&r, "host", |_| {});
    let a = spawn_client(&r, "alice", None, |_| {});
    let b = spawn_client(&r, "bob", None, |_| {});
    for n in [&host, &a, &b] {
        n.set_auto(true);
    }
    wait_for(5.0, "seated", || seated(&host) == 3).await;
    host.send(NetCommand::Start);
    wait_for(10.0, "a signed bundle on every node", || {
        [&host, &a, &b].iter().all(|n| !n.state().bundles.is_empty())
    })
    .await;
    let first = host.state().bundles[0].clone();
    assert_eq!(first.players.len(), 3);
    for n in [&host, &a, &b] {
        let got = n.state().bundles[0].clone();
        assert_eq!(got.hand_hash, first.hand_hash, "{} computed a different result", n.name);
        for p in &got.players {
            let sig = got.sigs.get(&p.pubkey).expect("every player signed");
            verify_b64(&p.pubkey, got.hand_hash.as_bytes(), sig).expect("valid signature");
        }
    }
}

/// Bot-only games and games with a single human never produce leaderboard results.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bot_games_produce_no_leaderboard_bundles() {
    let r = room(MemoryBus::new(), 3, 2_000);
    let host = spawn_host(&r, "host", |c| c.bots = vec![Skill::Easy, Skill::Easy]);
    host.set_auto(true);
    wait_for(5.0, "seated", || seated(&host) == 3).await;
    host.send(NetCommand::Start);
    wait_for(30.0, "a hand", || !host.state().results.is_empty()).await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(host.state().bundles.is_empty());
}

/// With approval on, strangers wait until the host says yes; the host can decline;
/// and a returning player is not asked again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_approval_gates_new_players_only() {
    let r = room(MemoryBus::new(), 4, 2_000);
    let host = spawn_host(&r, "host", |c| c.approve_joins = true);
    let alice = spawn_client(&r, "alice", None, |_| {});
    wait_for(5.0, "alice is waiting", || host.state().waiting == vec!["alice".to_string()]).await;
    assert_eq!(seated(&host), 1, "not seated yet");
    assert!(alice.state().seat.is_none());

    host.send(NetCommand::Approve(true));
    wait_for(5.0, "alice seated", || alice.state().seat.is_some() && seated(&host) == 2).await;

    let bob = spawn_client(&r, "bob", None, |_| {});
    wait_for(5.0, "bob is waiting", || host.state().waiting == vec!["bob".to_string()]).await;
    host.send(NetCommand::Approve(false));
    wait_for(5.0, "bob turned away", || bob.state().closed.is_some()).await;
    assert!(bob.state().closed.clone().unwrap().contains("declined"));
    assert_eq!(seated(&host), 2);

    // Alice crashes and comes back: straight in, no second approval.
    alice.send(NetCommand::Quit);
    wait_for(2.0, "alice gone", || alice.state().closed.is_some()).await;
    let alice2 = spawn_client(&r, "alice", Some(alice.id.clone()), |_| {});
    wait_for(5.0, "alice back without approval", || alice2.state().seat.is_some()).await;
    assert!(host.state().waiting.is_empty());
}
