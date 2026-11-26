//! Just enough H.264 (AVC) parsing to rebuild keyframe and display-order tables.
//!
//! Keyframes are IDR pictures (NAL type 5). Display order comes from the picture order count
//! (POC). Only `pic_order_cnt_type` 0 (POC sent in every slice) and 2 (display order equals
//! decode order) are supported; type 1 is rare in camera files. Section numbers refer to
//! ITU-T H.264.

use super::bits::{BitReader, unescape};

/// Fields of an SPS that slice parsing needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Sps {
    pub id: u32,
    pub separate_colour_plane: bool,
    pub log2_max_frame_num: u32,
    pub poc_type: u32,
    pub log2_max_poc_lsb: u32,
    pub frame_mbs_only: bool,
    pub width: u32,
    pub height: u32,
}

/// Fields of a PPS that slice parsing needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Pps {
    pub id: u32,
    pub sps_id: u32,
}

/// Parameter sets from an `avcC` box.
#[derive(Clone, Debug, PartialEq)]
pub struct AvcConfig {
    pub length_size: u8,
    pub sps: Vec<Sps>,
    pub pps: Vec<Pps>,
}

/// NAL unit type from the 1-byte H.264 NAL header.
pub fn nal_type(header0: u8) -> u8 {
    header0 & 0x1f
}

/// True for coded slices (types 1–5).
pub fn is_vcl(t: u8) -> bool {
    (1..=5).contains(&t)
}

impl AvcConfig {
    /// Parses an `avcC` payload (ISO/IEC 14496-15 §5.3.3).
    pub fn from_avcc(avcc: &[u8]) -> Result<Self, String> {
        let get = |i: usize| avcc.get(i).copied().ok_or("avcC truncated".to_string());
        let length_size = (get(4)? & 3) + 1;
        let mut cfg = AvcConfig {
            length_size,
            sps: Vec::new(),
            pps: Vec::new(),
        };
        let mut pos = 5;
        for kind in [7u8, 8u8] {
            let count = if kind == 7 {
                get(pos)? & 0x1f
            } else {
                get(pos)?
            };
            pos += 1;
            for _ in 0..count {
                let len = u16::from_be_bytes([get(pos)?, get(pos + 1)?]) as usize;
                let nal = avcc
                    .get(pos + 2..pos + 2 + len)
                    .ok_or("avcC NAL truncated")?;
                pos += 2 + len;
                let rbsp = unescape(nal.get(1..).ok_or("empty NAL")?);
                if kind == 7 {
                    cfg.sps
                        .push(parse_sps(&rbsp).map_err(|e| format!("SPS: {e}"))?);
                } else {
                    cfg.pps
                        .push(parse_pps(&rbsp).map_err(|e| format!("PPS: {e}"))?);
                }
            }
        }
        if cfg.sps.is_empty() || cfg.pps.is_empty() {
            return Err("avcC holds no SPS or no PPS".into());
        }
        Ok(cfg)
    }
}

/// Skips one scaling list (§7.3.2.1.1.1).
fn skip_scaling_list(r: &mut BitReader, size: usize) -> Result<(), String> {
    let (mut last, mut next) = (8i32, 8i32);
    for _ in 0..size {
        if next != 0 {
            let delta = r.se()?;
            next = (last + delta + 256) % 256;
        }
        if next != 0 {
            last = next;
        }
    }
    Ok(())
}

/// Parses an SPS (§7.3.2.1.1) up to `frame_mbs_only_flag`.
pub fn parse_sps(rbsp: &[u8]) -> Result<Sps, String> {
    let mut r = BitReader::new(rbsp);
    let profile_idc = r.u(8)?;
    r.skip(16)?; // constraint flags, level_idc
    let id = r.ue()?;
    let mut separate_colour_plane = false;
    if matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        let chroma = r.ue()?;
        if chroma == 3 {
            separate_colour_plane = r.flag()?;
        }
        r.ue()?; // bit_depth_luma_minus8
        r.ue()?; // bit_depth_chroma_minus8
        r.skip(1)?; // qpprime_y_zero_transform_bypass_flag
        if r.flag()? {
            let lists = if chroma == 3 { 12 } else { 8 };
            for i in 0..lists {
                if r.flag()? {
                    skip_scaling_list(&mut r, if i < 6 { 16 } else { 64 })?;
                }
            }
        }
    }
    let log2_max_frame_num = r.ue()? + 4;
    if log2_max_frame_num > 16 {
        return Err("log2_max_frame_num out of range".into());
    }
    let poc_type = r.ue()?;
    let mut log2_max_poc_lsb = 0;
    match poc_type {
        0 => {
            log2_max_poc_lsb = r.ue()? + 4;
            if log2_max_poc_lsb > 16 {
                return Err("log2_max_pic_order_cnt_lsb out of range".into());
            }
        }
        1 => {
            r.skip(1)?;
            r.se()?;
            r.se()?;
            let n = r.ue()?;
            for _ in 0..n {
                r.se()?;
            }
        }
        _ => {}
    }
    r.ue()?; // max_num_ref_frames
    r.skip(1)?; // gaps_in_frame_num_value_allowed_flag
    let width_mbs = r.ue()? + 1;
    let height_units = r.ue()? + 1;
    let frame_mbs_only = r.flag()?;
    // Level 6.2 allows 139264 macroblocks per frame; anything near u32 range is garbage.
    if width_mbs > 1 << 16 || height_units > 1 << 16 {
        return Err("picture size out of range".into());
    }
    let width = width_mbs * 16;
    Ok(Sps {
        id,
        separate_colour_plane,
        log2_max_frame_num,
        poc_type,
        log2_max_poc_lsb,
        frame_mbs_only,
        width,
        height: height_units * 16 * if frame_mbs_only { 1 } else { 2 },
    })
}

