//! FR-9: per-frame timing, split by stage, written as JSON lines so the test
//! plan's latency split can be computed offline. One record per encoded frame
//! on the host side, one per presented frame reported by the receiver.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// Wall-clock milliseconds since the Unix epoch, as f64 (same unit as JS `Date.now()`).
pub fn now_ms() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0)
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "rec", rename_all = "snake_case")]
pub enum Record {
    /// Host side: one encoded frame.
    Frame {
        /// Capture source: 0 = main display, otherwise the virtual display id.
        source: u32,
        seq: u64,
        capture_ms: f64,
        encode_done_ms: f64,
        send_ms: f64,
        bytes: usize,
        idr: bool,
    },
    /// Receiver side: one presented frame (from the probe data channel).
    Presented {
        session: u64,
        painted_host_ms: f64,
        presented_host_ms: f64,
        glass_to_glass_ms: f64,
    },
    /// Receiver capabilities (FR-5).
    Capabilities { session: u64, peer: String, jitter_buffer_target_supported: bool, jitter_buffer_target_ms: Option<f64>, user_agent: String },
    /// A keyframe request from the receiver and whether it was granted (FR-6).
    Pli { at_ms: f64, granted: bool },
    /// Session lifecycle.
    Event { at_ms: f64, what: String },
}

pub struct TimingLog {
    inner: Mutex<Inner>,
}

struct Inner {
    out: Option<BufWriter<File>>,
    frames: u64,
    bytes: u64,
    first_frame_ms: Option<f64>,
    last_frame_ms: Option<f64>,
    g2g: Vec<f64>,
    encode_stage: Vec<f64>,
    plis: u64,
    idrs_granted: u64,
}

impl TimingLog {
    pub fn new(path: Option<&Path>) -> std::io::Result<Self> {
        let out = match path {
            Some(p) => {
                if let Some(dir) = p.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                Some(BufWriter::new(File::create(p)?))
            }
            None => None,
        };
        Ok(Self {
            inner: Mutex::new(Inner {
                out,
                frames: 0,
                bytes: 0,
                first_frame_ms: None,
                last_frame_ms: None,
                g2g: Vec::new(),
                encode_stage: Vec::new(),
                plis: 0,
                idrs_granted: 0,
            }),
        })
    }

    pub fn record(&self, r: Record) {
        let mut g = self.inner.lock().unwrap();
        match &r {
            Record::Frame { capture_ms, encode_done_ms, bytes, .. } => {
                g.frames += 1;
                g.bytes += *bytes as u64;
                g.first_frame_ms.get_or_insert(*capture_ms);
                g.last_frame_ms = Some(*capture_ms);
                g.encode_stage.push(encode_done_ms - capture_ms);
            }
            Record::Presented { glass_to_glass_ms, .. } => g.g2g.push(*glass_to_glass_ms),
            Record::Pli { granted, .. } => {
                g.plis += 1;
                if *granted {
                    g.idrs_granted += 1;
                }
            }
            _ => {}
        }
        if let Some(out) = g.out.as_mut() {
            if let Ok(line) = serde_json::to_string(&r) {
                let _ = out.write_all(line.as_bytes());
                let _ = out.write_all(b"\n");
            }
        }
    }

    pub fn flush(&self) {
        if let Some(out) = self.inner.lock().unwrap().out.as_mut() {
            let _ = out.flush();
        }
    }

    /// Human summary for the end of a run (and the Step 5 report).
    pub fn summary(&self) -> Summary {
        let g = self.inner.lock().unwrap();
        let secs = match (g.first_frame_ms, g.last_frame_ms) {
            (Some(a), Some(b)) if b > a => (b - a) / 1000.0,
            _ => 0.0,
        };
        Summary {
            frames: g.frames,
            seconds: secs,
            fps: if secs > 0.0 { (g.frames.saturating_sub(1)) as f64 / secs } else { 0.0 },
            mbps: if secs > 0.0 { g.bytes as f64 * 8.0 / secs / 1e6 } else { 0.0 },
            encode_p95_ms: mullion_proto::clock::p95(&g.encode_stage),
            presented: g.g2g.len(),
            g2g_p50_ms: percentile(&g.g2g, 0.5),
            g2g_p95_ms: mullion_proto::clock::p95(&g.g2g),
            plis: g.plis,
            idrs_granted: g.idrs_granted,
        }
    }
}

fn percentile(v: &[f64], p: f64) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let mut s: Vec<f64> = v.iter().copied().filter(|x| x.is_finite()).collect();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let rank = ((p * s.len() as f64).ceil() as usize).clamp(1, s.len());
    Some(s[rank - 1])
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub frames: u64,
    pub seconds: f64,
    pub fps: f64,
    pub mbps: f64,
    pub encode_p95_ms: Option<f64>,
    pub presented: usize,
    pub g2g_p50_ms: Option<f64>,
    pub g2g_p95_ms: Option<f64>,
    pub plis: u64,
    pub idrs_granted: u64,
}

impl std::fmt::Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "frames={} over {:.1}s fps={:.1} send={:.1} Mbps encode_p95={} presented={} g2g_p50={} g2g_p95={} plis={} idrs={}",
            self.frames,
            self.seconds,
            self.fps,
            self.mbps,
            fmt_opt(self.encode_p95_ms),
            self.presented,
            fmt_opt(self.g2g_p50_ms),
            fmt_opt(self.g2g_p95_ms),
            self.plis,
            self.idrs_granted
        )
    }
}

fn fmt_opt(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.1}ms")).unwrap_or_else(|| "n/a".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_counts_frames_and_latency() {
        let log = TimingLog::new(None).unwrap();
        for i in 0..61u64 {
            let t = 1000.0 + i as f64 * (1000.0 / 60.0);
            log.record(Record::Frame { source: 0, seq: i, capture_ms: t, encode_done_ms: t + 4.0, send_ms: t + 4.5, bytes: 20_000, idr: i == 0 });
        }
        for g in [20.0, 25.0, 30.0, 80.0] {
            log.record(Record::Presented { session: 1, painted_host_ms: 0.0, presented_host_ms: g, glass_to_glass_ms: g });
        }
        log.record(Record::Pli { at_ms: 0.0, granted: true });
        log.record(Record::Pli { at_ms: 1.0, granted: false });
        let s = log.summary();
        assert_eq!(s.frames, 61);
        assert!((s.fps - 60.0).abs() < 0.5, "fps={}", s.fps);
        assert!((s.mbps - 9.6).abs() < 0.2, "mbps={}", s.mbps);
        assert_eq!(s.encode_p95_ms, Some(4.0));
        assert_eq!(s.g2g_p95_ms, Some(80.0));
        assert_eq!(s.g2g_p50_ms, Some(25.0));
        assert_eq!((s.plis, s.idrs_granted), (2, 1));
    }

    #[test]
    fn writes_json_lines() {
        let dir = std::env::temp_dir().join(format!("mullion-timing-{}", std::process::id()));
        let path = dir.join("t.jsonl");
        let log = TimingLog::new(Some(&path)).unwrap();
        log.record(Record::Event { at_ms: 1.0, what: "start".into() });
        log.record(Record::Frame { source: 0, seq: 0, capture_ms: 1.0, encode_done_ms: 2.0, send_ms: 3.0, bytes: 1, idr: true });
        log.flush();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains(r#""rec":"event""#));
        assert!(lines[1].contains(r#""rec":"frame""#) && lines[1].contains(r#""idr":true"#));
        let _ = std::fs::remove_dir_all(dir);
    }
}
