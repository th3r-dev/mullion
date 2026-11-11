//! FR-3: the signaling endpoint. Plain HTTP on the LAN (SRS A-7), port 47100
//! by default (A-8). `POST /offer` takes `{"type":"offer","sdp":"..."}` and
//! returns `{"type":"answer","sdp":"...","host_name":"..."}`. `GET /probe`
//! serves the latency probe page. Anything else is 404; wrong methods are 405;
//! bodies over the limit are 413 (T-10). Startup on a busy port is a named
//! error pointing at the flag (T-11).
//!
//! Layout map: `GET /screens` lists every display with its grid cell;
//! `POST /screens/layout` with `{"cells":[{"display_id":7,"col":-1,"row":0}]}`
//! moves screens and answers with the new `/screens`. A layout that cannot be
//! placed is 409 with the reason.
//!
//! Discovery: `GET /hello` says who this host is, so a receiver sweeping the
//! LAN can tell a Mullion host from any other HTTP server and list it by name.
//!
//! Web port: the same routes are also served on port 80 by default, so the
//! bare address (http://10.43.77.126) opens the receiver in any browser. If
//! that port cannot be had, the host carries on with the main port only.
//!
//! Pairing (pairing.rs): with a `Pairing` attached, only paired receivers may
//! stream. `POST /pair/start|exchange|confirm|cancel` pair a new receiver with the
//! code the Mac shows; `POST /challenge` then `POST /offer` carry a sealed
//! offer and answer. `/devices`, `/screens` and `/screens/layout` take a
//! request signed by a paired receiver (`X-Mullion-Auth`), or come from this
//! Mac itself (loopback, no foreign Origin). Without a `Pairing` (tests) the
//! host takes plain offers as before.

use std::io::Read;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use mullion_proto::signal::{clean_device_name, parse_offer, valid_device_id, Mode, MAX_BODY_BYTES};
use crate::screens::{CellAssign, LayoutRequest, Rejected, ScreenKind, ScreensView};
use crate::session::{DeviceInfo, DevicesSnapshot};
use serde::Serialize;
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};
use tracing::{info, warn};

use crate::pairing::{PairError, Pairing, Sealed};
use crate::probe_page::{icon_file, receiver_html, PROBE_HTML};

/// What the server hands a validated offer to. Returns the answer SDP.
pub trait OfferHandler: Send + Sync + 'static {
    fn answer(&self, offer_sdp: &str, mode: Mode, device: DeviceInfo, peer: SocketAddr) -> Result<String>;
    fn devices(&self) -> DevicesSnapshot {
        DevicesSnapshot::default()
    }
    fn screens(&self) -> Result<ScreensView> {
        Ok(ScreensView::default())
    }
    fn set_layout(&self, _cells: &[CellAssign]) -> Result<ScreensView> {
        Err(Rejected("arranging screens is not available on this host".into()).into())
    }
}

/// Largest `POST /screens/layout` body: a few dozen cells.
const MAX_LAYOUT_BYTES: usize = 16 * 1024;
/// Largest pairing or challenge request body.
const MAX_PAIR_BYTES: usize = 4 * 1024;

impl<F> OfferHandler for F
where
    F: Fn(&str, Mode, SocketAddr) -> Result<String> + Send + Sync + 'static,
{
    fn answer(&self, offer_sdp: &str, mode: Mode, _device: DeviceInfo, peer: SocketAddr) -> Result<String> {
        (self)(offer_sdp, mode, peer)
    }
}

#[derive(Serialize)]
struct AnswerBody<'a> {
    r#type: &'static str,
    sdp: &'a str,
    host_name: &'a str,
    /// Whether frames carry the latency strip; the receiver skips the
    /// per-frame readback when they do not.
    probe: bool,
}

pub struct Signaling {
    server: Server,
    /// Second listener on the web port (80), same routes.
    web: Option<Server>,
    handler: Arc<dyn OfferHandler>,
    host_name: String,
    config: serde_json::Value,
    lan_ips: Vec<String>,
    pairing: Option<Arc<Pairing>>,
}

/// `GET /hello`: enough for a receiver to list this host and connect to it.
#[derive(Serialize)]
struct Hello<'a> {
    /// Always "mullion"; a sweep ignores anything else answering on the port.
    app: &'static str,
    name: &'a str,
    version: &'static str,
    platform: &'static str,
    port: u16,
    /// LAN addresses this host answers on (not loopback).
    ips: &'a [String],
    /// Extend mode allowed (virtual displays).
    extend: bool,
    /// Receivers connected right now.
    receivers: usize,
    /// Stable id of this host; a receiver keeps its pairing under it.
    /// Absent: an older host that takes plain offers.
    #[serde(skip_serializing_if = "Option::is_none")]
    host_id: Option<String>,
}

fn platform_name() -> &'static str {
    match std::env::consts::OS {
        "macos" => "macOS",
        "windows" => "Windows",
        "linux" => "Linux",
        other => other,
    }
}

