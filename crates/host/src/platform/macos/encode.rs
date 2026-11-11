//! FR-2 / FR-8 on macOS: VideoToolbox H.264, hardware required, Constrained
//! Baseline, no frame reordering (no B-frames), real-time, CBR-ish via
//! AverageBitRate + DataRateLimits. Output is converted from AVCC to Annex B
//! with SPS/PPS prepended on every IDR, which is what the str0m packetizer
//! and every WebRTC receiver expect.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use objc2_core_foundation::{CFArray, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType, Type};
use objc2_core_media::{
    kCMSampleAttachmentKey_NotSync, kCMVideoCodecType_H264, CMSampleBuffer, CMTime, CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
};
use objc2_core_video::CVImageBuffer;
use objc2_video_toolbox::{
    kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AverageBitRate, kVTCompressionPropertyKey_DataRateLimits,
    kVTCompressionPropertyKey_ExpectedFrameRate, kVTCompressionPropertyKey_MaxKeyFrameInterval,
    kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, kVTCompressionPropertyKey_ProfileLevel, kVTCompressionPropertyKey_RealTime,
    kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder, kVTEncodeFrameOptionKey_ForceKeyFrame,
    kVTProfileLevel_H264_ConstrainedBaseline_AutoLevel, kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
    VTCompressionSession, VTEncodeInfoFlags, VTSession, VTSessionCopyProperty, VTSessionSetProperty,
};
use tracing::{debug, info, warn};

use crate::platform::{EncodedFrame, PipelineConfig};
use crate::timing::now_ms;
use mullion_proto::h264;

pub const NO_HW_ENCODER: &str = "No hardware H.264 encoder found";

/// Shared with the VT output callback through `ref_con`.
struct Shared {
    tx: Sender<EncodedFrame>,
    seq: AtomicU64,
    dropped: AtomicU64,
}

/// Per-frame data passed as `source_frame_refcon`, boxed and reclaimed in the callback.
struct FrameMeta {
    capture_ms: f64,
}

pub struct VtEncoder {
    session: CFRetained<VTCompressionSession>,
    shared: NonNull<Shared>,
    force_idr: AtomicBool,
    last_encode: Mutex<Instant>,
    frames_in: AtomicU64,
    fps: u32,
}

// SAFETY: VTCompressionSession is documented thread-safe for encode calls; the
// callback pointer stays valid for the session's lifetime.
unsafe impl Send for VtEncoder {}
unsafe impl Sync for VtEncoder {}

/// VTCompressionSession is a VTSession in C; the bindings do not express that.
fn as_session(s: &VTCompressionSession) -> &VTSession {
    unsafe { &*(s as *const VTCompressionSession as *const VTSession) }
}

fn set_prop(session: &VTCompressionSession, key: &CFString, value: &CFType) -> Result<()> {
    let st = unsafe { VTSessionSetProperty(as_session(session), key, Some(value)) };
    if st != 0 {
        bail!("VTSessionSetProperty({key}) failed: OSStatus {st}");
    }
    Ok(())
}

