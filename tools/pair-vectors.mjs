// Prints the receiver side of a fixed SPAKE2 exchange with pair.js, for the
// interop test in crates/host/src/pairing.rs (js_and_rust_agree).
//   node tools/pair-vectors.mjs <host msg B, base64>
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
const ctx = { window: {}, crypto: globalThis.crypto, TextEncoder, TextDecoder, btoa, atob, localStorage: { getItem() {}, setItem() {}, removeItem() {} } };
ctx.window = ctx; vm.createContext(ctx);
for (const f of ['vendor/noble.js', 'pair.js']) vm.runInContext(readFileSync(new URL(`../crates/app/assets/${f}`, import.meta.url), 'utf8'), ctx);
const t = ctx.mullionPair._test;
const a = t.spakeA('482913', 'cafe0123', 0x1234567890abcdef1234567890abcdefn);
const out = { msg_a: t.b64(a.msg) };
if (process.argv[2]) {
  const key = a.finish(t.unb64(process.argv[2]));
  out.key = Buffer.from(key).toString('hex');
  out.receiver_mac = t.b64(t.mac(t.derive(key, undefined, 'mullion pair receiver'), 'receiver'));
}
// Sealed offer and signed request with a fixed secret and challenge.
const cred = { device: 'dabc', secret: t.b64(new Uint8Array(32).fill(9)) };
out.offer = ctx.mullionPair.sealOffer(cred, t.b64(new Uint8Array(16).fill(5)), { sdp: 'v=0', mode: 'extend' });
const realNow = Date.now; ctx.Date = { now: () => 1700000000000 };
vm.runInContext('Date = globalThis.Date', ctx);
out.auth = ctx.mullionPair.signed(cred, 'POST', '/screens/layout', '{"cells":[]}');
console.log(JSON.stringify(out));