impl Signaling {
    /// Bind the listener. Fails with a message naming the port and the flag
    /// when the port is taken (T-11).
    pub fn bind(addr: SocketAddr, handler: Arc<dyn OfferHandler>, host_name: String) -> Result<Self> {
        let server = Server::http(addr).map_err(|e| {
            anyhow!("Can't listen on port {}: {}. Pick another with --port.", addr.port(), e)
        })?;
        Ok(Self { server, web: None, handler, host_name, config: serde_json::Value::Null, lan_ips: Vec::new(), pairing: None })
    }

    /// Effective switches, served at `/config`.
    pub fn with_config(mut self, config: serde_json::Value) -> Self {
        self.config = config;
        self
    }

    /// Also listen on `port` (0 = off) at the main listener's address. A port
    /// that cannot be had (in use, or privileged on this OS) is a warning, not
    /// an error: the main port keeps working.
    pub fn with_web_port(self, port: u16) -> Self {
        if port == 0 || port == self.local_addr().port() {
            return self;
        }
        let addr = SocketAddr::new(self.local_addr().ip(), port);
        self.with_web_addr(addr)
    }

    fn with_web_addr(mut self, addr: SocketAddr) -> Self {
        match Server::http(addr) {
            Ok(s) => {
                info!("also serving on http://{addr} (web port)");
                self.web = Some(s);
            }
            Err(e) => warn!("web port {}: {e}; the receiver stays on port {} only", addr.port(), self.local_addr().port()),
        }
        self
    }

    /// The web port actually being served, if any.
    pub fn web_port(&self) -> Option<u16> {
        self.web.as_ref().map(|s| match s.server_addr() {
            tiny_http::ListenAddr::IP(a) => a.port(),
            #[cfg(unix)]
            tiny_http::ListenAddr::Unix(_) => 0,
        })
    }

    /// Require pairing: only paired receivers may stream or read the layout.
    pub fn with_pairing(mut self, pairing: Arc<Pairing>) -> Self {
        self.pairing = Some(pairing);
        self
    }

    /// LAN addresses listed by `/hello`.
    pub fn with_lan_ips(mut self, ips: Vec<String>) -> Self {
        self.lan_ips = ips;
        self
    }

    pub fn local_addr(&self) -> SocketAddr {
        match self.server.server_addr() {
            tiny_http::ListenAddr::IP(a) => a,
            #[cfg(unix)]
            tiny_http::ListenAddr::Unix(_) => unreachable!("not a unix socket"),
        }
    }

    /// Serve forever on the current thread (the web port on a second thread).
    pub fn run(mut self) {
        let web = self.web.take();
        let this = Arc::new(self);
        if let Some(web) = web {
            let t = this.clone();
            std::thread::Builder::new()
                .name("mullion-web".into())
                .spawn(move || {
                    for req in web.incoming_requests() {
                        if let Err(e) = t.handle(req) {
                            warn!("web port: {e:#}");
                        }
                    }
                })
                .expect("spawn web port thread");
        }
        for req in this.server.incoming_requests() {
            if let Err(e) = this.handle(req) {
                warn!("signaling: {e:#}");
            }
        }
    }

