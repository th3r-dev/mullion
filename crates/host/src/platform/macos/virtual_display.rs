//! PLAN step 2 on macOS: a virtual display through `CGVirtualDisplay`.
//!
//! Private API (no public header), present and working on macOS 14 to 27 on
//! Apple Silicon; this is what every "tablet as second monitor" app on the Mac
//! uses. Risks are in PLAN.md: no App Store, may break on a macOS update. The
//! display disappears when the object is released, so there is no ghost
//! monitor after the host exits (step 2 gate).

use anyhow::{bail, Result};
use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject};
use objc2::msg_send;
use objc2_core_foundation::{CGPoint, CGSize};
use objc2_foundation::{NSArray, NSString};
use tracing::info;

pub struct VirtualDisplay {
    display: Retained<AnyObject>,
    pub display_id: u32,
    pub width: u32,
    pub height: u32,
}

// SAFETY: the object is only used to keep the display alive and read its id.
unsafe impl Send for VirtualDisplay {}
unsafe impl Sync for VirtualDisplay {}

fn class(name: &str) -> Result<&'static AnyClass> {
    AnyClass::get(&std::ffi::CString::new(name).unwrap())
        .ok_or_else(|| anyhow::anyhow!("{name} is not available on this macOS; run with --no-extend"))
}

impl VirtualDisplay {
    pub fn create(width: u32, height: u32, hz: u32, name: &str, serial: u32) -> Result<Self> {
        let desc_cls = class("CGVirtualDisplayDescriptor")?;
        let settings_cls = class("CGVirtualDisplaySettings")?;
        let mode_cls = class("CGVirtualDisplayMode")?;
        let display_cls = class("CGVirtualDisplay")?;

        // The private CGVirtualDisplay* setters take `unsigned int` (u32); a
        // usize argument is a type mismatch (objc2 debug builds panic on it).
        unsafe {
            let desc: Retained<AnyObject> = msg_send![msg_send![desc_cls, alloc], init];
            let ns_name = NSString::from_str(name);
            let _: () = msg_send![&*desc, setName: &*ns_name];
            let _: () = msg_send![&*desc, setMaxPixelsWide: width];
            let _: () = msg_send![&*desc, setMaxPixelsHigh: height];
            // 16:9 at ~100 dpi so macOS picks 1x scaling.
            let _: () = msg_send![&*desc, setSizeInMillimeters: CGSize::new(width as f64 * 0.2645, height as f64 * 0.2645)];
            let _: () = msg_send![&*desc, setVendorID: 0x4D4Cu32]; // "ML"
            let _: () = msg_send![&*desc, setProductID: 0x0001u32];
            let _: () = msg_send![&*desc, setSerialNum: serial];
            let _: () = msg_send![&*desc, setRedPrimary: CGPoint::new(0.64, 0.33)];
            let _: () = msg_send![&*desc, setGreenPrimary: CGPoint::new(0.30, 0.60)];
            let _: () = msg_send![&*desc, setBluePrimary: CGPoint::new(0.15, 0.06)];
            let _: () = msg_send![&*desc, setWhitePoint: CGPoint::new(0.3127, 0.3290)];
            let queue = DispatchQueue::main();
            let _: () = msg_send![&*desc, setQueue: &*queue];
            let term = RcBlock::new(|_a: *mut AnyObject, _b: *mut AnyObject| {
                tracing::warn!("virtual display terminated by the system");
            });
            let _: () = msg_send![&*desc, setTerminationHandler: &*term];

            let display: Retained<AnyObject> = msg_send![msg_send![display_cls, alloc], initWithDescriptor: &*desc];

            let mode: Retained<AnyObject> =
                msg_send![msg_send![mode_cls, alloc], initWithWidth: width, height: height, refreshRate: hz as f64];
            let modes: Retained<NSArray<AnyObject>> = NSArray::from_retained_slice(&[mode]);
            let settings: Retained<AnyObject> = msg_send![msg_send![settings_cls, alloc], init];
            let _: () = msg_send![&*settings, setHiDPI: 0u32];
            let _: () = msg_send![&*settings, setModes: &*modes];
            let ok: bool = msg_send![&*display, applySettings: &*settings];
            if !ok {
                bail!("CGVirtualDisplay applySettings failed");
            }
            let display_id: u32 = msg_send![&*display, displayID];
            if display_id == 0 {
                bail!("CGVirtualDisplay has no display id");
            }
            info!("virtual display '{name}' serial={serial} id={display_id} {width}x{height}@{hz}");
            Ok(Self { display, display_id, width, height })
        }
    }
}

impl Drop for VirtualDisplay {
    fn drop(&mut self) {
        info!("virtual display {} released", self.display_id);
        // Retained<AnyObject> release removes the display.
        let _ = &self.display;
    }
}

#[allow(dead_code)]
fn _assert_nsobject(_: &NSObject) {}
