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
    dioxus::launch(App);
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

    rsx! {
        document::Meta { name: "color-scheme", content: "light dark" }
        document::Link { rel: "icon", r#type: "image/svg+xml", href: asset!("/assets/icons/favicon.svg") }
        document::Meta { name: "viewport", content: "width=device-width,initial-scale=1,viewport-fit=cover" }
        style { {UI_CSS} }
        div { dangerous_inner_html: UI_HTML }
    }
}
