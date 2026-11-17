// Prints the relay's sealing keypair. Put the SECRET in `wrangler secret put SEAL_SECRET`,
// the PUBLIC in wrangler.toml ([vars] SEAL_PUBLIC) and in every player's config as `relay_key`.
const { subtle } = globalThis.crypto;
const b64u = (u8) => Buffer.from(u8).toString("base64url");
const kp = await subtle.generateKey({ name: "X25519" }, true, ["deriveBits"]);
const pkcs8 = new Uint8Array(await subtle.exportKey("pkcs8", kp.privateKey));
const pub = new Uint8Array(await subtle.exportKey("raw", kp.publicKey));
console.log("SEAL_SECRET =", b64u(pkcs8.slice(-32)));
console.log("SEAL_PUBLIC =", b64u(pub));