    fn handle(&self, mut req: Request) -> Result<()> {
        let url = req.url().to_string();
        let path = url.split('?').next().unwrap_or("/").to_string();
        let method = req.method().clone();
        let peer = req.remote_addr().copied().unwrap_or_else(|| "0.0.0.0:0".parse().unwrap());

        // CORS: the receiver app's WebView origin is never ours.
        if method == Method::Options {
            return req.respond(with_cors(Response::empty(204))).context("respond");
        }

        match (path.as_str(), &method) {
            ("/offer", Method::Post) => {
                let declared = req.body_length().unwrap_or(0);
                if declared > MAX_BODY_BYTES {
                    return respond_status(req, 413, "Offer too large");
                }
                let mut body = Vec::with_capacity(declared.min(MAX_BODY_BYTES));
                let n = req
                    .as_reader()
                    .take(MAX_BODY_BYTES as u64 + 1)
                    .read_to_end(&mut body)
                    .unwrap_or(0);
                if n > MAX_BODY_BYTES {
                    return respond_status(req, 413, "Offer too large");
                }
                // Paired hosts take only a sealed offer from a paired device.
                let (body, ctx) = match &self.pairing {
                    None => (body, None),
                    Some(p) => {
                        #[derive(serde::Deserialize)]
                        struct SealedOffer {
                            device: String,
                            sealed: Sealed,
                        }
                        let Ok(so) = serde_json::from_slice::<SealedOffer>(&body) else {
                            return respond_json(req, 401, &serde_json::json!({"pair": true, "error": "Pair this device with the Mac first."}));
                        };
                        if !p.is_paired(&so.device) {
                            return respond_json(req, 401, &serde_json::json!({"pair": true, "error": "This Mac no longer knows this device. Pair again."}));
                        }
                        match p.open_offer(&so.device, &so.sealed) {
                            Ok((plain, ctx)) => (plain, Some(ctx)),
                            Err(e) => {
                                warn!("offer from {peer} refused: {e:#}");
                                return respond_json(req, 401, &serde_json::json!({"pair": false, "error": "The offer did not check out. Connect again."}));
                            }
                        }
                    }
                };
                let offer = match parse_offer(&body) {
                    Ok(o) => o,
                    Err(413) => return respond_status(req, 413, "Offer too large"),
                    Err(_) => return respond_status(req, 400, "Body must be JSON {\"sdp\":\"v=0...\"}"),
                };
                let device = match &ctx {
                    // The device is who the pairing says, whatever the offer claims.
                    Some(c) => DeviceInfo { id: Some(c.device_id.clone()), name: clean_device_name(&offer.device_name, &c.device_name) },
                    None => DeviceInfo {
                        id: valid_device_id(&offer.device_id).map(str::to_string),
                        name: clean_device_name(&offer.device_name, &format!("Tablet {}", peer.ip())),
                    },
                };
                match self.handler.answer(&offer.sdp, offer.mode, device, peer) {
                    Ok(sdp) => {
                        info!("offer from {peer} answered ({:?})", offer.mode);
                        let mut body = serde_json::to_vec(&AnswerBody {
                            r#type: "answer",
                            sdp: &sdp,
                            host_name: &self.host_name,
                            probe: self.config.get("probe").and_then(|v| v.as_bool()).unwrap_or(false),
                        })?;
                        if let (Some(p), Some(c)) = (&self.pairing, &ctx) {
                            body = serde_json::to_vec(&serde_json::json!({ "sealed": p.seal_answer(c, &body) }))?;
                        }
                        let resp = Response::from_data(body)
                            .with_header(Header::from_bytes("Content-Type", "application/json").unwrap());
                        req.respond(with_cors(resp)).context("respond")
                    }
                    Err(e) => {
                        warn!("offer from {peer} rejected: {e:#}");
                        respond_status(req, 400, &format!("Offer rejected: {e}"))
                    }
                }
            }
            ("/pair/start" | "/pair/exchange" | "/pair/confirm" | "/pair/cancel" | "/challenge", Method::Post) => {
                let Some(p) = self.pairing.clone() else { return respond_status(req, 404, "Not found") };
                let Some(body) = read_body(&mut req, MAX_PAIR_BYTES) else { return respond_status(req, 413, "Too large") };
                let Ok(v) = serde_json::from_slice::<serde_json::Value>(&body) else { return respond_status(req, 400, "Body must be JSON") };
                let field = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
                let out: Result<serde_json::Value, PairError> = match path.as_str() {
                    "/pair/start" => {
                        let name = clean_device_name(&field("name"), &format!("Device {}", peer.ip()));
                        p.start(&name).map(|s| serde_json::to_value(s).unwrap_or_default())
                    }
                    "/pair/exchange" => p.exchange(&field("pairing"), &field("msg")).map(|m| serde_json::json!({ "msg": m })),
                    "/pair/cancel" => {
                        p.abandon(&field("pairing"));
                        Ok(serde_json::json!({}))
                    }
                    "/pair/confirm" => p.confirm(&field("pairing"), &field("mac"), &self.host_name).map(|c| serde_json::to_value(c).unwrap_or_default()),
                    _ => match p.challenge(&field("device")) {
                        Some(nonce) => Ok(serde_json::json!({ "nonce": nonce })),
                        None => return respond_json(req, 401, &serde_json::json!({"pair": true, "error": "Pair this device with the Mac first."})),
                    },
                };
                match out {
                    Ok(v) => respond_json(req, 200, &v),
                    Err(PairError::Bad(why)) => respond_json(req, 400, &serde_json::json!({ "error": why })),
                    Err(PairError::Gone) => respond_json(req, 410, &serde_json::json!({ "error": "No pairing in progress, or it expired." })),
                    Err(PairError::Busy) => respond_json(req, 429, &serde_json::json!({ "error": "Another device is pairing. Try again in a few seconds." })),
                    Err(PairError::WrongCode { left }) => respond_json(req, 403, &serde_json::json!({ "error": "Wrong code.", "left": left })),
                    Err(PairError::Full) => respond_json(req, 507, &serde_json::json!({ "error": "Too many paired devices. Forget some on the Mac." })),
                }
            }
            ("/offer", _) => respond_status(req, 405, "Use POST"),
            ("/probe", Method::Get) => {
                let resp = Response::from_string(PROBE_HTML)
                    .with_header(Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap());
                req.respond(with_cors(resp)).context("respond")
            }
            ("/receiver", Method::Get) | ("/", Method::Get) => {
                // Prefill the host IP the request came in on.
                let host_ip = req
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("Host"))
                    .map(|h| h.value.as_str().split(':').next().unwrap_or("").to_string())
                    .unwrap_or_default();
                let resp = Response::from_string(receiver_html(&host_ip, self.local_addr().port()))
                    .with_header(Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap())
                    .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
                req.respond(with_cors(resp)).context("respond")
            }
            ("/config", Method::Get) => {
                let body = serde_json::to_vec_pretty(&self.config)?;
                let resp = Response::from_data(body).with_header(Header::from_bytes("Content-Type", "application/json").unwrap());
                req.respond(with_cors(resp)).context("respond")
            }
            ("/devices", Method::Get) => {
                if !self.allowed(&req, peer, &path, b"") {
                    return respond_status(req, 401, "Pair this device with the Mac first");
                }
                let body = serde_json::to_vec(&self.handler.devices())?;
                let resp = Response::from_data(body).with_header(Header::from_bytes("Content-Type", "application/json").unwrap());
                req.respond(with_cors(resp)).context("respond")
            }
            ("/screens", Method::Get) => {
                if !self.allowed(&req, peer, &path, b"") {
                    return respond_status(req, 401, "Pair this device with the Mac first");
                }
                let view = self.handler.screens();
                self.respond_screens(req, view)
            }
            ("/screens", _) => respond_status(req, 405, "Use GET"),
            ("/screens/layout", Method::Post) => {
                let mut body = Vec::new();
                let n = req.as_reader().take(MAX_LAYOUT_BYTES as u64 + 1).read_to_end(&mut body).unwrap_or(0);
                if n > MAX_LAYOUT_BYTES {
                    return respond_status(req, 413, "Layout too large");
                }
                if !self.allowed(&req, peer, &path, &body) {
                    return respond_status(req, 401, "Pair this device with the Mac first");
                }
                let Ok(layout) = serde_json::from_slice::<LayoutRequest>(&body) else {
                    return respond_status(req, 400, "Body must be JSON {\"cells\":[{\"display_id\":7,\"col\":1,\"row\":0}]}");
                };
                info!("layout from {peer}: {:?}", layout.cells);
                let view = self.handler.set_layout(&layout.cells);
                self.respond_screens(req, view)
            }
            ("/screens/layout", _) => respond_status(req, 405, "Use POST"),
            (p, Method::Get | Method::Head) if icon_file(p).is_some() => {
                let (bytes, mime) = icon_file(p).unwrap();
                let resp = Response::from_data(bytes.to_vec())
                    .with_header(Header::from_bytes("Content-Type", mime).unwrap())
                    .with_header(Header::from_bytes("Cache-Control", "public, max-age=86400").unwrap());
                req.respond(with_cors(resp)).context("respond")
            }
            ("/hello", Method::Get) => {
                let hello = Hello {
                    app: "mullion",
                    name: &self.host_name,
                    version: mullion_proto::version::VERSION,
                    platform: platform_name(),
                    port: self.local_addr().port(),
                    ips: &self.lan_ips,
                    extend: self.config.get("extend").and_then(|v| v.as_bool()).unwrap_or(true),
                    receivers: self.handler.devices().devices.iter().filter(|d| d.connected).count(),
                    host_id: self.pairing.as_ref().map(|p| p.host_id()),
                };
                let resp = Response::from_data(serde_json::to_vec(&hello)?)
                    .with_header(Header::from_bytes("Content-Type", "application/json").unwrap())
                    .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
                req.respond(with_cors(resp)).context("respond")
            }
            // Other Mullion hosts on this host's networks, for the pages it
            // serves: a page cannot sweep the network itself (nearby.rs). On its
            // own thread, so offers are not held up for the 2 s or so it takes.
            ("/nearby", Method::Get) => {
                let port = self.local_addr().port();
                std::thread::spawn(move || {
                    let found = mullion_proto::nearby::sweep(port);
                    let resp = Response::from_data(serde_json::to_vec(&found).unwrap_or_default())
                        .with_header(Header::from_bytes("Content-Type", "application/json").unwrap())
                        .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
                    let _ = req.respond(with_cors(resp));
                });
                Ok(())
            }
            ("/health", Method::Get) => {
                let resp = Response::from_string(format!("mullion-host {}\n", self.host_name));
                req.respond(with_cors(resp)).context("respond")
            }
            _ => respond_status(req, 404, "Not found"),
        }
    }
}

