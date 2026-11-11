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

use dioxus::document::eval;
use dioxus::prelude::*;

const RECEIVER_JS: &str = include_str!("../assets/receiver.js");
const UI_JS: &str = include_str!("../assets/ui.js");
const LAYOUT_JS: &str = include_str!("../assets/layout.js");
const UI_CSS: &str = include_str!("../assets/ui.css");
const UI_HTML: &str = include_str!("../assets/ui.html");

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
        let _ = eval(&format!("window.MULLION_VERSION = {:?};", mullion_proto::version::VERSION)).await;
        let _ = eval(RECEIVER_JS).await;
        let _ = eval(UI_JS).await;
        let _ = eval(LAYOUT_JS).await;
    });

    // window.mullionNative, the page's way to native code:
    // - nearby(): the Nearby sweep (mullion_proto::nearby), which a page
    //   cannot do itself without starving its own Connect.
    // - fullscreen(on), desktop only: the page's Full screen button fills only
    //   the web view (WebView2 does not resize the window), so this makes the
    //   window itself borderless full screen.
    use_future(move || async move {
        let fullscreen = if cfg!(feature = "desktop") { "fullscreen: (on) => dioxus.send({ fullscreen: !!on })," } else { "" };
        let mut bridge = eval(&format!(
            "const waiting = [];
             window.mullionNative = {{ {fullscreen} nearby: () => new Promise((r) => {{ waiting.push(r); dioxus.send({{ nearby: true }}); }}) }};
             for (;;) {{ const found = await dioxus.recv(); waiting.splice(0).forEach((r) => r(found)); }}"
        ));
        while let Ok(msg) = bridge.recv::<serde_json::Value>().await {
            if msg["nearby"] == true {
                let (tx, rx) = futures_channel::oneshot::channel();
                std::thread::spawn(move || {
                    let _ = tx.send(mullion_proto::nearby::sweep(47100));
                });
                let _ = bridge.send(rx.await.unwrap_or_default());
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
