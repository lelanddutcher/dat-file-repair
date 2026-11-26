//! Just enough HEVC (H.265) parsing to rebuild keyframe and display-order tables.
//!
//! For every frame we need two facts:
//!
//! 1. Is it a random access point (IRAP, NAL types 16–21)? Those become `stss` entries.
//! 2. Its *picture order count* (POC): the display position of the frame. Long-GOP video stores
//!    frames in decode order, which differs from display order when B-frames are used. The
//!    difference becomes the `ctts` table.
//!
//! The POC is read from the first slice header of each frame. Parsing it needs a few fields
//! from the SPS and PPS, which come from the reference clip's `hvcC` box. Section numbers refer
//! to ITU-T H.265.

use super::bits::{BitReader, unescape};

/// Fields of a sequence parameter set (SPS) that slice parsing needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Sps {
    pub id: u32,
    pub chroma_format_idc: u32,
    pub separate_colour_plane: bool,
    pub width: u32,
    pub height: u32,
    pub bit_depth: u32,
    pub log2_max_poc_lsb: u32,
}

/// Fields of a picture parameter set (PPS) that slice parsing needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Pps {
    pub id: u32,
    pub sps_id: u32,
    pub output_flag_present: bool,
    pub num_extra_slice_header_bits: u32,
}

/// Parameter sets from an `hvcC` box.
#[derive(Clone, Debug, PartialEq)]
pub struct HevcConfig {
    /// Bytes used for each NAL length prefix in samples (usually 4).
    pub length_size: u8,
    pub sps: Vec<Sps>,
    pub pps: Vec<Pps>,
}

/// NAL unit type from the 2-byte HEVC NAL header.
pub fn nal_type(header0: u8) -> u8 {
    (header0 >> 1) & 0x3f
}

/// True for video coding layer NAL units (slices).
pub fn is_vcl(t: u8) -> bool {
    t < 32
}

/// True for random access points (BLA, IDR, CRA).
pub fn is_irap(t: u8) -> bool {
    (16..=21).contains(&t)
}

/// True for IDR pictures, which carry no POC in the slice header.
fn is_idr(t: u8) -> bool {
    t == 19 || t == 20
}

impl HevcConfig {
    /// Parses an `hvcC` payload (ISO/IEC 14496-15 §8.3.3).
    pub fn from_hvcc(hvcc: &[u8]) -> Result<Self, String> {
        if hvcc.len() < 23 {
            return Err("hvcC is too short".into());
        }
        let length_size = (hvcc[21] & 3) + 1;
        let num_arrays = hvcc[22] as usize;
        let mut pos = 23;
        let mut cfg = HevcConfig {
            length_size,
            sps: Vec::new(),
            pps: Vec::new(),
        };
        for _ in 0..num_arrays {
            let t = *hvcc.get(pos).ok_or("hvcC array truncated")? & 0x3f;
            let n = u16::from_be_bytes([
                *hvcc.get(pos + 1).ok_or("hvcC truncated")?,
                *hvcc.get(pos + 2).ok_or("hvcC truncated")?,
            ]);
            pos += 3;
            for _ in 0..n {
                let len = u16::from_be_bytes([
                    *hvcc.get(pos).ok_or("hvcC truncated")?,
                    *hvcc.get(pos + 1).ok_or("hvcC truncated")?,
                ]) as usize;
                let nal = hvcc
                    .get(pos + 2..pos + 2 + len)
                    .ok_or("hvcC NAL truncated")?;
                pos += 2 + len;
                // Skip the 2-byte NAL header.
                let rbsp = unescape(nal.get(2..).ok_or("empty NAL")?);
                match t {
                    33 => cfg
                        .sps
                        .push(parse_sps(&rbsp).map_err(|e| format!("SPS: {e}"))?),
                    34 => cfg
                        .pps
                        .push(parse_pps(&rbsp).map_err(|e| format!("PPS: {e}"))?),
                    _ => {}
                }
            }
        }
        if cfg.sps.is_empty() || cfg.pps.is_empty() {
            return Err("hvcC holds no SPS or no PPS".into());
        }
        Ok(cfg)
    }
}

