//! macOS: ScreenCaptureKit for capture, VideoToolbox for H.264.

mod arrange;
mod capture;
mod encode;
mod overlay;
mod virtual_display;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use objc2_core_foundation::{CFRetained, Type};
use objc2_core_video::CVImageBuffer;
use tracing::{info, warn};

use super::{EncodedFrame, Pipeline, PipelineConfig, Platform, Source, VirtualDisplay};
use crate::timing::now_ms;

pub use encode::probe_hardware_encoder;

/// A retained pixel buffer that may cross threads. CVPixelBuffer is
/// reference-counted and immutable once SCK hands it out.
struct SendImage(CFRetained<CVImageBuffer>);
unsafe impl Send for SendImage {}
unsafe impl Sync for SendImage {}

pub struct MacOs;

/// Display ids of the virtual displays this host created, in creation order.
static OUR_DISPLAYS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

impl Platform for MacOs {
    fn name(&self) -> &'static str {
        "macOS (ScreenCaptureKit + VideoToolbox)"
    }

    fn create_virtual_display(&self, w: u32, h: u32, hz: u32, name: &str, serial: u32) -> Result<Option<Box<dyn VirtualDisplay>>> {
        let vd = virtual_display::VirtualDisplay::create(w, h, hz, name, serial)?;
        // Give macOS a moment to register the display, then keep the Mac's own
        // screen as main and line ours up to its right.
        std::thread::sleep(Duration::from_millis(400));
        let mut ours = OUR_DISPLAYS.lock().unwrap();
        ours.push(vd.display_id);
        if let Err(e) = arrange::arrange(&ours) {
            warn!("arrange displays: {e:#}");
        }
        Ok(Some(Box::new(vd)))
    }

    fn start_pipeline(&self, source: Source, cfg: &PipelineConfig, tx: Sender<EncodedFrame>) -> Result<Box<dyn Pipeline>> {
        let encoder = Arc::new(encode::VtEncoder::new(cfg, tx)?);
        let probe_on = cfg.probe_overlay;
        let last_frame: Arc<Mutex<Option<Arc<SendImage>>>> = Arc::new(Mutex::new(None));

        let enc = encoder.clone();
        let lf = last_frame.clone();
        let display_id = match source {
            Source::Main => None,
            Source::Virtual(id) => Some(id),
        };
        let capture = capture::ScreenCapture::start(
            display_id,
            cfg,
            Box::new(move |image: &CVImageBuffer| {
                let t = now_ms();
                if probe_on {
                    overlay::burn_strip(image, t as u64);
                }
                *lf.lock().unwrap() = Some(Arc::new(SendImage(image.retain())));
                enc.encode(image, t);
            }),
        )?;

        // T-5: when ScreenCaptureKit stops delivering (static screen), re-encode
        // the last frame at `idle_fps` so the receiver never times out.
        let stop = Arc::new(AtomicBool::new(false));
        let idle_enc = encoder.clone();
        let idle_stop = stop.clone();
        let idle_period = Duration::from_millis(1000 / cfg.idle_fps.max(1) as u64);
        let idle_thread = std::thread::Builder::new()
            .name("mullion-idle".into())
            .spawn(move || {
                while !idle_stop.load(Ordering::Relaxed) {
                    std::thread::sleep(idle_period / 2);
                    let since = idle_enc.since_last_encode();
                    if since >= idle_period {
                        let img = last_frame.lock().unwrap().clone();
                        if let Some(img) = img {
                            let t = now_ms();
                            if probe_on {
                                overlay::burn_strip(&img.0, t as u64);
                            }
                            idle_enc.encode(&img.0, t);
                        }
                    }
                }
            })?;

        info!("pipeline started for {:?}: {}x{} @ {} fps, {} kbps", source, cfg.width, cfg.height, cfg.fps, cfg.bitrate_kbps);
        Ok(Box::new(MacPipeline { capture: Some(capture), encoder, stop, idle_thread: Some(idle_thread) }))
    }
}

struct MacPipeline {
    capture: Option<capture::ScreenCapture>,
    encoder: Arc<encode::VtEncoder>,
    stop: Arc<AtomicBool>,
    idle_thread: Option<std::thread::JoinHandle<()>>,
}

impl Pipeline for MacPipeline {
    fn request_idr(&self) {
        self.encoder.force_idr();
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(c) = self.capture.take() {
            c.stop();
        }
        if let Some(t) = self.idle_thread.take() {
            let _ = t.join();
        }
        self.encoder.finish();
        info!("pipeline stopped");
    }
}

impl Drop for MacPipeline {
    fn drop(&mut self) {
        if self.capture.is_some() {
            self.stop();
        }
        if Arc::strong_count(&self.encoder) > 1 {
            warn!("encoder still referenced after pipeline stop");
        }
    }
}

impl VirtualDisplay for virtual_display::VirtualDisplay {
    fn source(&self) -> Source {
        Source::Virtual(self.display_id)
    }
}

impl Drop for MacOs {
    fn drop(&mut self) {
        OUR_DISPLAYS.lock().unwrap().clear();
    }
}
