// Global leaderboard: verifies co-signed hand results and keeps ratings in D1.
// A hand counts only if EVERY human player in it signed the same result, so a host
// cannot invent a win; and only once, keyed by the hand hash.

import { b64uDecode, ed25519Verify, openSealed, sha256Hex } from "./crypto.js";

export const MIN_HANDS = 50;
export const MAX_AGE_MS = 30 * 24 * 3600 * 1000;
const CAP_BB = 20;
// Anti-farming: a win only earns rating from an opponent who has already played at least
// MIN_DISTINCT *other* people, and no single opponent can ever pay out more than PAIR_CAP
// rating points in total. One person playing themselves from several accounts gets nothing
// from fresh accounts and a small, fixed ceiling from established ones.
export const MIN_DISTINCT = 5;
export const PAIR_CAP = 25;
const USERNAME = /^[A-Za-z0-9_-]{3,16}$/;

export const handHash = (b) =>
  sha256Hex(
    `pokertui-result-v1|${b.room}|${b.hand_no}|${b.commit}|` +
      [...b.players].sort((x, y) => (x.pubkey < y.pubkey ? -1 : 1)).map((p) => `${p.pubkey}:${p.delta}`).join(","),
  );

/** Returns null if the bundle is valid, otherwise the reason it is not. */
export async function checkBundle(b, now) {
  if (!b || !Array.isArray(b.players) || typeof b.sigs !== "object") return "malformed";
  if (b.players.length < 2 || b.players.length > 9) return "player count";
  if (new Set(b.players.map((p) => p.pubkey)).size !== b.players.length) return "duplicate player";
  if (!(b.bb > 0) || !Number.isFinite(b.ts_ms)) return "malformed";
  if (now - b.ts_ms > MAX_AGE_MS || b.ts_ms - now > 24 * 3600 * 1000) return "too old";
  if (b.players.some((p) => !Number.isInteger(p.delta) || Math.abs(p.delta) > b.bb * 5000)) return "implausible";
  if ((await handHash(b)) !== b.hand_hash) return "hash mismatch";
  for (const p of b.players) {
    if (!(await ed25519Verify(p.pubkey, b.hand_hash, b.sigs[p.pubkey] ?? ""))) return "missing signature";
  }
  return null;
}

/** Apply one verified hand. Returns "accepted" or "duplicate". */
export async function applyBundle(db, b, now) {
  const ins = await db.prepare("INSERT OR IGNORE INTO hands (hand_hash, submitted_at) VALUES (?, ?)").bind(b.hand_hash, now).run();
  if ((ins.meta?.changes ?? 0) === 0) return "duplicate";
  const score = (p) => 0.5 * Math.max(-CAP_BB, Math.min(CAP_BB, p.delta / b.bb));
  const losers = b.players.filter((p) => p.delta < 0);
  const lost = losers.reduce((n, p) => n - p.delta, 0);
  const stmts = [];
  const rating = new Map(b.players.map((p) => [p.pubkey, p.delta < 0 ? score(p) : 0]));

  for (const w of b.players.filter((p) => p.delta > 0)) {
    let earned = 0;
    for (const l of losers) {
      const share = score(w) * (-l.delta / lost);
      const others = await db.prepare("SELECT COUNT(*) AS n FROM pairs WHERE winner = ?1 AND loser != ?2").bind(l.pubkey, w.pubkey).first();
      if (others.n < MIN_DISTINCT) continue; // a fresh account is not a real opponent yet
      const row = await db.prepare("SELECT gained FROM pairs WHERE winner = ?1 AND loser = ?2").bind(w.pubkey, l.pubkey).first();
      const credit = Math.min(share, Math.max(0, PAIR_CAP - (row?.gained ?? 0)));
      if (credit <= 0) continue;
      earned += credit;
      stmts.push(
        db.prepare(
          `INSERT INTO pairs (winner, loser, gained) VALUES (?1, ?2, ?3)
           ON CONFLICT(winner, loser) DO UPDATE SET gained = gained + ?3`,
        ).bind(w.pubkey, l.pubkey, credit),
      );
    }
    rating.set(w.pubkey, earned);
  }
  for (const x of b.players) {
    for (const y of b.players) {
      if (x.pubkey !== y.pubkey) {
        stmts.push(db.prepare("INSERT OR IGNORE INTO pairs (winner, loser, gained) VALUES (?1, ?2, 0)").bind(x.pubkey, y.pubkey));
      }
    }
    stmts.push(
      db.prepare(
        `INSERT INTO players (pubkey, hands, won, net_bb, rating, updated_at) VALUES (?1, 1, ?2, ?3, 1000 + ?4, ?5)
         ON CONFLICT(pubkey) DO UPDATE SET hands = hands + 1, won = won + ?2, net_bb = net_bb + ?3,
           rating = rating + ?4, updated_at = ?5`,
      ).bind(x.pubkey, x.delta > 0 ? 1 : 0, x.delta / b.bb, rating.get(x.pubkey), now),
    );
  }
  await db.batch(stmts);
  return "accepted";
}

