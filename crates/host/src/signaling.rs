//! FR-3: the signaling endpoint. Plain HTTP on the LAN (SRS A-7), port 47100
//! by default (A-8). `POST /offer` takes `{"type":"offer","sdp":"..."}` and
//! returns `{"type":"answer","sdp":"...","host_name":"..."}`. `GET /probe`
//! serves the latency probe page. Anything else is 404; wrong methods are 405;
//! bodies over the limit are 413 (T-10). Startup on a busy port is a named
//! error pointing at the flag (T-11).

use std::io::Read;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use mullion_proto::signal::{clean_device_name, parse_offer, valid_device_id, Mode, MAX_BODY_BYTES};
use crate::session::{DeviceInfo, DevicesSnapshot};
use serde::Serialize;
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};
use tracing::{info, warn};

use crate::probe_page::{receiver_html, PROBE_HTML};

/// What the server hands a validated offer to. Returns the answer SDP.
pub trait OfferHandler: Send + Sync + 'static {
    fn answer(&self, offer_sdp: &str, mode: Mode, device: DeviceInfo, peer: SocketAddr) -> Result<String>;
    fn devices(&self) -> DevicesSnapshot {
        DevicesSnapshot::default()
    }
}

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
}

pub struct Signaling {
    server: Server,
    handler: Arc<dyn OfferHandler>,
    host_name: String,
}

impl Signaling {
    /// Bind the listener. Fails with a message naming the port and the flag
    /// when the port is taken (T-11).
    pub fn bind(addr: SocketAddr, handler: Arc<dyn OfferHandler>, host_name: String) -> Result<Self> {
        let server = Server::http(addr).map_err(|e| {
            anyhow!("Can't listen on port {}: {}. Pick another with --port.", addr.port(), e)
        })?;
        Ok(Self { server, handler, host_name })
    }

    pub fn local_addr(&self) -> SocketAddr {
        match self.server.server_addr() {
            tiny_http::ListenAddr::IP(a) => a,
            #[cfg(unix)]
            tiny_http::ListenAddr::Unix(_) => unreachable!("not a unix socket"),
        }
    }

    /// Serve forever on the current thread.
    pub fn run(self) {
        for req in self.server.incoming_requests() {
            let resp = self.handle(req);
            if let Err(e) = resp {
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
                let offer = match parse_offer(&body) {
                    Ok(o) => o,
                    Err(413) => return respond_status(req, 413, "Offer too large"),
                    Err(_) => return respond_status(req, 400, "Body must be JSON {\"sdp\":\"v=0...\"}"),
                };
                let device = DeviceInfo {
                    id: valid_device_id(&offer.device_id).map(str::to_string),
                    name: clean_device_name(&offer.device_name, &format!("Tablet {}", peer.ip())),
                };
                match self.handler.answer(&offer.sdp, offer.mode, device, peer) {
                    Ok(sdp) => {
                        info!("offer from {peer} answered ({:?})", offer.mode);
                        let body = serde_json::to_vec(&AnswerBody { r#type: "answer", sdp: &sdp, host_name: &self.host_name })?;
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
                let resp = Response::from_string(receiver_html(&host_ip))
                    .with_header(Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap())
                    .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
                req.respond(with_cors(resp)).context("respond")
            }
            ("/devices", Method::Get) => {
                let body = serde_json::to_vec(&self.handler.devices())?;
                let resp = Response::from_data(body).with_header(Header::from_bytes("Content-Type", "application/json").unwrap());
                req.respond(with_cors(resp)).context("respond")
            }
            ("/health", Method::Get) => {
                let resp = Response::from_string(format!("mullion-host {}\n", self.host_name));
                req.respond(with_cors(resp)).context("respond")
            }
            _ => respond_status(req, 404, "Not found"),
        }
    }
}

fn with_cors<R: Read>(r: Response<R>) -> Response<R> {
    r.with_header(Header::from_bytes("Access-Control-Allow-Origin", "*").unwrap())
        .with_header(Header::from_bytes("Access-Control-Allow-Methods", "POST, GET, OPTIONS").unwrap())
        .with_header(Header::from_bytes("Access-Control-Allow-Headers", "Content-Type").unwrap())
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
