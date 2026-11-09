//! Host-side latency probe (TEST_PLAN "How latency is measured", SRS A-13).
//! Burns the `mullion_proto::probe` strip into the top rows of the captured
//! frame right before encoding, so the measurement needs nothing on screen and
//! measures exactly capture -> encode -> network -> decode -> glass.
//! Only the luma plane of a 420v buffer is touched (~100 KB per frame).

use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeightOfPlane,
    CVPixelBufferGetPixelFormatType, CVPixelBufferGetPlaneCount, CVPixelBufferGetWidthOfPlane, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};

use mullion_proto::probe;

const FMT_420V: u32 = u32::from_be_bytes(*b"420v");
const FMT_420F: u32 = u32::from_be_bytes(*b"420f");

/// Height of the strip as a fraction of the frame (matches the probe page's 12%).
const STRIP_FRAC: f64 = 0.12;

/// Draw the strip for `host_ms` into `buf`. Returns false if the buffer is not
/// a bi-planar 4:2:0 image we know how to write.
pub fn burn_strip(buf: &CVPixelBuffer, host_ms: u64) -> bool {
    let fmt = CVPixelBufferGetPixelFormatType(buf);
    if fmt != FMT_420V && fmt != FMT_420F {
        return false;
    }
    if CVPixelBufferGetPlaneCount(buf) < 2 {
        return false;
    }
    let bits = probe::encode(host_ms);
    let (white, black): (u8, u8) = if fmt == FMT_420V { (235, 16) } else { (255, 0) };
    unsafe {
        if CVPixelBufferLockBaseAddress(buf, CVPixelBufferLockFlags::empty()) != 0 {
            return false;
        }
        let w = CVPixelBufferGetWidthOfPlane(buf, 0);
        let h = CVPixelBufferGetHeightOfPlane(buf, 0);
        let stride = CVPixelBufferGetBytesPerRowOfPlane(buf, 0);
        let base = CVPixelBufferGetBaseAddressOfPlane(buf, 0) as *mut u8;
        if base.is_null() || w == 0 || h == 0 {
            CVPixelBufferUnlockBaseAddress(buf, CVPixelBufferLockFlags::empty());
            return false;
        }
        let strip_h = ((h as f64 * STRIP_FRAC) as usize).max(24).min(h);
        let mut row = vec![black; w];
        let bw = w as f64 / probe::BLOCKS as f64;
        for (i, b) in bits.iter().enumerate() {
            let x0 = (i as f64 * bw) as usize;
            let x1 = (((i + 1) as f64 * bw) as usize).min(w);
            let v = if *b == 1 { white } else { black };
            row[x0..x1].fill(v);
        }
        for y in 0..strip_h {
            std::ptr::copy_nonoverlapping(row.as_ptr(), base.add(y * stride), w);
        }
        // Chroma plane: neutral grey over the strip so colour fringes do not
        // move the luma threshold.
        let cw = CVPixelBufferGetWidthOfPlane(buf, 1);
        let ch = CVPixelBufferGetHeightOfPlane(buf, 1);
        let cstride = CVPixelBufferGetBytesPerRowOfPlane(buf, 1);
        let cbase = CVPixelBufferGetBaseAddressOfPlane(buf, 1) as *mut u8;
        if !cbase.is_null() {
            let rows = (strip_h / 2).min(ch);
            for y in 0..rows {
                std::ptr::write_bytes(cbase.add(y * cstride), 128, cw * 2);
            }
        }
        CVPixelBufferUnlockBaseAddress(buf, CVPixelBufferLockFlags::empty());
    }
    true
}
