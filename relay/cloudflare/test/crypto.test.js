import test from "node:test";
import assert from "node:assert/strict";
import { createCipheriv, randomBytes } from "node:crypto";
import { chachaPolyOpen, openSealed, ed25519Verify } from "../src/crypto.js";
import { fixture } from "./helpers.js";

test("ChaCha20-Poly1305 matches node's implementation, including odd lengths", () => {
  for (const n of [0, 1, 15, 16, 17, 63, 64, 65, 200, 1000]) {
    const key = randomBytes(32), nonce = randomBytes(12), msg = randomBytes(n);
    const c = createCipheriv("chacha20-poly1305", key, nonce, { authTagLength: 16 });
    const sealed = Buffer.concat([c.update(msg), c.final(), c.getAuthTag()]);
    assert.deepEqual(Buffer.from(chachaPolyOpen(key, nonce, sealed)), msg, `length ${n}`);
    const bad = Buffer.from(sealed);
    bad[0] ^= 1;
    assert.throws(() => chachaPolyOpen(key, nonce, bad));
  }
});

test("opens a box sealed by the Rust client and verifies its signatures", async () => {
  const f = fixture();
  const bundle = JSON.parse(new TextDecoder().decode(await openSealed(f.server_secret, f.server_public, f.good)));
  assert.equal(bundle.hand_hash, f.hand_hash);
  assert.equal(bundle.players.length, 2);
  for (const p of bundle.players) {
    assert.ok(await ed25519Verify(p.pubkey, bundle.hand_hash, bundle.sigs[p.pubkey]));
  }
  assert.equal(await ed25519Verify(f.alice, bundle.hand_hash + "x", bundle.sigs[f.alice]), false);
});

test("a box sealed to someone else does not open", async () => {
  const f = fixture();
  const other = Buffer.alloc(32, 5).toString("base64url");
  await assert.rejects(openSealed(other, f.server_public, f.good));
});
