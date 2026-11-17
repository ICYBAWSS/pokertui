import test from "node:test";
import assert from "node:assert/strict";
import { subtle } from "node:crypto";
import { handleFrame, handHash, applyBundle, MIN_HANDS, MIN_DISTINCT, PAIR_CAP } from "../src/leaderboard.js";
import { b64uEncode } from "../src/crypto.js";
import { newDb, fixture } from "./helpers.js";

const NOW = 1_700_000_100_000;
const ctx = (extra = {}) => {
  const f = fixture();
  return { now: NOW, sealSecret: f.server_secret, sealPublic: f.server_public, country: "US", region: "California", ...extra };
};

test("a Rust-signed, Rust-sealed hand is accepted once, then recognised as a duplicate", async () => {
  const db = newDb(), f = fixture();
  let r = await handleFrame(db, { op: "lb_hands", items: [f.good] }, ctx());
  assert.deepEqual(r.results, ["accepted"]);
  r = await handleFrame(db, { op: "lb_hands", items: [f.good] }, ctx());
  assert.deepEqual(r.results, ["duplicate"]);
  const rows = db.raw.prepare("SELECT hands, won, net_bb FROM players ORDER BY net_bb").all();
  assert.equal(rows.length, 2);
  assert.deepEqual(rows.map((x) => x.hands), [1, 1]);
  assert.deepEqual(rows.map((x) => x.net_bb), [-3, 3]); // 150 chips at 50/hand = 3 bb
});

test("a hand missing one player's signature is rejected and changes nothing", async () => {
  const db = newDb(), f = fixture();
  const r = await handleFrame(db, { op: "lb_hands", items: [f.unsigned] }, ctx());
  assert.match(r.results[0], /^rejected: missing signature/);
  assert.equal(db.raw.prepare("SELECT COUNT(*) AS n FROM players").get().n, 0);
});

test("garbage, hands from the far past and oversized batches are refused", async () => {
  const db = newDb(), f = fixture();
  let r = await handleFrame(db, { op: "lb_hands", items: [{ epk: "x", nonce: "y", ct: "z" }] }, ctx());
  assert.match(r.results[0], /unreadable/);
  r = await handleFrame(db, { op: "lb_hands", items: [f.good] }, ctx({ now: NOW + 31 * 24 * 3600 * 1000 }));
  assert.match(r.results[0], /too old/);
  r = await handleFrame(db, { op: "lb_hands", items: new Array(51).fill(f.good) }, ctx());
  assert.equal(r.ok, false);
});

async function newPlayer() {
  const kp = await subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);
  const pub = b64uEncode(new Uint8Array(await subtle.exportKey("raw", kp.publicKey)));
  const sign = async (msg) => b64uEncode(new Uint8Array(await subtle.sign({ name: "Ed25519" }, kp.privateKey, new TextEncoder().encode(msg))));
  return { pub, sign };
}

async function profile(p, username, { listed = true, show_state = false } = {}) {
  const sig = await p.sign(`pokertui-profile-v1|${username}|${listed ? 1 : 0}|${show_state ? 1 : 0}`);
  return { op: "lb_profile", pubkey: p.pub, username, listed, show_state, sig };
}

test("usernames are unique (case-insensitive) and validated", async () => {
  const db = newDb(), [a, b] = [await newPlayer(), await newPlayer()];
  assert.equal((await handleFrame(db, await profile(a, "River_Rat"), ctx())).ok, true);
  assert.equal((await handleFrame(db, await profile(b, "river_rat"), ctx())).error, "taken");
  assert.equal((await handleFrame(db, await profile(a, "river_rat"), ctx())).ok, true, "the owner may re-case it");
  assert.equal((await handleFrame(db, await profile(b, "x"), ctx())).ok, false, "too short");
  assert.equal((await handleFrame(db, await profile(b, "has space"), ctx())).ok, false);
  const forged = { ...(await profile(b, "fresh_name")), sig: (await profile(a, "fresh_name")).sig };
  assert.equal((await handleFrame(db, forged, ctx())).error, "bad signature");
});