/// Skips `profile_tier_level()` (§7.3.3).
fn skip_profile_tier_level(r: &mut BitReader, max_sub_layers_minus1: u32) -> Result<(), String> {
    r.skip(88)?; // general profile space, tier, idc, compatibility flags, constraint flags
    r.skip(8)?; // general_level_idc
    let mut profile_present = [false; 8];
    let mut level_present = [false; 8];
    for i in 0..max_sub_layers_minus1 as usize {
        profile_present[i] = r.flag()?;
        level_present[i] = r.flag()?;
    }
    if max_sub_layers_minus1 > 0 {
        for _ in max_sub_layers_minus1..8 {
            r.skip(2)?;
        }
    }
    for i in 0..max_sub_layers_minus1 as usize {
        if profile_present[i] {
            r.skip(88)?;
        }
        if level_present[i] {
            r.skip(8)?;
        }
    }
    Ok(())
}

/// Parses the start of an SPS (§7.3.2.2) up to `log2_max_pic_order_cnt_lsb_minus4`.
pub fn parse_sps(rbsp: &[u8]) -> Result<Sps, String> {
    let mut r = BitReader::new(rbsp);
    r.skip(4)?; // sps_video_parameter_set_id
    let max_sub_layers_minus1 = r.u(3)?;
    r.skip(1)?; // temporal_id_nesting
    skip_profile_tier_level(&mut r, max_sub_layers_minus1)?;
    let id = r.ue()?;
    let chroma_format_idc = r.ue()?;
    let separate_colour_plane = chroma_format_idc == 3 && r.flag()?;
    let width = r.ue()?;
    let height = r.ue()?;
    if r.flag()? {
        // conformance window offsets
        for _ in 0..4 {
            r.ue()?;
        }
    }
    let bit_depth = r.ue()? + 8;
    r.ue()?; // bit_depth_chroma_minus8
    let log2_max_poc_lsb = r.ue()? + 4;
    if log2_max_poc_lsb > 16 {
        return Err("log2_max_pic_order_cnt_lsb out of range".into());
    }
    Ok(Sps {
        id,
        chroma_format_idc,
        separate_colour_plane,
        width,
        height,
        bit_depth,
        log2_max_poc_lsb,
    })
}

/// Parses the start of a PPS (§7.3.2.3).
pub fn parse_pps(rbsp: &[u8]) -> Result<Pps, String> {
    let mut r = BitReader::new(rbsp);
    let id = r.ue()?;
    let sps_id = r.ue()?;
    r.skip(1)?; // dependent_slice_segments_enabled_flag
    let output_flag_present = r.flag()?;
    let num_extra_slice_header_bits = r.u(3)?;
    Ok(Pps {
        id,
        sps_id,
        output_flag_present,
        num_extra_slice_header_bits,
    })
}

/// What the first slice header of a frame tells us.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slice {
    pub nal_type: u8,
    pub temporal_id: u8,
    /// `slice_pic_order_cnt_lsb` (0 for IDR).
    pub poc_lsb: u32,
    pub max_poc_lsb: u32,
}

