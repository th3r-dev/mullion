//! `mullion-host`: PoC 1. Captures the main display, encodes in hardware,
//! streams over WebRTC to one receiver, logs per-frame timing.
//!
//! Two ways to run: from a terminal (`tools/run-host.sh`, logs in `logs/`,
//! settings from `.env`), or as Mullion.app, a menu bar app (logs in
//! `~/Library/Logs/Mullion`, settings from
//! `~/Library/Application Support/Mullion/mullion.env`).

#[cfg(target_os = "macos")]
mod menubar;
#[cfg(target_os = "macos")]
mod update;
#[cfg(target_os = "macos")]
mod viewer;

use mullion_host::{config, pairing, platform, session, timing};

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use clap::Parser;
use tracing::info;

#[derive(Parser, Debug)]
#[command(name = "mullion-host", about = "Mullion host, PoC 1: mirror this display to one tablet")]
struct Args {
    /// Signaling port (SRS A-8). Env: MULLION_PORT.
    #[arg(long)]
    port: Option<u16>,
    /// Address to listen on.
    #[arg(long, default_value = "0.0.0.0")]
    bind: IpAddr,
    /// Output width. Env: MULLION_WIDTH.
    #[arg(long)]
    width: Option<u32>,
    /// Output height. Env: MULLION_HEIGHT.
    #[arg(long)]
    height: Option<u32>,
    /// Frames per second cap. Env: MULLION_FPS.
    #[arg(long)]
    fps: Option<u32>,
    /// Target bitrate in kbit/s (FR-2: 8000 to 15000). Env: MULLION_BITRATE_KBPS.
    #[arg(long)]
    bitrate_kbps: Option<u32>,
    /// Allow a software encoder (FR-8 says no; for diagnosing T-22). Env: MULLION_REQUIRE_HW_ENCODER=false.
    #[arg(long, default_value_t = false)]
    allow_software_encoder: bool,
    /// Timing log path (JSON lines). Default: logs/host-<unix>.jsonl
    #[arg(long)]
    log: Option<PathBuf>,
    /// Disable Extend mode (no virtual display; avoids the private CGVirtualDisplay API). Env: MULLION_EXTEND=false.
    #[arg(long, default_value_t = false)]
    no_extend: bool,
    /// Burn the latency probe strip into the top of every frame (latency runs). Env: MULLION_PROBE=true.
    #[arg(long, default_value_t = false, conflicts_with = "no_probe")]
    probe: bool,
    /// Do not burn the latency probe strip into the frames (the default). Env: MULLION_PROBE=false.
    #[arg(long, default_value_t = false)]
    no_probe: bool,
    /// Seconds an orphaned virtual display lingers before removal. Env: MULLION_DISPLAY_GRACE_SECS.
    #[arg(long)]
    display_grace_secs: Option<u64>,
    /// Do not auto-arrange displays. Env: MULLION_ARRANGE=false.
    #[arg(long, default_value_t = false)]
    no_arrange: bool,
    /// One receiver at a time (newest replaces). Env: MULLION_MULTI_SESSION=false.
    #[arg(long, default_value_t = false)]
    single_session: bool,
    /// Verbose logging. Env: MULLION_DEBUG=true.
    #[arg(long, default_value_t = false)]
    debug: bool,
    /// Print the effective switches and exit.
    #[arg(long, default_value_t = false)]
    show_config: bool,
    /// Only check that a hardware encoder can be opened, then exit (T-22).
    #[arg(long, default_value_t = false)]
    check_encoder: bool,
    /// Also serve on this port so the bare address opens in a browser (0 = off). Env: MULLION_WEB_PORT.
    #[arg(long)]
    web_port: Option<u16>,
    /// Run as the menu bar app (automatic inside Mullion.app).
    #[arg(long, default_value_t = false)]
    menu_bar: bool,
}

/// The pairing store, for the code prompt's "Don't Allow" (the notifier is
/// built before the `Arc` exists).
static PAIRING: std::sync::OnceLock<Arc<pairing::Pairing>> = std::sync::OnceLock::new();

/// Where Mullion.app keeps its files.
struct AppPaths {
    logs: PathBuf,
    support: PathBuf,
}

impl AppPaths {
    fn new() -> Self {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| "/tmp".into()));
        Self { logs: home.join("Library/Logs/Mullion"), support: home.join("Library/Application Support/Mullion") }
    }
}

/// Inside `Something.app/Contents/MacOS/`?
fn in_app_bundle() -> bool {
    std::env::current_exe().map(|p| p.to_string_lossy().contains(".app/Contents/MacOS/")).unwrap_or(false)
}

