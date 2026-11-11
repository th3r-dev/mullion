//! Pairing: a receiver may stream only after the Mac's owner has let it in.
//!
//! First time: the receiver asks to pair (`/pair/start`) and the Mac shows a
//! 6-digit code. The person types it on the receiver, and both sides run
//! SPAKE2 with it (`/pair/exchange`, `/pair/confirm`). The code never crosses
//! the network, and someone listening learns nothing they could test guesses
//! against; someone in the middle gets one guess per attempt, and a pairing
//! allows three. On success the host gives the receiver its own device id and
//! a 32-byte secret, sealed with the SPAKE2 key.
//!
//! Every connection after that: the receiver gets a one-time challenge
//! (`/challenge`) and sends its offer sealed with a key from its secret and
//! that challenge. The host answers the same way. So only a paired receiver
//! can start a stream, an offer cannot be replayed, and nobody on the LAN can
//! read or change the SDP. The SDP carries the DTLS fingerprints, so the
//! media (already DTLS-SRTP encrypted) is bound to the paired pair of ends.
//!
//! Other requests from paired receivers (the layout map) carry
//! `X-Mullion-Auth: <device>.<unix ms>.<HMAC>` over method, path, time and
//! body. The JS side is crates/app/assets/pair.js; the two must match.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use spake2::{Ed25519Group, Identity, Password, Spake2};
use subtle::ConstantTimeEq;
use tracing::{info, warn};

/// SPAKE2 identity of the receiver side (A). The host side (B) is
/// `"mullion host " + host_id`, so a code is good for this host only.
pub const ID_RECEIVER: &[u8] = b"mullion receiver";
/// How long a pairing code is good for.
const CODE_TTL: Duration = Duration::from_secs(120);
/// Wrong codes before the pairing is cancelled.
const MAX_FAILURES: u8 = 3;
/// A new pairing may replace a pending one only after this long, so a
/// stream of requests cannot keep the owner's prompt flickering.
const MIN_RESTART: Duration = Duration::from_secs(5);
const CHALLENGE_TTL: Duration = Duration::from_secs(30);
/// Allowed clock difference for signed requests.
const AUTH_SKEW_MS: u64 = 60_000;
const MAX_DEVICES: usize = 64;

/// What the owner should see. Called outside the lock.
pub enum Notice {
    /// Show `code` for the device named `name`.
    Code { name: String, code: String },
    /// Hide the code: paired, cancelled, expired or replaced.
    Closed,
}