function stateName(country, region) {
  if (!country) return null;
  if (country === "US" && region) return region;
  try { return new Intl.DisplayNames(["en"], { type: "region" }).of(country) ?? country; } catch { return country; }
}

/** One request frame in, one reply object out. `ctx` = { region, country, now, sealSecret, sealPublic }. */
export async function handleFrame(db, frame, ctx) {
  switch (frame.op) {
    case "lb_profile": {
      const { pubkey, username, listed, show_state, sig } = frame;
      if (!USERNAME.test(username ?? "")) return { ok: false, error: "username must be 3-16 letters, numbers, _ or -" };
      const msg = `pokertui-profile-v1|${username}|${listed ? 1 : 0}|${show_state ? 1 : 0}`;
      if (!(await ed25519Verify(pubkey, msg, sig))) return { ok: false, error: "bad signature" };
      const taken = await db.prepare("SELECT pubkey FROM players WHERE username = ?1 COLLATE NOCASE AND pubkey != ?2").bind(username, pubkey).first();
      if (taken) return { ok: false, error: "taken" };
      // Only the state name is kept, never the IP address.
      const state = show_state ? stateName(ctx.country, ctx.region) : null;
      await db.prepare(
        `INSERT INTO players (pubkey, username, state, show_state, listed, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(pubkey) DO UPDATE SET username = ?2, state = ?3, show_state = ?4, listed = ?5, updated_at = ?6`,
      ).bind(pubkey, username, state, show_state ? 1 : 0, listed ? 1 : 0, ctx.now).run();
      return { ok: true };
    }
    case "lb_hands": {
      if (!Array.isArray(frame.items) || frame.items.length > 50) return { ok: false, error: "batch too large" };
      const bundles = [];
      for (const sealed of frame.items) {
        try {
          bundles.push(JSON.parse(new TextDecoder().decode(await openSealed(ctx.sealSecret, ctx.sealPublic, sealed))));
        } catch {
          bundles.push(null);
        }
      }
      const order = bundles.map((b, i) => [b?.ts_ms ?? 0, i]).sort((a, b) => a[0] - b[0]);
      const results = new Array(bundles.length);
      for (const [, i] of order) {
        const bad = bundles[i] ? await checkBundle(bundles[i], ctx.now) : "unreadable";
        results[i] = bad ? `rejected: ${bad}` : await applyBundle(db, bundles[i], ctx.now);
      }
      return { ok: true, results };
    }
    case "lb_get": {
      const top = Math.max(1, Math.min(100, frame.top | 0 || 100));
      const { results } = await db.prepare(
        `SELECT username, state FROM players WHERE listed = 1 AND username IS NOT NULL AND hands >= ?1
         ORDER BY rating DESC, hands DESC, username LIMIT ?2`,
      ).bind(MIN_HANDS, top).all();
      return { ok: true, rows: results.map((r, i) => ({ rank: i + 1, username: r.username, state: r.state ?? null })) };
    }
    case "lb_me": {
      const me = await db.prepare("SELECT username, hands, won, net_bb, rating, listed FROM players WHERE pubkey = ?1").bind(frame.pubkey).first();
      if (!me) return { ok: true, me: null };
      const ahead = await db.prepare("SELECT COUNT(*) AS n FROM players WHERE listed = 1 AND hands >= ?1 AND rating > ?2").bind(MIN_HANDS, me.rating).first();
      const ranked = me.listed && me.hands >= MIN_HANDS;
      return { ok: true, me: { ...me, rank: ranked ? ahead.n + 1 : null } };
    }
    default:
      return { ok: false, error: "unknown op" };
  }
}