impl VtEncoder {
    pub fn new(cfg: &PipelineConfig, tx: Sender<EncodedFrame>) -> Result<Self> {
        let shared = Box::into_raw(Box::new(Shared { tx, seq: AtomicU64::new(0), dropped: AtomicU64::new(0) }));

        let spec: Option<CFRetained<CFDictionary<CFString, CFBoolean>>> = if cfg.require_hardware {
            Some(CFDictionary::from_slices(
                &[unsafe { kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder }],
                &[CFBoolean::new(true)],
            ))
        } else {
            None
        };

        let mut raw: *mut VTCompressionSession = std::ptr::null_mut();
        let st = unsafe {
            VTCompressionSession::create(
                None,
                cfg.width as i32,
                cfg.height as i32,
                kCMVideoCodecType_H264,
                spec.as_deref().map(|d| d.as_opaque()),
                None,
                None,
                Some(output_callback),
                shared as *mut c_void,
                NonNull::new_unchecked(&mut raw),
            )
        };
        if st != 0 || raw.is_null() {
            unsafe { drop(Box::from_raw(shared)) };
            // kVTCouldNotFindVideoEncoderErr = -12908; with RequireHardware set this
            // is the "no hardware encoder" case (FR-8).
            if st == -12908 || st == -12915 {
                bail!("{NO_HW_ENCODER} (OSStatus {st})");
            }
            bail!("VTCompressionSessionCreate failed: OSStatus {st}");
        }
        let session = unsafe { CFRetained::from_raw(NonNull::new_unchecked(raw)) };

        let t = |b: bool| -> CFRetained<CFType> { unsafe { CFRetained::cast_unchecked(CFBoolean::new(b).retain()) } };
        let n = |v: i32| -> CFRetained<CFType> { unsafe { CFRetained::cast_unchecked(CFNumber::new_i32(v)) } };
        unsafe {
            set_prop(&session, kVTCompressionPropertyKey_RealTime, &t(true))?;
            set_prop(&session, kVTCompressionPropertyKey_AllowFrameReordering, &t(false))?;
            set_prop(&session, kVTCompressionPropertyKey_ProfileLevel, kVTProfileLevel_H264_ConstrainedBaseline_AutoLevel)?;
            set_prop(&session, kVTCompressionPropertyKey_AverageBitRate, &n((cfg.bitrate_kbps * 1000) as i32))?;
            set_prop(&session, kVTCompressionPropertyKey_ExpectedFrameRate, &n(cfg.fps as i32))?;
            // Keyframes only on request (FR-2); a long interval keeps the bitrate flat.
            set_prop(&session, kVTCompressionPropertyKey_MaxKeyFrameInterval, &n((cfg.fps * 600) as i32))?;
            set_prop(&session, kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, &n(600))?;
            // Cap any one-second window at 1.5x the average (T-7: no window above 20 Mbps at 10 Mbps).
            let limit_bytes = CFNumber::new_i32((cfg.bitrate_kbps as i32 * 1000 / 8) * 3 / 2);
            let limit_secs = CFNumber::new_f64(1.0);
            let limits: CFRetained<CFArray<CFNumber>> = CFArray::from_objects(&[&*limit_bytes, &*limit_secs]);
            set_prop(&session, kVTCompressionPropertyKey_DataRateLimits, limits.as_opaque())?;
        }

        // FR-8: confirm we really got hardware.
        let using_hw = unsafe {
            let mut out: *const CFType = std::ptr::null();
            let st = VTSessionCopyProperty(
                as_session(&session),
                kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder,
                None,
                &mut out as *mut *const CFType as *mut c_void,
            );
            if st == 0 && !out.is_null() {
                let v = CFRetained::from_raw(NonNull::new_unchecked(out as *mut CFType));
                v.downcast::<CFBoolean>().map(|b| b.as_bool()).unwrap_or(false)
            } else {
                false
            }
        };
        if cfg.require_hardware && !using_hw {
            unsafe { session.invalidate() };
            unsafe { drop(Box::from_raw(shared)) };
            bail!("{NO_HW_ENCODER} (VideoToolbox reports software encoder)");
        }
        info!("VideoToolbox H.264 session: hardware={using_hw}, {} kbps", cfg.bitrate_kbps);

        let st = unsafe { session.prepare_to_encode_frames() };
        if st != 0 {
            warn!("prepare_to_encode_frames: OSStatus {st}");
        }

        Ok(Self {
            session,
            shared: unsafe { NonNull::new_unchecked(shared) },
            force_idr: AtomicBool::new(true), // first frame is an IDR (T-6)
            last_encode: Mutex::new(Instant::now()),
            frames_in: AtomicU64::new(0),
            fps: cfg.fps,
        })
    }

    pub fn force_idr(&self) {
        self.force_idr.store(true, Ordering::SeqCst);
    }

    pub fn since_last_encode(&self) -> Duration {
        self.last_encode.lock().unwrap().elapsed()
    }

    /// Submit one frame. Non-blocking; output arrives on the VT callback.
    pub fn encode(&self, image: &CVImageBuffer, capture_ms: f64) {
        let idx = self.frames_in.fetch_add(1, Ordering::Relaxed);
        *self.last_encode.lock().unwrap() = Instant::now();
        let pts = unsafe { CMTime::new(idx as i64, self.fps as i32) };
        let dur = unsafe { CMTime::new(1, self.fps as i32) };
        let props: Option<CFRetained<CFDictionary<CFString, CFBoolean>>> = if self.force_idr.swap(false, Ordering::SeqCst) {
            Some(CFDictionary::from_slices(&[unsafe { kVTEncodeFrameOptionKey_ForceKeyFrame }], &[CFBoolean::new(true)]))
        } else {
            None
        };
        let meta = Box::into_raw(Box::new(FrameMeta { capture_ms }));
        let mut flags = VTEncodeInfoFlags::empty();
        let st = unsafe {
            self.session.encode_frame(image, pts, dur, props.as_deref().map(|d| d.as_opaque()), meta as *mut c_void, &mut flags)
        };
        if st != 0 {
            warn!("VTCompressionSessionEncodeFrame: OSStatus {st}");
            unsafe { drop(Box::from_raw(meta)) };
        }
    }

    /// Flush and tear down (FR-7).
    pub fn finish(&self) {
        unsafe {
            let _ = self.session.complete_frames(CMTime::new(0, 1));
            self.session.invalidate();
        }
        let dropped = unsafe { self.shared.as_ref() }.dropped.load(Ordering::Relaxed);
        if dropped > 0 {
            warn!("encoder dropped {dropped} frames");
        }
    }
}

