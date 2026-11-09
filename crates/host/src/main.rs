//! `mullion-host`: PoC 1. Captures the main display, encodes in hardware,
//! streams over WebRTC to one receiver, logs per-frame timing.

mod config;
mod platform;
mod probe_page;
mod session;
mod signaling;
mod timing;

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
}

fn main() -> Result<()> {
    // Env fills the defaults; explicit flags win.
    let args = Args::parse();
    let mut sw = config::Switches::from_env();
    if let Some(v) = args.port { sw.port = v; }
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
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| default_filter.into()))
        .init();
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

    let host_name = hostname::get().map(|h| h.to_string_lossy().into_owned()).unwrap_or_else(|_| "mullion-host".into());
    let log_path = args.log.clone().unwrap_or_else(|| {
        PathBuf::from("logs").join(format!("host-{}.jsonl", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()))
    });
    let log = Arc::new(timing::TimingLog::new(Some(&log_path))?);
    info!("timing log: {}", log_path.display());

    // Advertise every usable local IP so loopback and LAN receivers both connect.
    let mut ips: Vec<IpAddr> = vec![];
    if let Ok(ip) = local_ip_address::local_ip() {
        ips.push(ip);
    }
    if let Ok(list) = local_ip_address::list_afinet_netifas() {
        for (_, ip) in list {
            if ip.is_ipv4() && !ips.contains(&ip) && !ip.is_loopback() && !ip.is_unspecified() {
                ips.push(ip);
            }
        }
    }
    ips.push("127.0.0.1".parse().unwrap());

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
    let (handle, session_thread) =
        session::start(
            session::SessionConfig {
                advertise_ips: ips.clone(),
                pipeline,
                extend: sw.extend,
                display_grace: sw.display_grace(),
                multi_session: sw.multi_session,
            },
            platform,
            log.clone(),
        )?;

    let addr = SocketAddr::new(args.bind, sw.port);
    let signaling = signaling::Signaling::bind(addr, Arc::new(session::SharedHandle(Mutex::new(handle))), host_name.clone())?
        .with_config(serde_json::to_value(&sw)?);
    let lan = ips.first().map(|ip| ip.to_string()).unwrap_or_else(|| "127.0.0.1".into());
    info!("signaling on http://{lan}:{}/offer  receiver: http://{lan}:{}/receiver  config: /config", sw.port, sw.port);
    println!("Mullion host {host_name}: enter {lan} on the tablet.");

    let log_for_exit = log.clone();
    ctrlc_handler(move || {
        log_for_exit.flush();
        println!("\n{}", log_for_exit.summary());
        std::process::exit(0);
    });

    std::thread::spawn(move || signaling.run());
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
