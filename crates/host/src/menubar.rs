//! Mullion.app: the host as a menu bar app (macOS).
//!
//! When the host runs from inside an app bundle (or with `--menu-bar`), the
//! main thread runs AppKit instead of waiting on the session thread. There is
//! no Dock icon (LSUIElement); a status item shows the laptop-and-tablet glyph,
//! dimmed while no device is connected (checked every 2 s), and its menu is
//! rebuilt each time it opens:
//!
//! ```text
//! Install Update 0.1.0-beta.8…    (only when GitHub has a newer release)
//! ─────
//! Mullion on DAISY-MAC
//! Tablets connect to 10.43.77.126
//! Copy Address                     ⌘C
//! ─────
//! Connected
//!     SM-X710 · Extend
//! Mullion Windows PC leaves in 21 s
//! ─────
//! Connect to Another Computer…     (viewer.rs: watch another host here)
//! Arrange Screens…
//! Allow Screen Recording…          (only while it is missing)
//! Show Logs
//! ─────
//! Check for Updates…
//! Version 0.1.0-beta.7
//! Quit Mullion                     ⌘Q
//! ```
//!
//! Wording follows BRAND.md "Voice": plain, specific, device names.

use std::cell::OnceCell;
use std::path::PathBuf;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSApplicationActivationPolicy, NSImage, NSMenu, NSMenuDelegate,
    NSMenuItem, NSPasteboard, NSPasteboardTypeString, NSStatusBar, NSStatusItem, NSVariableStatusItemLength, NSWorkspace,
};
use objc2_core_graphics::{CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess};
use objc2_foundation::{NSData, NSObject, NSObjectProtocol, NSSize, NSString, NSTimer, NSURL};

use crate::session::DevicesSnapshot;
use crate::update::{Status, Updater};
use dispatch2::DispatchQueue;
use mullion_proto::version::VERSION;

const GLYPH: &[u8] = include_bytes!("../macos/MenuBarIcon.png");
const PRIVACY_SCREEN: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture";

/// What the menu shows and does; filled in by `main`.
pub struct MenuModel {
    pub host_name: String,
    /// What a tablet opens in a browser: the bare LAN address when the web
    /// port (80) is served, otherwise address:port.
    pub address: String,
    pub port: u16,
    pub logs_dir: PathBuf,
    pub devices: Box<dyn Fn() -> DevicesSnapshot>,
    /// Flush logs before the process exits.
    pub on_quit: Box<dyn Fn()>,
    pub updater: Updater,
    /// How many receivers are paired, and forgetting them all.
    pub paired: Box<dyn Fn() -> usize>,
    pub forget_paired: Box<dyn Fn()>,
}

pub struct Ivars {
    model: MenuModel,
    item: OnceCell<Retained<NSStatusItem>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop impl.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MullionMenuController"]
    #[ivars = Ivars]
    pub struct Controller;

    unsafe impl NSObjectProtocol for Controller {}

