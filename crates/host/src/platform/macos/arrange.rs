//! Keep the Mac usable when virtual displays come and go (public CG APIs).
//!
//! macOS sometimes makes a freshly created virtual display the *main* display
//! (menu bar, cursor home) or starts mirroring the built-in display to it.
//! After every virtual display is created we (1) un-mirror every display,
//! (2) put the built-in display at the origin so it stays main, and (3) line
//! the virtual displays up to its right, each in its own slot, so the
//! arrangement is predictable until the user moves them in Display Settings.

use anyhow::{bail, Result};
use objc2_core_graphics::{
    CGBeginDisplayConfiguration, CGCompleteDisplayConfiguration, CGConfigureDisplayMirrorOfDisplay, CGConfigureDisplayOrigin,
    CGConfigureOption, CGDirectDisplayID, CGDisplayBounds, CGDisplayConfigRef, CGDisplayIsBuiltin, CGGetOnlineDisplayList,
    CGMainDisplayID, kCGNullDirectDisplay,
};
use tracing::{info, warn};

/// What to do with our displays' positions.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Put the newest of ours to the right of everything else; displays the
    /// user already arranged stay where they are (after creating one).
    LineUp,
    /// Keep ours where the user put them; only make sure the built-in stays
    /// main and nothing mirrors (after removing one).
    Keep,
}

/// `virtual_ids` are ours, in creation order.
pub fn arrange(virtual_ids: &[CGDirectDisplayID], placement: Placement) -> Result<()> {
    let mut ids = [0u32; 32];
    let mut n = 0u32;
    let err = unsafe { CGGetOnlineDisplayList(32, ids.as_mut_ptr(), &mut n) };
    if err != objc2_core_graphics::CGError::Success {
        bail!("CGGetOnlineDisplayList: {err:?}");
    }
    let online = &ids[..n as usize];
    let builtin = online.iter().copied().find(|d| CGDisplayIsBuiltin(*d));
    // Physical displays that are not ours and not built-in (an external monitor).
    let others: Vec<u32> = online.iter().copied().filter(|d| Some(*d) != builtin && !virtual_ids.contains(d)).collect();
    let anchor = builtin.or_else(|| others.first().copied());
    let Some(anchor) = anchor else {
        warn!("no physical display found; leaving arrangement alone");
        return Ok(());
    };

    let mut cfg: CGDisplayConfigRef = std::ptr::null_mut();
    let err = unsafe { CGBeginDisplayConfiguration(&mut cfg) };
    if err != objc2_core_graphics::CGError::Success {
        bail!("CGBeginDisplayConfiguration: {err:?}");
    }
    unsafe {
        // (1) nothing mirrors anything.
        for d in online {
            CGConfigureDisplayMirrorOfDisplay(cfg, *d, kCGNullDirectDisplay);
        }
        // (2) the physical display is main, at the origin.
        CGConfigureDisplayOrigin(cfg, anchor, 0, 0);
        let mut x = CGDisplayBounds(anchor).size.width as i32;
        for d in &others {
            CGConfigureDisplayOrigin(cfg, *d, x, 0);
            x += CGDisplayBounds(*d).size.width as i32;
        }
        // (3) the newest of ours goes right of everything (LineUp); the rest untouched.
        if placement == Placement::LineUp {
            if let Some(newest) = virtual_ids.last() {
                let right_edge = online
                    .iter()
                    .filter(|d| *d != newest)
                    .map(|d| { let b = CGDisplayBounds(*d); (b.origin.x + b.size.width) as i32 })
                    .max()
                    .unwrap_or(x);
                CGConfigureDisplayOrigin(cfg, *newest, right_edge.max(x), 0);
            }
        }
        let err = CGCompleteDisplayConfiguration(cfg, CGConfigureOption::ForSession);
        if err != objc2_core_graphics::CGError::Success {
            bail!("CGCompleteDisplayConfiguration: {err:?}");
        }
    }
    info!("arranged displays: main={} virtual={:?}", CGMainDisplayID(), virtual_ids);
    Ok(())
}
