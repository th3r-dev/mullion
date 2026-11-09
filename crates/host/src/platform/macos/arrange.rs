//! Keep the Mac usable when virtual displays come and go (public CG APIs).
//!
//! macOS sometimes makes a freshly created virtual display the *main* display
//! (menu bar, cursor home) or starts mirroring the built-in display to it.
//! After every virtual display is created we (1) un-mirror every display,
//! (2) put the built-in display at the origin so it stays main, and (3) put
//! the newest virtual display right of everything. Displays already placed,
//! by the user in System Settings or on the layout map, stay where they are.
//!
//! [`list`] and [`set_origins`] back the layout map (`/screens`). Every
//! configuration transaction takes [`CONFIG`] so the session thread (create,
//! remove) and the signaling thread (layout map) never interleave.

use anyhow::{bail, Result};
use objc2_core_graphics::{
    CGBeginDisplayConfiguration, CGCompleteDisplayConfiguration, CGConfigureDisplayMirrorOfDisplay, CGConfigureDisplayOrigin,
    CGConfigureOption, CGDirectDisplayID, CGDisplayBounds, CGDisplayConfigRef, CGDisplayIsBuiltin, CGDisplayIsMain,
    CGDisplayMirrorsDisplay, CGGetOnlineDisplayList, CGMainDisplayID, kCGNullDirectDisplay,
};
use tracing::{info, warn};

use crate::platform::DisplayInfo;

static CONFIG: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn online() -> Result<Vec<CGDirectDisplayID>> {
    let mut ids = [0u32; 32];
    let mut n = 0u32;
    let err = unsafe { CGGetOnlineDisplayList(32, ids.as_mut_ptr(), &mut n) };
    if err != objc2_core_graphics::CGError::Success {
        bail!("CGGetOnlineDisplayList: {err:?}");
    }
    Ok(ids[..n as usize].to_vec())
}

/// Displays that show desktop space (mirrors of another display left out).
pub fn list() -> Result<Vec<DisplayInfo>> {
    Ok(online()?
        .into_iter()
        .filter(|d| CGDisplayMirrorsDisplay(*d) == kCGNullDirectDisplay)
        .map(|d| {
            let b = CGDisplayBounds(d);
            DisplayInfo {
                id: d,
                x: b.origin.x.round() as i32,
                y: b.origin.y.round() as i32,
                width: b.size.width.round() as u32,
                height: b.size.height.round() as u32,
                main: CGDisplayIsMain(d),
                builtin: CGDisplayIsBuiltin(d),
            }
        })
        .collect())
}

/// Move displays in one transaction; the OS nudges anything that would not
/// touch, so callers read the result back with [`list`].
pub fn set_origins(origins: &[(u32, i32, i32)]) -> Result<()> {
    let _g = CONFIG.lock().unwrap_or_else(|e| e.into_inner());
    let online = online()?;
    let mut cfg: CGDisplayConfigRef = std::ptr::null_mut();
    let err = unsafe { CGBeginDisplayConfiguration(&mut cfg) };
    if err != objc2_core_graphics::CGError::Success {
        bail!("CGBeginDisplayConfiguration: {err:?}");
    }
    unsafe {
        for &(id, x, y) in origins {
            if !online.contains(&id) {
                CGCompleteDisplayConfiguration(cfg, CGConfigureOption::ForSession);
                bail!("display {id} is not connected any more");
            }
            CGConfigureDisplayOrigin(cfg, id, x, y);
        }
        let err = CGCompleteDisplayConfiguration(cfg, CGConfigureOption::ForSession);
        if err != objc2_core_graphics::CGError::Success {
            bail!("CGCompleteDisplayConfiguration: {err:?}");
        }
    }
    info!("layout map moved displays: {origins:?}");
    Ok(())
}

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
    let _g = CONFIG.lock().unwrap_or_else(|e| e.into_inner());
    let ids = online()?;
    let online = &ids[..];
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
        // (2) the physical display is main, at the origin. Moving the main
        // display shifts the desktop space, so only do it when it is not there.
        let a = CGDisplayBounds(anchor);
        if !CGDisplayIsMain(anchor) || a.origin.x != 0.0 || a.origin.y != 0.0 {
            CGConfigureDisplayOrigin(cfg, anchor, 0, 0);
        }
        let x = a.size.width as i32;
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
