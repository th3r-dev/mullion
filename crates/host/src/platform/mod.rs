//! The four parts that differ by OS (PLAN.md "The platform trait"). PoC 1 only
//! needs capture and encode; `create_monitor` and `inject` arrive in steps 2 and 5.

use std::sync::mpsc::Sender;

use anyhow::Result;

#[cfg(target_os = "macos")]
pub mod macos;

/// One encoded access unit, Annex B, ready for the WebRTC packetizer.
#[derive(Debug, Clone)]
pub struct EncodedFrame {
    pub seq: u64,
    /// Host wall clock when the capture callback delivered the frame, ms.
    pub capture_ms: f64,
    /// Host wall clock when the encoder output callback fired, ms.
    pub encode_done_ms: f64,
    /// 90 kHz RTP timestamp derived from the capture time.
    pub rtp_time_90k: u64,
    pub idr: bool,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct PipelineConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    /// FR-8: refuse to run without a hardware encoder.
    pub require_hardware: bool,
    /// T-5: re-encode the last frame when the screen is static, at this rate.
    pub idle_fps: u32,
    /// Burn the latency probe strip into the top of every frame (A-13).
    pub probe_overlay: bool,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self { width: 1920, height: 1080, fps: 60, bitrate_kbps: 10_000, require_hardware: true, idle_fps: 2, probe_overlay: true }
    }
}

/// A running capture+encode pipeline. Dropping it stops capture and frees the
/// encoder (FR-7).
pub trait Pipeline: Send {
    /// FR-2 / FR-6: make the next encoded frame an IDR.
    fn request_idr(&self);
    fn stop(&mut self);
}

pub trait Platform: Send + Sync {
    fn name(&self) -> &'static str;
    /// Start capturing the main display and encoding it; frames go to `tx`.
    fn start_pipeline(&self, cfg: &PipelineConfig, tx: Sender<EncodedFrame>) -> Result<Box<dyn Pipeline>>;
}

/// The platform for the OS this binary was built for.
pub fn current() -> Result<Box<dyn Platform>> {
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(macos::MacOs))
    }
    #[cfg(not(target_os = "macos"))]
    {
        anyhow::bail!("Mullion host for this OS arrives in PLAN.md steps 2 (Windows) and 7 (Ubuntu)")
    }
}