/// `KEY=VALUE` lines (as in `.env`); variables already set win.
fn load_env_file(path: &std::path::Path) {
    let Ok(text) = std::fs::read_to_string(path) else { return };
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if let Some((k, v)) = line.split_once('=') {
            let (k, v) = (k.trim(), v.trim().trim_matches('"'));
            if k.starts_with("MULLION_") && std::env::var_os(k).is_none() {
                std::env::set_var(k, v);
            }
        }
    }
}

/// Keep the newest `keep` timing logs; they are a few MB each.
fn prune_logs(dir: &std::path::Path, keep: usize) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut logs: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().map(|n| n.to_string_lossy().starts_with("host-") && n.to_string_lossy().ends_with(".jsonl")).unwrap_or(false))
        .collect();
    logs.sort();
    let n = logs.len().saturating_sub(keep);
    for p in logs.into_iter().take(n) {
        let _ = std::fs::remove_file(p);
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    let app = (args.menu_bar || in_app_bundle()).then(AppPaths::new);
    if let Some(a) = &app {
        load_env_file(&a.support.join("mullion.env"));
    }
    let as_app = app.is_some();
    let result = run(args, app);
    #[cfg(target_os = "macos")]
    if as_app {
        if let Err(e) = &result {
            menubar::fatal("Mullion could not start", &format!("{e:#}"));
        }
    }
    let _ = as_app;
    result
}