/// Parses the first slice segment header of a frame (§7.3.6.1). `nal` starts with the 2-byte
/// NAL header and may be cut short; ~64 bytes are plenty.
pub fn parse_slice(nal: &[u8], cfg: &HevcConfig) -> Result<Slice, String> {
    if nal.len() < 3 {
        return Err("slice NAL too short".into());
    }
    let t = nal_type(nal[0]);
    let temporal_id = (nal[1] & 7).checked_sub(1).ok_or("temporal id is zero")?;
    let rbsp = unescape(&nal[2..]);
    let mut r = BitReader::new(&rbsp);
    let first_slice = r.flag()?;
    if !first_slice {
        return Err("frame does not start with its first slice segment".into());
    }
    if (16..=23).contains(&t) {
        r.skip(1)?; // no_output_of_prior_pics_flag
    }
    let pps_id = r.ue()?;
    let pps = cfg
        .pps
        .iter()
        .find(|p| p.id == pps_id)
        .ok_or_else(|| format!("frame uses PPS {pps_id}, which the reference does not have"))?;
    let sps = cfg.sps.iter().find(|s| s.id == pps.sps_id).ok_or_else(|| {
        format!(
            "PPS refers to SPS {}, which the reference does not have",
            pps.sps_id
        )
    })?;
    r.skip(pps.num_extra_slice_header_bits as usize)?;
    let slice_type = r.ue()?;
    if slice_type > 2 {
        return Err(format!("invalid slice type {slice_type}"));
    }
    if pps.output_flag_present {
        r.skip(1)?;
    }
    if sps.separate_colour_plane {
        r.skip(2)?;
    }
    let poc_lsb = if is_idr(t) {
        0
    } else {
        r.u(sps.log2_max_poc_lsb)?
    };
    Ok(Slice {
        nal_type: t,
        temporal_id,
        poc_lsb,
        max_poc_lsb: 1 << sps.log2_max_poc_lsb,
    })
}

/// Turns per-frame POC LSBs into full POC values (§8.3.1).
///
/// The output is `(sequence, poc)`. `sequence` increases at every IRAP that restarts POC
/// numbering, so sorting by `(sequence, poc)` gives display order across the whole file.
#[derive(Default)]
pub struct PocTracker {
    started: bool,
    sequence: u32,
    prev_lsb: u32,
    prev_msb: i64,
}

impl PocTracker {
    pub fn next(&mut self, s: &Slice) -> (u32, i64) {
        // IDR and BLA always restart; a CRA restarts only at the start of the stream.
        let restarts = is_irap(s.nal_type) && (s.nal_type <= 20 || !self.started);
        let msb = if restarts {
            if self.started {
                self.sequence += 1;
            }
            0
        } else {
            let max = s.max_poc_lsb as i64;
            let (lsb, prev) = (s.poc_lsb as i64, self.prev_lsb as i64);
            if lsb < prev && prev - lsb >= max / 2 {
                self.prev_msb + max
            } else if lsb > prev && lsb - prev > max / 2 {
                self.prev_msb - max
            } else {
                self.prev_msb
            }
        };
        self.started = true;
        let poc = msb + s.poc_lsb as i64;
        // Only TemporalId 0 pictures that are not RADL/RASL/sub-layer non-reference update the
        // "previous" values (prevTid0Pic).
        let sub_layer_non_ref = s.nal_type <= 14 && s.nal_type % 2 == 0;
        let leading = (6..=9).contains(&s.nal_type);
        if s.temporal_id == 0 && !sub_layer_non_ref && !leading {
            self.prev_lsb = s.poc_lsb;
            self.prev_msb = msb;
        }
        (self.sequence, poc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slice(t: u8, lsb: u32) -> Slice {
        Slice {
            nal_type: t,
            temporal_id: 0,
            poc_lsb: lsb,
            max_poc_lsb: 256,
        }
    }

    #[test]
    fn poc_wraps_and_restarts() {
        let mut p = PocTracker::default();
        assert_eq!(p.next(&slice(19, 0)), (0, 0));
        assert_eq!(p.next(&slice(1, 120)), (0, 120));
        assert_eq!(p.next(&slice(1, 240)), (0, 240));
        // LSB wraps from 240 to 4: MSB grows by 256.
        assert_eq!(p.next(&slice(1, 4)), (0, 260));
        // A CRA in the middle of the stream keeps counting.
        assert_eq!(p.next(&slice(21, 8)), (0, 264));
        // LSB 250 after 8 is read as a step back (264 - 14), not forward.
        assert_eq!(p.next(&slice(1, 250)), (0, 250));
        // An IDR restarts numbering in a new sequence.
        assert_eq!(p.next(&slice(20, 0)), (1, 0));
    }
}
