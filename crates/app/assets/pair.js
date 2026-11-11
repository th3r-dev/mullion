// Pairing and sealed signaling, the receiver's side of crates/host/src/pairing.rs.
// The two must match byte for byte. Crypto comes from vendor/noble.js
// (window.MullionNoble) because a page served over http://<LAN IP> is not a
// secure context and has no crypto.subtle; crypto.getRandomValues works there.
//
//   pair(base, { name, hostName, askCode })  first time: SPAKE2 with the code the host shows
//   load(hostId) / forget(hostId)  the stored credential for a host
//   sealOffer / openAnswer         the offer and answer, sealed with the device secret
//   signed(cred, method, path, body)  X-Mullion-Auth header for other requests
(function () {
  'use strict';
  const N = window.MullionNoble;
  const P = N.ed25519.Point;
  const L = P.Fn.ORDER;
  const enc = new TextEncoder();
  const dec = new TextDecoder();
  const KEY = 'mullion.pair.';

  // --- bytes ---
  const concat = (...parts) => {
    const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
    let o = 0;
    for (const p of parts) { out.set(p, o); o += p.length; }
    return out;
  };
  const fromHex = (h) => Uint8Array.from(h.match(/../g).map((b) => parseInt(b, 16)));
  const toBig = (b) => b.reduce((n, x) => (n << 8n) | BigInt(x), 0n); // big-endian
  const b64 = (b) => { let s = ''; for (const x of b) s += String.fromCharCode(x); return btoa(s); };
  const unb64 = (s) => Uint8Array.from(atob(s), (c) => c.charCodeAt(0));
  const random = (n) => crypto.getRandomValues(new Uint8Array(n));

  // --- SPAKE2, as the Rust spake2 0.4 crate does it on Ed25519 ---
  const M = P.fromBytes(fromHex('15cfd18e385952982b6a8f8c7854963b58e34388c8e6dae891db756481a02312'));
  const NN = P.fromBytes(fromHex('f04f2e7eb734b2a8f8b472eaf9c3c632576ac64aea650b496a8a20ff00e583c3'));
  const ID_A = enc.encode('mullion receiver');
  // Password scalar: HKDF-SHA256(salt "", info "SPAKE2 pw", 48 bytes), read big-endian, mod L.
  const pwScalar = (pw) => toBig(N.hkdf(N.sha256, pw, new Uint8Array(0), enc.encode('SPAKE2 pw'), 48)) % L;
  function randomScalar() {
    for (;;) { const s = toBig(random(64)) % L; if (s !== 0n) return s; }
  }
  // Side A. `x` is for tests only.
  function spakeA(code, hostId, x) {
    const pw = enc.encode(code);
    const idB = enc.encode('mullion host ' + hostId);
    const w = pwScalar(pw);
    x = x || randomScalar();
    const Xb = P.BASE.multiply(x).add(M.multiply(w)).toBytes();
    return {
      msg: concat([0x41], Xb),
      finish(msgB) {
        if (msgB.length !== 33 || msgB[0] !== 0x42) throw new Error('bad SPAKE2 message');
        const Yb = msgB.subarray(1);
        const K = P.fromBytes(Yb).add(NN.multiply(w).negate()).multiply(x);
        return N.sha256(concat(N.sha256(pw), N.sha256(ID_A), N.sha256(idB), Xb, Yb, K.toBytes()));
      },
    };
  }

  // --- keys, MACs, sealing ---
  const derive = (ikm, salt, info) => N.hkdf(N.sha256, ikm, salt, enc.encode(info), 32);
  const mac = (key, msg) => N.hmac(N.sha256, key, typeof msg === 'string' ? enc.encode(msg) : msg);
  function seal(key, aad, plain) {
    const n = random(12);
    return { n: b64(n), c: b64(N.chacha20poly1305(key, n, enc.encode(aad)).encrypt(plain)) };
  }
  const open = (key, aad, s) => N.chacha20poly1305(key, unb64(s.n), enc.encode(aad)).decrypt(unb64(s.c));
  function sameBytes(a, b) {
    if (a.length !== b.length) return false;
    let d = 0; for (let i = 0; i < a.length; i++) d |= a[i] ^ b[i];
    return d === 0;
  }

  // --- stored credentials, one per host ---
  function load(hostId) {
    try { return JSON.parse(localStorage.getItem(KEY + hostId) || 'null'); } catch (e) { return null; }
  }
  function store(cred) { try { localStorage.setItem(KEY + cred.host_id, JSON.stringify(cred)); } catch (e) { /* */ } }
  function forget(hostId) { try { localStorage.removeItem(KEY + hostId); } catch (e) { /* */ } }

  // A request to a Mullion host. In the apps it goes through native code
  // (window.mullionNative.http, app main.rs): the Android page is https and its
  // WebView blocks http:// requests as mixed content. In a browser, fetch.
  // Resolves to a Response either way; network failures reject with TypeError
  // and an aborted signal with AbortError, as fetch does.
  async function hostFetch(url, init) {
    init = init || {};
    const http = window.mullionNative && window.mullionNative.http;
    if (!http) return fetch(url, Object.assign({ cache: 'no-store', credentials: 'omit' }, init));
    const abortErr = () => new DOMException('The request was aborted.', 'AbortError');
    const call = http(init.method || 'GET', url, init.headers || {}, init.body || '');
    const s = init.signal;
    const r = await (s ? Promise.race([call, new Promise((_, no) => { if (s.aborted) no(abortErr()); s.addEventListener('abort', () => no(abortErr())); })]) : call);
    if (r.error) throw new TypeError(r.error);
    const empty = [101, 103, 204, 205, 304].includes(r.status);
    return new Response(empty ? null : r.body, { status: r.status });
  }
  window.mullionFetch = hostFetch;

  async function post(url, body) {
    const r = await hostFetch(url, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) });
    let data = null;
    try { data = await r.json(); } catch (e) { /* plain text error */ }
    return { status: r.status, data };
  }

  // First connection to a host: ask it to show a code, ask the person for it,
  // and run SPAKE2. askCode({ hostName, error }) resolves to the typed code,
  // or null to give up. Resolves to the stored credential, or null.
  async function pair(base, opts) {
    // The host is a Mac or, sharing its screen, a tablet: call it by its name.
    const who = opts.hostName || 'the host';
    const Who = who[0].toUpperCase() + who.slice(1);
    const s = await post(base + '/pair/start', { name: opts.name });
    if (s.status === 429) throw new Error(`Another device is pairing with ${who}. Try again in a few seconds.`);
    if (s.status !== 200 || !s.data) throw new Error(`${Who} did not start pairing.`);
    const { pairing, host_id: hostId } = s.data;
    let error = '';
    for (;;) {
      const code = await opts.askCode({ hostName: opts.hostName, error });
      if (code == null) {
        post(base + '/pair/cancel', { pairing }).catch(() => {});
        return null;
      }
      const a = spakeA(code.replace(/\D/g, ''), hostId);
      const x = await post(base + '/pair/exchange', { pairing, msg: b64(a.msg) });
      if (x.status === 410) throw new Error('The code expired. Connect again for a new one.');
      if (x.status !== 200) throw new Error('Pairing failed.');
      const key = a.finish(unb64(x.data.msg));
      const k = { r: derive(key, undefined, 'mullion pair receiver'), h: derive(key, undefined, 'mullion pair host'), s: derive(key, undefined, 'mullion pair seal') };
      const c = await post(base + '/pair/confirm', { pairing, mac: b64(mac(k.r, 'receiver')) });
      if (c.status === 403) {
        const left = c.data && c.data.left;
        if (!left) throw new Error('Wrong code too many times. Connect again for a new one.');
        error = `That code is not the one on ${who}. ${left === 1 ? '1 try' : left + ' tries'} left.`;
        continue;
      }
      if (c.status === 410) throw new Error('The code expired. Connect again for a new one.');
      if (c.status !== 200) throw new Error('Pairing failed.');
      // The host proves it knew the code too, so this is the host that showed it.
      if (!sameBytes(unb64(c.data.mac), mac(k.h, 'host'))) throw new Error(`${Who} could not prove the code. Pairing stopped.`);
      const plain = JSON.parse(dec.decode(open(k.s, 'mullion pair seal', c.data.credential)));
      const cred = { host_id: plain.host_id, host_name: plain.host_name, device: plain.device, secret: plain.secret };
      store(cred);
      return cred;
    }
  }

  // Offer and answer, sealed with keys from the device secret and the host's
  // one-time challenge.
  function sealOffer(cred, nonce, obj) {
    const key = derive(unb64(cred.secret), unb64(nonce), 'mullion offer');
    return seal(key, 'offer:' + cred.device, enc.encode(JSON.stringify(obj)));
  }
  function openAnswer(cred, nonce, sealed) {
    const key = derive(unb64(cred.secret), unb64(nonce), 'mullion answer');
    return JSON.parse(dec.decode(open(key, 'answer:' + cred.device, sealed)));
  }

  // X-Mullion-Auth for requests other than the offer (layout map).
  function signed(cred, method, path, body) {
    const ts = String(Date.now());
    const key = derive(unb64(cred.secret), undefined, 'mullion request');
    const msg = concat(enc.encode(`${method}\n${path}\n${ts}\n`), body ? enc.encode(body) : new Uint8Array(0));
    return `${cred.device}.${ts}.${b64(mac(key, msg))}`;
  }

  window.mullionPair = { pair, load, forget, sealOffer, openAnswer, signed, _test: { spakeA, derive, mac, b64, unb64 } };
})();
