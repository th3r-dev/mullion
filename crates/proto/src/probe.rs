//! The latency probe strip (TEST_PLAN "How latency is measured").
//!
//! The host's probe page paints a strip of blocks each frame. The receiver
//! samples the strip from the presented video frame and recovers the host
//! wall-clock time at which that frame was painted. The strip is:
//!
//! * 2 sync blocks: always white, then black (lets the reader find the strip and
//!   the per-block threshold even when the video is scaled or letterboxed);
//! * 32 data blocks: the low 32 bits of host wall-clock milliseconds, MSB first,
//!   white = 1, black = 0;
//! * 2 parity blocks: even parity of the low and high 16 bits;
//! * 2 tail blocks: always black, then white, so the reader can find the right
//!   edge of the strip when the video is pillarboxed.
//!
//! 38 blocks in total. Every reader-side rule (threshold, parity, sync) lives
//! here so the JS in the receiver and the Rust tests agree on one definition.

/// Number of blocks in the strip, including sync and parity.
pub const BLOCKS: usize = 38;
pub const SYNC_BLOCKS: usize = 2;
pub const DATA_BLOCKS: usize = 32;
pub const PARITY_BLOCKS: usize = 2;
pub const TAIL_BLOCKS: usize = 2;

/// Encode a host wall-clock millisecond value into the 38 block values (1 = white).
pub fn encode(ms: u64) -> [u8; BLOCKS] {
    let v = (ms & 0xFFFF_FFFF) as u32;
    let mut out = [0u8; BLOCKS];
    out[0] = 1;
    out[1] = 0;
    for i in 0..DATA_BLOCKS {
        out[SYNC_BLOCKS + i] = ((v >> (31 - i)) & 1) as u8;
    }
    out[SYNC_BLOCKS + DATA_BLOCKS] = ((v & 0xFFFF).count_ones() & 1) as u8;
    out[SYNC_BLOCKS + DATA_BLOCKS + 1] = ((v >> 16).count_ones() & 1) as u8;
    out[BLOCKS - 2] = 0;
    out[BLOCKS - 1] = 1;
    out
}

/// Why a strip could not be decoded.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum DecodeError {
    /// Fewer than 38 samples.
    TooShort,
    /// The sync blocks (white, black ... black, white) do not read as such.
    NoSync,
    /// Parity mismatch: the frame was probably sampled mid-scanout or scaled badly.
    Parity,
}

/// Decode a strip from per-block luma samples (0..=255). The sync blocks give the
/// threshold: anything closer to the white sample than to the black one is a 1.
pub fn decode(luma: &[u8]) -> Result<u32, DecodeError> {
    if luma.len() < BLOCKS {
        return Err(DecodeError::TooShort);
    }
    let white = (luma[0] as i32 + luma[BLOCKS - 1] as i32) / 2;
    let black = (luma[1] as i32 + luma[BLOCKS - 2] as i32) / 2;
    if white - black < 64 {
        return Err(DecodeError::NoSync);
    }
    let mid = (white + black) / 2;
    let bit = |i: usize| (luma[i] as i32 > mid) as u32;
    if bit(0) != 1 || bit(1) != 0 || bit(BLOCKS - 2) != 0 || bit(BLOCKS - 1) != 1 {
        return Err(DecodeError::NoSync);
    }
    let mut v: u32 = 0;
    for i in 0..DATA_BLOCKS {
        v = (v << 1) | bit(SYNC_BLOCKS + i);
    }
    let p_lo = bit(SYNC_BLOCKS + DATA_BLOCKS);
    let p_hi = bit(SYNC_BLOCKS + DATA_BLOCKS + 1);
    if p_lo != (v & 0xFFFF).count_ones() & 1 || p_hi != (v >> 16).count_ones() & 1 {
        return Err(DecodeError::Parity);
    }
    Ok(v)
}

/// Reconstruct a full millisecond timestamp from the 32-bit value in the strip,
/// given a reference time (the receiver's estimate of host time, in ms). Picks
/// the unwrapping closest to the reference, so the 49-day wrap is handled.
pub fn unwrap_ms(low32: u32, reference_ms: u64) -> u64 {
    let base = reference_ms & !0xFFFF_FFFF;
    let candidates = [
        base.wrapping_sub(1 << 32) | low32 as u64,
        base | low32 as u64,
        base.wrapping_add(1 << 32) | low32 as u64,
    ];
    candidates
        .into_iter()
        .min_by_key(|c| c.abs_diff(reference_ms))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_luma(bits: &[u8; BLOCKS]) -> Vec<u8> {
        bits.iter().map(|b| if *b == 1 { 240 } else { 16 }).collect()
    }

    #[test]
    fn roundtrip_exact() {
        for ms in [0u64, 1, 0xFFFF_FFFF, 0x1234_5678_9ABC, u64::MAX] {
            let strip = encode(ms);
            assert_eq!(strip[0], 1);
            assert_eq!(strip[1], 0);
            assert_eq!(strip[BLOCKS - 2], 0);
            assert_eq!(strip[BLOCKS - 1], 1);
            assert_eq!(decode(&to_luma(&strip)), Ok((ms & 0xFFFF_FFFF) as u32), "ms={ms}");
        }
    }

    #[test]
    fn roundtrip_with_compression_noise() {
        // H.264 at 8 Mbps smears edges; blocks come back as 60..200, not 16..240.
        let ms = 1_762_678_000_123u64;
        let strip = encode(ms);
        let luma: Vec<u8> = strip
            .iter()
            .enumerate()
            .map(|(i, b)| if *b == 1 { 200 - (i % 5) as u8 * 7 } else { 60 + (i % 3) as u8 * 9 })
            .collect();
        assert_eq!(decode(&luma), Ok((ms & 0xFFFF_FFFF) as u32));
    }

    #[test]
    fn too_short_and_no_sync() {
        assert_eq!(decode(&[0u8; 10]), Err(DecodeError::TooShort));
        let flat = vec![128u8; BLOCKS];
        assert_eq!(decode(&flat), Err(DecodeError::NoSync));
        // Right marker missing (e.g. the sampler ran past the content edge).
        let mut luma = to_luma(&encode(5));
        luma[BLOCKS - 1] = 16;
        assert_eq!(decode(&luma), Err(DecodeError::NoSync));
    }

    #[test]
    fn single_bit_flip_is_caught() {
        let strip = encode(0xDEAD_BEEF);
        let mut luma = to_luma(&strip);
        luma[SYNC_BLOCKS + 7] = 255 - luma[SYNC_BLOCKS + 7];
        assert_eq!(decode(&luma), Err(DecodeError::Parity));
    }

    #[test]
    fn unwrap_picks_nearest_epoch() {
        let now = 1_762_678_000_123u64;
        let low = (now & 0xFFFF_FFFF) as u32;
        assert_eq!(unwrap_ms(low, now), now);
        assert_eq!(unwrap_ms(low, now + 5_000), now);
        assert_eq!(unwrap_ms(low, now - 5_000), now);
        // Just past a wrap boundary: the reference sits in the next epoch.
        let before_wrap = (1u64 << 32) * 411 - 10;
        let low = (before_wrap & 0xFFFF_FFFF) as u32;
        assert_eq!(unwrap_ms(low, before_wrap + 30), before_wrap);
    }
}
