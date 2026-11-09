//! Keyframe request policy (FR-6, T-18). The receiver's PLIs are turned into
//! encoder IDR requests, but never more than `max_per_window` per window, so a
//! burst of PLIs during loss does not turn the stream into all-keyframes.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct IdrLimiter {
    window: Duration,
    max_per_window: usize,
    granted: VecDeque<Instant>,
}

impl IdrLimiter {
    pub fn new(window: Duration, max_per_window: usize) -> Self {
        Self { window, max_per_window, granted: VecDeque::new() }
    }

    /// FR-6 default: at most 2 IDRs per second.
    pub fn default_policy() -> Self {
        Self::new(Duration::from_secs(1), 2)
    }

    /// Record a PLI at `now`. Returns `true` when an IDR should be requested.
    pub fn request(&mut self, now: Instant) -> bool {
        while let Some(front) = self.granted.front() {
            if now.duration_since(*front) >= self.window {
                self.granted.pop_front();
            } else {
                break;
            }
        }
        if self.granted.len() < self.max_per_window {
            self.granted.push_back(now);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thirty_plis_in_one_second_yield_two_idrs() {
        let mut l = IdrLimiter::default_policy();
        let t0 = Instant::now();
        let granted = (0..30)
            .filter(|i| l.request(t0 + Duration::from_millis(i * 30)))
            .count();
        assert_eq!(granted, 2);
    }

    #[test]
    fn window_slides() {
        let mut l = IdrLimiter::default_policy();
        let t0 = Instant::now();
        assert!(l.request(t0));
        assert!(l.request(t0 + Duration::from_millis(100)));
        assert!(!l.request(t0 + Duration::from_millis(900)));
        // First grant ages out at t0 + 1 s.
        assert!(l.request(t0 + Duration::from_millis(1000)));
        assert!(!l.request(t0 + Duration::from_millis(1050)));
        assert!(l.request(t0 + Duration::from_millis(1100)));
    }

    #[test]
    fn first_request_always_granted() {
        let mut l = IdrLimiter::new(Duration::from_secs(5), 1);
        assert!(l.request(Instant::now()));
    }
}
