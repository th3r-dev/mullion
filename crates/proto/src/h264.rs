//! Minimal H.264 bitstream inspection: enough to check FR-2 (profile, level,
//! no B-frames, IDR on request) in unit tests, and to convert the AVCC
//! length-prefixed output of VideoToolbox into the Annex B stream a WebRTC
//! packetizer takes. Not a decoder.

/// NAL unit types we care about (H.264 table 7-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NalType {
    NonIdrSlice,
    IdrSlice,
    Sei,
    Sps,
    Pps,
    Aud,
    Other(u8),
}

impl NalType {
    pub fn from_header(b: u8) -> Self {
        match b & 0x1F {
            1 => NalType::NonIdrSlice,
            5 => NalType::IdrSlice,
            6 => NalType::Sei,
            7 => NalType::Sps,
            8 => NalType::Pps,
            9 => NalType::Aud,
            t => NalType::Other(t),
        }
    }
}

/// Split an Annex B stream (3- or 4-byte start codes) into NAL unit payloads.
pub fn split_annex_b(data: &[u8]) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    let mut i = 0;
    let mut start: Option<usize> = None;
    while i + 2 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            if let Some(s) = start {
                let mut end = i;
                // A 4-byte start code has a leading zero that belongs to the code.
                if end > s && data[end - 1] == 0 {
                    end -= 1;
                }
                if end > s {
                    nals.push(&data[s..end]);
                }
            }
            i += 3;
            start = Some(i);
        } else {
            i += 1;
        }
    }
    if let Some(s) = start {
        if s < data.len() {
            nals.push(&data[s..]);
        }
    }
    nals
}

/// Split an AVCC (length-prefixed) sample into NAL unit payloads.
/// `len_size` is 1, 2 or 4 (VideoToolbox uses 4). Stops at the first bad length.
pub fn split_avcc(data: &[u8], len_size: usize) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    let mut i = 0;
    while i + len_size <= data.len() {
        let mut n: usize = 0;
        for k in 0..len_size {
            n = (n << 8) | data[i + k] as usize;
        }
        i += len_size;
        if n == 0 || i + n > data.len() {
            break;
        }
        nals.push(&data[i..i + n]);
        i += n;
    }
    nals
}

/// Build an Annex B access unit from NAL payloads, 4-byte start codes.
pub fn to_annex_b<'a>(nals: impl IntoIterator<Item = &'a [u8]>) -> Vec<u8> {
    let mut out = Vec::new();
    for n in nals {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(n);
    }
    out
}

/// Remove emulation-prevention bytes (00 00 03 -> 00 00) from an RBSP.
fn unescape(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &b in nal {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        out.push(b);
        if b == 0 {
            zeros += 1;
        } else {
            zeros = 0;
        }
    }
    out
}

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    fn bit(&mut self) -> Option<u32> {
        let byte = *self.data.get(self.pos / 8)?;
        let b = (byte >> (7 - (self.pos % 8))) & 1;
        self.pos += 1;
        Some(b as u32)
    }
    fn bits(&mut self, n: u32) -> Option<u32> {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | self.bit()?;
        }
        Some(v)
    }
    /// Unsigned Exp-Golomb.
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        let rest = self.bits(zeros)?;
        Some((1u32 << zeros) - 1 + rest)
    }
}

/// Fields of an SPS relevant to FR-2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sps {
    pub profile_idc: u8,
    /// constraint_set0..5 flags as bits 7..2 of the byte.
    pub constraint_flags: u8,
    pub level_idc: u8,
}

impl Sps {
    /// Constrained Baseline is profile 66 with constraint_set1_flag set
    /// (H.264 A.2.1.1). Many encoders also use 77/100 with set1; WebRTC's
    /// `42e0xx` means exactly 66 + set1.
    pub fn is_constrained_baseline(&self) -> bool {
        self.profile_idc == 66 && (self.constraint_flags & 0x40) != 0
    }
    /// The `profile-level-id` string as used in SDP.
    pub fn profile_level_id(&self) -> String {
        format!("{:02x}{:02x}{:02x}", self.profile_idc, self.constraint_flags, self.level_idc)
    }
}

/// Parse the first three bytes of an SPS NAL (after the header byte).
pub fn parse_sps(nal: &[u8]) -> Option<Sps> {
    if nal.len() < 4 || NalType::from_header(nal[0]) != NalType::Sps {
        return None;
    }
    Some(Sps { profile_idc: nal[1], constraint_flags: nal[2], level_idc: nal[3] })
}

/// Slice types from the slice header (H.264 table 7-6), mod 5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliceType {
    P,
    B,
    I,
    Sp,
    Si,
}

/// Parse `slice_type` from a coded slice NAL (types 1 and 5).
pub fn parse_slice_type(nal: &[u8]) -> Option<SliceType> {
    match NalType::from_header(*nal.first()?) {
        NalType::NonIdrSlice | NalType::IdrSlice => {}
        _ => return None,
    }
    let rbsp = unescape(&nal[1..]);
    let mut r = BitReader::new(&rbsp);
    let _first_mb = r.ue()?;
    let t = r.ue()?;
    Some(match t % 5 {
        0 => SliceType::P,
        1 => SliceType::B,
        2 => SliceType::I,
        3 => SliceType::Sp,
        _ => SliceType::Si,
    })
}

