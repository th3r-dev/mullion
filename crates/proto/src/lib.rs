//! Mullion protocol crate: everything that is pure logic and shared between the
//! host and the receiver. No OS calls, all unit-tested.

pub mod clock;
pub mod h264;
pub mod keyframe;
pub mod layout;
pub mod probe;
pub mod signal;
pub mod version;