/// A failed pairing step, with the HTTP status it maps to.
#[derive(Debug, PartialEq)]
pub enum PairError {
    /// 400: malformed request.
    Bad(&'static str),
    /// 410: no pairing with that id, or it expired.
    Gone,
    /// 429: another device started pairing a moment ago.
    Busy,
    /// 403: the code was wrong; `left` attempts remain (0 = pairing cancelled).
    WrongCode { left: u8 },
    /// 507: too many paired devices.
    Full,
}

#[derive(Serialize, Deserialize, Default)]
struct Store {
    host_id: String,
    devices: Vec<Device>,
}

#[derive(Serialize, Deserialize, Clone)]
struct Device {
    id: String,
    name: String,
    /// 32 bytes, base64.
    secret: String,
    paired_at: u64,
}

struct Pending {
    id: String,
    code: String,
    name: String,
    started: Instant,
    failures: u8,
    /// SPAKE2 key from the latest `/pair/exchange`.
    key: Option<Vec<u8>>,
}

struct Inner {
    store: Store,
    pending: Option<Pending>,
    challenges: HashMap<String, ([u8; 16], Instant)>,
}

pub struct Pairing {
    path: Option<PathBuf>,
    inner: Mutex<Inner>,
    notify: Box<dyn Fn(Notice) + Send + Sync>,
}

/// `/pair/start` reply.
#[derive(Serialize)]
pub struct Started {
    pub pairing: String,
    pub host_id: String,
    pub expires_in: u64,
}

/// `/pair/confirm` reply: the host's confirmation and the sealed credential.
#[derive(Serialize)]
pub struct Confirmed {
    pub mac: String,
    pub credential: Sealed,
}

/// ChaCha20-Poly1305 output on the wire.
#[derive(Serialize, Deserialize, Clone)]
pub struct Sealed {
    /// 12-byte nonce, base64.
    pub n: String,
    /// Ciphertext and tag, base64.
    pub c: String,
}

/// A verified offer: who sent it, and the key to seal the answer with.
pub struct OfferCtx {
    pub device_id: String,
    pub device_name: String,
    answer_key: [u8; 32],
}

impl Pairing {
    /// Load paired devices from `path` (created on first pairing). `None`
    /// keeps everything in memory (tests).
    pub fn load(path: Option<PathBuf>, notify: Box<dyn Fn(Notice) + Send + Sync>) -> Self {
        let mut store: Store = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let fresh = store.host_id.is_empty();
        if fresh {
            store.host_id = hex(&random::<16>());
        }
        let p = Self {
            path,
            inner: Mutex::new(Inner { store, pending: None, challenges: HashMap::new() }),
            notify,
        };
        if fresh {
            p.save(&p.lock().store);
        }
        p
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn host_id(&self) -> String {
        self.lock().store.host_id.clone()
    }

    pub fn is_paired(&self, device: &str) -> bool {
        self.lock().store.devices.iter().any(|d| d.id == device)
    }

    pub fn paired_count(&self) -> usize {
        self.lock().store.devices.len()
    }

    /// `/pair/start`: open a pairing and show its code on the Mac.
    pub fn start(&self, name: &str) -> Result<Started, PairError> {
        let (started, replaced) = {
            let mut g = self.lock();
            if let Some(p) = &g.pending {
                if p.started.elapsed() < MIN_RESTART {
                    return Err(PairError::Busy);
                }
            }
            if g.store.devices.len() >= MAX_DEVICES {
                return Err(PairError::Full);
            }
            let replaced = g.pending.is_some();
            let p = Pending {
                id: hex(&random::<16>()),
                code: six_digits(),
                name: name.to_string(),
                started: Instant::now(),
                failures: 0,
                key: None,
            };
            let out = Started { pairing: p.id.clone(), host_id: g.store.host_id.clone(), expires_in: CODE_TTL.as_secs() };
            let notice = Notice::Code { name: p.name.clone(), code: p.code.clone() };
            g.pending = Some(p);
            ((out, notice), replaced)
        };
        info!("pairing requested by {name}");
        if replaced {
            (self.notify)(Notice::Closed);
        }
        (self.notify)(started.1);
        Ok(started.0)
    }

    /// `/pair/exchange`: run the host side of SPAKE2 with the receiver's
    /// message. Returns the host's message. Learns nothing about the code
    /// yet; `/pair/confirm` does.
    pub fn exchange(&self, pairing: &str, msg_a: &str) -> Result<String, PairError> {
        let msg_a = B64.decode(msg_a).map_err(|_| PairError::Bad("msg is not base64"))?;
        let mut g = self.lock();
        let id_b = host_identity(&g.store.host_id);
        let p = self.live_pending(&mut g, pairing)?;
        let (spake, msg_b) = Spake2::<Ed25519Group>::start_b(
            &Password::new(p.code.as_bytes()),
            &Identity::new(ID_RECEIVER),
            &Identity::new(&id_b),
        );
        let key = spake.finish(&msg_a).map_err(|_| PairError::Bad("bad SPAKE2 message"))?;
        p.key = Some(key);
        Ok(B64.encode(msg_b))
    }

    /// `/pair/confirm`: check the receiver's confirmation. Right code: the
    /// device is paired and gets its credential. Wrong code: one attempt used.
    pub fn confirm(&self, pairing: &str, mac: &str, host_name: &str) -> Result<Confirmed, PairError> {
        let mac = B64.decode(mac).map_err(|_| PairError::Bad("mac is not base64"))?;
        let (result, closed) = {
            let mut g = self.lock();
            let host_id = g.store.host_id.clone();
            let p = self.live_pending(&mut g, pairing)?;
            let key = p.key.clone().ok_or(PairError::Bad("exchange first"))?;
            let keys = PairKeys::new(&key);
            if !bool::from(keys.receiver_mac().ct_eq(&mac)) {
                p.failures += 1;
                let left = MAX_FAILURES.saturating_sub(p.failures);
                p.key = None;
                warn!("pairing {}: wrong code ({left} attempts left)", p.name);
                if left == 0 {
                    g.pending = None;
                }
                (Err(PairError::WrongCode { left }), left == 0)
            } else {
                let name = p.name.clone();
                g.pending = None;
                let secret = random::<32>();
                let device = Device {
                    id: format!("d{}", hex(&random::<8>())),
                    name: name.clone(),
                    secret: B64.encode(secret),
                    paired_at: unix_ms() / 1000,
                };
                let plain = serde_json::json!({
                    "device": device.id, "secret": device.secret, "host_id": host_id, "host_name": host_name,
                });
                let credential = seal(&keys.seal, b"mullion pair seal", plain.to_string().as_bytes());
                g.store.devices.push(device);
                self.save(&g.store);
                info!("paired {name}");
                (Ok(Confirmed { mac: B64.encode(keys.host_mac()), credential }), true)
            }
        };
        if closed {
            (self.notify)(Notice::Closed);
        }
        result
    }

    /// The owner pressed Cancel on the code prompt.
    pub fn cancel(&self) {
        if self.lock().pending.take().is_some() {
            info!("pairing cancelled on the Mac");
        }
    }

    /// The receiver gave up (`/pair/cancel`): take the code off the Mac.
    pub fn abandon(&self, pairing: &str) {
        let gone = {
            let mut g = self.lock();
            let mine = g.pending.as_ref().is_some_and(|p| p.id == pairing);
            if mine {
                g.pending = None;
            }
            mine
        };
        if gone {
            (self.notify)(Notice::Closed);
        }
    }

    /// Forget every paired device: each has to pair again.
    pub fn forget_all(&self) {
        let mut g = self.lock();
        g.store.devices.clear();
        g.challenges.clear();
        self.save(&g.store);
        info!("forgot all paired devices");
    }

    fn live_pending<'a>(&self, g: &'a mut Inner, pairing: &str) -> Result<&'a mut Pending, PairError> {
        match &g.pending {
            Some(p) if p.id == pairing && p.started.elapsed() <= CODE_TTL => {}
            Some(p) if p.id == pairing => {
                g.pending = None;
                (self.notify)(Notice::Closed);
                return Err(PairError::Gone);
            }
            _ => return Err(PairError::Gone),
        }
        Ok(g.pending.as_mut().unwrap())
    }

