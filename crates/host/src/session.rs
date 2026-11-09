//! The WebRTC session (FR-3 to FR-7, FR-9). One thread owns the `Rtc` and
//! follows str0m's rule: one mutation, then drain `poll_output` to `Timeout`.
//! Everything else (UDP packets, encoded frames, new offers) arrives on one
//! channel so the loop has a single wait point.
//!
//! One receiver at a time (PoC 1 scope). A new offer replaces the old session.

use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use mullion_proto::keyframe::IdrLimiter;
use mullion_proto::signal::{Mode, ProbeMessage};
use serde::Serialize;
use str0m::change::SdpOffer;
use str0m::channel::ChannelId;
use str0m::format::Codec;
use str0m::media::{MediaTime, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc};
use tracing::{debug, info, warn};

use crate::platform::{EncodedFrame, Pipeline, PipelineConfig, Platform, Source, VirtualDisplay};
use crate::timing::{now_ms, Record, TimingLog};

/// Who is connecting, from the offer.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    /// `None` when the receiver sent no usable id: anonymous, gets a fresh display.
    pub id: Option<String>,
    pub name: String,
}

/// One row of `/devices`.
#[derive(Debug, Clone, Serialize)]
pub struct DeviceStatus {
    pub session: u64,
    pub device_id: Option<String>,
    pub name: String,
    pub mode: Mode,
    pub connected: bool,
    pub peer: Option<String>,
    /// OS display id of the screen this device shows (virtual for Extend, main for Mirror).
    pub display_id: Option<u32>,
    pub screen_name: String,
    pub frames_sent: u64,
}

/// A screen whose device left; listed by `/devices` under `pending_removal`.
#[derive(Debug, Clone, Serialize)]
pub struct OrphanedScreen {
    pub screen_name: String,
    pub display_id: u32,
    pub removes_in_secs: u64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct DevicesSnapshot {
    pub devices: Vec<DeviceStatus>,
    pub pending_removal: Vec<OrphanedScreen>,
}

pub enum Msg {
    NewRtc(Rtc, Mode, DeviceInfo),
    Packet { at: Instant, source: SocketAddr, destination: SocketAddr, data: Vec<u8> },
    Frame(Source, EncodedFrame),
    Shutdown,
}

/// Handle given to the signaling server: builds an `Rtc`, answers the offer,
/// and ships the `Rtc` to the session thread.
pub struct SessionHandle {
    tx: Sender<Msg>,
    local_addrs: Vec<SocketAddr>,
    devices: Arc<Mutex<DevicesSnapshot>>,
}

impl SessionHandle {
    /// Snapshot of connected devices for `/devices`.
    pub fn devices(&self) -> DevicesSnapshot {
        self.devices.lock().unwrap().clone()
    }

    pub fn answer(&self, offer_sdp: &str, mode: Mode, device: DeviceInfo) -> Result<String> {
        let offer = SdpOffer::from_sdp_string(offer_sdp).map_err(|e| anyhow!("bad SDP: {e}"))?;
        let mut rtc = Rtc::builder()
            .set_ice_lite(true)
            .clear_codecs()
            .enable_h264(true)
            .build(Instant::now());
        // Only Constrained Baseline, packetization-mode 1 (SRS A-6).
        rtc.codec_config().params().to_vec(); // touch to keep API usage visible
        for addr in &self.local_addrs {
            let c = Candidate::host(*addr, "udp").map_err(|e| anyhow!("candidate {addr}: {e}"))?;
            if rtc.add_local_candidate(c).is_none() {
                warn!("candidate {addr} rejected");
            }
        }
        let answer = rtc.sdp_api().accept_offer(offer).map_err(|e| anyhow!("accept offer: {e}"))?;
        self.tx.send(Msg::NewRtc(rtc, mode, device)).map_err(|_| anyhow!("session thread gone"))?;
        Ok(answer.to_sdp_string())
    }

