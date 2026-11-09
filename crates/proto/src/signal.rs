//! Signaling messages (FR-3) and limits (T-10). The receiver POSTs an SDP offer
//! as JSON to `/offer`; the host replies with the SDP answer. Data-channel
//! messages for the probe (FR-9, clock sync) are also defined here.

use serde::{Deserialize, Serialize};

/// Default signaling port (SRS A-8).
pub const DEFAULT_PORT: u16 = 47100;

/// Largest accepted request body. An SDP offer is a few KB; anything near a
/// megabyte is not an offer (T-10 expects 413 for 2 MB).
pub const MAX_BODY_BYTES: usize = 64 * 1024;

/// Default mDNS-less service name used in logs and the probe page title.
pub const SERVICE_NAME: &str = "mullion";

/// What a receiver wants to see (PLAN.md "Three modes per device"; Control is step 9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// A copy of the main display.
    #[default]
    Mirror,
    /// Its own part of the desktop, on a virtual display.
    Extend,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    pub sdp: String,
    #[serde(default)]
    pub mode: Mode,
    /// Stable per-device id chosen by the receiver (UUID kept in its storage).
    /// Lets a reconnecting device get its own virtual display back.
    #[serde(default)]
    pub device_id: String,
    /// Human name shown in Display Settings and the host UI (BRAND.md: name devices).
    #[serde(default)]
    pub device_name: String,
}

/// Longest device name accepted; longer ones are cut, not rejected.
pub const MAX_DEVICE_NAME: usize = 32;

/// Normalize a device name for display: trimmed, cut to [`MAX_DEVICE_NAME`],
/// control characters removed, a default when empty.
pub fn clean_device_name(name: &str, fallback: &str) -> String {
    let cleaned: String = name.chars().filter(|c| !c.is_control()).collect::<String>().trim().chars().take(MAX_DEVICE_NAME).collect();
    if cleaned.is_empty() {
        fallback.to_string()
    } else {
        cleaned
    }
}

/// Validate a device id: 1 to 64 chars of `[A-Za-z0-9_-]`. Anything else becomes `None`,
/// and the host treats the session as anonymous (fresh display each time).
pub fn valid_device_id(id: &str) -> Option<&str> {
    if (1..=64).contains(&id.len()) && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        Some(id)
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub sdp: String,
    /// Host name, shown in the receiver's UI (BRAND.md voice: name the device).
    pub host_name: String,
}

/// Validate an offer body. Returns the parsed offer or the HTTP status to reply.
pub fn parse_offer(body: &[u8]) -> Result<Offer, u16> {
    if body.len() > MAX_BODY_BYTES {
        return Err(413);
    }
    let offer: Offer = serde_json::from_slice(body).map_err(|_| 400u16)?;
    if !offer.sdp.starts_with("v=0") {
        return Err(400);
    }
    Ok(offer)
}

/// Messages on the `probe` data channel, JSON-encoded, `type`-tagged so the JS
/// side can switch on one field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProbeMessage {
    /// receiver -> host
    Ping { t0: f64 },
    /// host -> receiver
    Pong { t0: f64, t1: f64 },
    /// receiver -> host: one presented frame (FR-9, receiver side).
    Presented {
        /// host paint time read from the strip, ms, already unwrapped
        painted_host_ms: f64,
        /// receiver presentation time, ms, receiver clock
        presented_ms: f64,
        /// receiver's current offset estimate, host - receiver, ms
        offset_ms: f64,
    },
    /// receiver -> host: FR-5 outcome, sent once after connect.
    Capabilities {
        jitter_buffer_target_supported: bool,
        jitter_buffer_target_ms: Option<f64>,
        user_agent: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offer_ok() {
        let o = parse_offer(br#"{"sdp":"v=0\r\no=- 1 1 IN IP4 0.0.0.0\r\n"}"#).unwrap();
        assert!(o.sdp.starts_with("v=0"));
        assert_eq!(o.mode, Mode::Mirror);
        let o = parse_offer(br#"{"sdp":"v=0","mode":"extend"}"#).unwrap();
        assert_eq!(o.mode, Mode::Extend);
        assert_eq!(parse_offer(br#"{"sdp":"v=0","mode":"control"}"#).unwrap_err(), 400);
        let o = parse_offer(br#"{"sdp":"v=0","mode":"extend","device_id":"abc-123","device_name":"DAISY-TAB"}"#).unwrap();
        assert_eq!((o.device_id.as_str(), o.device_name.as_str()), ("abc-123", "DAISY-TAB"));
    }

    #[test]
    fn device_name_and_id_rules() {
        assert_eq!(clean_device_name("  DAISY-TAB\n", "Tablet"), "DAISY-TAB");
        assert_eq!(clean_device_name("", "Tablet"), "Tablet");
        assert_eq!(clean_device_name(&"x".repeat(100), "T").len(), MAX_DEVICE_NAME);
        assert_eq!(valid_device_id("7f3a-Bc_9"), Some("7f3a-Bc_9"));
        assert_eq!(valid_device_id(""), None);
        assert_eq!(valid_device_id("has space"), None);
        assert_eq!(valid_device_id(&"a".repeat(65)), None);
    }

    #[test]
    fn offer_malformed_is_400() {
        assert_eq!(parse_offer(b"not json").unwrap_err(), 400);
        assert_eq!(parse_offer(br#"{"sdp":"hello"}"#).unwrap_err(), 400);
        assert_eq!(parse_offer(br#"{"nope":1}"#).unwrap_err(), 400);
    }

    #[test]
    fn offer_too_big_is_413() {
        let big = vec![b'a'; 2 * 1024 * 1024];
        assert_eq!(parse_offer(&big).unwrap_err(), 413);
    }

    #[test]
    fn probe_messages_are_type_tagged() {
        let m: ProbeMessage = serde_json::from_str(r#"{"type":"ping","t0":12.5}"#).unwrap();
        assert_eq!(m, ProbeMessage::Ping { t0: 12.5 });
        let s = serde_json::to_string(&ProbeMessage::Pong { t0: 1.0, t1: 2.0 }).unwrap();
        assert_eq!(s, r#"{"type":"pong","t0":1.0,"t1":2.0}"#);
        let c: ProbeMessage = serde_json::from_str(
            r#"{"type":"capabilities","jitter_buffer_target_supported":false,"jitter_buffer_target_ms":null,"user_agent":"x"}"#,
        )
        .unwrap();
        assert!(matches!(c, ProbeMessage::Capabilities { jitter_buffer_target_supported: false, .. }));
    }
}
