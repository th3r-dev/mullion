//! Mullion receiver app (SRS FR-4, FR-5, FR-9 receiver side). One Dioxus
//! codebase: `--features desktop` is the loopback build (WKWebView on macOS),
//! `--features mobile` is the Android tablet build (System WebView).
//!
//! The UI is the same HTML/CSS/JS the host serves at `/receiver`
//! (`assets/ui.html`, `ui.css`, `ui.js`, `layout.js`, `receiver.js`), so there is one place
//! where the interface is crafted. Rust owns the shell, lifecycle and the
//! platform pieces that arrive in later steps (touch injection, haptics).

// Windows release build: a window app, no console window behind it.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(any(target_os = "android", test))]
mod share;

use dioxus::document::eval;
use dioxus::prelude::*;

const NOBLE_JS: &str = include_str!("../assets/vendor/noble.js");
const PAIR_JS: &str = include_str!("../assets/pair.js");
const RECEIVER_JS: &str = include_str!("../assets/receiver.js");
const UI_JS: &str = include_str!("../assets/ui.js");
const LAYOUT_JS: &str = include_str!("../assets/layout.js");
const UI_CSS: &str = include_str!("../assets/ui.css");
const UI_HTML: &str = include_str!("../assets/ui.html");

/// Which app this is, for the page's update check (ui.js): it offers this
/// platform's file from the newest GitHub release.
const APP_PLATFORM: &str = if cfg!(target_os = "android") {
    "android"
} else if cfg!(windows) {
    "windows"
} else {
    "desktop"
};

/// Run `f` on its own thread and wait for it without blocking the UI.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = futures_channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.await.expect("the worker thread sends before it ends")
}

fn main() {
    // Desktop (the Windows viewer): our title, not "Dioxus App". The window
    // icon comes from the .exe's icon resource (build.rs). Closing the window
    // first hides it, so App can say bye to the host before it really closes.
    #[cfg(feature = "desktop")]
    {
        let cfg = dioxus::desktop::Config::new()
            .with_window(dioxus::desktop::WindowBuilder::new().with_title("Mullion"))
            .with_close_behaviour(dioxus::desktop::WindowCloseBehaviour::WindowHides);
        // Windows: one data folder for every release (see `windows_data_dir`).
        #[cfg(windows)]
        let cfg = match windows_data_dir() {
            Some(dir) => cfg.with_data_directory(dir),
            None => cfg,
        };
        dioxus::LaunchBuilder::desktop().with_cfg(cfg).launch(App);
    }
    #[cfg(not(feature = "desktop"))]
    dioxus::launch(App);
}

/// Where WebView2 keeps localStorage (recent hosts, device name, mode):
/// %LOCALAPPDATA%\Mullion. Dioxus's default is a folder named after the .exe,
/// and every release has a new file name, so each update started empty.
#[cfg(all(feature = "desktop", windows))]
fn windows_data_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| std::path::PathBuf::from(d).join("Mullion"))
}

