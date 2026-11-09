//! `mullion-host`: PoC 1. Captures the main display, encodes in hardware,
//! streams over WebRTC to one receiver, logs per-frame timing.

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
    /// Signaling port (SRS A-8).
    #[arg(long, default_value_t = mullion_proto::signal::DEFAULT_PORT)]
    port: u16,
    /// Address to listen on.
    #[arg(long, default_value = "0.0.0.0")]
    bind: IpAddr,
    /// Output width.
    #[arg(long, default_value_t = 1920)]
    width: u32,
    /// Output height.
    #[arg(long, default_value_t = 1080)]
    height: u32,
    /// Frames per second cap.
    #[arg(long, default_value_t = 60)]
    fps: u32,
    /// Target bitrate in kbit/s (FR-2: 8000 to 15000).
    #[arg(long, default_value_t = 10_000)]
    bitrate_kbps: u32,
    /// Allow a software encoder (FR-8 says no; this flag exists for diagnosing T-22).
    #[arg(long, default_value_t = false)]
    allow_software_encoder: bool,
    /// Timing log path (JSON lines). Default: logs/host-<unix>.jsonl
    #[arg(long)]
    log: Option<PathBuf>,
    /// Disable Extend mode (no virtual display; avoids the private CGVirtualDisplay API).
    #[arg(long, default_value_t = false)]
    no_extend: bool,
    /// Do not burn the latency probe strip into the frames.
    #[arg(long, default_value_t = false)]
    no_probe: bool,
    /// Only check that a hardware encoder can be opened, then exit (T-22).
    #[arg(long, default_value_t = false)]
    check_encoder: bool,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "mullion_host=info,str0m=warn".into()))
        .init();
    let args = Args::parse();

    if args.check_encoder {
        #[cfg(target_os = "macos")]
        {
            platform::macos::probe_hardware_encoder(args.width, args.height)?;
            println!("hardware H.264 encoder OK at {}x{}", args.width, args.height);
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
        width: args.width,
        height: args.height,
        fps: args.fps,
        bitrate_kbps: args.bitrate_kbps,
        require_hardware: !args.allow_software_encoder,
        probe_overlay: !args.no_probe,
        ..Default::default()
    };
    let (handle, session_thread) =
        session::start(session::SessionConfig { advertise_ips: ips.clone(), pipeline, extend: !args.no_extend }, platform, log.clone())?;

    let addr = SocketAddr::new(args.bind, args.port);
    let signaling = signaling::Signaling::bind(addr, Arc::new(session::SharedHandle(Mutex::new(handle))), host_name.clone())?;
    let lan = ips.first().map(|ip| ip.to_string()).unwrap_or_else(|| "127.0.0.1".into());
    info!("signaling on http://{lan}:{}/offer  probe page: http://{lan}:{}/probe", args.port, args.port);
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