impl Signaling {
    /// May this request read or change the layout? Yes for this Mac itself
    /// (loopback, and no Origin or a loopback one, so a web page cannot
    /// borrow it) and for a request signed by a paired device. Without
    /// pairing (tests), everything is allowed as before.
    fn allowed(&self, req: &Request, peer: SocketAddr, path: &str, body: &[u8]) -> bool {
        let Some(p) = &self.pairing else { return true };
        if peer.ip().is_loopback() && header(req, "Origin").map_or(true, |o| loopback_origin(&o)) {
            return true;
        }
        header(req, "X-Mullion-Auth").is_some_and(|h| p.verify_request(&h, req.method().as_str(), path, body).is_some())
    }

    fn respond_screens(&self, req: Request, view: Result<ScreensView>) -> Result<()> {
        match view {
            Ok(mut v) => {
                v.host_name = self.host_name.clone();
                for s in v.screens.iter_mut().filter(|s| s.kind == ScreenKind::Host) {
                    s.name = self.host_name.clone();
                }
                let body = serde_json::to_vec(&v)?;
                let resp = Response::from_data(body)
                    .with_header(Header::from_bytes("Content-Type", "application/json").unwrap())
                    .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
                req.respond(with_cors(resp)).context("respond")
            }
            Err(e) if e.is::<Rejected>() => respond_status(req, 409, &e.to_string()),
            Err(e) => {
                warn!("screens: {e:#}");
                respond_status(req, 500, &format!("Can't read the displays: {e}"))
            }
        }
    }
}

