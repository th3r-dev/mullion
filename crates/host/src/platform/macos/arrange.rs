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

/// Which displays are ours, in creation order.
pub fn arrange(virtual_ids: &[CGDirectDisplayID]) -> Result<()> {
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
        // (3) our displays to the right, in order, only if still at a default spot.
        for d in virtual_ids {
            if online.contains(d) {
                CGConfigureDisplayOrigin(cfg, *d, x, 0);
                x += CGDisplayBounds(*d).size.width.max(1.0) as i32;
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
