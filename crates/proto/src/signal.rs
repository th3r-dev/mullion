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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    pub sdp: String,
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