    unsafe impl NSMenuDelegate for Controller {
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &NSMenu) {
            self.rebuild(menu);
        }
    }

    impl Controller {
        /// Every 2 s: full-strength glyph while a device is connected, dimmed otherwise.
        #[unsafe(method(refreshGlyph:))]
        fn refresh_glyph(&self, _timer: Option<&AnyObject>) {
            self.sync_glyph();
        }

        #[unsafe(method(installUpdate:))]
        fn install_update(&self, _sender: Option<&AnyObject>) {
            if let Status::Available(r) = self.ivars().model.updater.status() {
                let updater = self.ivars().model.updater.clone();
                std::thread::spawn(move || {
                    if let Err(e) = updater.install(r) {
                        on_main(move |mtm| {
                            alert(mtm, "Couldn't download the update", &format!("{e:#}"), None);
                        });
                    }
                });
            }
        }

        #[unsafe(method(checkUpdates:))]
        fn check_updates(&self, _sender: Option<&AnyObject>) {
            let updater = self.ivars().model.updater.clone();
            std::thread::spawn(move || {
                let status = updater.check();
                on_main(move |mtm| match status {
                    Status::Available(r) => {
                        let detail = format!("You have {VERSION}. Install downloads the installer and opens it.");
                        if !alert_with(mtm, &format!("Mullion {} is available", r.version), &detail, "Later", Some("Install")) {
                            std::thread::spawn(move || {
                                if let Err(e) = updater.install(r) {
                                    on_main(move |mtm| {
                                        alert(mtm, "Couldn't download the update", &format!("{e:#}"), None);
                                    });
                                }
                            });
                        }
                    }
                    Status::UpToDate => {
                        alert(mtm, "Mullion is up to date", &format!("{VERSION} is the newest version."), None);
                    }
                    Status::Failed(e) => {
                        alert(mtm, "Couldn't check for updates", &e, None);
                    }
                    _ => {}
                });
            });
        }

        #[unsafe(method(copyAddress:))]
        fn copy_address(&self, _sender: Option<&AnyObject>) {
            copy_text(&self.ivars().model.address);
        }

        #[unsafe(method(connectToComputer:))]
        fn connect_to_computer(&self, _sender: Option<&AnyObject>) {
            crate::viewer::show(self.ivars().model.port, self.as_ref(), self.mtm());
        }

        #[unsafe(method(arrangeScreens:))]
        fn arrange_screens(&self, _sender: Option<&AnyObject>) {
            open_url(&format!("http://127.0.0.1:{}/receiver#screens", self.ivars().model.port));
        }

        #[unsafe(method(allowScreenRecording:))]
        fn allow_screen_recording(&self, _sender: Option<&AnyObject>) {
            if !CGRequestScreenCaptureAccess() {
                open_url(PRIVACY_SCREEN);
            }
        }

        #[unsafe(method(forgetPaired:))]
        fn forget_paired(&self, _sender: Option<&AnyObject>) {
            let m = &self.ivars().model;
            let n = (m.paired)();
            let msg = format!("Forget {}?", if n == 1 { "the paired device".to_string() } else { format!("all {n} paired devices") });
            if alert_with(
                self.mtm(),
                &msg,
                "Each one will need a new code from this Mac before it can show it again. Devices showing it now are disconnected.",
                "Forget",
                Some("Cancel"),
            ) {
                (m.forget_paired)();
            }
        }

        #[unsafe(method(showLogs:))]
        fn show_logs(&self, _sender: Option<&AnyObject>) {
            let dir = &self.ivars().model.logs_dir;
            let _ = std::fs::create_dir_all(dir);
            open_url(&format!("file://{}", dir.display()));
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: Option<&AnyObject>) {
            (self.ivars().model.on_quit)();
            NSApplication::sharedApplication(self.mtm()).terminate(None);
        }
    }
);

