//! Mullion receiver app (SRS FR-4, FR-5, FR-9 receiver side). One Dioxus
//! codebase: `--features desktop` is the loopback build (WKWebView on macOS),
//! `--features mobile` is the Android tablet build (System WebView).
//!
//! The UI is the same HTML/CSS/JS the host serves at `/receiver`
//! (`assets/ui.html`, `ui.css`, `ui.js`, `receiver.js`), so there is one place
//! where the interface is crafted. Rust owns the shell, lifecycle and the
//! platform pieces that arrive in later steps (touch injection, haptics).

use dioxus::document::eval;
use dioxus::prelude::*;

const RECEIVER_JS: &str = include_str!("../assets/receiver.js");
const UI_JS: &str = include_str!("../assets/ui.js");
const UI_CSS: &str = include_str!("../assets/ui.css");
const UI_HTML: &str = include_str!("../assets/ui.html");

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    // Boot the shared scripts once the shared markup is in the document.
    use_future(move || async move {
        let _ = eval(RECEIVER_JS).await;
        let _ = eval(UI_JS).await;
    });

    rsx! {
        document::Meta { name: "color-scheme", content: "light dark" }
        document::Meta { name: "viewport", content: "width=device-width,initial-scale=1,viewport-fit=cover" }
        style { {UI_CSS} }
        div { dangerous_inner_html: UI_HTML }
    }
}
