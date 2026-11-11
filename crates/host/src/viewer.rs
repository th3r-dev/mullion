//! Connect to Another Computer… (macOS): a window that shows another Mullion
//! host on this Mac. It is the same receiver page a tablet's browser opens,
//! served by this host at `/receiver#viewer` and shown in a WKWebView, so the
//! Mac becomes a client the same way a browser would. In viewer mode the page
//! hides this host from Nearby and refuses to connect to it.
//!
//! While the window is open the app takes a Dock icon and a menu bar (Edit
//! menu, so ⌘V pastes a copied address). Closing the window blanks the page,
//! which ends the stream, and the app goes back to the menu bar only. The
//! window itself is kept and reused.

use std::cell::OnceCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject, Sel};
use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSEventModifierFlags, NSMenu, NSMenuItem,
    NSWindow, NSWindowDelegate, NSWindowStyleMask, NSWorkspace,
};
use objc2_foundation::{NSNotification, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSURLRequest, NSURL};
use objc2_web_kit::{
    WKAudiovisualMediaTypes, WKNavigationAction, WKUIDelegate, WKWebView, WKWebViewConfiguration, WKWindowFeatures,
};

struct Viewer {
    window: Retained<NSWindow>,
    web: Retained<WKWebView>,
    _delegate: Retained<Delegate>,
}

thread_local! {
    static VIEWER: OnceCell<Viewer> = const { OnceCell::new() };
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop impl.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MullionViewerDelegate"]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSWindowDelegate for Delegate {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _n: &NSNotification) {
            VIEWER.with(|v| {
                if let Some(v) = v.get() {
                    load(&v.web, "about:blank");
                }
            });
            NSApplication::sharedApplication(self.mtm()).setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        }
    }

    unsafe impl WKUIDelegate for Delegate {
        // target=_blank links (GitHub, Sponsor) open in the default browser.
        #[unsafe(method(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        fn create_web_view(
            &self,
            _web: &WKWebView,
            _config: &WKWebViewConfiguration,
            action: &WKNavigationAction,
            _features: &WKWindowFeatures,
        ) -> *mut WKWebView {
            if let Some(url) = unsafe { action.request() }.URL() {
                NSWorkspace::sharedWorkspace().openURL(&url);
            }
            std::ptr::null_mut()
        }
    }
);

fn load(web: &WKWebView, url: &str) {
    if let Some(u) = NSURL::URLWithString(&NSString::from_str(url)) {
        unsafe { web.loadRequest(&NSURLRequest::requestWithURL(&u)) };
    }
}

/// Open the viewer window, or bring it forward if it is already open.
/// `quit` receives `quit:` from the main menu (the menu bar controller, which
/// flushes logs before exiting).
pub fn show(port: u16, quit: &AnyObject, mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    if app.mainMenu().is_none() {
        app.setMainMenu(Some(&main_menu(quit, mtm)));
    }
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);

    VIEWER.with(|cell| {
        let v = cell.get_or_init(|| build(mtm));
        if !v.window.isVisible() {
            load(&v.web, &format!("http://127.0.0.1:{port}/receiver#viewer"));
        }
        v.window.makeKeyAndOrderFront(None);
    });
}

fn build(mtm: MainThreadMarker) -> Viewer {
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1024.0, 700.0));
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Resizable
        | NSWindowStyleMask::Miniaturizable;
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(NSWindow::alloc(mtm), rect, style, NSBackingStoreType::Buffered, false)
    };
    // Kept in VIEWER for the life of the app; AppKit must not release it on close.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str("Mullion"));
    window.center();
    window.setFrameAutosaveName(&NSString::from_str("MullionViewer"));

    let delegate: Retained<Delegate> = unsafe { msg_send![Delegate::alloc(mtm), init] };
    let web = unsafe {
        let config = WKWebViewConfiguration::new(mtm);
        // The stream's <video> is muted autoplay; the Full screen button uses the Fullscreen API.
        config.setMediaTypesRequiringUserActionForPlayback(WKAudiovisualMediaTypes::None);
        config.preferences().setElementFullscreenEnabled(true);
        let web = WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), rect, &config);
        web.setUIDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        web
    };
    window.setContentView(Some(&web));
    window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    Viewer { window, web, _delegate: delegate }
}

/// Mullion / Edit / Window, so the usual shortcuts work while the window is up.
fn main_menu(quit: &AnyObject, mtm: MainThreadMarker) -> Retained<NSMenu> {
    let bar = NSMenu::new(mtm);
    let cmd = NSEventModifierFlags::Command;
    // (title, action, key, modifiers, target). A nil target sends the action
    // to the first responder: the web view, or the window for Close.
    let sub = |title: &str, items: &[(&str, Sel, &str, NSEventModifierFlags, Option<&AnyObject>)]| {
        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
        for (t, action, key, mods, target) in items {
            let item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &NSString::from_str(t), Some(*action), &NSString::from_str(key))
            };
            item.setKeyEquivalentModifierMask(*mods);
            unsafe { item.setTarget(*target) };
            menu.addItem(&item);
        }
        let holder = NSMenuItem::new(mtm);
        holder.setSubmenu(Some(&menu));
        bar.addItem(&holder);
    };
    sub("Mullion", &[("Quit Mullion", sel!(quit:), "q", cmd, Some(quit))]);
    sub(
        "Edit",
        &[
            ("Undo", sel!(undo:), "z", cmd, None),
            ("Redo", sel!(redo:), "z", cmd | NSEventModifierFlags::Shift, None),
            ("Cut", sel!(cut:), "x", cmd, None),
            ("Copy", sel!(copy:), "c", cmd, None),
            ("Paste", sel!(paste:), "v", cmd, None),
            ("Select All", sel!(selectAll:), "a", cmd, None),
        ],
    );
    sub(
        "Window",
        &[
            ("Close", sel!(performClose:), "w", cmd, None),
            ("Minimize", sel!(performMiniaturize:), "m", cmd, None),
            ("Enter Full Screen", sel!(toggleFullScreen:), "f", cmd | NSEventModifierFlags::Control, None),
        ],
    );
    bar
}