impl Controller {
    fn new(model: MenuModel, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars { model, item: OnceCell::new() });
        unsafe { msg_send![super(this), init] }
    }

    fn sync_glyph(&self) {
        let connected = (self.ivars().model.devices)().devices.iter().any(|d| d.connected);
        if let Some(button) = self.ivars().item.get().and_then(|i| i.button(self.mtm())) {
            button.setAppearsDisabled(!connected);
            let tip = if connected { "Mullion: showing this Mac" } else { "Mullion: no devices connected" };
            button.setToolTip(Some(&NSString::from_str(tip)));
        }
    }

    fn label(&self, menu: &NSMenu, text: &str, indent: isize) {
        // No action: AppKit shows it greyed out, as information.
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(self.mtm()), &NSString::from_str(text), None, &NSString::new())
        };
        item.setIndentationLevel(indent);
        menu.addItem(&item);
    }

    fn action(&self, menu: &NSMenu, text: &str, action: Sel, key: &str) {
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(self.mtm()),
                &NSString::from_str(text),
                Some(action),
                &NSString::from_str(key),
            )
        };
        let target: &AnyObject = self.as_ref();
        unsafe { item.setTarget(Some(target)) };
        menu.addItem(&item);
    }

    fn rebuild(&self, menu: &NSMenu) {
        let m = &self.ivars().model;
        let mtm = self.mtm();
        menu.removeAllItems();
        let update = m.updater.status();
        match &update {
            Status::Available(r) => {
                self.action(menu, &format!("Install Update {}…", r.version), sel!(installUpdate:), "");
                menu.addItem(&NSMenuItem::separatorItem(mtm));
            }
            Status::Downloading(r) => {
                self.label(menu, &format!("Downloading {}…", r.version), 0);
                menu.addItem(&NSMenuItem::separatorItem(mtm));
            }
            _ => {}
        }
        self.label(menu, &format!("Mullion on {}", m.host_name), 0);
        self.label(menu, &format!("Open {} on a tablet", m.address), 0);
        self.action(menu, "Copy Address", sel!(copyAddress:), "c");
        menu.addItem(&NSMenuItem::separatorItem(mtm));

        let snap = (m.devices)();
        let connected: Vec<_> = snap.devices.iter().filter(|d| d.connected).collect();
        if connected.is_empty() {
            self.label(menu, "No devices connected", 0);
        } else {
            self.label(menu, "Connected", 0);
            for d in connected {
                self.label(menu, &format!("{} · {:?}", d.name, d.mode), 1);
            }
        }
        for p in &snap.pending_removal {
            self.label(menu, &format!("{} leaves in {} s", p.screen_name, p.removes_in_secs), 0);
        }
        menu.addItem(&NSMenuItem::separatorItem(mtm));

        self.action(menu, "Connect to Another Computer…", sel!(connectToComputer:), "");
        self.action(menu, "Arrange Screens…", sel!(arrangeScreens:), "");
        if !CGPreflightScreenCaptureAccess() {
            self.action(menu, "Allow Screen Recording…", sel!(allowScreenRecording:), "");
        }
        let paired = (m.paired)();
        if paired > 0 {
            self.action(menu, &format!("Forget Paired Devices ({paired})…"), sel!(forgetPaired:), "");
        }
        self.action(menu, "Show Logs", sel!(showLogs:), "");
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        match update {
            Status::Checking => self.label(menu, "Checking for Updates…", 0),
            Status::Downloading(_) => {}
            _ => self.action(menu, "Check for Updates…", sel!(checkUpdates:), ""),
        }
        self.label(menu, &format!("Version {VERSION}"), 0);
        self.action(menu, "Quit Mullion", sel!(quit:), "q");
    }
}

fn open_url(url: &str) {
    if let Some(u) = NSURL::URLWithString(&NSString::from_str(url)) {
        NSWorkspace::sharedWorkspace().openURL(&u);
    }
}

fn copy_text(text: &str) {
    let pb = NSPasteboard::generalPasteboard();
    pb.clearContents();
    pb.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString });
}

/// Run `f` on the main thread (AppKit) from a worker thread.
fn on_main(f: impl FnOnce(MainThreadMarker) + Send + 'static) {
    DispatchQueue::main().exec_async(move || f(MainThreadMarker::new().expect("main queue runs on the main thread")));
}

/// Set while the pairing code is on screen, so closing it never closes some
/// other alert.
static PAIR_PROMPT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Show the code a receiver must type to pair. `on_refuse` runs when the
/// owner says no. Called from any thread.
pub fn show_pair_code(name: String, code: String, on_refuse: Box<dyn Fn() + Send>) {
    on_main(move |mtm| {
        close_pair_code_now(mtm);
        PAIR_PROMPT.store(true, std::sync::atomic::Ordering::SeqCst);
        let spaced = format!("{} {}", &code[..3], &code[3..]);
        let refused = !alert_with(
            mtm,
            &format!("{name} wants to show this Mac"),
            &format!("Enter this code on {name}:\n\n{spaced}\n\nThe code works for 2 minutes. If you are not connecting {name}, click Don't Allow."),
            "OK",
            Some("Don't Allow"),
        );
        if PAIR_PROMPT.swap(false, std::sync::atomic::Ordering::SeqCst) && refused {
            on_refuse();
        }
    });
}