/// Summary of one access unit in Annex B form, for tests and the timing log.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AccessUnit {
    pub has_sps: bool,
    pub has_pps: bool,
    pub is_idr: bool,
    pub has_b_slice: bool,
    pub sps: Option<Sps>,
}

pub fn inspect_annex_b(data: &[u8]) -> AccessUnit {
    let mut au = AccessUnit::default();
    for nal in split_annex_b(data) {
        match NalType::from_header(nal[0]) {
            NalType::Sps => {
                au.has_sps = true;
                au.sps = parse_sps(nal);
            }
            NalType::Pps => au.has_pps = true,
            NalType::IdrSlice => au.is_idr = true,
            NalType::NonIdrSlice => {
                if parse_slice_type(nal) == Some(SliceType::B) {
                    au.has_b_slice = true;
                }
            }
            _ => {}
        }
    }
    au
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real SPS from x264, Constrained Baseline level 3.1 (42e01f).
    const SPS_42E01F: &[u8] = &[0x67, 0x42, 0xe0, 0x1f, 0xda, 0x01, 0x40, 0x16, 0xec, 0x04, 0x40, 0x00, 0x00, 0x03, 0x00, 0x40, 0x00, 0x00, 0x0f, 0x03, 0xc5, 0x8b, 0x92];
    const PPS: &[u8] = &[0x68, 0xce, 0x3c, 0x80];

    #[test]
    fn splits_three_and_four_byte_start_codes() {
        let mut s = vec![0, 0, 0, 1];
        s.extend_from_slice(SPS_42E01F);
        s.extend_from_slice(&[0, 0, 1]);
        s.extend_from_slice(PPS);
        s.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88, 0x84, 0x00]);
        let nals = split_annex_b(&s);
        assert_eq!(nals.len(), 3);
        assert_eq!(NalType::from_header(nals[0][0]), NalType::Sps);
        assert_eq!(NalType::from_header(nals[1][0]), NalType::Pps);
        assert_eq!(NalType::from_header(nals[2][0]), NalType::IdrSlice);
        assert_eq!(nals[1], PPS);
    }

    #[test]
    fn avcc_to_annex_b_roundtrip() {
        let mut avcc = Vec::new();
        for n in [SPS_42E01F, PPS] {
            avcc.extend_from_slice(&(n.len() as u32).to_be_bytes());
            avcc.extend_from_slice(n);
        }
        let nals = split_avcc(&avcc, 4);
        assert_eq!(nals, vec![SPS_42E01F, PPS]);
        let annexb = to_annex_b(nals);
        assert_eq!(split_annex_b(&annexb), vec![SPS_42E01F, PPS]);
    }

    #[test]
    fn avcc_bad_length_stops_cleanly() {
        let bad = [0, 0, 0, 200, 1, 2, 3];
        assert!(split_avcc(&bad, 4).is_empty());
        let zero = [0, 0, 0, 0, 1, 2, 3];
        assert!(split_avcc(&zero, 4).is_empty());
    }

    #[test]
    fn sps_fields_and_profile_level_id() {
        let sps = parse_sps(SPS_42E01F).unwrap();
        assert_eq!(sps, Sps { profile_idc: 66, constraint_flags: 0xe0, level_idc: 31 });
        assert!(sps.is_constrained_baseline());
        assert_eq!(sps.profile_level_id(), "42e01f");
        // High profile is not constrained baseline even with set1.
        let hi = Sps { profile_idc: 100, constraint_flags: 0x40, level_idc: 42 };
        assert!(!hi.is_constrained_baseline());
        assert_eq!(hi.profile_level_id(), "64402a");
        assert!(parse_sps(PPS).is_none());
    }

    #[test]
    fn slice_type_parses_i_p_b() {
        // Hand-built slice headers: first_mb_in_slice=0 (ue '1'), then slice_type ue.
        // slice_type 7 (I) = ue '0001000'; 5 (P) = '00110'; 6 (B) = '00111'.
        let i_slice = [0x65, 0b1000_1000, 0x00]; // '1' + '0001000'
        let p_slice = [0x41, 0b1001_1000]; // '1' + '00110'
        let b_slice = [0x41, 0b1001_1100]; // '1' + '00111'
        assert_eq!(parse_slice_type(&i_slice), Some(SliceType::I));
        assert_eq!(parse_slice_type(&p_slice), Some(SliceType::P));
        assert_eq!(parse_slice_type(&b_slice), Some(SliceType::B));
        assert_eq!(parse_slice_type(PPS), None);
    }

    #[test]
    fn emulation_prevention_is_removed() {
        assert_eq!(unescape(&[0, 0, 3, 1]), vec![0, 0, 1]);
        assert_eq!(unescape(&[0, 0, 3, 0, 0, 3]), vec![0, 0, 0, 0]);
        assert_eq!(unescape(&[0, 3, 0, 3]), vec![0, 3, 0, 3]);
    }

    #[test]
    fn inspect_flags_idr_and_b() {
        let mut idr = to_annex_b([SPS_42E01F, PPS]);
        idr.extend_from_slice(&[0, 0, 0, 1, 0x65, 0b1000_1000, 0]);
        let au = inspect_annex_b(&idr);
        assert!(au.has_sps && au.has_pps && au.is_idr && !au.has_b_slice);
        assert_eq!(au.sps.unwrap().level_idc, 31);

        let b = to_annex_b([&[0x41u8, 0b1001_1100][..]]);
        let au = inspect_annex_b(&b);
        assert!(!au.is_idr && au.has_b_slice);
    }
}
