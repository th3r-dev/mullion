//! Mullion receiver app (SRS FR-4, FR-5, FR-9 receiver side). One Dioxus
//! codebase: `--features desktop` is the loopback build (WKWebView on macOS),
//! `--features mobile` is the Android tablet build (System WebView).
//! The WebRTC work lives in `assets/receiver.js`, the same file the browser
//! smoke page uses; Rust owns the UI, state and the BRAND.md copy.

use dioxus::document::eval;
use dioxus::prelude::*;

const RECEIVER_JS: &str = include_str!("../assets/receiver.js");

fn main() {
    dioxus::launch(App);
}

#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Stats {
    status: String,
    mode: Option<String>,
    error: Option<String>,
    host_name: Option<String>,
    offset_ms: Option<f64>,
    rtt_ms: Option<f64>,
    presented: u64,
    decoded: u64,
    decode_fail: u64,
    dropped: u64,
    g2g_p50: Option<f64>,
    g2g_p95: Option<f64>,
    g2g_last: Option<f64>,
    caps: Option<serde_json::Value>,
    video_width: u32,
    video_height: u32,
}

#[component]
fn App() -> Element {
    let mut host = use_signal(|| String::new());
    let mut stats = use_signal(Stats::default);
    let mut full = use_signal(|| false);
    let mut mode = use_signal(|| "mirror".to_string());
    let mut name = use_signal(String::new);

    // Load receiver.js once and poll its stats 4x a second.
    use_future(move || async move {
        let _ = eval(RECEIVER_JS).await;
        if let Ok(v) = eval("return mullion.deviceName()").await {
            if let Some(n) = v.as_str() {
                name.set(n.to_string());
            }
        }
        loop {
            if let Ok(v) = eval("return JSON.stringify(window.mullion ? mullion.stats() : {})").await {
                if let Some(s) = v.as_str() {
                    if let Ok(parsed) = serde_json::from_str::<Stats>(s) {
                        stats.set(parsed);
                    }
                }
            }
            async_std_sleep(250).await;
        }
    });

    let connect = move |_| {
        let ip = host.read().trim().to_string();
        if ip.is_empty() {
            return;
        }
        let js = format!(
            "mullion.setDeviceName({n}); mullion.connect({ip}, document.getElementById('v'), {{mode: {m}, name: {n}}});",
            ip = serde_json::to_string(&ip).unwrap(),
            m = serde_json::to_string(&*mode.read()).unwrap(),
            n = serde_json::to_string(&*name.read()).unwrap()
        );
        let _ = eval(&js);
    };
    let disconnect = move |_| {
        let _ = eval("mullion.disconnect();");
    };

    let s = stats.read().clone();
    let f = |v: Option<f64>| v.map(|x| format!("{x:.1}")).unwrap_or_else(|| "–".into());
    let jbt = s
        .caps
        .as_ref()
        .map(|c| {
            if c["jitter_buffer_target_supported"].as_bool().unwrap_or(false) {
                format!("yes {}", c["jitter_buffer_target_ms"])
            } else {
                "no".into()
            }
        })
        .unwrap_or_else(|| "–".into());
    let status_line = match s.status.as_str() {
        "idle" => "No host yet. Enter the Mac's IP and tap Connect.".to_string(),
        "connecting" => format!("Connecting to {}", host.read()),
        "connected" => format!("Connected to {}", s.host_name.clone().unwrap_or_default()),
        "streaming" => format!("Showing {}", s.host_name.clone().unwrap_or_default()),
        "lost" => format!("Lost {}. Tap Connect to try again.", s.host_name.clone().unwrap_or_default()),
        "error" => s.error.clone().unwrap_or_else(|| "Can't connect.".into()),
        other => other.to_string(),
    };

    rsx! {
        style { {CSS} }
        div { class: if *full.read() { "root fs" } else { "root" },
            div { class: "bar",
                input {
                    placeholder: "Host IP, e.g. 10.43.77.126",
                    value: "{host}",
                    oninput: move |e| host.set(e.value()),
                    inputmode: "decimal",
                }
                input {
                    placeholder: "This device's name",
                    value: "{name}",
                    oninput: move |e| name.set(e.value()),
                    style: "width:150px",
                }
                select {
                    value: "{mode}",
                    onchange: move |e| mode.set(e.value()),
                    option { value: "mirror", "Mirror" }
                    option { value: "extend", "Extend" }
                }
                button { class: "primary", onclick: connect, "Connect" }
                button { onclick: disconnect, "Disconnect" }
                button { onclick: move |_| full.toggle(), if *full.read() { "Exit full screen" } else { "Full screen" } }
                span { class: "status", "{status_line}" }
            }
            video { id: "v", muted: true, autoplay: true, playsinline: true }
            div { class: "hud",
                "{s.mode.clone().unwrap_or_default()} {s.status} {s.video_width}x{s.video_height}  offset {f(s.offset_ms)} rtt {f(s.rtt_ms)}\n"
                "presented {s.presented} decoded {s.decoded} fail {s.decode_fail} dropped {s.dropped}\n"
                "g2g last {f(s.g2g_last)} p50 {f(s.g2g_p50)} p95 {f(s.g2g_p95)} ms\n"
                "jbt {jbt}"
            }
        }
    }
}

async fn async_std_sleep(ms: u64) {
    // Dioxus ships its own timer; avoid pulling tokio into the mobile build.
    let _ = eval(&format!("await new Promise(r => setTimeout(r, {ms}));")).await;
}

const CSS: &str = r#"
html,body{margin:0;background:#0F1113;color:#ECEDEE;font:15px system-ui;height:100%}
.root{height:100vh;display:flex;flex-direction:column}
.bar{display:flex;gap:8px;align-items:center;padding:8px 12px;background:#111418;flex-wrap:wrap}
.fs .bar{display:none}
input,select{font:inherit;padding:6px 8px;border-radius:6px;border:1px solid #2A2E33;background:#0F1113;color:#ECEDEE;font-variant-numeric:tabular-nums} input{width:200px}
button{font:inherit;padding:6px 12px;border-radius:6px;border:1px solid #2A2E33;background:#0F1113;color:#ECEDEE}
button.primary{background:#5B93FF;color:#0F1113;border:0;font-weight:600}
.status{font-size:13px;opacity:.9}
video{flex:1;min-height:0;width:100%;background:#000;object-fit:contain}
.hud{position:fixed;right:12px;bottom:12px;background:rgba(17,20,24,.86);padding:6px 10px;border-radius:6px;font-size:12px;font-variant-numeric:tabular-nums;white-space:pre}
"#;