fn with_cors<R: Read>(r: Response<R>) -> Response<R> {
    r.with_header(Header::from_bytes("Access-Control-Allow-Origin", "*").unwrap())
        .with_header(Header::from_bytes("Access-Control-Allow-Methods", "POST, GET, OPTIONS").unwrap())
        .with_header(Header::from_bytes("Access-Control-Allow-Headers", "Content-Type, X-Mullion-Auth").unwrap())
        // Private Network Access: a receiver page from another origin (the app's
        // WebView, a page saved on the tablet) may ask a LAN address.
        .with_header(Header::from_bytes("Access-Control-Allow-Private-Network", "true").unwrap())
}

fn header(req: &Request, name: &'static str) -> Option<String> {
    req.headers().iter().find(|h| h.field.equiv(name)).map(|h| h.value.as_str().to_string())
}

/// `http://127.0.0.1:8080`, `http://localhost`, `http://[::1]:47100`.
fn loopback_origin(origin: &str) -> bool {
    let host = origin.split("://").nth(1).unwrap_or("");
    let host = if let Some(rest) = host.strip_prefix('[') { rest.split(']').next().unwrap_or("") } else { host.split(':').next().unwrap_or("") };
    host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// The whole body, or `None` when it is longer than `limit`.
fn read_body(req: &mut Request, limit: usize) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    let n = req.as_reader().take(limit as u64 + 1).read_to_end(&mut body).unwrap_or(0);
    (n <= limit).then_some(body)
}

fn respond_json(req: Request, code: u16, v: &serde_json::Value) -> Result<()> {
    let resp = Response::from_data(serde_json::to_vec(v)?)
        .with_status_code(StatusCode(code))
        .with_header(Header::from_bytes("Content-Type", "application/json").unwrap())
        .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
    req.respond(with_cors(resp)).context("respond")
}