    /// `/challenge`: a one-time nonce for the next offer from `device`.
    /// `None` when the device is not paired (it must pair first).
    pub fn challenge(&self, device: &str) -> Option<String> {
        let mut g = self.lock();
        g.store.devices.iter().find(|d| d.id == device)?;
        g.challenges.retain(|_, (_, t)| t.elapsed() < CHALLENGE_TTL);
        let nonce = random::<16>();
        g.challenges.insert(device.to_string(), (nonce, Instant::now()));
        Some(B64.encode(nonce))
    }

    /// Open a sealed offer. Uses up the device's challenge either way.
    pub fn open_offer(&self, device: &str, sealed: &Sealed) -> Result<(Vec<u8>, OfferCtx)> {
        let mut g = self.lock();
        let (nonce, at) = g.challenges.remove(device).ok_or_else(|| anyhow!("no challenge for this device"))?;
        if at.elapsed() > CHALLENGE_TTL {
            return Err(anyhow!("challenge expired"));
        }
        let d = g.store.devices.iter().find(|d| d.id == device).ok_or_else(|| anyhow!("not paired"))?;
        let secret = B64.decode(&d.secret).context("stored secret")?;
        let offer_key = derive(&secret, Some(&nonce), b"mullion offer");
        let plain = open(&offer_key, format!("offer:{device}").as_bytes(), sealed)?;
        let ctx = OfferCtx {
            device_id: d.id.clone(),
            device_name: d.name.clone(),
            answer_key: derive(&secret, Some(&nonce), b"mullion answer"),
        };
        Ok((plain, ctx))
    }

    pub fn seal_answer(&self, ctx: &OfferCtx, plain: &[u8]) -> Sealed {
        seal(&ctx.answer_key, format!("answer:{}", ctx.device_id).as_bytes(), plain)
    }

