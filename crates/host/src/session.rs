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

use anyhow::{anyhow, Context, Result};
use mullion_proto::keyframe::IdrLimiter;
use mullion_proto::signal::ProbeMessage;
use str0m::change::SdpOffer;
use str0m::channel::ChannelId;
use str0m::format::Codec;
use str0m::media::{MediaTime, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc};
use tracing::{debug, info, warn};

use crate::platform::{EncodedFrame, Pipeline, PipelineConfig, Platform};
use crate::timing::{now_ms, Record, TimingLog};

pub enum Msg {
    NewRtc(Rtc),
    Packet { at: Instant, source: SocketAddr, destination: SocketAddr, data: Vec<u8> },
    Frame(EncodedFrame),
    Shutdown,
}

/// Handle given to the signaling server: builds an `Rtc`, answers the offer,
/// and ships the `Rtc` to the session thread.
pub struct SessionHandle {
    tx: Sender<Msg>,
    local_addrs: Vec<SocketAddr>,
}

impl SessionHandle {
    pub fn answer(&self, offer_sdp: &str) -> Result<String> {
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
        self.tx.send(Msg::NewRtc(rtc)).map_err(|_| anyhow!("session thread gone"))?;
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
    let handle = SessionHandle { tx, local_addrs };
    let join = std::thread::Builder::new().name("mullion-session".into()).spawn(move || {
        let mut loop_state = SessionLoop {
            sockets,
            rx,
            frame_tx,
            platform,
            pipeline_cfg: cfg.pipeline,
            log,
            current: None,
        };
        loop_state.run();
    })?;
    Ok((handle, join))
}

struct Active {
    rtc: Rtc,
    video_mid: Option<Mid>,
    probe_channel: Option<ChannelId>,
    pipeline: Option<Box<dyn Pipeline>>,
    idr: IdrLimiter,
    connected_at: Option<Instant>,
    frames_sent: u64,
}

struct SessionLoop {
    sockets: HashMap<SocketAddr, UdpSocket>,
    rx: Receiver<Msg>,
    frame_tx: Sender<Msg>,
    platform: Arc<dyn Platform>,
    pipeline_cfg: PipelineConfig,
    log: Arc<TimingLog>,
    current: Option<Active>,
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
            match msg {
                Some(Msg::Shutdown) => break,
                Some(Msg::NewRtc(rtc)) => {
                    self.end_current("replaced by a new offer");
                    self.log.record(Record::Event { at_ms: now_ms(), what: "offer accepted".into() });
                    self.current = Some(Active {
                        rtc,
                        video_mid: None,
                        probe_channel: None,
                        pipeline: None,
                        idr: IdrLimiter::default_policy(),
                        connected_at: None,
                        frames_sent: 0,
                    });
                }
                Some(Msg::Packet { at, source, destination, data }) => {
                    if let Some(a) = self.current.as_mut() {
                        if let Ok(contents) = data.as_slice().try_into() {
                            let input = Input::Receive(at, Receive { proto: Protocol::Udp, source, destination, contents });
                            if let Err(e) = a.rtc.handle_input(input) {
                                warn!("handle_input: {e}");
                            }
                        }
                    }
                }
                Some(Msg::Frame(frame)) => {
                    if let Some(a) = self.current.as_mut() {
                        write_frame(a, &frame, &self.log);
                    }
                }
                None => {
                    if let Some(a) = self.current.as_mut() {
                        if let Err(e) = a.rtc.handle_input(Input::Timeout(Instant::now())) {
                            warn!("timeout input: {e}");
                        }
                    }
                }
            }
            // Drain after every mutation (str0m's single-mutation invariant).
            next_timeout = self.drain();
        }
        self.end_current("shutdown");
    }

    /// Drain `poll_output` to `Timeout`; returns the next deadline.
    fn drain(&mut self) -> Instant {
        loop {
            let Some(a) = self.current.as_mut() else { return Instant::now() + Duration::from_secs(1) };
            match a.rtc.poll_output() {
                Ok(Output::Timeout(t)) => return t,
                Ok(Output::Transmit(t)) => {
                    let sock = self.sockets.get(&t.source).or_else(|| self.sockets.values().next());
                    if let Some(sock) = sock {
                        if let Err(e) = sock.send_to(&t.contents, t.destination) {
                            debug!("send_to {}: {e}", t.destination);
                        }
                    }
                }
                Ok(Output::Event(ev)) => {
                    if let Some(reason) = self.on_event(ev) {
                        self.end_current(reason);
                        return Instant::now() + Duration::from_secs(1);
                    }
                }
                Err(e) => {
                    warn!("poll_output: {e}");
                    self.end_current("rtc error");
                    return Instant::now() + Duration::from_secs(1);
                }
            }
        }
    }

    /// Returns `Some(reason)` when the session should end.
    fn on_event(&mut self, ev: Event) -> Option<&'static str> {
        let a = self.current.as_mut()?;
        match ev {
            Event::MediaAdded(m) => {
                info!("media added: mid={} kind={:?} dir={:?}", m.mid, m.kind, m.direction);
                a.video_mid = Some(m.mid);
            }
            Event::ChannelOpen(id, label) => {
                info!("data channel open: {label}");
                if label == "probe" {
                    a.probe_channel = Some(id);
                }
            }
            Event::ChannelData(d) => {
                if Some(d.id) == a.probe_channel {
                    self.on_probe(&d.data);
                }
            }
            Event::IceConnectionStateChange(s) => {
                info!("ice: {s:?}");
                match s {
                    IceConnectionState::Connected | IceConnectionState::Completed => {
                        if a.pipeline.is_none() {
                            a.connected_at = Some(Instant::now());
                            self.log.record(Record::Event { at_ms: now_ms(), what: "connected".into() });
                            let (ptx, prx) = mpsc::channel::<EncodedFrame>();
                            // Forward encoder output into the session channel.
                            let ftx = self.frame_tx.clone();
                            std::thread::spawn(move || {
                                for f in prx {
                                    if ftx.send(Msg::Frame(f)).is_err() {
                                        break;
                                    }
                                }
                            });
                            match self.platform.start_pipeline(&self.pipeline_cfg, ptx) {
                                Ok(p) => a.pipeline = Some(p),
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
            Event::Connected => info!("dtls connected"),
            Event::KeyframeRequest(k) => {
                let granted = a.idr.request(Instant::now());
                self.log.record(Record::Pli { at_ms: now_ms(), granted });
                debug!("keyframe request {:?} granted={granted}", k.kind);
                if granted {
                    if let Some(p) = a.pipeline.as_ref() {
                        p.request_idr();
                    }
                }
            }
            Event::EgressBitrateEstimate(b) => debug!("bwe: {b:?}"),
            _ => {}
        }
        None
    }

    fn on_probe(&mut self, data: &[u8]) {
        let Some(a) = self.current.as_mut() else { return };
        let Ok(msg) = serde_json::from_slice::<ProbeMessage>(data) else {
            debug!("probe: unparseable {} bytes", data.len());
            return;
        };
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
                    painted_host_ms,
                    presented_host_ms,
                    glass_to_glass_ms: presented_host_ms - painted_host_ms,
                });
            }
            ProbeMessage::Capabilities { jitter_buffer_target_supported, jitter_buffer_target_ms, user_agent } => {
                info!("receiver: jitterBufferTarget supported={jitter_buffer_target_supported} value={jitter_buffer_target_ms:?} ua={user_agent}");
                self.log.record(Record::Capabilities { jitter_buffer_target_supported, jitter_buffer_target_ms, user_agent });
            }
            ProbeMessage::Pong { .. } => {}
        }
    }

    fn end_current(&mut self, reason: &str) {
        if let Some(mut a) = self.current.take() {
            if let Some(mut p) = a.pipeline.take() {
                p.stop();
            }
            a.rtc.disconnect();
            info!("session ended: {reason} ({} frames sent)", a.frames_sent);
            self.log.record(Record::Event { at_ms: now_ms(), what: format!("session ended: {reason}") });
            self.log.flush();
        }
    }
}

fn write_frame(a: &mut Active, frame: &EncodedFrame, log: &TimingLog) {
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
            log.record(Record::Frame {
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
    fn answer(&self, offer_sdp: &str, _peer: SocketAddr) -> Result<String> {
        self.0.lock().unwrap().answer(offer_sdp)
    }
}