fn run(args: Args, app: Option<AppPaths>) -> Result<()> {
    // Env fills the defaults; explicit flags win.
    let mut sw = config::Switches::from_env();
    if let Some(v) = args.port { sw.port = v; }
    if let Some(v) = args.web_port { sw.web_port = v; }
    if let Some(v) = args.width { sw.width = v; }
    if let Some(v) = args.height { sw.height = v; }
    if let Some(v) = args.fps { sw.fps = v; }
    if let Some(v) = args.bitrate_kbps { sw.bitrate_kbps = v; }
    if let Some(v) = args.display_grace_secs { sw.display_grace_secs = v; }
    if args.allow_software_encoder { sw.require_hw_encoder = false; }
    if args.no_extend { sw.extend = false; }
    if args.probe { sw.probe = true; }
    if args.no_probe { sw.probe = false; }
    if args.no_arrange { sw.arrange = false; }
    if args.single_session { sw.multi_session = false; }
    if args.debug { sw.debug = true; }
    let sw = sw;

    if args.show_config {
        println!("{}", serde_json::to_string_pretty(&sw)?);
        return Ok(());
    }

    let default_filter = if sw.debug { "mullion_host=debug,str0m=info" } else { "mullion_host=info,str0m=warn" };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| default_filter.into());
    match &app {
        // The app has no terminal: text log next to the timing logs.
        Some(a) => {
            std::fs::create_dir_all(&a.logs)?;
            prune_logs(&a.logs, 10);
            let file = std::fs::OpenOptions::new().create(true).append(true).open(a.logs.join("host.log"))?;
            tracing_subscriber::fmt().with_env_filter(filter).with_ansi(false).with_writer(Mutex::new(file)).init();
        }
        None => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
    info!("switches: {}", sw.banner());

    if args.check_encoder {
        #[cfg(target_os = "macos")]
        {
            platform::macos::probe_hardware_encoder(sw.width, sw.height)?;
            println!("hardware H.264 encoder OK at {}x{}", sw.width, sw.height);
            return Ok(());
        }
        #[cfg(not(target_os = "macos"))]
        anyhow::bail!("no encoder on this OS yet");
    }

    let platform: Arc<dyn platform::Platform> = Arc::from(platform::current()?);
    info!("platform: {}", platform.name());

    // macOS reports "Name.local"; people know the machine as "Name".
    let host_name = hostname::get()
        .map(|h| h.to_string_lossy().trim_end_matches(".local").to_string())
        .unwrap_or_else(|_| "mullion-host".into());
    let logs_dir = app.as_ref().map(|a| a.logs.clone()).unwrap_or_else(|| PathBuf::from("logs"));
    let log_path = args.log.clone().unwrap_or_else(|| {
        logs_dir.join(format!("host-{}.jsonl", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()))
    });
    let log = Arc::new(timing::TimingLog::new(Some(&log_path))?);
    info!("timing log: {}", log_path.display());

    let ips = mullion_host::lan_ips();

    let pipeline = platform::PipelineConfig {
        width: sw.width,
        height: sw.height,
        fps: sw.fps,
        bitrate_kbps: sw.bitrate_kbps,
        require_hardware: sw.require_hw_encoder,
        probe_overlay: sw.probe,
        ..Default::default()
    };
    platform::set_arrange_enabled(sw.arrange);

    // Pairing: receivers must be let in with a code shown here. Kept with the
    // app's other files, also when run from a terminal.
    let in_app = app.is_some();
    let started = mullion_host::start(mullion_host::HostOptions {
        platform,
        addr: SocketAddr::new(args.bind, sw.port),
        web_port: sw.web_port,
        host_name: host_name.clone(),
        kind: "Mac",
        session: session::SessionConfig {
            advertise_ips: ips,
            pipeline,
            extend: sw.extend,
            display_grace: sw.display_grace(),
            multi_session: sw.multi_session,
        },
        config: serde_json::to_value(&sw)?,
        log: log.clone(),
        pairing_store: AppPaths::new().support.join("paired.json"),
        notify: Box::new(move |n| match n {
            pairing::Notice::Code { name, code } => {
                println!("Pairing code for {name}: {} {}", &code[..3], &code[3..]);
                #[cfg(target_os = "macos")]
                if in_app {
                    let p = PAIRING.get().cloned();
                    menubar::show_pair_code(name, code, Box::new(move || if let Some(p) = &p { p.cancel() }));
                }
            }
            pairing::Notice::Closed => {
                #[cfg(target_os = "macos")]
                if in_app {
                    menubar::close_pair_code();
                }
            }
        }),
    });
    let mullion_host::Host { shared, pairing, address, session_thread } = match started {
        Ok(h) => h,
        #[cfg(target_os = "macos")]
        Err(e) if in_app && e.downcast_ref::<mullion_host::PortInUse>().is_some() => menubar::fatal(
            "Mullion is already running",
            &format!("Port {} is in use, most likely by another Mullion (the app or tools/run-host.sh). Quit that one, then open Mullion again.\n\n{e:#}", sw.port),
        ),
        Err(e) => return Err(e),
    };
    let _ = PAIRING.set(pairing.clone());
    info!("signaling on port {}  receiver: http://{address}/  config: /config", sw.port);
    println!("Mullion host {host_name}: open http://{address} on the tablet.");

    let log_for_exit = log.clone();
    ctrlc_handler(move || {
        log_for_exit.flush();
        println!("\n{}", log_for_exit.summary());
        std::process::exit(0);
    });

    #[cfg(target_os = "macos")]
    if let Some(a) = &app {
        use mullion_host::signaling::OfferHandler;
        let first_launch = !a.support.join("welcomed").exists();
        let _ = std::fs::create_dir_all(&a.support);
        let _ = std::fs::write(a.support.join("welcomed"), b"");
        let shared_for_menu = shared.clone();
        let log_for_quit = log.clone();
        menubar::run(
            menubar::MenuModel {
                host_name: host_name.clone(),
                address: address.clone(),
                port: sw.port,
                logs_dir: a.logs.clone(),
                devices: Box::new(move || shared_for_menu.devices()),
                paired: Box::new({
                    let p = pairing.clone();
                    move || p.paired_count()
                }),
                forget_paired: Box::new({
                    let p = pairing.clone();
                    let s = shared.clone();
                    move || {
                        p.forget_all();
                        s.0.lock().unwrap_or_else(|e| e.into_inner()).end_all();
                    }
                }),
                on_quit: Box::new(move || log_for_quit.flush()),
                updater: {
                    let u = update::Updater::default();
                    if sw.update_check {
                        u.start();
                    }
                    u
                },
            },
            first_launch,
        );
    }
    let _ = &app;
    let _ = session_thread.join();
    Ok(())
}

/// Minimal SIGINT handler without another dependency.
fn ctrlc_handler<F: Fn() + Send + 'static>(f: F) {
    static HANDLER: Mutex<Option<Box<dyn Fn() + Send>>> = Mutex::new(None);
    *HANDLER.lock().unwrap() = Some(Box::new(f));
    extern "C" fn on_sigint(_: i32) {
        if let Some(h) = HANDLER.lock().unwrap().as_ref() {
            h();
        }
        std::process::exit(130);
    }
    unsafe {
        libc_signal(2, on_sigint as extern "C" fn(i32) as usize);
    }
}

extern "C" {
    #[link_name = "signal"]
    fn libc_signal(sig: i32, handler: usize) -> usize;
}