impl Drop for VtEncoder {
    fn drop(&mut self) {
        // The session is invalidated in `finish`; invalidate again is harmless.
        unsafe { self.session.invalidate() };
        // After invalidate no callback can fire, so the shared box is ours again.
        unsafe { drop(Box::from_raw(self.shared.as_ptr())) };
    }
}

unsafe extern "C-unwind" fn output_callback(
    ref_con: *mut c_void,
    frame_ref_con: *mut c_void,
    status: i32,
    flags: VTEncodeInfoFlags,
    sample: *mut CMSampleBuffer,
) {
    let shared = &*(ref_con as *const Shared);
    let meta = if frame_ref_con.is_null() { None } else { Some(Box::from_raw(frame_ref_con as *mut FrameMeta)) };
    if status != 0 || sample.is_null() || flags.contains(VTEncodeInfoFlags::FrameDropped) {
        shared.dropped.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let sample = &*sample;
    let encode_done_ms = now_ms();
    let capture_ms = meta.map(|m| m.capture_ms).unwrap_or(encode_done_ms);

    let is_idr = is_sync_sample(sample);
    let mut data = Vec::with_capacity(64 * 1024);
    if is_idr {
        match parameter_sets(sample) {
            Ok(sets) => {
                for s in sets {
                    data.extend_from_slice(&[0, 0, 0, 1]);
                    data.extend_from_slice(&s);
                }
            }
            Err(e) => warn!("no parameter sets on IDR: {e}"),
        }
    }
    match sample_bytes(sample) {
        Ok(avcc) => {
            for nal in h264::split_avcc(&avcc, 4) {
                data.extend_from_slice(&[0, 0, 0, 1]);
                data.extend_from_slice(nal);
            }
        }
        Err(e) => {
            warn!("sample bytes: {e}");
            return;
        }
    }

    let seq = shared.seq.fetch_add(1, Ordering::Relaxed);
    let rtp_time_90k = (capture_ms * 90.0) as u64;
    let frame = EncodedFrame { seq, capture_ms, encode_done_ms, rtp_time_90k, idr: is_idr, data };
    if seq == 0 {
        let au = h264::inspect_annex_b(&frame.data);
        debug!("first access unit: {:?}", au);
    }
    let _ = shared.tx.send(frame);
}

fn is_sync_sample(sample: &CMSampleBuffer) -> bool {
    unsafe {
        let Some(arr) = sample.sample_attachments_array(false) else { return true };
        let arr: &CFArray<CFDictionary> = arr.cast_unchecked();
        let Some(dict) = arr.get(0) else { return true };
        let dict: &CFDictionary<CFString, CFBoolean> = dict.cast_unchecked();
        match dict.get(kCMSampleAttachmentKey_NotSync) {
            Some(b) => !b.as_bool(),
            None => true,
        }
    }
}

fn parameter_sets(sample: &CMSampleBuffer) -> Result<Vec<Vec<u8>>> {
    unsafe {
        let desc = sample.format_description().ok_or_else(|| anyhow!("no format description"))?;
        let mut count: usize = 0;
        let st = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(&desc, 0, std::ptr::null_mut(), std::ptr::null_mut(), &mut count, std::ptr::null_mut());
        if st != 0 {
            bail!("GetH264ParameterSetAtIndex(count): {st}");
        }
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let mut ptr: *const u8 = std::ptr::null();
            let mut size: usize = 0;
            let st = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(&desc, i, &mut ptr, &mut size, std::ptr::null_mut(), std::ptr::null_mut());
            if st != 0 || ptr.is_null() {
                bail!("GetH264ParameterSetAtIndex({i}): {st}");
            }
            out.push(std::slice::from_raw_parts(ptr, size).to_vec());
        }
        Ok(out)
    }
}

fn sample_bytes(sample: &CMSampleBuffer) -> Result<Vec<u8>> {
    unsafe {
        let block = sample.data_buffer().ok_or_else(|| anyhow!("no data buffer"))?;
        let len = block.data_length();
        let mut out = vec![0u8; len];
        let st = block.copy_data_bytes(0, len, NonNull::new_unchecked(out.as_mut_ptr() as *mut c_void));
        if st != 0 {
            bail!("CMBlockBufferCopyDataBytes: {st}");
        }
        Ok(out)
    }
}

/// FR-8 / T-22 probe without capturing: can a hardware session be created?
pub fn probe_hardware_encoder(width: u32, height: u32) -> Result<()> {
    let (tx, _rx) = std::sync::mpsc::channel();
    let cfg = PipelineConfig { width, height, require_hardware: true, ..Default::default() };
    let enc = VtEncoder::new(&cfg, tx)?;
    enc.finish();
    Ok(())
}
