//! The host's streaming core, shared by the Mac menu bar app (`main.rs`) and
//! the Android app's "Share this screen": WebRTC sessions, signaling, pairing
//! and the layout map. A frontend brings its own `Platform` (capture and
//! encode) and its own way to show the pairing code, then calls [`start`].

pub mod config;
pub mod pairing;
pub mod platform;
pub mod probe_page;
pub mod screens;
pub mod session;
pub mod signaling;
pub mod timing;

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;

/// Every usable local IPv4 address, the main one first, loopback last, so
/// loopback and LAN receivers both connect.
pub fn lan_ips() -> Vec<IpAddr> {
    let mut ips: Vec<IpAddr> = vec![];
    if let Ok(ip) = local_ip_address::local_ip() {
        ips.push(ip);
    }
    if let Ok(list) = local_ip_address::list_afinet_netifas() {
        for (_, ip) in list {
            if ip.is_ipv4() && !ips.contains(&ip) && !ip.is_loopback() && !ip.is_unspecified() {
                ips.push(ip);
            }
        }
    }
    ips.push(IpAddr::from([127, 0, 0, 1]));
    ips
}

/// What a frontend decides; [`start`] does the rest.
pub struct HostOptions {
    pub platform: Arc<dyn platform::Platform>,
    /// Signaling listener.
    pub addr: SocketAddr,
    /// Second port for the receiver page (80 on the Mac), 0 = none.
    pub web_port: u16,
    pub host_name: String,
    /// What receivers are told this host is ("Mac", "tablet").
    pub kind: &'static str,
    /// `advertise_ips` usually from [`lan_ips`].
    pub session: session::SessionConfig,
    /// Effective switches, served at `/config`.
    pub config: serde_json::Value,
    pub log: Arc<timing::TimingLog>,
    pub pairing_store: PathBuf,
    /// Show or hide the pairing code.
    pub notify: Box<dyn Fn(pairing::Notice) + Send + Sync>,
}

/// A running host. Signaling serves on its own thread.
pub struct Host {
    pub shared: Arc<session::SharedHandle>,
    pub pairing: Arc<pairing::Pairing>,
    /// What to type in a browser: the bare address when port 80 is ours,
    /// otherwise address:port.
    pub address: String,
    pub session_thread: std::thread::JoinHandle<()>,
}

/// The signaling port could not be had: most likely another Mullion has it.
#[derive(Debug)]
pub struct PortInUse(pub u16);

impl std::fmt::Display for PortInUse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "port {} is in use", self.0)
    }
}

pub fn start(o: HostOptions) -> Result<Host> {
    let lan: Vec<String> = o.session.advertise_ips.iter().filter(|ip| !ip.is_loopback()).map(|ip| ip.to_string()).collect();
    let (handle, session_thread) = session::start(o.session, o.platform, o.log)?;
    let pairing = Arc::new(pairing::Pairing::load(Some(o.pairing_store), o.notify));
    let shared = Arc::new(session::SharedHandle(Mutex::new(handle)));
    let signaling = signaling::Signaling::bind(o.addr, shared.clone(), o.host_name)
        .map_err(|e| e.context(PortInUse(o.addr.port())))?
        .with_kind(o.kind)
        .with_config(o.config)
        .with_lan_ips(lan.clone())
        .with_pairing(pairing.clone())
        .with_web_port(o.web_port);
    let ip = lan.first().cloned().unwrap_or_else(|| "127.0.0.1".into());
    let address = match signaling.web_port() {
        Some(80) => ip,
        Some(p) => format!("{ip}:{p}"),
        None => format!("{ip}:{}", o.addr.port()),
    };
    std::thread::spawn(move || signaling.run());
    Ok(Host { shared, pairing, address, session_thread })
}
