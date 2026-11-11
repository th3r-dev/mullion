//! Runtime switches for debugging, read from `MULLION_*` environment variables.
//! Every switch has a CLI flag too; the environment overrides the flag's
//! default, and an explicit flag still wins. `/config` shows the live values.
//!
//! Booleans accept `1/0`, `true/false`, `yes/no`, `on/off` (any case).

use std::time::Duration;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Switches {
    /// MULLION_EXTEND: allow Extend mode (virtual displays, private API on macOS).
    pub extend: bool,
    /// MULLION_DISPLAY_GRACE_SECS: how long an orphaned screen lingers; 0 removes at once.
    pub display_grace_secs: u64,
    /// MULLION_ARRANGE: auto-arrange displays after create/remove.
    pub arrange: bool,
    /// MULLION_PROBE: burn the latency strip into frames. Off by default: it is a
    /// measurement aid (TEST_PLAN NFR-1) and covers the top of the picture.
    pub probe: bool,
    /// MULLION_REQUIRE_HW_ENCODER: FR-8, refuse a software encoder.
    pub require_hw_encoder: bool,
    /// MULLION_MULTI_SESSION: several receivers at once; false = newest replaces.
    pub multi_session: bool,
    /// MULLION_DEBUG: verbose logging for the host and str0m.
    pub debug: bool,
    /// MULLION_UPDATE_CHECK: Mullion.app asks GitHub for a newer release every
    /// 6 hours. Off: only "Check for Updates…" asks.
    pub update_check: bool,
    /// MULLION_PORT, MULLION_FPS, MULLION_BITRATE_KBPS, MULLION_WIDTH, MULLION_HEIGHT.
    pub port: u16,
    /// MULLION_WEB_PORT: also serve on this port, so the bare address
    /// (http://10.43.77.126) opens the receiver in any browser. 0 = off.
    pub web_port: u16,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub width: u32,
    pub height: u32,
}

impl Default for Switches {
    fn default() -> Self {
        Self {
            extend: true,
            display_grace_secs: 30,
            arrange: true,
            probe: false,
            require_hw_encoder: true,
            multi_session: true,
            debug: false,
            update_check: true,
            port: mullion_proto::signal::DEFAULT_PORT,
            web_port: 80,
            fps: 60,
            bitrate_kbps: 10_000,
            width: 1920,
            height: 1080,
        }
    }
}

pub fn parse_bool(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

impl Switches {
    /// Defaults overridden by whatever `MULLION_*` variables are set.
    pub fn from_env() -> Self {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let mut s = Self::default();
        let b = |k: &str, cur: bool| get(k).and_then(|v| parse_bool(&v)).unwrap_or(cur);
        s.extend = b("MULLION_EXTEND", s.extend);
        s.arrange = b("MULLION_ARRANGE", s.arrange);
        s.probe = b("MULLION_PROBE", s.probe);
        s.require_hw_encoder = b("MULLION_REQUIRE_HW_ENCODER", s.require_hw_encoder);
        s.multi_session = b("MULLION_MULTI_SESSION", s.multi_session);
        s.debug = b("MULLION_DEBUG", s.debug);
        s.update_check = b("MULLION_UPDATE_CHECK", s.update_check);
        let n = |k: &str| get(k).and_then(|v| v.trim().parse::<u64>().ok());
        if let Some(v) = n("MULLION_DISPLAY_GRACE_SECS") {
            s.display_grace_secs = v;
        }
        if let Some(v) = n("MULLION_PORT") {
            s.port = v as u16;
        }
        if let Some(v) = n("MULLION_WEB_PORT") {
            s.web_port = v as u16;
        }
        if let Some(v) = n("MULLION_FPS") {
            s.fps = v as u32;
        }
        if let Some(v) = n("MULLION_BITRATE_KBPS") {
            s.bitrate_kbps = v as u32;
        }
        if let Some(v) = n("MULLION_WIDTH") {
            s.width = v as u32;
        }
        if let Some(v) = n("MULLION_HEIGHT") {
            s.height = v as u32;
        }
        s
    }

    pub fn display_grace(&self) -> Duration {
        Duration::from_secs(self.display_grace_secs)
    }

    /// One line per switch, for the startup banner.
    pub fn banner(&self) -> String {
        format!(
            "extend={} grace={}s arrange={} probe={} require_hw={} multi_session={} debug={} {}x{}@{} {}kbps port={} web_port={}",
            self.extend, self.display_grace_secs, self.arrange, self.probe, self.require_hw_encoder, self.multi_session, self.debug,
            self.width, self.height, self.fps, self.bitrate_kbps, self.port, self.web_port
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn bools_accept_common_spellings() {
        for t in ["1", "true", "TRUE", "yes", "On"] {
            assert_eq!(parse_bool(t), Some(true), "{t}");
        }
        for f in ["0", "false", "no", "OFF"] {
            assert_eq!(parse_bool(f), Some(false), "{f}");
        }
        assert_eq!(parse_bool("maybe"), None);
    }

    #[test]
    fn env_overrides_defaults_and_ignores_garbage() {
        let env: HashMap<&str, &str> = [
            ("MULLION_EXTEND", "off"),
            ("MULLION_DISPLAY_GRACE_SECS", "0"),
            ("MULLION_PROBE", "nonsense"),
            ("MULLION_PORT", "5000"),
            ("MULLION_WEB_PORT", "0"),
            ("MULLION_FPS", "abc"),
        ]
        .into_iter()
        .collect();
        let s = Switches::from_lookup(|k| env.get(k).map(|v| v.to_string()));
        assert!(!s.extend);
        assert_eq!(s.display_grace_secs, 0);
        assert!(!s.probe, "garbage keeps the default");
        assert_eq!(s.port, 5000);
        assert_eq!(s.web_port, 0, "0 turns the web port off");
        assert_eq!(Switches::default().web_port, 80);
        assert_eq!(s.fps, 60);
        assert!(s.arrange && s.multi_session && s.require_hw_encoder && !s.debug);
    }
}