#[component]
fn App() -> Element {
    // Boot the shared scripts once the shared markup is in the document.
    use_future(move || async move {
        let _ = eval(&format!("window.MULLION_VERSION = {:?}; window.MULLION_APP = {:?};", mullion_proto::version::VERSION, APP_PLATFORM)).await;
        let _ = eval(NOBLE_JS).await;
        let _ = eval(PAIR_JS).await;
        let _ = eval(RECEIVER_JS).await;
        let _ = eval(UI_JS).await;
        let _ = eval(LAYOUT_JS).await;
    });

    // window.mullionNative, the page's way to native code:
    // - nearby(): the Nearby sweep (mullion_proto::nearby), which a page
    //   cannot do itself without starving its own Connect.
    // - http(method, url, headers, body): a request to a Mullion host
    //   (mullion_proto::lanhttp). The Android page is https://dioxus.index.html
    //   and its WebView blocks every http:// request as mixed content, so the
    //   page cannot reach a host with fetch.
    // - share(on, name), pairDeny(), Android only: "Share this screen"
    //   (share.rs). Its state and pairing codes come back as a
    //   `mullion-native` event on window ({kind: 'share' | 'pair_code' | 'pair_closed'}).
    //   While Android pauses the app (its consent and permission dialogs,
    //   another app in front) those events wait until the page next sends
    //   something; poke() is that something.
    // - fullscreen(on), desktop only: the page's Full screen button fills only
    //   the web view (WebView2 does not resize the window), so this makes the
    //   window itself borderless full screen.
    // Replies are tagged, and slow work runs on its own thread, so one sweep
    // never holds up a request or the full screen toggle.
    use_future(move || async move {
        let fullscreen = if cfg!(feature = "desktop") { "fullscreen: (on) => dioxus.send({ fullscreen: !!on })," } else { "" };
        let share = if cfg!(target_os = "android") {
            "share: (on, name) => dioxus.send({ share: { on: !!on, name: String(name || '') } }), pairDeny: () => dioxus.send({ pairDeny: true }), poke: () => dioxus.send({ poke: true }),"
        } else {
            ""
        };
        let bridge = eval(&format!(
            "const nearby = [], pending = new Map(); let next = 1;
             window.mullionNative = {{ {fullscreen} {share}
               nearby: () => new Promise((r) => {{ if (nearby.push(r) === 1) dioxus.send({{ nearby: true }}); }}),
               http: (method, url, headers, body) => new Promise((r) => {{ const id = next++; pending.set(id, r); dioxus.send({{ http: {{ id, method, url, headers: headers || {{}}, body: body || '' }} }}); }}),
             }};
             for (;;) {{
               const m = await dioxus.recv();
               if (m.kind === 'nearby') nearby.splice(0).forEach((r) => r(m.found));
               else if (m.kind === 'http') {{ const r = pending.get(m.id); pending.delete(m.id); if (r) r(m); }}
               else window.dispatchEvent(new CustomEvent('mullion-native', {{ detail: m }}));
             }}"
        ));
        #[cfg(target_os = "android")]
        spawn(async move {
            use futures_util::StreamExt;
            let mut events = share::events();
            while let Some(v) = events.next().await {
                let _ = bridge.send(v);
            }
        });
        let mut rx = bridge;
        while let Ok(msg) = rx.recv::<serde_json::Value>().await {
            if msg["nearby"] == true {
                spawn(async move {
                    let found = blocking(|| mullion_proto::nearby::sweep(47100)).await;
                    let _ = bridge.send(serde_json::json!({ "kind": "nearby", "found": found }));
                });
            }
            if let Some(req) = msg.get("http").cloned() {
                spawn(async move {
                    let id = req["id"].clone();
                    let reply = blocking(move || {
                        let s = |k: &str| req[k].as_str().unwrap_or("").to_string();
                        let headers: Vec<(String, String)> = req["headers"]
                            .as_object()
                            .map(|m| m.iter().filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string()))).collect())
                            .unwrap_or_default();
                        mullion_proto::lanhttp::request(&s("method"), &s("url"), &headers, s("body").as_bytes(), std::time::Duration::from_secs(8))
                    })
                    .await;
                    let _ = bridge.send(match reply {
                        Ok(r) => serde_json::json!({ "kind": "http", "id": id, "status": r.status, "body": r.body }),
                        Err(e) => serde_json::json!({ "kind": "http", "id": id, "error": e }),
                    });
                });
            }
            #[cfg(target_os = "android")]
            {
                if let Some(on) = msg["share"]["on"].as_bool() {
                    share::set_share(on, msg["share"]["name"].as_str().unwrap_or("").to_string());
                }
                if msg["pairDeny"] == true {
                    share::deny_pairing();
                }
            }
            #[cfg(feature = "desktop")]
            if let Some(on) = msg["fullscreen"].as_bool() {
                dioxus::desktop::window().set_fullscreen(on);
            }
        }
    });

    #[cfg(feature = "desktop")]
    {
        // Closing the window: the page's pagehide does not fire when a native
        // window closes, so tell the host we are leaving here (its screen then
        // goes at once, not after the reconnect grace), give the message a
        // moment to go out, then close for real.
        let closing = dioxus::desktop::window();
        dioxus::desktop::use_wry_event_handler(move |event, _| {
            use dioxus::desktop::tao::event::{Event, WindowEvent};
            if let Event::WindowEvent { event: WindowEvent::CloseRequested, .. } = event {
                let win = closing.clone();
                spawn(async move {
                    let _ = eval("if (window.mullion) mullion.leave(); await new Promise((r) => setTimeout(r, 250));").await;
                    win.set_close_behavior(dioxus::desktop::WindowCloseBehaviour::WindowCloses);
                    win.close();
                });
            }
        });
    }

    rsx! {
        document::Meta { name: "color-scheme", content: "light dark" }
        document::Link { rel: "icon", r#type: "image/svg+xml", href: asset!("/assets/icons/favicon.svg") }
        document::Meta { name: "viewport", content: "width=device-width,initial-scale=1,viewport-fit=cover" }
        style { {UI_CSS} }
        div { dangerous_inner_html: UI_HTML }
    }
}