    /// Check `X-Mullion-Auth` on a request from a paired device. Returns the
    /// device id when the signature is good and fresh.
    pub fn verify_request(&self, header: &str, method: &str, path: &str, body: &[u8]) -> Option<String> {
        let mut it = header.splitn(3, '.');
        let (device, ts, mac) = (it.next()?, it.next()?, it.next()?);
        let ts_ms: u64 = ts.parse().ok()?;
        if unix_ms().abs_diff(ts_ms) > AUTH_SKEW_MS {
            return None;
        }
        let mac = B64.decode(mac).ok()?;
        let g = self.lock();
        let d = g.store.devices.iter().find(|d| d.id == device)?;
        let secret = B64.decode(&d.secret).ok()?;
        let want = request_mac(&secret, method, path, ts, body);
        bool::from(want.ct_eq(&mac)).then(|| d.id.clone())
    }

    fn save(&self, store: &Store) {
        let Some(path) = &self.path else { return };
        if let Err(e) = write_private(path, &serde_json::to_vec_pretty(store).unwrap_or_default()) {
            warn!("can't save paired devices to {}: {e:#}", path.display());
        }
    }
}

/// Keys from the SPAKE2 secret: one per direction for confirmation, one to
/// seal the credential.
pub(crate) struct PairKeys {
    receiver: [u8; 32],
    host: [u8; 32],
    pub(crate) seal: [u8; 32],
}

impl PairKeys {
    pub(crate) fn new(spake_key: &[u8]) -> Self {
        Self {
            receiver: derive(spake_key, None, b"mullion pair receiver"),
            host: derive(spake_key, None, b"mullion pair host"),
            seal: derive(spake_key, None, b"mullion pair seal"),
        }
    }
    pub(crate) fn receiver_mac(&self) -> Vec<u8> {
        hmac(&self.receiver, b"receiver")
    }
    pub(crate) fn host_mac(&self) -> Vec<u8> {
        hmac(&self.host, b"host")
    }
}

pub(crate) fn host_identity(host_id: &str) -> Vec<u8> {
    format!("mullion host {host_id}").into_bytes()
}

pub(crate) fn derive(ikm: &[u8], salt: Option<&[u8]>, info: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    Hkdf::<Sha256>::new(salt, ikm).expand(info, &mut out).expect("32 bytes is a valid HKDF length");
    out
}

fn hmac(key: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC takes any key length");
    m.update(msg);
    m.finalize().into_bytes().to_vec()
}

pub(crate) fn request_mac(secret: &[u8], method: &str, path: &str, ts: &str, body: &[u8]) -> Vec<u8> {
    let key = derive(secret, None, b"mullion request");
    let mut msg = format!("{method}\n{path}\n{ts}\n").into_bytes();
    msg.extend_from_slice(body);
    hmac(&key, &msg)
}

pub(crate) fn seal(key: &[u8; 32], aad: &[u8], plain: &[u8]) -> Sealed {
    let nonce = random::<12>();
    let ct = ChaCha20Poly1305::new(key.into())
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: plain, aad })
        .expect("encrypting in memory cannot fail");
    Sealed { n: B64.encode(nonce), c: B64.encode(ct) }
}

pub(crate) fn open(key: &[u8; 32], aad: &[u8], s: &Sealed) -> Result<Vec<u8>> {
    let nonce = B64.decode(&s.n).context("nonce")?;
    let ct = B64.decode(&s.c).context("ciphertext")?;
    if nonce.len() != 12 {
        return Err(anyhow!("nonce must be 12 bytes"));
    }
    ChaCha20Poly1305::new(key.into())
        .decrypt(Nonce::from_slice(&nonce), Payload { msg: &ct, aad })
        .map_err(|_| anyhow!("does not decrypt: wrong key or tampered"))
}

fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::getrandom(&mut b).expect("the OS random source is available");
    b
}

