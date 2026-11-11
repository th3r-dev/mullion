//! Update check against GitHub Releases.
//!
//! Every 6 hours (and on "Check for Updates") the host asks the GitHub API for
//! the newest release of th3r-dev/mullion, pre-releases (betas) included, and
//! remembers it when it is newer than this build. "Install Update…" downloads
//! the release's `.pkg` to ~/Downloads and opens it in Installer, which asks
//! for the password and replaces the app (its preinstall script quits this one).
//!
//! HTTP goes through the system `curl` (TLS from the OS, no TLS crate in the
//! host). The download is trusted the same way the release page is: HTTPS
//! from github.com. The package is not notarized, so curl is used rather than
//! a browser download, which Gatekeeper would quarantine and refuse to open.

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use mullion_proto::version::{is_newer, VERSION};
use serde::Deserialize;
use tracing::{info, warn};

const RELEASES: &str = "https://api.github.com/repos/th3r-dev/mullion/releases?per_page=20";
const EVERY: Duration = Duration::from_secs(6 * 3600);

#[derive(Clone, Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    draft: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Clone, Debug, Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Clone, Debug)]
pub struct Release {
    /// "0.1.0-beta.8", without the tag's "v".
    pub version: String,
    pub page: String,
    /// The Mac installer, if the release has one.
    pub package: Option<(String, String)>, // (file name, URL)
}

#[derive(Clone, Debug, Default)]
pub enum Status {
    #[default]
    NotChecked,
    Checking,
    UpToDate,
    Available(Release),
    Downloading(Release),
    Failed(String),
}

#[derive(Clone, Default)]
pub struct Updater(Arc<Mutex<Status>>);

impl Updater {
    pub fn status(&self) -> Status {
        self.0.lock().unwrap().clone()
    }

    fn set(&self, s: Status) {
        *self.0.lock().unwrap() = s;
    }

    /// Check now, then every 6 hours, on a background thread.
    pub fn start(&self) {
        let me = self.clone();
        std::thread::Builder::new()
            .name("mullion-update".into())
            .spawn(move || loop {
                me.check();
                std::thread::sleep(EVERY);
            })
            .expect("spawn update thread");
    }

    /// Check once (blocking, up to ~15 s). A download in progress is left alone.
    pub fn check(&self) -> Status {
        if matches!(self.status(), Status::Downloading(_)) {
            return self.status();
        }
        self.set(Status::Checking);
        let s = match newest() {
            Ok(Some(r)) if is_newer(&r.version, VERSION) => {
                info!("update available: {} (this is {VERSION})", r.version);
                Status::Available(r)
            }
            Ok(_) => Status::UpToDate,
            Err(e) => {
                warn!("update check: {e:#}");
                Status::Failed(format!("{e:#}"))
            }
        };
        self.set(s.clone());
        s
    }

    /// Download the package and open it in Installer (blocking). Without a
    /// package for this OS, open the release page instead.
    pub fn install(&self, r: Release) -> Result<()> {
        let Some((name, url)) = r.package.clone() else {
            return open(&r.page);
        };
        self.set(Status::Downloading(r.clone()));
        let result = (|| {
            let dir = downloads_dir();
            std::fs::create_dir_all(&dir)?;
            let path = dir.join(&name);
            let part = dir.join(format!("{name}.part"));
            curl(&["-fL", "--retry", "2", "--max-time", "600", "-o"], Some(&part), &url)?;
            std::fs::rename(&part, &path)?;
            info!("update downloaded to {}", path.display());
            open(&path.to_string_lossy())
        })();
        self.set(match &result {
            Ok(()) => Status::Available(r),
            Err(e) => Status::Failed(format!("{e:#}")),
        });
        result
    }
}

/// The newest non-draft release (pre-releases included).
fn newest() -> Result<Option<Release>> {
    let body = curl(&["-fsSL", "--max-time", "15", "-H", "Accept: application/vnd.github+json"], None, RELEASES)?;
    let list: Vec<GhRelease> = serde_json::from_slice(&body).context("GitHub answered with something other than a release list")?;
    let mut best: Option<Release> = None;
    for r in list.into_iter().filter(|r| !r.draft) {
        let version = r.tag_name.trim_start_matches('v').to_string();
        if best.as_ref().is_some_and(|b| !is_newer(&version, &b.version)) {
            continue;
        }
        let package = r
            .assets
            .iter()
            .find(|a| a.name.ends_with(".pkg"))
            .map(|a| (a.name.clone(), a.browser_download_url.clone()));
        best = Some(Release { version, page: r.html_url, package });
    }
    Ok(best)
}

fn curl(args: &[&str], out: Option<&PathBuf>, url: &str) -> Result<Vec<u8>> {
    if !url.starts_with("https://") {
        bail!("refusing a non-HTTPS update URL: {url}");
    }
    let mut c = Command::new("curl");
    c.args(args);
    if let Some(p) = out {
        c.arg(p);
    }
    c.arg("-A").arg(format!("Mullion/{VERSION}")).arg(url);
    let o = c.output().context("run curl")?;
    if !o.status.success() {
        return Err(anyhow!("{} ({})", String::from_utf8_lossy(&o.stderr).trim(), url));
    }
    Ok(o.stdout)
}

fn downloads_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("Downloads")
}

fn open(target: &str) -> Result<()> {
    let ok = Command::new("open").arg(target).status().context("run open")?.success();
    if ok { Ok(()) } else { bail!("could not open {target}") }
}