/// Take the pairing code off screen (paired, expired or replaced).
pub fn close_pair_code() {
    on_main(close_pair_code_now);
}

fn close_pair_code_now(mtm: MainThreadMarker) {
    if PAIR_PROMPT.swap(false, std::sync::atomic::Ordering::SeqCst) {
        let app = NSApplication::sharedApplication(mtm);
        if app.modalWindow().is_some() {
            app.abortModal();
        }
    }
}

/// A modal message with an OK button (and optionally a second one). Returns
/// true when the first button was chosen.
pub fn alert(mtm: MainThreadMarker, message: &str, detail: &str, second: Option<&str>) -> bool {
    alert_with(mtm, message, detail, "OK", second)
}

fn alert_with(mtm: MainThreadMarker, message: &str, detail: &str, first: &str, second: Option<&str>) -> bool {
    let app = NSApplication::sharedApplication(mtm);
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    let a = NSAlert::new(mtm);
    a.setMessageText(&NSString::from_str(message));
    a.setInformativeText(&NSString::from_str(detail));
    a.addButtonWithTitle(&NSString::from_str(first));
    if let Some(s) = second {
        a.addButtonWithTitle(&NSString::from_str(s));
    }
    a.runModal() == NSAlertFirstButtonReturn
}

/// Show `message` and exit: for errors before the menu exists (port taken).
pub fn fatal(message: &str, detail: &str) -> ! {
    if let Some(mtm) = MainThreadMarker::new() {
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        alert(mtm, message, detail, None);
    }
    std::process::exit(1);
}

/// Run the menu bar app on the main thread. Never returns; Quit exits.
pub fn run(model: MenuModel, first_launch: bool) -> ! {
    let mtm = MainThreadMarker::new().expect("the menu bar runs on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let controller = Controller::new(model, mtm);
    let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    if let Some(button) = item.button(mtm) {
        let data = NSData::with_bytes(GLYPH);
        if let Some(img) = NSImage::initWithData(NSImage::alloc(), &data) {
            img.setSize(NSSize::new(18.0, 18.0));
            img.setTemplate(true);
            button.setImage(Some(&img));
        } else {
            button.setTitle(&NSString::from_str("Mullion"));
        }
    }
    let menu = NSMenu::new(mtm);
    menu.setDelegate(Some(ProtocolObject::from_ref(&*controller)));
    controller.rebuild(&menu);
    item.setMenu(Some(&menu));
    let _ = controller.ivars().item.set(item);
    controller.sync_glyph();
    unsafe {
        let target: &AnyObject = controller.as_ref();
        NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(2.0, target, sel!(refreshGlyph:), None, true);
    }

    // Ask for Screen Recording now, so it is ready before the first tablet.
    let can_record = CGPreflightScreenCaptureAccess() || CGRequestScreenCaptureAccess();
    if first_launch {
        let m = &controller.ivars().model;
        let detail = format!(
            "Mullion runs in the menu bar, next to the clock.\n\nOn a tablet on this Wi-Fi, open {} in the browser and tap {} under Nearby.{}",
            m.address,
            m.host_name,
            if can_record { "" } else { "\n\nMullion needs Screen Recording to show this Mac on your tablet. Allow it in System Settings, then open Mullion again." }
        );
        if !alert(mtm, "Mullion is on", &detail, Some("Copy Address")) {
            copy_text(&m.address);
        }
    }
    app.run();
    std::process::exit(0);
}