/// Parses the start of a PPS (§7.3.2.2).
pub fn parse_pps(rbsp: &[u8]) -> Result<Pps, String> {
    let mut r = BitReader::new(rbsp);
    Ok(Pps {
        id: r.ue()?,
        sps_id: r.ue()?,
    })
}

/// What the first slice header of a frame tells us.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slice {
    pub nal_type: u8,
    pub is_reference: bool,
    pub poc_type: u32,
    pub poc_lsb: u32,
    pub max_poc_lsb: u32,
}

/// Parses a slice header (§7.3.3) up to `pic_order_cnt_lsb`.
pub fn parse_slice(nal: &[u8], cfg: &AvcConfig) -> Result<Slice, String> {
    if nal.len() < 2 {
        return Err("slice NAL too short".into());
    }
    let t = nal_type(nal[0]);
    let is_reference = (nal[0] >> 5) & 3 != 0;
    let rbsp = unescape(&nal[1..]);
    let mut r = BitReader::new(&rbsp);
    r.ue()?; // first_mb_in_slice
    let slice_type = r.ue()?;
    if slice_type > 9 {
        return Err(format!("invalid slice type {slice_type}"));
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
    if sps.separate_colour_plane {
        r.skip(2)?;
    }
    r.skip(sps.log2_max_frame_num as usize)?; // frame_num
    if !sps.frame_mbs_only && r.flag()? {
        r.skip(1)?; // bottom_field_flag
    }
    if t == 5 {
        r.ue()?; // idr_pic_id
    }
    let poc_lsb = if sps.poc_type == 0 {
        r.u(sps.log2_max_poc_lsb)?
    } else {
        0
    };
    Ok(Slice {
        nal_type: t,
        is_reference,
        poc_type: sps.poc_type,
        poc_lsb,
        max_poc_lsb: 1 << sps.log2_max_poc_lsb,
    })
}

/// Turns POC LSBs into `(sequence, poc)` pairs (§8.2.1.1). See the HEVC tracker for the idea.
#[derive(Default)]
pub struct PocTracker {
    started: bool,
    sequence: u32,
    prev_lsb: u32,
    prev_msb: i64,
    decode_index: i64,
}

impl PocTracker {
    pub fn next(&mut self, s: &Slice) -> Result<(u32, i64), String> {
        let idr = s.nal_type == 5;
        if idr && self.started {
            self.sequence += 1;
        }
        self.started = true;
        self.decode_index += 1;
        match s.poc_type {
            // Display order equals decode order.
            2 => Ok((self.sequence, self.decode_index)),
            0 => {
                if idr {
                    self.prev_lsb = 0;
                    self.prev_msb = 0;
                }
                let max = s.max_poc_lsb as i64;
                let (lsb, prev) = (s.poc_lsb as i64, self.prev_lsb as i64);
                let msb = if lsb < prev && prev - lsb >= max / 2 {
                    self.prev_msb + max
                } else if lsb > prev && lsb - prev > max / 2 {
                    self.prev_msb - max
                } else {
                    self.prev_msb
                };
                if s.is_reference {
                    self.prev_lsb = s.poc_lsb;
                    self.prev_msb = msb;
                }
                Ok((self.sequence, msb + lsb))
            }
            t => Err(format!("H.264 pic_order_cnt_type {t} is not supported yet")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poc_type0_orders_b_frames() {
        let mut p = PocTracker::default();
        let s = |t, r, lsb| Slice {
            nal_type: t,
            is_reference: r,
            poc_type: 0,
            poc_lsb: lsb,
            max_poc_lsb: 16,
        };
        assert_eq!(p.next(&s(5, true, 0)).unwrap(), (0, 0));
        assert_eq!(p.next(&s(1, true, 6)).unwrap(), (0, 6));
        assert_eq!(p.next(&s(1, false, 2)).unwrap(), (0, 2));
        // Wrap: 12 then 2 (=18).
        assert_eq!(p.next(&s(1, true, 12)).unwrap(), (0, 12));
        assert_eq!(p.next(&s(1, true, 2)).unwrap(), (0, 18));
        assert_eq!(p.next(&s(5, true, 0)).unwrap(), (1, 0));
    }
}