/// Uniform 000000-999999.
fn six_digits() -> String {
    loop {
        let n = u32::from_le_bytes(random::<4>());
        // Reject the top sliver so every code is equally likely.
        if n < 4_294_000_000 {
            return format!("{:06}", n % 1_000_000);
        }
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub(crate) fn unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Write `bytes` readable by this user only (the file holds device secrets).
fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(&tmp)?.write_all(bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// A pairing and the code it showed.
    fn pairing() -> (Pairing, Arc<Mutex<Vec<String>>>) {
        let shown = Arc::new(Mutex::new(Vec::new()));
        let s = shown.clone();
        let p = Pairing::load(None, Box::new(move |n| {
            if let Notice::Code { code, .. } = n {
                s.lock().unwrap().push(code);
            }
        }));
        (p, shown)
    }

    /// The receiver's side, as pair.js does it.
    fn receiver(p: &Pairing, pairing: &str, code: &str) -> Result<Confirmed, PairError> {
        let id_b = host_identity(&p.host_id());
        let (spake, msg_a) =
            Spake2::<Ed25519Group>::start_a(&Password::new(code.as_bytes()), &Identity::new(ID_RECEIVER), &Identity::new(&id_b));
        let msg_b = B64.decode(p.exchange(pairing, &B64.encode(msg_a))?).unwrap();
        let keys = PairKeys::new(&spake.finish(&msg_b).unwrap());
        let c = p.confirm(pairing, &B64.encode(keys.receiver_mac()), "Mac")?;
        assert_eq!(B64.decode(&c.mac).unwrap(), keys.host_mac(), "host proves it knew the code");
        let cred: serde_json::Value = serde_json::from_slice(&open(&keys.seal, b"mullion pair seal", &c.credential).unwrap()).unwrap();
        assert_eq!(cred["host_id"], p.host_id());
        Ok(c)
    }

    #[test]
    fn right_code_pairs_and_wrong_codes_run_out() {
        let (p, shown) = pairing();
        let s = p.start("iPad").unwrap();
        let code = shown.lock().unwrap()[0].clone();
        assert_eq!(code.len(), 6);
        let wrong = if code == "000000" { "111111" } else { "000000" };
        assert_eq!(receiver(&p, &s.pairing, wrong).err(), Some(PairError::WrongCode { left: 2 }));
        assert!(receiver(&p, &s.pairing, &code).is_ok());
        assert_eq!(p.paired_count(), 1);
        // A finished pairing cannot be reused.
        assert_eq!(receiver(&p, &s.pairing, &code).err(), Some(PairError::Gone));

        p.lock().pending = None;
        let s = p.start("Pixel").unwrap();
        let code = shown.lock().unwrap()[1].clone();
        let wrong = if code == "000000" { "111111" } else { "000000" };
        for left in [2, 1, 0] {
            assert_eq!(receiver(&p, &s.pairing, wrong).err(), Some(PairError::WrongCode { left }));
        }
        assert_eq!(receiver(&p, &s.pairing, &code).err(), Some(PairError::Gone), "three wrong codes end the pairing");
        assert_eq!(p.paired_count(), 1);
    }

    /// pair.js and the spake2 crate agree. The receiver half comes from
    /// `node tools/pair-vectors.mjs <msg_b>` with a fixed scalar; the host half
    /// uses a seeded RNG. Same code, ids and messages must give the same key.
    #[test]
    fn js_and_rust_agree() {
        use rand::SeedableRng;
        let (spake, msg_b) = Spake2::<Ed25519Group>::start_b_with_rng(
            &Password::new(b"482913"),
            &Identity::new(ID_RECEIVER),
            &Identity::new(&host_identity("cafe0123")),
            rand::rngs::StdRng::seed_from_u64(7),
        );
        assert_eq!(B64.encode(msg_b), "QoDhvUnKUD79cyOl9/XYCHhm4fsOoCqAuKvLwJOKkyXm");
        let msg_a = B64.decode("Qf68NRRbowfCin/xPzWi9LHL78uyHmJ9Ws+FGjsWUfGQ").unwrap();
        let key = spake.finish(&msg_a).unwrap();
        assert_eq!(hex(&key), "2cf7f820018b00c4bf72c547258b68d225b514e49b62e848cc36cbfad1534691");
        assert_eq!(B64.encode(PairKeys::new(&key).receiver_mac()), "Wxz+wml1hXz6lntChcerOHQSxf427BkJRUs0he5KAEM=");

        // An offer sealed by pair.js (secret 9s, challenge 5s) opens here.
        let sealed = Sealed { n: "CVRRmdltZaTU8rPu".into(), c: "89z8t3KJsirxvVtWFnQpm2MnGSdF6j/oLtEupLyTvt2r6dIMtO/GSFu+jkcm".into() };
        let plain = open(&derive(&[9u8; 32], Some(&[5u8; 16]), b"mullion offer"), b"offer:dabc", &sealed).unwrap();
        assert_eq!(plain, br#"{"sdp":"v=0","mode":"extend"}"#);
        // And its signed request header matches.
        let mac = request_mac(&[9u8; 32], "POST", "/screens/layout", "1700000000000", br#"{"cells":[]}"#);
        assert_eq!(B64.encode(mac), "sDGScS+0z/QvRvk0xJqALguLSXiaSVKpUOMZVKb1JmI=");
    }

    #[test]
    fn a_second_request_cannot_replace_a_fresh_one() {
        let (p, _) = pairing();
        p.start("A").unwrap();
        assert!(matches!(p.start("B"), Err(PairError::Busy)));
    }

    #[test]
    fn sealed_offer_needs_the_secret_and_a_fresh_challenge() {
        let (p, shown) = pairing();
        let s = p.start("iPad").unwrap();
        let code = shown.lock().unwrap()[0].clone();
        receiver(&p, &s.pairing, &code).unwrap();
        let (device, secret) = {
            let g = p.lock();
            let d = &g.store.devices[0];
            (d.id.clone(), B64.decode(&d.secret).unwrap())
        };
        assert!(p.challenge("d-unknown").is_none(), "unpaired devices get no challenge");

        let nonce = B64.decode(p.challenge(&device).unwrap()).unwrap();
        let key = derive(&secret, Some(&nonce), b"mullion offer");
        let sealed = seal(&key, format!("offer:{device}").as_bytes(), b"{\"sdp\":\"v=0\"}");
        let (plain, ctx) = p.open_offer(&device, &sealed).unwrap();
        assert_eq!(plain, b"{\"sdp\":\"v=0\"}");
        let answer = p.seal_answer(&ctx, b"answer");
        assert_eq!(open(&derive(&secret, Some(&nonce), b"mullion answer"), format!("answer:{device}").as_bytes(), &answer).unwrap(), b"answer");
        // Replay: the challenge is used up.
        assert!(p.open_offer(&device, &sealed).is_err());
        // Wrong secret.
        p.challenge(&device).unwrap();
        let bad = seal(&[7u8; 32], format!("offer:{device}").as_bytes(), b"x");
        assert!(p.open_offer(&device, &bad).is_err());
    }

    #[test]
    fn signed_requests_check_device_time_and_body() {
        let (p, shown) = pairing();
        let s = p.start("iPad").unwrap();
        let code = shown.lock().unwrap()[0].clone();
        receiver(&p, &s.pairing, &code).unwrap();
        let (device, secret) = {
            let g = p.lock();
            (g.store.devices[0].id.clone(), B64.decode(&g.store.devices[0].secret).unwrap())
        };
        let ts = unix_ms().to_string();
        let mac = B64.encode(request_mac(&secret, "POST", "/screens/layout", &ts, b"{}"));
        let h = format!("{device}.{ts}.{mac}");
        assert_eq!(p.verify_request(&h, "POST", "/screens/layout", b"{}"), Some(device.clone()));
        assert_eq!(p.verify_request(&h, "POST", "/screens/layout", b"{\"x\":1}"), None, "body is signed");
        assert_eq!(p.verify_request(&h, "GET", "/screens", b"{}"), None, "path and method are signed");
        let old = (unix_ms() - 120_000).to_string();
        let mac = B64.encode(request_mac(&secret, "GET", "/screens", &old, b""));
        assert_eq!(p.verify_request(&format!("{device}.{old}.{mac}"), "GET", "/screens", b""), None, "stale");
        p.forget_all();
        let ts = unix_ms().to_string();
        let mac = B64.encode(request_mac(&secret, "GET", "/screens", &ts, b""));
        assert_eq!(p.verify_request(&format!("{device}.{ts}.{mac}"), "GET", "/screens", b""), None, "forgotten");
    }

    #[test]
    fn store_survives_a_restart_and_is_private() {
        let dir = std::env::temp_dir().join(format!("mullion-pair-test-{}", hex(&random::<4>())));
        let path = dir.join("paired.json");
        let shown = Arc::new(Mutex::new(Vec::<String>::new()));
        let s2 = shown.clone();
        let p = Pairing::load(Some(path.clone()), Box::new(move |n| {
            if let Notice::Code { code, .. } = n {
                s2.lock().unwrap().push(code);
            }
        }));
        let s = p.start("iPad").unwrap();
        let code = shown.lock().unwrap()[0].clone();
        receiver(&p, &s.pairing, &code).unwrap();
        let again = Pairing::load(Some(path.clone()), Box::new(|_| {}));
        assert_eq!(again.host_id(), p.host_id());
        assert_eq!(again.paired_count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}