    pub fn shutdown(&self) {
        let _ = self.tx.send(Msg::Shutdown);
    }
}

pub struct SessionConfig {
    /// One UDP socket is bound per IP so every datagram has a known destination
    /// that matches an ICE candidate.
    pub advertise_ips: Vec<std::net::IpAddr>,
    pub pipeline: PipelineConfig,
    /// Allow Extend mode (creates a virtual display on first use).
    pub extend: bool,
}

/// Start the UDP socket, the reader thread and the session thread.
pub fn start(
    cfg: SessionConfig,
    platform: Arc<dyn Platform>,
    log: Arc<TimingLog>,
) -> Result<(SessionHandle, std::thread::JoinHandle<()>)> {
    let (tx, rx) = mpsc::channel::<Msg>();
    let mut sockets: HashMap<SocketAddr, UdpSocket> = HashMap::new();
    for ip in &cfg.advertise_ips {
        let socket = match UdpSocket::bind((*ip, 0)) {
            Ok(s) => s,
            Err(e) => {
                warn!("bind UDP on {ip}: {e}");
                continue;
            }
        };
        let local = socket.local_addr()?;
        // UDP reader per socket: blocks, forwards datagrams with their destination.
        let reader_socket = socket.try_clone()?;
        let reader_tx = tx.clone();
        std::thread::Builder::new().name(format!("mullion-udp-{ip}")).spawn(move || {
            let mut buf = vec![0u8; 2000];
            loop {
                match reader_socket.recv_from(&mut buf) {
                    Ok((n, source)) => {
                        let m = Msg::Packet { at: Instant::now(), source, destination: local, data: buf[..n].to_vec() };
                        if reader_tx.send(m).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        warn!("udp recv: {e}");
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
            }
        })?;
        sockets.insert(local, socket);
    }
    if sockets.is_empty() {
        anyhow::bail!("no UDP socket could be bound");
    }
    let local_addrs: Vec<SocketAddr> = sockets.keys().copied().collect();
    info!("WebRTC media on UDP {}", local_addrs.iter().map(|a| a.to_string()).collect::<Vec<_>>().join(", "));

    let frame_tx = tx.clone();
    let devices: Arc<Mutex<DevicesSnapshot>> = Arc::new(Mutex::new(DevicesSnapshot::default()));
    let handle = SessionHandle { tx, local_addrs, devices: devices.clone() };
    let join = std::thread::Builder::new().name("mullion-session".into()).spawn(move || {
        let mut loop_state = SessionLoop {
            sockets,
            rx,
            frame_tx,
            platform,
            pipeline_cfg: cfg.pipeline,
            log,
            sessions: Vec::new(),
            pipelines: HashMap::new(),
            displays: HashMap::new(),
            next_serial: 1,
            extend_allowed: cfg.extend,
            devices,
            idr: IdrLimiter::default_policy(),
            next_id: 0,
        };
        loop_state.run();
    })?;
    Ok((handle, join))
}

struct Active {
    id: u64,
    mode: Mode,
    device: DeviceInfo,
    source: Option<Source>,
    rtc: Rtc,
    /// Remote address once ICE picks a pair; used to route incoming packets.
    peer: Option<SocketAddr>,
    video_mid: Option<Mid>,
    probe_channel: Option<ChannelId>,
    connected: bool,
    frames_sent: u64,
}

struct OwnedDisplay {
    display: Box<dyn VirtualDisplay>,
    name: String,
    /// Set when the owning device has no session; the display is removed once
    /// this is older than [`DISPLAY_GRACE`]. Cleared on reconnect.
    orphaned_at: Option<Instant>,
}

/// How long a device's virtual display survives after its last session ends.
/// Long enough for a Wi-Fi hiccup or a mode switch, short enough that a
/// tablet that walked away does not leave a ghost monitor.
pub const DISPLAY_GRACE: Duration = Duration::from_secs(30);

struct SessionLoop {
    sockets: HashMap<SocketAddr, UdpSocket>,
    rx: Receiver<Msg>,
    frame_tx: Sender<Msg>,
    platform: Arc<dyn Platform>,
    pipeline_cfg: PipelineConfig,
    log: Arc<TimingLog>,
    /// All receivers. PoC 1 is specified for one, but several make testing
    /// (tablet + loopback) possible; they share one pipeline (PLAN step 8, Mirror).
    sessions: Vec<Active>,
    /// One pipeline per captured source; Mirror sessions share Main, Extend
    /// sessions share the virtual display.
    pipelines: HashMap<Source, Box<dyn Pipeline>>,
    /// One virtual display per device (PLAN step 4), keyed by device id, or by
    /// session id for anonymous devices. Kept until the host exits so a
    /// reconnecting device gets its screen and window layout back.
    displays: HashMap<String, OwnedDisplay>,
    next_serial: u32,
    extend_allowed: bool,
    devices: Arc<Mutex<DevicesSnapshot>>,
    idr: IdrLimiter,
    next_id: u64,
}

impl SessionLoop {
    fn run(&mut self) {
        let mut next_timeout = Instant::now() + Duration::from_secs(1);
        loop {
            let wait = next_timeout.saturating_duration_since(Instant::now());
            let msg = match self.rx.recv_timeout(wait) {
                Ok(m) => Some(m),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            };
            let mut touched: Vec<usize> = Vec::new();
            match msg {
                Some(Msg::Shutdown) => break,
                Some(Msg::NewRtc(rtc, mode, device)) => {
                    self.next_id += 1;
                    // A device that reconnects (same id) replaces its old session.
                    if let Some(id) = device.id.as_deref() {
                        if let Some(old) = self.sessions.iter().position(|a| a.device.id.as_deref() == Some(id)) {
                            self.end_session(old, "replaced by the same device");
                        }
                    }
                    self.log.record(Record::Event { at_ms: now_ms(), what: format!("offer accepted #{} {} ({mode:?})", self.next_id, device.name) });
                    self.sessions.push(Active {
                        id: self.next_id,
                        mode,
                        device,
                        source: None,
                        rtc,
                        peer: None,
                        video_mid: None,
                        probe_channel: None,
                        connected: false,
                        frames_sent: 0,
                    });
                    touched.push(self.sessions.len() - 1);
                }
                Some(Msg::Packet { at, source, destination, data }) => {
                    // Route by known peer; unknown sources (ICE checks) go to every
                    // session without a peer yet. str0m ignores packets not for it.
                    let targets: Vec<usize> = {
                        let known: Vec<usize> = self.sessions.iter().enumerate().filter(|(_, a)| a.peer == Some(source)).map(|(i, _)| i).collect();
                        if known.is_empty() {
                            self.sessions.iter().enumerate().filter(|(_, a)| a.peer.is_none()).map(|(i, _)| i).collect()
                        } else {
                            known
                        }
                    };
                    for i in targets {
                        if let Ok(contents) = data.as_slice().try_into() {
                            let input = Input::Receive(at, Receive { proto: Protocol::Udp, source, destination, contents });
                            if let Err(e) = self.sessions[i].rtc.handle_input(input) {
                                debug!("handle_input: {e}");
                            }
                            touched.push(i);
                        }
                    }
                }
                Some(Msg::Frame(source, frame)) => {
                    // Log each encoded frame once per source, not once per viewer.
                    let mut logged = false;
                    for (i, a) in self.sessions.iter_mut().enumerate() {
                        if a.connected && a.source == Some(source) {
                            write_frame(a, &frame, &self.log, !logged);
                            logged = true;
                            touched.push(i);
                        }
                    }
                }
                None => {
                    let now = Instant::now();
                    for (i, a) in self.sessions.iter_mut().enumerate() {
                        if let Err(e) = a.rtc.handle_input(Input::Timeout(now)) {
                            debug!("timeout input: {e}");
                        }
                        touched.push(i);
                    }
                }
            }
            // Drain after every mutation (str0m's single-mutation invariant).
            // Draining every session is cheap and keeps the invariant simple.
            next_timeout = self.drain_all();
            self.reap_displays();
            self.publish_devices();
            // Wake at least once a second so grace periods expire without traffic.
            let tick = Instant::now() + Duration::from_secs(1);
            if tick < next_timeout {
                next_timeout = tick;
            }
        }
        self.end_all("shutdown");
    }

    /// Drain every session's `poll_output` to `Timeout`; return the earliest deadline.
    fn drain_all(&mut self) -> Instant {
        let mut earliest = Instant::now() + Duration::from_secs(1);
        let mut ended: Vec<(usize, &'static str)> = Vec::new();
        for i in 0..self.sessions.len() {
            loop {
                let out = self.sessions[i].rtc.poll_output();
                match out {
                    Ok(Output::Timeout(t)) => {
                        if t < earliest {
                            earliest = t;
                        }
                        break;
                    }
                    Ok(Output::Transmit(t)) => {
                        if self.sessions[i].peer.is_none() && self.sessions[i].connected {
                            self.sessions[i].peer = Some(t.destination);
                        }
                        let sock = self.sockets.get(&t.source).or_else(|| self.sockets.values().next());
                        if let Some(sock) = sock {
                            if let Err(e) = sock.send_to(&t.contents, t.destination) {
                                debug!("send_to {}: {e}", t.destination);
                            }
                        }
                    }
                    Ok(Output::Event(ev)) => {
                        if let Some(reason) = self.on_event(i, ev) {
                            ended.push((i, reason));
                            break;
                        }
                    }
                    Err(e) => {
                        warn!("poll_output: {e}");
                        ended.push((i, "rtc error"));
                        break;
                    }
                }
            }
        }
        for (i, reason) in ended.into_iter().rev() {
            self.end_session(i, reason);
        }
        earliest
    }

    /// Returns `Some(reason)` when session `i` should end.
    fn on_event(&mut self, i: usize, ev: Event) -> Option<&'static str> {
        match ev {
            Event::MediaAdded(m) => {
                info!("#{} media added: mid={} kind={:?} dir={:?}", self.sessions[i].id, m.mid, m.kind, m.direction);
                self.sessions[i].video_mid = Some(m.mid);
            }
            Event::ChannelOpen(id, label) => {
                info!("#{} data channel open: {label}", self.sessions[i].id);
                if label == "probe" {
                    self.sessions[i].probe_channel = Some(id);
                }
            }
            Event::ChannelData(d) => {
                if Some(d.id) == self.sessions[i].probe_channel {
                    self.on_probe(i, &d.data);
                }
            }
            Event::IceConnectionStateChange(s) => {
                info!("#{} ice: {s:?}", self.sessions[i].id);
                match s {
                    IceConnectionState::Connected | IceConnectionState::Completed => {
                        if !self.sessions[i].connected {
                            self.sessions[i].connected = true;
                            self.log.record(Record::Event { at_ms: now_ms(), what: format!("connected #{}", self.sessions[i].id) });
                            let mode = self.sessions[i].mode;
                            match self.ensure_pipeline(i, mode) {
                                Ok(src) => {
                                    self.sessions[i].source = Some(src);
                                    // A joining receiver needs an IDR to start decoding.
                                    if let Some(p) = self.pipelines.get(&src) {
                                        p.request_idr();
                                    }
                                }
                                Err(e) => {
                                    warn!("pipeline: {e:#}");
                                    return Some("pipeline failed");
                                }
                            }
                        }
                    }
                    IceConnectionState::Disconnected => return Some("ice disconnected"),
                    _ => {}
                }
            }
            Event::Connected => info!("#{} dtls connected", self.sessions[i].id),
            Event::KeyframeRequest(k) => {
                let granted = self.idr.request(Instant::now());
                self.log.record(Record::Pli { at_ms: now_ms(), granted });
                debug!("#{} keyframe request {:?} granted={granted}", self.sessions[i].id, k.kind);
                if granted {
                    if let Some(p) = self.sessions[i].source.and_then(|s| self.pipelines.get(&s)) {
                        p.request_idr();
                    }
                }
            }
            Event::EgressBitrateEstimate(b) => debug!("bwe: {b:?}"),
            _ => {}
        }
        None
    }

    /// The capture source for `mode`, starting its pipeline (and the virtual
    /// display) if needed.
    fn ensure_pipeline(&mut self, i: usize, mode: Mode) -> Result<Source> {
        let source = match mode {
            Mode::Mirror => Source::Main,
            Mode::Extend => {
                if !self.extend_allowed {
                    anyhow::bail!("Extend mode is off on this host (--no-extend)");
                }
                let key = self.sessions[i].device.id.clone().unwrap_or_else(|| format!("session-{}", self.sessions[i].id));
                if !self.displays.contains_key(&key) {
                    let c = &self.pipeline_cfg;
                    let name = format!("Mullion {}", self.sessions[i].device.name);
                    let serial = self.next_serial;
                    self.next_serial += 1;
                    match self.platform.create_virtual_display(c.width, c.height, c.fps, &name, serial)? {
                        Some(vd) => {
                            self.displays.insert(key.clone(), OwnedDisplay { display: vd, name: name.clone(), orphaned_at: None });
                        }
                        None => anyhow::bail!("This OS cannot create a virtual display yet"),
                    }
                    self.log.record(Record::Event { at_ms: now_ms(), what: format!("virtual display created: {name}") });
                }
                let d = self.displays.get_mut(&key).unwrap();
                if d.orphaned_at.take().is_some() {
                    info!("{} reclaimed by its device", d.name);
                }
                d.display.source()
            }
        };
        if self.pipelines.contains_key(&source) {
            return Ok(source);
        }
        let (ptx, prx) = mpsc::channel::<EncodedFrame>();
        let ftx = self.frame_tx.clone();
        std::thread::spawn(move || {
            for f in prx {
                if ftx.send(Msg::Frame(source, f)).is_err() {
                    break;
                }
            }
        });
        let p = self.platform.start_pipeline(source, &self.pipeline_cfg, ptx)?;
        self.pipelines.insert(source, p);
        Ok(source)
    }

    /// Refresh the `/devices` snapshot.
    fn publish_devices(&self) {
        let rows: Vec<DeviceStatus> = self
            .sessions
            .iter()
            .map(|a| {
                let (display_id, screen_name) = match a.source {
                    Some(Source::Virtual(id)) => {
                        let name = self.displays.values().find(|d| d.display.source() == Source::Virtual(id)).map(|d| d.name.clone()).unwrap_or_default();
                        (Some(id), name)
                    }
                    Some(Source::Main) => (None, "Main display".to_string()),
                    None => (None, String::new()),
                };
                DeviceStatus {
                    session: a.id,
                    device_id: a.device.id.clone(),
                    name: a.device.name.clone(),
                    mode: a.mode,
                    connected: a.connected,
                    peer: a.peer.map(|p| p.ip().to_string()),
                    display_id,
                    screen_name,
                    frames_sent: a.frames_sent,
                }
            })
            .collect();
        let now = Instant::now();
        let pending_removal: Vec<OrphanedScreen> = self
            .displays
            .values()
            .filter_map(|d| {
                let t = d.orphaned_at?;
                let id = match d.display.source() {
                    Source::Virtual(id) => id,
                    _ => return None,
                };
                Some(OrphanedScreen { screen_name: d.name.clone(), display_id: id, removes_in_secs: DISPLAY_GRACE.saturating_sub(now.duration_since(t)).as_secs() })
            })
            .collect();
        *self.devices.lock().unwrap() = DevicesSnapshot { devices: rows, pending_removal };
    }

    fn on_probe(&mut self, i: usize, data: &[u8]) {
        let Ok(msg) = serde_json::from_slice::<ProbeMessage>(data) else {
            debug!("probe: unparseable {} bytes", data.len());
            return;
        };
        let a = &mut self.sessions[i];
        match msg {
            ProbeMessage::Ping { t0 } => {
                let reply = ProbeMessage::Pong { t0, t1: now_ms() };
                if let (Some(id), Ok(bytes)) = (a.probe_channel, serde_json::to_vec(&reply)) {
                    if let Some(mut ch) = a.rtc.channel(id) {
                        let _ = ch.write(false, &bytes);
                    }
                }
            }
            ProbeMessage::Presented { painted_host_ms, presented_ms, offset_ms } => {
                let presented_host_ms = presented_ms + offset_ms;
                self.log.record(Record::Presented {
                    session: a.id,
                    painted_host_ms,
                    presented_host_ms,
                    glass_to_glass_ms: presented_host_ms - painted_host_ms,
                });
            }
            ProbeMessage::Capabilities { jitter_buffer_target_supported, jitter_buffer_target_ms, user_agent } => {
                info!("#{} receiver: jitterBufferTarget supported={jitter_buffer_target_supported} value={jitter_buffer_target_ms:?} ua={user_agent}", a.id);
                let peer = a.peer.map(|p| p.ip().to_string()).unwrap_or_default();
                self.log.record(Record::Capabilities { session: a.id, peer, jitter_buffer_target_supported, jitter_buffer_target_ms, user_agent });
            }
            ProbeMessage::Pong { .. } => {}
        }
    }

    fn end_session(&mut self, i: usize, reason: &str) {
        if i >= self.sessions.len() {
            return;
        }
        let mut a = self.sessions.remove(i);
        a.rtc.disconnect();
        info!("#{} {} session ended: {reason} ({} frames sent)", a.id, a.device.name, a.frames_sent);
        self.log.record(Record::Event { at_ms: now_ms(), what: format!("session #{} ended: {reason}", a.id) });
        self.log.flush();
        // FR-7: last receiver of a source gone -> stop that capture, free its encoder.
        if let Some(src) = a.source {
            if !self.sessions.iter().any(|s| s.connected && s.source == Some(src)) {
                if let Some(mut p) = self.pipelines.remove(&src) {
                    p.stop();
                }
                // The device's screen stays for DISPLAY_GRACE so a reconnect gets
                // its window layout back; after that it is removed (see `reap_displays`).
                if let Source::Virtual(id) = src {
                    for d in self.displays.values_mut() {
                        if d.display.source() == Source::Virtual(id) && d.orphaned_at.is_none() {
                            d.orphaned_at = Some(Instant::now());
                            info!("{} kept for {} s in case {} comes back", d.name, DISPLAY_GRACE.as_secs(), a.device.name);
                        }
                    }
                }
            }
        }
    }

    /// Remove virtual displays whose device has been gone longer than the grace period.
    fn reap_displays(&mut self) {
        let now = Instant::now();
        let gone: Vec<String> = self
            .displays
            .iter()
            .filter(|(_, d)| d.orphaned_at.map(|t| now.duration_since(t) >= DISPLAY_GRACE).unwrap_or(false))
            .map(|(k, _)| k.clone())
            .collect();
        for k in gone {
            if let Some(d) = self.displays.remove(&k) {
                info!("{} removed: its device did not come back", d.name);
                self.log.record(Record::Event { at_ms: now_ms(), what: format!("virtual display removed: {}", d.name) });
                self.platform.virtual_display_removed(d.display.source());
                drop(d);
            }
        }
    }

    fn end_all(&mut self, reason: &str) {
        while !self.sessions.is_empty() {
            let last = self.sessions.len() - 1;
            self.end_session(last, reason);
        }
    }
}

fn write_frame(a: &mut Active, frame: &EncodedFrame, log: &TimingLog, log_it: bool) {
    let source_id = match a.source {
        Some(Source::Virtual(id)) => id,
        _ => 0,
    };
    let Some(mid) = a.video_mid else { return };
    let Some(writer) = a.rtc.writer(mid) else { return };
    let Some(pt) = writer.payload_params().find(|p| p.spec().codec == Codec::H264).map(|p| p.pt()) else {
        warn!("no H264 payload type negotiated");
        return;
    };
    let send_ms = now_ms();
    let rtp_time = MediaTime::from_90khz(frame.rtp_time_90k);
    match writer.write(pt, Instant::now(), rtp_time, frame.data.clone()) {
        Ok(()) => {
            a.frames_sent += 1;
            if !log_it {
                return;
            }
            log.record(Record::Frame {
                source: source_id,
                seq: frame.seq,
                capture_ms: frame.capture_ms,
                encode_done_ms: frame.encode_done_ms,
                send_ms,
                bytes: frame.data.len(),
                idr: frame.idr,
            });
        }
        Err(e) => warn!("write frame {}: {e}", frame.seq),
    }
}

/// Shared so the signaling thread can call `answer` through `Arc<dyn OfferHandler>`.
pub struct SharedHandle(pub Mutex<SessionHandle>);

impl crate::signaling::OfferHandler for SharedHandle {
    fn answer(&self, offer_sdp: &str, mode: Mode, device: DeviceInfo, _peer: SocketAddr) -> Result<String> {
        self.0.lock().unwrap().answer(offer_sdp, mode, device)
    }

    fn devices(&self) -> DevicesSnapshot {
        self.0.lock().unwrap().devices()
    }
}