test("state is stored only when opted in, cleared when switched off, and the IP is never stored", async () => {
  const db = newDb(), p = await newPlayer();
  await handleFrame(db, await profile(p, "nitbox", { show_state: true }), ctx());
  assert.equal(db.raw.prepare("SELECT state FROM players").get().state, "California");
  await handleFrame(db, await profile(p, "nitbox", { show_state: true }), ctx({ country: "CA", region: "Ontario" }));
  assert.equal(db.raw.prepare("SELECT state FROM players").get().state, "Canada", "outside the US: country only");
  await handleFrame(db, await profile(p, "nitbox", { show_state: false }), ctx());
  assert.equal(db.raw.prepare("SELECT state FROM players").get().state, null);
  const columns = db.raw.prepare("PRAGMA table_info(players)").all().map((c) => c.name);
  assert.ok(!columns.some((c) => /ip|addr/i.test(c)), `no IP-like column in ${columns}`);
});

test("the board shows rank, username and state, only for opted-in players with enough hands", async () => {
  const db = newDb();
  const rows = [
    ["pk1", "ace_king", "Texas", 1, MIN_HANDS, 1200],
    ["pk2", "river_rat", "California", 1, MIN_HANDS + 5, 1500],
    ["pk3", "hidden", null, 0, 500, 2000],
    ["pk4", "newbie", null, 1, MIN_HANDS - 1, 3000],
    ["pk5", "nitbox", null, 1, MIN_HANDS, 900],
  ];
  for (const [pk, u, s, l, h, r] of rows) {
    db.raw.prepare("INSERT INTO players (pubkey, username, state, listed, hands, rating) VALUES (?,?,?,?,?,?)").run(pk, u, s, l, h, r);
  }
  const { rows: board } = await handleFrame(db, { op: "lb_get", top: 10 }, ctx());
  assert.deepEqual(board, [
    { rank: 1, username: "river_rat", state: "California" },
    { rank: 2, username: "ace_king", state: "Texas" },
    { rank: 3, username: "nitbox", state: null },
  ]);
  const me = (await handleFrame(db, { op: "lb_me", pubkey: "pk1" }, ctx())).me;
  assert.equal(me.rank, 2);
  assert.equal((await handleFrame(db, { op: "lb_me", pubkey: "pk3" }, ctx())).me.rank, null, "unlisted players have no rank");
});

test("handHash is order-independent and sensitive to every field", async () => {
  const p = [{ pubkey: "b", delta: -5 }, { pubkey: "a", delta: 5 }];
  const base = { room: "r", hand_no: 1, commit: "c", players: p };
  const h = await handHash(base);
  assert.equal(await handHash({ ...base, players: [...p].reverse() }), h);
  assert.notEqual(await handHash({ ...base, hand_no: 2 }), h);
  assert.notEqual(await handHash({ ...base, players: [{ pubkey: "b", delta: -6 }, p[1]] }), h);
});

// Direct hand application (signatures are checked elsewhere), to test the rating rules.
let n = 0;
const hand = (winner, loser, chips = 1000) => ({
  hand_hash: `h${n++}`, room: "r", hand_no: n, commit: "c", ts_ms: NOW, bb: 50,
  players: [{ pubkey: winner, delta: chips }, { pubkey: loser, delta: -chips }], sigs: {},
});
const rating = (db, pk) => db.raw.prepare("SELECT rating FROM players WHERE pubkey = ?").get(pk)?.rating ?? 1000;

test("playing yourself from a fresh account earns nothing", async () => {
  const db = newDb();
  for (let i = 0; i < 200; i++) await applyBundle(db, hand("main", "alt"), NOW);
  assert.equal(rating(db, "main"), 1000, "no rating from an account with no other opponents");
  assert.ok(rating(db, "alt") < 1000, "the alt still loses rating");
});

test("an established opponent pays out, but only up to the per-opponent cap", async () => {
  const db = newDb();
  for (let i = 0; i < MIN_DISTINCT; i++) await applyBundle(db, hand(`friend${i}`, "vet"), NOW); // vet meets 5 others
  await applyBundle(db, hand("main", "vet"), NOW);
  const first = rating(db, "main") - 1000;
  assert.ok(first > 0, "a real opponent counts");
  for (let i = 0; i < 300; i++) await applyBundle(db, hand("main", "vet"), NOW);
  assert.ok(rating(db, "main") - 1000 <= PAIR_CAP + 1e-9, `farming one opponent is capped at ${PAIR_CAP}`);
});

test("a ring of fresh accounts cannot bootstrap itself past the caps", async () => {
  const db = newDb();
  const ring = ["a", "b", "c", "d", "e", "f"];
  for (let round = 0; round < 40; round++) {
    for (const x of ring) for (const y of ring) if (x !== y) await applyBundle(db, hand(x, y, 500), NOW);
  }
  for (const x of ring) assert.ok(rating(db, x) - 1000 <= PAIR_CAP * (ring.length - 1) + 1e-9);
});
