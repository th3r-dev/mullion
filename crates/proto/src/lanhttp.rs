//! The apps' requests to a Mullion host, made natively.
//!
//! The Android app's page is `https://dioxus.index.html`, and its WebView
//! blocks every `http://` request from it as mixed content, so `fetch` cannot
//! reach a host at all (hosts serve plain HTTP on the LAN). The page hands
//! such requests to native code instead (`window.mullionNative.http`, app
//! main.rs), which sends them here.
//!
//! Deliberately narrow, since any page script could call it: plain HTTP to a
//! private or loopback IPv4 address only, GET or POST, two request headers
//! (`Content-Type`, `X-Mullion-Auth`), 64 KiB out, 2 MiB back, one total
//! deadline.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

const MAX_REQUEST_BODY: usize = 64 * 1024;
const MAX_RESPONSE: usize = 2 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// What came back: the status and the body as text.
#[derive(Debug, PartialEq)]
pub struct Reply {
    pub status: u16,
    pub body: String,
}

/// Send one request and read the whole reply within `deadline`. `Err` is a
/// short reason for the page ("can't connect", "not a LAN address", ...).
pub fn request(method: &str, url: &str, headers: &[(String, String)], body: &[u8], deadline: Duration) -> Result<Reply, String> {
    let until = Instant::now() + deadline;
    if !matches!(method, "GET" | "POST") {
        return Err(format!("method {method} is not allowed"));
    }
    if body.len() > MAX_REQUEST_BODY {
        return Err("request body too large".into());
    }
    let (ip, port, path) = parse_url(url)?;
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {ip}:{port}\r\nConnection: close\r\nContent-Length: {}\r\n", body.len());
    for (k, v) in headers {
        let allowed = k.eq_ignore_ascii_case("Content-Type") || k.eq_ignore_ascii_case("X-Mullion-Auth");
        if !allowed {
            return Err(format!("header {k} is not allowed"));
        }
        if v.chars().any(|c| c.is_control()) {
            return Err(format!("bad value for header {k}"));
        }
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let mut out = head.into_bytes();
    out.extend_from_slice(body);

    let mut s = TcpStream::connect_timeout(&SocketAddr::from((ip, port)), CONNECT_TIMEOUT.min(until.saturating_duration_since(Instant::now())))
        .map_err(|e| format!("can't connect: {e}"))?;
    s.set_write_timeout(Some(left(until)?)).map_err(|e| e.to_string())?;
    s.write_all(&out).map_err(|e| format!("can't send: {e}"))?;

    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        s.set_read_timeout(Some(left(until)?)).map_err(|e| e.to_string())?;
        match s.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > MAX_RESPONSE {
                    return Err("reply too large".into());
                }
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => return Err("no answer in time".into()),
            Err(e) => return Err(format!("can't read: {e}")),
        }
    }
    parse_reply(&buf)
}

fn left(until: Instant) -> Result<Duration, String> {
    let d = until.saturating_duration_since(Instant::now());
    if d.is_zero() {
        Err("no answer in time".into())
    } else {
        Ok(d)
    }
}

/// `http://10.0.0.5:47100/path?q` -> (10.0.0.5, 47100, "/path?q").
fn parse_url(url: &str) -> Result<(Ipv4Addr, u16, String), String> {
    let rest = url.strip_prefix("http://").ok_or("only http:// to a LAN address")?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| "bad port")?),
        None => (hostport, 80),
    };
    let ip: Ipv4Addr = host.parse().map_err(|_| "only an IPv4 address")?;
    if !(ip.is_private() || ip.is_loopback()) {
        return Err("not a LAN address".into());
    }
    if path.chars().any(|c| c.is_control() || c == ' ') {
        return Err("bad path".into());
    }
    Ok((ip, port, path.to_string()))
}

