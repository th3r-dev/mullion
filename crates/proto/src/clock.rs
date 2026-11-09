//! NTP-style clock alignment over the data channel (TEST_PLAN step 3).
//!
//! The receiver sends `t0`, the host replies with its own `t1`, the receiver
//! notes `t2` on arrival. For the ping with the lowest round trip the offset
//! `host - receiver` is estimated as `t1 - (t0 + t2) / 2`. All values are
//! wall-clock milliseconds as `f64` so JS can use the same arithmetic.

use serde::{Deserialize, Serialize};

/// Ping sent receiver -> host.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Ping {
    pub t0: f64,
}

/// Pong sent host -> receiver.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Pong {
    pub t0: f64,
    pub t1: f64,
}

/// One completed round trip as seen by the receiver.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub t0: f64,
    pub t1: f64,
    pub t2: f64,
}

impl Sample {
    pub fn rtt(&self) -> f64 {
        self.t2 - self.t0
    }
    /// `host_clock - receiver_clock`, assuming a symmetric path.
    pub fn offset(&self) -> f64 {
        self.t1 - (self.t0 + self.t2) / 2.0
    }
}

/// Clock offset chosen from a set of samples: the one with the smallest round
/// trip, because that is the one with the least queueing on the path. Returns
/// `None` for an empty set or if any sample is non-finite / negative RTT.
pub fn best_offset(samples: &[Sample]) -> Option<Sample> {
    samples
        .iter()
        .copied()
        .filter(|s| s.rtt().is_finite() && s.rtt() >= 0.0)
        .min_by(|a, b| a.rtt().partial_cmp(&b.rtt()).unwrap())
}

/// Glass-to-glass latency of one frame, in ms, given the time the receiver
/// presented it (receiver clock) and the host paint time read from the strip
/// (host clock).
pub fn glass_to_glass(presented_receiver_ms: f64, painted_host_ms: f64, offset_host_minus_receiver: f64) -> f64 {
    (presented_receiver_ms + offset_host_minus_receiver) - painted_host_ms
}

/// p95 of a sample set (nearest-rank). `None` when empty.
pub fn p95(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut v: Vec<f64> = values.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let rank = ((0.95 * v.len() as f64).ceil() as usize).clamp(1, v.len());
    Some(v[rank - 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset_from_symmetric_path() {
        // Host clock is 1000 ms ahead; 10 ms each way.
        let s = Sample { t0: 0.0, t1: 1010.0, t2: 20.0 };
        assert_eq!(s.rtt(), 20.0);
        assert_eq!(s.offset(), 1000.0);
    }

    #[test]
    fn lowest_rtt_wins() {
        let samples = [
            Sample { t0: 0.0, t1: 1030.0, t2: 60.0 }, // queued, offset looks like 1000 anyway
            Sample { t0: 100.0, t1: 1105.0, t2: 110.0 }, // 10 ms rtt, offset 1000
            Sample { t0: 200.0, t1: 1240.0, t2: 260.0 },
        ];
        let b = best_offset(&samples).unwrap();
        assert_eq!(b.rtt(), 10.0);
        assert_eq!(b.offset(), 1000.0);
    }

    #[test]
    fn empty_or_garbage_gives_none() {
        assert!(best_offset(&[]).is_none());
        assert!(best_offset(&[Sample { t0: 10.0, t1: 0.0, t2: 5.0 }]).is_none());
        assert!(best_offset(&[Sample { t0: f64::NAN, t1: 0.0, t2: 5.0 }]).is_none());
    }

    #[test]
    fn glass_to_glass_uses_offset() {
        // Receiver presents at its 500; host painted at its 1470; host is +1000.
        assert_eq!(glass_to_glass(500.0, 1470.0, 1000.0), 30.0);
    }

    #[test]
    fn p95_nearest_rank() {
        let v: Vec<f64> = (1..=100).map(|x| x as f64).collect();
        assert_eq!(p95(&v), Some(95.0));
        assert_eq!(p95(&[7.0]), Some(7.0));
        assert_eq!(p95(&[]), None);
        assert_eq!(p95(&[f64::NAN]), None);
    }

    #[test]
    fn ping_pong_json_shape_is_stable_for_js() {
        let p: Pong = serde_json::from_str(r#"{"t0":1.5,"t1":2.5}"#).unwrap();
        assert_eq!(p, Pong { t0: 1.5, t1: 2.5 });
        assert_eq!(serde_json::to_string(&Ping { t0: 3.0 }).unwrap(), r#"{"t0":3.0}"#);
    }
}
