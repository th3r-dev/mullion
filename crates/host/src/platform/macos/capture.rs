//! FR-1 on macOS: ScreenCaptureKit capture of the main display, scaled to the
//! configured size with the aspect ratio preserved (letterboxed), delivering
//! IOSurface-backed `CVImageBuffer`s in 420v so the encoder takes them as is.

use std::sync::mpsc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchQueueAttr};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::CVImageBuffer;
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol, NSString};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamDelegate, SCStreamOutput,
    SCStreamOutputType,
};
use tracing::{info, warn};

use crate::platform::PipelineConfig;

/// '420v': kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange.
const PIXEL_FORMAT_420V: u32 = u32::from_be_bytes(*b"420v");

pub type FrameCallback = Box<dyn Fn(&CVImageBuffer) + Send + Sync>;

pub struct Ivars {
    on_frame: FrameCallback,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; Output has no Drop.
    #[unsafe(super(NSObject))]
    #[name = "MullionStreamOutput"]
    #[ivars = Ivars]
    pub struct Output;

    unsafe impl NSObjectProtocol for Output {}

    unsafe impl SCStreamOutput for Output {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn did_output(&self, _stream: &SCStream, sample_buffer: &CMSampleBuffer, kind: SCStreamOutputType) {
            if kind != SCStreamOutputType::Screen {
                return;
            }
            // SCK sends buffers without an image when nothing changed or the
            // display is idle; those carry only attachments and are skipped.
            if let Some(image) = unsafe { sample_buffer.image_buffer() } {
                (self.ivars().on_frame)(&image);
            }
        }
    }

    unsafe impl SCStreamDelegate for Output {
        #[unsafe(method(stream:didStopWithError:))]
        unsafe fn did_stop(&self, _stream: &SCStream, error: &NSError) {
            warn!("ScreenCaptureKit stopped: {}", error.localizedDescription());
        }
    }
);

impl Output {
    fn new(on_frame: FrameCallback) -> Retained<Self> {
        let this = Self::alloc().set_ivars(Ivars { on_frame });
        unsafe { msg_send![super(this), init] }
    }
}

pub struct ScreenCapture {
    stream: Retained<SCStream>,
    _output: Retained<Output>,
    _queue: dispatch2::DispatchRetained<DispatchQueue>,
}

// SAFETY: SCStream and our output object are used from the dispatch queue and
// from the thread that stops them; SCK documents its objects as thread-safe.
unsafe impl Send for ScreenCapture {}

impl ScreenCapture {
    pub fn start(display_id: Option<u32>, cfg: &PipelineConfig, on_frame: FrameCallback) -> Result<Self> {
        let disp: Retained<SCDisplay> = pick_display(display_id)?;
        let (dw, dh) = unsafe { (disp.width(), disp.height()) };
        info!("capturing display {} ({}x{} points) -> {}x{}", unsafe { disp.displayID() }, dw, dh, cfg.width, cfg.height);

        let filter = unsafe {
            SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), &disp, &NSArray::new())
        };

        let config = unsafe {
            let c = SCStreamConfiguration::new();
            c.setWidth(cfg.width as usize);
            c.setHeight(cfg.height as usize);
            c.setPreservesAspectRatio(true); // letterbox, never crop (T-2)
            c.setScalesToFit(true);
            c.setPixelFormat(PIXEL_FORMAT_420V);
            c.setMinimumFrameInterval(CMTime::new(1, cfg.fps as i32));
            c.setQueueDepth(3);
            c.setShowsCursor(true);
            c.setStreamName(Some(&NSString::from_str("Mullion")));
            c
        };

        let output = Output::new(on_frame);
        let delegate: &ProtocolObject<dyn SCStreamDelegate> = ProtocolObject::from_ref(&*output);
        let stream = unsafe { SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &config, Some(delegate)) };

        let queue = DispatchQueue::new("app.mullion.capture", DispatchQueueAttr::SERIAL);
        let out_proto: &ProtocolObject<dyn SCStreamOutput> = ProtocolObject::from_ref(&*output);
        unsafe {
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(out_proto, SCStreamOutputType::Screen, Some(&queue))
                .map_err(|e| anyhow!("addStreamOutput: {}", e.localizedDescription()))?;
        }

        let (tx, rx) = mpsc::channel::<Option<String>>();
        let block = RcBlock::new(move |err: *mut NSError| {
            let msg = if err.is_null() { None } else { Some(unsafe { (*err).localizedDescription().to_string() }) };
            let _ = tx.send(msg);
        });
        unsafe { stream.startCaptureWithCompletionHandler(Some(&block)) };
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(None) => {}
            Ok(Some(e)) => bail!("ScreenCaptureKit could not start: {e}"),
            Err(_) => bail!("ScreenCaptureKit did not answer within 10 s"),
        }

        Ok(Self { stream, _output: output, _queue: queue })
    }

    pub fn stop(self) {
        let (tx, rx) = mpsc::channel::<()>();
        let block = RcBlock::new(move |_err: *mut NSError| {
            let _ = tx.send(());
        });
        unsafe { self.stream.stopCaptureWithCompletionHandler(Some(&block)) };
        let _ = rx.recv_timeout(Duration::from_secs(5));
    }
}

/// The main display (or the one with `want` id) via SCShareableContent. A
/// denied Screen Recording permission surfaces here (T-4) with the BRAND.md wording.
fn pick_display(want: Option<u32>) -> Result<Retained<SCDisplay>> {
    let (tx, rx) = mpsc::channel::<Result<Retained<SCShareableContent>>>();
    let block = RcBlock::new(move |content: *mut SCShareableContent, err: *mut NSError| {
        let r = if !content.is_null() {
            Ok(unsafe { Retained::retain(content) }.unwrap())
        } else {
            let msg = if err.is_null() { "unknown error".to_string() } else { unsafe { (*err).localizedDescription().to_string() } };
            Err(anyhow!(msg))
        };
        let _ = tx.send(r);
    });
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&block) };
    let content = rx
        .recv_timeout(Duration::from_secs(15))
        .context("ScreenCaptureKit did not list displays within 15 s")?
        .map_err(|e| {
            anyhow!(
                "Mullion needs Screen Recording to show this Mac on your tablet. Open System Settings > Privacy & Security > Screen & System Audio Recording. ({e})"
            )
        })?;
    let displays = unsafe { content.displays() };
    if let Some(id) = want {
        return displays
            .iter()
            .find(|d| unsafe { d.displayID() } == id)
            .ok_or_else(|| anyhow!("Display {id} is not listed by ScreenCaptureKit"));
    }
    // The main display is the one whose frame starts at the origin.
    let main = displays.iter().find(|d| {
        let f = unsafe { d.frame() };
        f.origin.x == 0.0 && f.origin.y == 0.0
    });
    main.or_else(|| displays.iter().next()).ok_or_else(|| anyhow!("No display to capture"))
}
