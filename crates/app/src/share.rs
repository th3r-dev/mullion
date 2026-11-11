//! "Share this screen" (Android): the tablet becomes a Mirror-only Mullion
//! host, so a Mac (Connect to Another Computer…) or a browser shows it.
//!
//! Capture and encoding are Android's (crates/app/android/kotlin/Share.kt:
//! MediaProjection into a hardware H.264 encoder, in a foreground service).
//! Each encoder output reaches `MullionNative.onH264` here, becomes an
//! `EncodedFrame`, and goes to the host's sessions (mullion_host, the same
//! core as the Mac's). Pairing works as on the Mac: the tablet shows the code.

use mullion_host::platform::EncodedFrame;

/// MediaCodec.BufferInfo flags.
const FLAG_KEY_FRAME: i32 = 1;
const FLAG_CODEC_CONFIG: i32 = 2;

/// Encoder output to `EncodedFrame`s. MediaCodec hands over SPS/PPS once, as
/// a codec-config buffer; receivers that join later need them in front of
/// every IDR, so they are kept and prepended (Annex B either way).
#[derive(Default)]
pub struct Assembler {
    config: Vec<u8>,
    seq: u64,
}

impl Assembler {
    pub fn push(&mut self, flags: i32, bytes: &[u8], pts_us: i64, now_ms: f64) -> Option<EncodedFrame> {
        if flags & FLAG_CODEC_CONFIG != 0 {
            self.config = bytes.to_vec();
            return None;
        }
        let idr = flags & FLAG_KEY_FRAME != 0;
        let has_sps = mullion_proto::h264::split_annex_b(bytes).first().is_some_and(|n| !n.is_empty() && n[0] & 0x1f == 7);
        let data = if idr && !has_sps { [self.config.as_slice(), bytes].concat() } else { bytes.to_vec() };
        self.seq += 1;
        Some(EncodedFrame {
            seq: self.seq,
            capture_ms: now_ms,
            encode_done_ms: now_ms,
            rtp_time_90k: (pts_us.max(0) as u64) * 9 / 100,
            idr,
            data,
        })
    }
}

#[cfg(target_os = "android")]
pub use android::{deny_pairing, events, set_fullscreen, set_share};

#[cfg(target_os = "android")]
mod android {
    use super::Assembler;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::Sender;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use anyhow::{bail, Result};
    use dioxus::mobile::wry::prelude::dispatch;
    use futures_channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
    use jni::objects::{JByteBuffer, JClass, JString};
    use jni::sys::{jboolean, jint, jlong, JNI_FALSE, JNI_TRUE};
    use jni::JNIEnv;
    use mullion_host::pairing::Notice;
    use mullion_host::platform::{DisplayInfo, EncodedFrame, Pipeline, PipelineConfig, Platform, Source, VirtualDisplay};
    use serde_json::{json, Value};

    #[derive(Default)]
    struct State {
        sharing: bool,
        size: (u32, u32),
        asm: Assembler,
        sink: Option<Sender<EncodedFrame>>,
        host: Option<mullion_host::Host>,
        name: String,
    }

    static STATE: Mutex<Option<State>> = Mutex::new(None);
    /// Set by a session (join, picture loss); the encoder reads it on its next output.
    static WANT_IDR: AtomicBool = AtomicBool::new(false);
    /// Events for the page (main.rs forwards them over the bridge).
    static EVENTS: Mutex<Option<UnboundedSender<Value>>> = Mutex::new(None);

    fn with<T>(f: impl FnOnce(&mut State) -> T) -> T {
        let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
        f(g.get_or_insert_with(State::default))
    }