fn parse_reply(buf: &[u8]) -> Result<Reply, String> {
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n").ok_or("incomplete reply")?;
    let head = String::from_utf8_lossy(&buf[..split]);
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .filter(|l| l.starts_with("HTTP/1."))
        .and_then(|l| l.split(' ').nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or("not an HTTP reply")?;
    let chunked = lines.any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("transfer-encoding:") && l.contains("chunked")
    });
    let raw = &buf[split + 4..];
    let body = if chunked { dechunk(raw)? } else { raw.to_vec() };
    Ok(Reply { status, body: String::from_utf8_lossy(&body).into_owned() })
}

fn dechunk(mut raw: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let eol = raw.windows(2).position(|w| w == b"\r\n").ok_or("bad chunked reply")?;
        let size_text = String::from_utf8_lossy(&raw[..eol]);
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16).map_err(|_| "bad chunk size")?;
        raw = &raw[eol + 2..];
        if size == 0 {
            return Ok(out);
        }
        if raw.len() < size + 2 {
            return Err("truncated chunk".into());
        }
        out.extend_from_slice(&raw[..size]);
        raw = &raw[size + 2..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// Serve one request; reply with `response`. Returns the port and the request seen.
    fn serve(response: &'static [u8]) -> (u16, std::sync::mpsc::Receiver<String>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut req = vec![0u8; 4096];
            let n = c.read(&mut req).unwrap();
            let _ = tx.send(String::from_utf8_lossy(&req[..n]).into_owned());
            c.write_all(response).unwrap();
        });
        (port, rx)
    }

    #[test]
    fn posts_and_reads_a_reply() {
        let (port, seen) = serve(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\n\r\n{\"nonce\":1}");
        let h = vec![("Content-Type".to_string(), "application/json".to_string())];
        let r = request("POST", &format!("http://127.0.0.1:{port}/challenge"), &h, b"{\"device\":\"d1\"}", Duration::from_secs(3)).unwrap();
        assert_eq!(r, Reply { status: 200, body: "{\"nonce\":1}".into() });
        let req = seen.recv().unwrap();
        assert!(req.starts_with("POST /challenge HTTP/1.1\r\n"), "{req}");
        assert!(req.contains("Content-Length: 15\r\n") && req.ends_with("{\"device\":\"d1\"}"), "{req}");
    }

    #[test]
    fn reads_chunked_replies_and_error_statuses() {
        let (port, _) = serve(b"HTTP/1.1 401 Unauthorized\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n");
        let r = request("GET", &format!("http://127.0.0.1:{port}/screens"), &[], b"", Duration::from_secs(3)).unwrap();
        assert_eq!(r, Reply { status: 401, body: "hello world".into() });
    }

    #[test]
    fn refuses_what_a_page_should_not_reach() {
        let d = Duration::from_secs(1);
        for url in ["https://10.0.0.5/hello", "http://8.8.8.8/hello", "http://example.com/hello", "http://10.0.0.5:99999/"] {
            assert!(request("GET", url, &[], b"", d).is_err(), "{url}");
        }
        assert!(request("DELETE", "http://127.0.0.1:1/", &[], b"", d).is_err());
        let cookie = vec![("Cookie".to_string(), "x".to_string())];
        assert!(request("GET", "http://127.0.0.1:1/", &cookie, b"", d).unwrap_err().contains("not allowed"));
        let smuggle = vec![("X-Mullion-Auth".to_string(), "a\r\nHost: evil".to_string())];
        assert!(request("GET", "http://127.0.0.1:1/", &smuggle, b"", d).unwrap_err().contains("bad value"));
    }

    #[test]
    fn a_silent_host_hits_the_deadline() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (_c, _) = l.accept().unwrap();
            std::thread::sleep(Duration::from_secs(3));
        });
        let t = Instant::now();
        let e = request("GET", &format!("http://127.0.0.1:{port}/hello"), &[], b"", Duration::from_millis(600)).unwrap_err();
        assert!(t.elapsed() < Duration::from_millis(1500), "{e}");
        assert!(e.contains("in time"), "{e}");
    }
}