fn respond_status(req: Request, code: u16, msg: &str) -> Result<()> {
    let resp = Response::from_string(format!("{msg}\n")).with_status_code(StatusCode(code));
    req.respond(with_cors(resp)).context("respond")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn start(handler: impl OfferHandler) -> SocketAddr {
        let s = Signaling::bind("127.0.0.1:0".parse().unwrap(), Arc::new(handler), "TEST-MAC".into()).unwrap();
        let addr = s.local_addr();
        std::thread::spawn(move || s.run());
        addr
    }

    fn echo_handler(sdp: &str, _mode: Mode, _peer: SocketAddr) -> Result<String> {
        if sdp.contains("m=video") {
            Ok("v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 108\r\na=fmtp:108 packetization-mode=1;profile-level-id=42e01f\r\n".into())
        } else {
            Err(anyhow!("no video m-line"))
        }
    }

    fn agent() -> ureq::Agent {
        ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(5)).build()
    }

    #[test]
    fn t9_valid_offer_gets_answer_with_h264_mode_1() {
        let addr = start(echo_handler);
        let r = agent()
            .post(&format!("http://{addr}/offer"))
            .send_string(r#"{"type":"offer","sdp":"v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 108\r\n"}"#)
            .unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(r.header("Access-Control-Allow-Origin"), Some("*"));
        let body: serde_json::Value = r.into_json().unwrap();
        assert_eq!(body["type"], "answer");
        assert_eq!(body["host_name"], "TEST-MAC");
        assert_eq!(body["probe"], false, "no config: strip assumed off");
        assert!(body["sdp"].as_str().unwrap().contains("packetization-mode=1"));
    }

    #[test]
    fn t10_malformed_big_and_wrong_method() {
        let addr = start(echo_handler);
        let a = agent();
        let url = format!("http://{addr}/offer");

        let r = a.post(&url).send_string("not json").unwrap_err();
        assert!(matches!(r, ureq::Error::Status(400, _)), "{r:?}");

        let r = a.post(&url).send_string(r#"{"sdp":"v=0\r\nm=audio 9\r\n"}"#).unwrap_err();
        assert!(matches!(r, ureq::Error::Status(400, _)), "handler rejection is 400: {r:?}");

        let big = vec![b'a'; 2 * 1024 * 1024];
        let r = a.post(&url).send_bytes(&big).unwrap_err();
        assert!(matches!(r, ureq::Error::Status(413, _)), "{r:?}");

        let r = a.get(&url).call().unwrap_err();
        assert!(matches!(r, ureq::Error::Status(405, _)), "{r:?}");

        let r = a.get(&format!("http://{addr}/nope")).call().unwrap_err();
        assert!(matches!(r, ureq::Error::Status(404, _)), "{r:?}");

        // Host keeps running after all of that.
        let r = a.get(&format!("http://{addr}/health")).call().unwrap();
        assert_eq!(r.status(), 200);
        let r = a.get(&format!("http://{addr}/probe")).call().unwrap();
        assert!(r.into_string().unwrap().contains("Mullion probe"));
    }

    #[test]
    fn icons_and_manifest_are_served() {
        let addr = start(echo_handler);
        let a = agent();
        let r = a.get(&format!("http://{addr}/favicon.ico")).call().unwrap();
        assert_eq!(r.header("Content-Type"), Some("image/x-icon"));
        let mut buf = Vec::new();
        r.into_reader().read_to_end(&mut buf).unwrap();
        assert_eq!(&buf[..4], &[0, 0, 1, 0], "ICO header");
        let r = a.get(&format!("http://{addr}/icons/manifest.webmanifest")).call().unwrap();
        let m: serde_json::Value = r.into_json().unwrap();
        assert_eq!(m["name"], "Mullion");
        assert_eq!(m["icons"].as_array().unwrap().len(), 3);
        let r = a.get(&format!("http://{addr}/receiver")).call().unwrap();
        let html = r.into_string().unwrap();
        assert!(html.contains(r#"rel="manifest""#) && html.contains("apple-touch-icon"));
    }

    /// A desk of host + one tablet right of it; layout requests go through the real planner.
    struct FakeDesk(std::sync::Mutex<Vec<crate::platform::DisplayInfo>>);

    impl FakeDesk {
        fn new() -> Self {
            use crate::platform::DisplayInfo;
            Self(std::sync::Mutex::new(vec![
                DisplayInfo { id: 1, x: 0, y: 0, width: 1512, height: 982, main: true, builtin: true },
                DisplayInfo { id: 7, x: 1512, y: 0, width: 1920, height: 1080, main: false, builtin: false },
            ]))
        }
        fn view(&self) -> ScreensView {
            let (_, cells) = crate::screens::current_cells(&self.0.lock().unwrap()).unwrap();
            ScreensView {
                can_arrange: true,
                screens: std::iter::once((1, ScreenKind::Host, (0, 0)))
                    .chain(cells.iter().map(|(d, c)| (d.id, ScreenKind::Device, *c)))
                    .map(|(id, kind, (col, row))| crate::screens::ScreenView {
                        display_id: id, kind, name: String::new(), builtin: id == 1, x: 0, y: 0, width: 1, height: 1, col, row,
                        device_id: None, connected: None, removes_in_secs: None,
                    })
                    .collect(),
                ..Default::default()
            }
        }
    }

    impl OfferHandler for FakeDesk {
        fn answer(&self, sdp: &str, mode: Mode, _: DeviceInfo, peer: SocketAddr) -> Result<String> {
            echo_handler(sdp, mode, peer)
        }
        fn screens(&self) -> Result<ScreensView> {
            Ok(self.view())
        }
        fn set_layout(&self, cells: &[CellAssign]) -> Result<ScreensView> {
            let mut desk = self.0.lock().unwrap();
            let (moves, _) = crate::screens::plan(&desk, cells)?;
            for (id, x, y) in moves {
                let d = desk.iter_mut().find(|d| d.id == id).unwrap();
                d.x = x;
                d.y = y;
            }
            drop(desk);
            Ok(self.view())
        }
    }

    #[test]
    fn web_port_serves_the_same_routes() {
        let s = Signaling::bind("127.0.0.1:0".parse().unwrap(), Arc::new(echo_handler), "TEST-MAC".into())
            .unwrap()
            .with_web_addr("127.0.0.1:0".parse().unwrap());
        let main = s.local_addr();
        let web = s.web_port().expect("web listener bound");
        assert_ne!(web, main.port());
        std::thread::spawn(move || s.run());
        let a = agent();
        let page = a.get(&format!("http://127.0.0.1:{web}/")).call().unwrap().into_string().unwrap();
        assert!(page.contains("mullionLayout"), "the receiver page at /");
        let v: serde_json::Value = a.get(&format!("http://127.0.0.1:{web}/hello")).call().unwrap().into_json().unwrap();
        assert_eq!(v["port"], main.port(), "/hello names the main port, where offers go");
        assert_eq!(a.get(&format!("http://{main}/health")).call().unwrap().status(), 200);
    }

    #[test]
    fn web_port_taken_is_not_fatal() {
        let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let taken = busy.local_addr().unwrap();
        let s = Signaling::bind("127.0.0.1:0".parse().unwrap(), Arc::new(echo_handler), "TEST-MAC".into())
            .unwrap()
            .with_web_addr(taken);
        assert_eq!(s.web_port(), None);
        let s = s.with_web_port(0);
        assert_eq!(s.web_port(), None, "0 is off");
    }

    #[test]
    fn hello_identifies_the_host_for_discovery() {
        let s = Signaling::bind("127.0.0.1:0".parse().unwrap(), Arc::new(echo_handler), "TEST-MAC".into())
            .unwrap()
            .with_config(serde_json::json!({"extend": false}))
            .with_lan_ips(vec!["10.0.0.5".into()]);
        let addr = s.local_addr();
        std::thread::spawn(move || s.run());
        let a = agent();
        let r = a.get(&format!("http://{addr}/hello")).call().unwrap();
        assert_eq!(r.header("Access-Control-Allow-Origin"), Some("*"));
        let v: serde_json::Value = r.into_json().unwrap();
        assert_eq!(v["app"], "mullion");
        assert_eq!(v["name"], "TEST-MAC");
        assert_eq!(v["port"], addr.port());
        assert_eq!(v["ips"], serde_json::json!(["10.0.0.5"]));
        assert_eq!(v["extend"], false);
        assert_eq!(v["receivers"], 0);
        assert!(v["version"].as_str().unwrap().contains('.'));
        // A preflight from another origin on the LAN is allowed.
        let r = a.request("OPTIONS", &format!("http://{addr}/hello")).set("Access-Control-Request-Private-Network", "true").call().unwrap();
        assert_eq!(r.status(), 204);
        assert_eq!(r.header("Access-Control-Allow-Private-Network"), Some("true"));
    }

    #[test]
    fn layout_map_lists_and_moves_screens() {
        let addr = start(FakeDesk::new());
        let a = agent();
        let v: serde_json::Value = a.get(&format!("http://{addr}/screens")).call().unwrap().into_json().unwrap();
        assert_eq!(v["host_name"], "TEST-MAC");
        assert_eq!(v["screens"][0]["kind"], "host");
        assert_eq!(v["screens"][0]["name"], "TEST-MAC", "host tile carries the host name");
        assert_eq!((v["screens"][1]["col"].as_i64(), v["screens"][1]["row"].as_i64()), (Some(1), Some(0)));

        let url = format!("http://{addr}/screens/layout");
        let r = a.post(&url).send_string(r#"{"cells":[{"display_id":7,"col":0,"row":-1}]}"#).unwrap();
        let v: serde_json::Value = r.into_json().unwrap();
        assert_eq!((v["screens"][1]["col"].as_i64(), v["screens"][1]["row"].as_i64()), (Some(0), Some(-1)), "now above the host");

        let r = a.post(&url).send_string(r#"{"cells":[{"display_id":7,"col":0,"row":0}]}"#).unwrap_err();
        match r {
            ureq::Error::Status(409, resp) => assert!(resp.into_string().unwrap().contains("host")),
            other => panic!("expected 409, got {other:?}"),
        }
        let r = a.post(&url).send_string("[]").unwrap_err();
        assert!(matches!(r, ureq::Error::Status(400, _)), "{r:?}");
        let r = a.get(&url).call().unwrap_err();
        assert!(matches!(r, ureq::Error::Status(405, _)), "{r:?}");
        let r = a.post(&url).send_bytes(&vec![b' '; 64 * 1024]).unwrap_err();
        assert!(matches!(r, ureq::Error::Status(413, _)), "{r:?}");
    }

    /// With pairing on: plain offers are refused, a receiver pairs with the
    /// code over HTTP, its sealed offer gets a sealed answer, a replay is
    /// refused, and the layout map takes this Mac or a signed request only.
    #[test]
    fn paired_host_takes_only_sealed_offers_and_signed_layout_requests() {
        use crate::pairing::{self as pr, Pairing, PairKeys};
        use base64::{engine::general_purpose::STANDARD as B64, Engine};
        use spake2::{Ed25519Group, Identity, Password, Spake2};

        let codes = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let c2 = codes.clone();
        let pairing = Arc::new(Pairing::load(None, Box::new(move |n| {
            if let pr::Notice::Code { code, .. } = n {
                c2.lock().unwrap().push(code);
            }
        })));
        let desk = FakeDesk::new();
        let s = Signaling::bind("127.0.0.1:0".parse().unwrap(), Arc::new(desk), "TEST-MAC".into()).unwrap().with_pairing(pairing.clone());
        let addr = s.local_addr();
        std::thread::spawn(move || s.run());
        let a = agent();
        let url = |p: &str| format!("http://{addr}{p}");
        let post = |p: &str, v: serde_json::Value| -> (u16, serde_json::Value) {
            match a.post(&url(p)).send_json(v) {
                Ok(r) => (r.status(), r.into_json().unwrap_or_default()),
                Err(ureq::Error::Status(c, r)) => (c, r.into_json().unwrap_or_default()),
                Err(e) => panic!("{e}"),
            }
        };

        // /hello names the host; a plain offer is refused with "pair".
        let hello: serde_json::Value = a.get(&url("/hello")).call().unwrap().into_json().unwrap();
        let host_id = hello["host_id"].as_str().unwrap().to_string();
        let (code, v) = post("/offer", serde_json::json!({"type": "offer", "sdp": "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 108\r\n"}));
        assert_eq!((code, v["pair"].as_bool()), (401, Some(true)));

        // Pair: start, SPAKE2 with the code the Mac showed, confirm.
        let (code, v) = post("/pair/start", serde_json::json!({"name": "iPad"}));
        assert_eq!(code, 200);
        let pid = v["pairing"].as_str().unwrap().to_string();
        let shown = codes.lock().unwrap()[0].clone();
        let (spake, msg_a) = Spake2::<Ed25519Group>::start_a(
            &Password::new(shown.as_bytes()),
            &Identity::new(pr::ID_RECEIVER),
            &Identity::new(&pr::host_identity(&host_id)),
        );
        let (_, v) = post("/pair/exchange", serde_json::json!({"pairing": pid, "msg": B64.encode(msg_a)}));
        let keys = PairKeys::new(&spake.finish(&B64.decode(v["msg"].as_str().unwrap()).unwrap()).unwrap());
        let (code, v) = post("/pair/confirm", serde_json::json!({"pairing": pid, "mac": B64.encode(keys.receiver_mac())}));
        assert_eq!(code, 200, "{v}");
        let sealed: pr::Sealed = serde_json::from_value(v["credential"].clone()).unwrap();
        let cred: serde_json::Value = serde_json::from_slice(&pr::open(&keys.seal, b"mullion pair seal", &sealed).unwrap()).unwrap();
        let device = cred["device"].as_str().unwrap().to_string();
        let secret = B64.decode(cred["secret"].as_str().unwrap()).unwrap();

        // Connect: challenge, sealed offer, sealed answer.
        let (_, v) = post("/challenge", serde_json::json!({"device": device}));
        let nonce = B64.decode(v["nonce"].as_str().unwrap()).unwrap();
        let offer = serde_json::json!({"type": "offer", "sdp": "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 108\r\n", "mode": "extend", "device_name": "iPad"});
        let sealed = pr::seal(&pr::derive(&secret, Some(&nonce), b"mullion offer"), format!("offer:{device}").as_bytes(), offer.to_string().as_bytes());
        let body = serde_json::json!({"device": device, "sealed": sealed});
        let (code, v) = post("/offer", body.clone());
        assert_eq!(code, 200, "{v}");
        let answer: pr::Sealed = serde_json::from_value(v["sealed"].clone()).unwrap();
        let answer: serde_json::Value =
            serde_json::from_slice(&pr::open(&pr::derive(&secret, Some(&nonce), b"mullion answer"), format!("answer:{device}").as_bytes(), &answer).unwrap()).unwrap();
        assert_eq!(answer["type"], "answer");
        assert_eq!(answer["host_name"], "TEST-MAC");
        // The same sealed offer again: its challenge is spent.
        let (code, v) = post("/offer", body);
        assert_eq!((code, v["pair"].as_bool()), (401, Some(false)));

        // Layout map: this Mac (loopback, no Origin) yes; a web page's Origin no; signed yes.
        assert_eq!(a.get(&url("/screens")).call().unwrap().status(), 200);
        let r = a.get(&url("/screens")).set("Origin", "https://evil.example").call().unwrap_err();
        assert!(matches!(r, ureq::Error::Status(401, _)), "{r:?}");
        let r = a.get(&url("/devices")).set("Origin", "https://evil.example").call().unwrap_err();
        assert!(matches!(r, ureq::Error::Status(401, _)), "{r:?}");
        let ts = pr::unix_ms().to_string();
        let mac = B64.encode(pr::request_mac(&secret, "GET", "/screens", &ts, b""));
        let r = a.get(&url("/screens")).set("Origin", "https://evil.example").set("X-Mullion-Auth", &format!("{device}.{ts}.{mac}")).call().unwrap();
        assert_eq!(r.status(), 200);
    }

    #[test]
    fn t10_chunked_body_over_limit_is_413() {
        let addr = start(echo_handler);
        use std::io::Write;
        let mut s = std::net::TcpStream::connect(addr).unwrap();
        s.write_all(b"POST /offer HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n").unwrap();
        let chunk = vec![b'a'; 32 * 1024];
        for _ in 0..3 {
            write!(s, "{:x}\r\n", chunk.len()).unwrap();
            s.write_all(&chunk).unwrap();
            s.write_all(b"\r\n").unwrap();
        }
        s.write_all(b"0\r\n\r\n").unwrap();
        let mut resp = String::new();
        let _ = s.read_to_string(&mut resp);
        assert!(resp.starts_with("HTTP/1.1 413"), "{resp}");
    }

    #[test]
    fn t11_port_in_use_names_port_and_flag() {
        let taken = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = taken.local_addr().unwrap();
        let err = Signaling::bind(addr, Arc::new(echo_handler), "x".into()).err().unwrap();
        let msg = format!("{err:#}");
        assert!(msg.contains(&addr.port().to_string()), "{msg}");
        assert!(msg.contains("--port"), "{msg}");
    }
}