    fn emit(v: Value) {
        if let Some(tx) = EVENTS.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let _ = tx.unbounded_send(v);
        }
    }

    /// The page's end of the events: share state and pairing codes.
    pub fn events() -> UnboundedReceiver<Value> {
        let (tx, rx) = unbounded();
        *EVENTS.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        rx
    }

    /// Start (asks Android for consent first) or stop sharing. `name` is what
    /// receivers see this tablet as.
    pub fn set_share(on: bool, name: String) {
        with(|s| s.name = name);
        let method = if on { "startShare" } else { "stopShare" };
        dispatch(move |env, activity, _| {
            if let Err(e) = env.call_method(activity, method, "()V", &[]) {
                tracing::warn!("{method}: {e}");
                let _ = env.exception_clear();
                emit(json!({ "kind": "share", "state": "off", "error": "This tablet can't share its screen." }));
            }
        });
    }

    /// The page's Full screen button: Android's web view ignores the page's
    /// own request, so the activity hides the system bars instead.
    pub fn set_fullscreen(on: bool) {
        dispatch(move |env, activity, _| {
            if env.call_method(activity, "setFullscreen", "(Z)V", &[on.into()]).is_err() {
                let _ = env.exception_clear();
            }
        });
    }

    /// "Don't Allow" next to the pairing code.
    pub fn deny_pairing() {
        if let Some(p) = with(|s| s.host.as_ref().map(|h| h.pairing.clone())) {
            p.cancel();
        }
    }

    struct AndroidPlatform;

    struct SharePipeline;

    impl Pipeline for SharePipeline {
        fn request_idr(&self) {
            WANT_IDR.store(true, Ordering::Relaxed);
        }
        fn stop(&mut self) {
            with(|s| s.sink = None);
        }
    }

    impl Platform for AndroidPlatform {
        fn name(&self) -> &'static str {
            "Android (MediaProjection + MediaCodec)"
        }
        fn start_pipeline(&self, source: Source, _: &PipelineConfig, tx: Sender<EncodedFrame>) -> Result<Box<dyn Pipeline>> {
            if source != Source::Main {
                bail!("a tablet can only be mirrored");
            }
            if !with(|s| s.sharing) {
                bail!("this tablet is not sharing its screen");
            }
            with(|s| s.sink = Some(tx));
            WANT_IDR.store(true, Ordering::Relaxed);
            Ok(Box::new(SharePipeline))
        }
        fn create_virtual_display(&self, _: u32, _: u32, _: u32, _: &str, _: u32) -> Result<Option<Box<dyn VirtualDisplay>>> {
            Ok(None)
        }
        fn displays(&self) -> Result<Vec<DisplayInfo>> {
            let (width, height) = with(|s| s.size);
            Ok(vec![DisplayInfo { id: 1, x: 0, y: 0, width, height, main: true, builtin: true }])
        }
    }

    /// The host runs from the first share on (its port and pairing store stay);
    /// between shares, connecting is refused by `start_pipeline`.
    fn start_host(dir: PathBuf, size: (u32, u32), name: String) -> Result<mullion_host::Host> {
        mullion_host::start(mullion_host::HostOptions {
            platform: Arc::new(AndroidPlatform),
            addr: ([0, 0, 0, 0], 47100).into(),
            web_port: 0,
            host_name: if name.trim().is_empty() { "Android tablet".into() } else { name },
            kind: "tablet",
            session: mullion_host::session::SessionConfig {
                advertise_ips: mullion_host::lan_ips(),
                pipeline: PipelineConfig { width: size.0, height: size.1, fps: 30, require_hardware: false, ..Default::default() },
                extend: false,
                display_grace: Duration::from_secs(5),
                multi_session: true,
            },
            config: json!({ "extend": false }),
            log: Arc::new(mullion_host::timing::TimingLog::new(None)?),
            pairing_store: dir.join("paired.json"),
            notify: Box::new(|n| match n {
                Notice::Code { name, code } => emit(json!({ "kind": "pair_code", "name": name, "code": code })),
                Notice::Closed => emit(json!({ "kind": "pair_closed" })),
            }),
        })
    }

    fn now_ms() -> f64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0)
    }

    /// Share.kt: one encoder output buffer. Returns whether the next frame
    /// should be a key frame.
    #[no_mangle]
    pub extern "system" fn Java_dev_dioxus_main_MullionNative_onH264(
        env: JNIEnv,
        _: JClass,
        buf: JByteBuffer,
        offset: jint,
        size: jint,
        pts_us: jlong,
        flags: jint,
    ) -> jboolean {
        let (Ok(ptr), Ok(cap)) = (env.get_direct_buffer_address(&buf), env.get_direct_buffer_capacity(&buf)) else { return JNI_FALSE };
        let (off, len) = (offset.max(0) as usize, size.max(0) as usize);
        if ptr.is_null() || off + len > cap {
            return JNI_FALSE;
        }
        // SAFETY: a direct buffer of `cap` bytes, valid until Share.kt releases
        // it after this call returns; the range was checked above.
        let bytes = unsafe { std::slice::from_raw_parts(ptr.add(off), len) };
        with(|s| {
            if let Some(f) = s.asm.push(flags, bytes, pts_us, now_ms()) {
                if let Some(tx) = &s.sink {
                    if tx.send(f).is_err() {
                        s.sink = None;
                    }
                }
            }
        });
        if WANT_IDR.swap(false, Ordering::Relaxed) { JNI_TRUE } else { JNI_FALSE }
    }

    /// Share.kt: 1 sharing at `width`x`height`, 0 stopped, -1 the person
    /// declined, -2 capture failed. `dir` is the app's files folder.
    #[no_mangle]
    pub extern "system" fn Java_dev_dioxus_main_MullionNative_onShare(mut env: JNIEnv, _: JClass, state: jint, width: jint, height: jint, dir: JString) {
        let dir: String = env.get_string(&dir).map(|s| s.into()).unwrap_or_default();
        match state {
            1 => {
                let size = (width.max(0) as u32, height.max(0) as u32);
                let (started, name) = with(|s| {
                    s.sharing = true;
                    s.size = size;
                    s.asm = Assembler::default();
                    (s.host.is_some(), s.name.clone())
                });
                if !started {
                    match start_host(PathBuf::from(dir), size, name) {
                        Ok(h) => with(|s| s.host = Some(h)),
                        Err(e) => {
                            tracing::warn!("share: {e:#}");
                            emit(json!({ "kind": "share", "state": "off", "error": format!("Sharing could not start: {e}") }));
                            set_share(false, String::new());
                            return;
                        }
                    }
                }
                let address = with(|s| s.host.as_ref().map(|h| h.address.clone()).unwrap_or_default());
                emit(json!({ "kind": "share", "state": "on", "address": address, "width": size.0, "height": size.1 }));
            }
            _ => {
                let shared = with(|s| {
                    s.sharing = false;
                    s.sink = None;
                    s.host.as_ref().map(|h| h.shared.clone())
                });
                if let Some(sh) = shared {
                    sh.0.lock().unwrap_or_else(|e| e.into_inner()).end_all();
                }
                let error = match state {
                    -1 => Some("Sharing needs your OK. Tap Share this screen to ask again."),
                    s if s < 0 => Some("This tablet could not start sharing its screen."),
                    _ => None,
                };
                emit(json!({ "kind": "share", "state": "off", "error": error }));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPS_PPS: &[u8] = &[0, 0, 0, 1, 0x67, 0x42, 0xc0, 0x1f, 0, 0, 0, 1, 0x68, 0xce, 0x3c, 0x80];
    const IDR: &[u8] = &[0, 0, 0, 1, 0x65, 0x88, 0x84];
    const P: &[u8] = &[0, 0, 0, 1, 0x41, 0x9a, 0x02];

    #[test]
    fn keeps_sps_pps_and_puts_them_in_front_of_every_idr() {
        let mut a = Assembler::default();
        assert!(a.push(FLAG_CODEC_CONFIG, SPS_PPS, 0, 0.0).is_none());
        let k = a.push(FLAG_KEY_FRAME, IDR, 1_000_000, 0.0).unwrap();
        assert!(k.idr);
        assert_eq!(k.data, [SPS_PPS, IDR].concat());
        assert_eq!(k.rtp_time_90k, 90_000);
        let p = a.push(0, P, 1_033_333, 0.0).unwrap();
        assert!(!p.idr);
        assert_eq!(p.data, P);
        assert_eq!(p.rtp_time_90k, 92_999);
        assert_eq!((k.seq, p.seq), (1, 2));
        let au = mullion_proto::h264::inspect_annex_b(&k.data);
        assert!(au.has_sps && au.has_pps && au.is_idr, "{au:?}");
    }

    #[test]
    fn an_idr_that_already_carries_sps_is_left_alone() {
        let mut a = Assembler::default();
        a.push(FLAG_CODEC_CONFIG, SPS_PPS, 0, 0.0);
        let with_sps = [SPS_PPS, IDR].concat();
        assert_eq!(a.push(FLAG_KEY_FRAME | 8, &with_sps, 0, 0.0).unwrap().data, with_sps);
    }
}
