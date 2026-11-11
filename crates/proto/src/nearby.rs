//! Nearby: find Mullion hosts on this device's networks by asking `GET /hello`
//! at every address of each private /24 this device is on.
//!
//! This runs natively (the apps, and the host for the pages it serves), never
//! in a page. A browser keeps the socket of a probe to an empty address for the
//! OS connect timeout (75 s on macOS) even after the page gives up on it, so a
//! page-side sweep of 254 addresses filled the browser's socket pool (about
//! 256) and Connect waited behind it: "Failed to fetch", "Load failed".
//! Here an unanswered connect is dropped after [`CONNECT_TIMEOUT`].

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::sync::Mutex;
use std::time::Duration;

use serde_json::Value;

const CONNECT_TIMEOUT: Duration = Duration::from_millis(400);
const READ_TIMEOUT: Duration = Duration::from_millis(1000);
const THREADS: usize = 64;

/// Every Mullion host answering on `port` in this device's networks, as its
/// `/hello` JSON plus `"ip"`: the address that answered.
pub fn sweep(port: u16) -> Vec<Value> {
    sweep_nets(&lan_ipv4s(), port)
}

/// This device's private IPv4 addresses, the default-route one first. VPN
/// tunnels are left out: their /24 is someone else's network.
pub fn lan_ipv4s() -> Vec<Ipv4Addr> {
    let mut out = Vec::new();
    let mut add = |ip: IpAddr| {
        if let IpAddr::V4(v4) = ip {
            if v4.is_private() && !out.contains(&v4) {
                out.push(v4);
            }
        }
    };
    if let Ok(ip) = local_ip_address::local_ip() {
        add(ip);
    }
    if let Ok(list) = local_ip_address::list_afinet_netifas() {
        let tunnel = |n: &str| ["utun", "tun", "ppp", "ipsec", "wg"].iter().any(|p| n.starts_with(p));
        list.into_iter().filter(|(n, _)| !tunnel(n)).for_each(|(_, ip)| add(ip));
    }
    out
}

fn sweep_nets(own: &[Ipv4Addr], port: u16) -> Vec<Value> {
    let mut targets = Vec::new();
    for ip in own {
        let [a, b, c, _] = ip.octets();
        for d in 1..255 {
            let t = Ipv4Addr::new(a, b, c, d);
            if !own.contains(&t) && !targets.contains(&t) {
                targets.push(t);
            }
        }
    }
    let queue = Mutex::new(targets.into_iter());
    let found = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..THREADS {
            s.spawn(|| loop {
                let next = queue.lock().unwrap().next();
                let Some(ip) = next else { break };
                if let Some(h) = hello(ip, port) {
                    found.lock().unwrap().push(h);
                }
            });
        }
    });
    found.into_inner().unwrap()
}

/// Ask one address. `None` unless a Mullion host answered.
pub fn hello(ip: Ipv4Addr, port: u16) -> Option<Value> {
    let mut s = TcpStream::connect_timeout(&SocketAddr::from((ip, port)), CONNECT_TIMEOUT).ok()?;
    s.set_read_timeout(Some(READ_TIMEOUT)).ok()?;
    s.set_write_timeout(Some(READ_TIMEOUT)).ok()?;
    // One write: `write!` on the stream sends each formatted piece on its own,
    // and a listener that reads once and closes then resets the connection,
    // which loses its reply.
    let req = format!("GET /hello HTTP/1.0\r\nHost: {ip}:{port}\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).ok()?;
    // Up to 64 KiB, until the host closes or goes quiet.
    let mut buf = Vec::new();
    let _ = s.take(64 * 1024).read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf);
    let (head, body) = text.split_once("\r\n\r\n")?;
    if !head.starts_with("HTTP/1.") || head.split(' ').nth(1) != Some("200") {
        return None;
    }
    let mut v: Value = serde_json::from_str(body.trim()).ok()?;
    if v.get("app").and_then(Value::as_str) != Some("mullion") {
        return None;
    }
    v["ip"] = ip.to_string().into();
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn serve_once(response: &'static str) -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            // Read the whole request before answering: closing with some of it
            // unread resets the connection and the client loses the reply.
            let mut req = Vec::new();
            let mut chunk = [0u8; 1024];
            while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                match c.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => req.extend_from_slice(&chunk[..n]),
                }
            }
            c.write_all(response.as_bytes()).unwrap();
        });
        port
    }

    #[test]
    fn hello_accepts_only_mullion() {
        let port = serve_once("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"app\":\"mullion\",\"name\":\"Mac\",\"port\":47100}");
        let h = hello(Ipv4Addr::LOCALHOST, port).expect("a Mullion host");
        assert_eq!(h["name"], "Mac");
        assert_eq!(h["ip"], "127.0.0.1");

        let port = serve_once("HTTP/1.1 200 OK\r\n\r\n{\"app\":\"other\"}");
        assert!(hello(Ipv4Addr::LOCALHOST, port).is_none());
        let port = serve_once("HTTP/1.1 404 Not Found\r\n\r\n{\"app\":\"mullion\"}");
        assert!(hello(Ipv4Addr::LOCALHOST, port).is_none());
    }

    #[test]
    fn sweep_finds_a_host_on_its_24_but_not_itself() {
        let port = serve_once("HTTP/1.1 200 OK\r\n\r\n{\"app\":\"mullion\",\"name\":\"Mac\"}");
        // "This device" is 127.0.0.2: 127.0.0.1 is a neighbour on its /24.
        let found = sweep_nets(&[Ipv4Addr::new(127, 0, 0, 2)], port);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0]["ip"], "127.0.0.1");
        let port = serve_once("HTTP/1.1 200 OK\r\n\r\n{\"app\":\"mullion\"}");
        assert!(sweep_nets(&[Ipv4Addr::LOCALHOST], port).is_empty());
    }

    #[test]
    fn nothing_listening_is_none() {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        assert!(hello(Ipv4Addr::LOCALHOST, port).is_none());
    }
}
