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
    pub bit_depth_chroma: u32,
    pub log2_max_poc_lsb: u32,
    /// The rest of the SPS that a full slice-header parse needs. `None` when the SPS could
    /// not be read that far (the short parse above is still usable).
    pub ext: Option<SpsExt>,
}

/// One short-term reference picture set, reduced to what slice parsing needs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StRps {
    pub delta_s0: Vec<i32>,
    pub used_s0: Vec<bool>,
    pub delta_s1: Vec<i32>,
    pub used_s1: Vec<bool>,
}

impl StRps {
    fn num_delta_pocs(&self) -> usize {
        self.delta_s0.len() + self.delta_s1.len()
    }
    fn num_used(&self) -> u32 {
        self.used_s0
            .iter()
            .chain(&self.used_s1)
            .filter(|&&u| u)
            .count() as u32
    }
}

/// SPS fields after `log2_max_pic_order_cnt_lsb` (§7.3.2.2.1), up to the VUI.
#[derive(Clone, Debug, PartialEq)]
pub struct SpsExt {
    pub log2_ctb_size: u32,
    pub sample_adaptive_offset: bool,
    pub st_rps: Vec<StRps>,
    pub long_term_refs_present: bool,
    pub num_long_term_ref_pics_sps: u32,
    pub temporal_mvp: bool,
}

impl Sps {
    /// `ChromaArrayType`: 0 for monochrome or separate colour planes.
    pub fn chroma_array_type(&self) -> u32 {
        if self.separate_colour_plane {
            0
        } else {
            self.chroma_format_idc
        }
    }

    /// Picture size in coding tree blocks `(width, height)`.
    pub fn size_in_ctbs(&self) -> Option<(u32, u32)> {
        let ctb = 1u32 << self.ext.as_ref()?.log2_ctb_size;
        Some((self.width.div_ceil(ctb), self.height.div_ceil(ctb)))
    }

    /// Short description such as `4096x2160 4:2:2 10-bit`.
    pub fn describe(&self) -> String {
        let chroma = match self.chroma_format_idc {
            0 => "4:0:0",
            1 => "4:2:0",
            2 => "4:2:2",
            _ => "4:4:4",
        };
        format!(
            "{}x{} {chroma} {}-bit",
            self.width, self.height, self.bit_depth
        )
    }
}

/// Fields of a picture parameter set (PPS) that slice parsing needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Pps {
    pub id: u32,
    pub sps_id: u32,
    pub output_flag_present: bool,
    pub num_extra_slice_header_bits: u32,
    /// The rest of the PPS, when it could be read.
    pub ext: Option<PpsExt>,
}

/// PPS fields that a full slice-header parse needs (§7.3.2.3.1).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PpsExt {
    pub dependent_slice_segments: bool,
    pub cabac_init_present: bool,
    pub num_ref_idx_l0_default: u32,
    pub num_ref_idx_l1_default: u32,
    pub slice_chroma_qp_offsets_present: bool,
    pub weighted_pred: bool,
    pub weighted_bipred: bool,
    pub tiles: bool,
    pub entropy_coding_sync: bool,
    pub loop_filter_across_slices: bool,
    pub deblocking_override_enabled: bool,
    pub deblocking_disabled: bool,
    pub lists_modification_present: bool,
    pub slice_header_extension_present: bool,
    pub chroma_qp_offset_list: bool,
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
    let bit_depth_chroma = r.ue()? + 8;
    let log2_max_poc_lsb = r.ue()? + 4;
    if log2_max_poc_lsb > 16 {
        return Err("log2_max_pic_order_cnt_lsb out of range".into());
    }
    let ext = parse_sps_ext(&mut r, max_sub_layers_minus1, log2_max_poc_lsb).ok();
    Ok(Sps {
        id,
        chroma_format_idc,
        separate_colour_plane,
        width,
        height,
        bit_depth,
        bit_depth_chroma,
        log2_max_poc_lsb,
        ext,
    })
}

/// Skips `scaling_list_data()` (§7.3.4).
fn skip_scaling_list_data(r: &mut BitReader) -> Result<(), String> {
    for size_id in 0..4u32 {
        let step = if size_id == 3 { 3 } else { 1 };
        let mut matrix_id = 0;
        while matrix_id < 6 {
            if !r.flag()? {
                r.ue()?; // scaling_list_pred_matrix_id_delta
            } else {
                let coefs = 64.min(1 << (4 + (size_id << 1)));
                if size_id > 1 {
                    r.se()?; // dc coef
                }
                for _ in 0..coefs {
                    r.se()?;
                }
            }
            matrix_id += step;
        }
    }
    Ok(())
}

/// Parses `st_ref_pic_set(idx)` (§7.3.7) and derives its pictures (§7.4.8).
fn parse_st_rps(r: &mut BitReader, idx: usize, sets: &[StRps]) -> Result<StRps, String> {
    let inter = idx != 0 && r.flag()?;
    if inter {
        let delta_idx = if idx == sets.len() {
            r.ue()? as usize + 1
        } else {
            1
        };
        let reference = sets
            .get(idx.checked_sub(delta_idx).ok_or("bad delta_idx")?)
            .ok_or("RPS refers to a missing set")?;
        let sign = r.flag()?;
        let abs = r.ue()? as i32 + 1;
        let delta_rps = if sign { -abs } else { abs };
        let n = reference.num_delta_pocs();
        let mut used = vec![false; n + 1];
        let mut use_delta = vec![true; n + 1];
        for j in 0..=n {
            used[j] = r.flag()?;
            if !used[j] {
                use_delta[j] = r.flag()?;
            }
        }
        let neg = reference.delta_s0.len();
        let mut out = StRps::default();
        // Equations 7-61 and 7-62.
        for j in (0..reference.delta_s1.len()).rev() {
            let d = reference.delta_s1[j] + delta_rps;
            if d < 0 && use_delta[neg + j] {
                out.delta_s0.push(d);
                out.used_s0.push(used[neg + j]);
            }
        }
        if delta_rps < 0 && use_delta[n] {
            out.delta_s0.push(delta_rps);
            out.used_s0.push(used[n]);
        }
        for j in 0..neg {
            let d = reference.delta_s0[j] + delta_rps;
            if d < 0 && use_delta[j] {
                out.delta_s0.push(d);
                out.used_s0.push(used[j]);
            }
        }
        for j in (0..neg).rev() {
            let d = reference.delta_s0[j] + delta_rps;
            if d > 0 && use_delta[j] {
                out.delta_s1.push(d);
                out.used_s1.push(used[j]);
            }
        }
        if delta_rps > 0 && use_delta[n] {
            out.delta_s1.push(delta_rps);
            out.used_s1.push(used[n]);
        }
        for j in 0..reference.delta_s1.len() {
            let d = reference.delta_s1[j] + delta_rps;
            if d > 0 && use_delta[neg + j] {
                out.delta_s1.push(d);
                out.used_s1.push(used[neg + j]);
            }
        }
        Ok(out)
    } else {
        let neg = r.ue()?;
        let pos = r.ue()?;
        if neg > 16 || pos > 16 {
            return Err("too many reference pictures".into());
        }
        let mut out = StRps::default();
        let mut poc = 0i32;
        for _ in 0..neg {
            poc -= r.ue()? as i32 + 1;
            out.delta_s0.push(poc);
            out.used_s0.push(r.flag()?);
        }
        poc = 0;
        for _ in 0..pos {
            poc += r.ue()? as i32 + 1;
            out.delta_s1.push(poc);
            out.used_s1.push(r.flag()?);
        }
        Ok(out)
    }
}

/// Reads the SPS from `sps_sub_layer_ordering_info_present_flag` to the VUI flag.
fn parse_sps_ext(
    r: &mut BitReader,
    max_sub_layers_minus1: u32,
    log2_max_poc_lsb: u32,
) -> Result<SpsExt, String> {
    let ordering_all = r.flag()?;
    let first = if ordering_all {
        0
    } else {
        max_sub_layers_minus1
    };
    for _ in first..=max_sub_layers_minus1 {
        r.ue()?;
        r.ue()?;
        r.ue()?;
    }
    let log2_min_cb = r.ue()? + 3;
    let log2_ctb_size = log2_min_cb + r.ue()?;
    if !(4..=6).contains(&log2_ctb_size) {
        return Err("CTB size out of range".into());
    }
    r.ue()?; // log2_min_luma_transform_block_size_minus2
    r.ue()?; // log2_diff_max_min_luma_transform_block_size
    r.ue()?; // max_transform_hierarchy_depth_inter
    r.ue()?; // max_transform_hierarchy_depth_intra
    if r.flag()? && r.flag()? {
        skip_scaling_list_data(r)?;
    }
    r.skip(1)?; // amp_enabled_flag
    let sample_adaptive_offset = r.flag()?;
    if r.flag()? {
        // pcm
        r.skip(8)?;
        r.ue()?;
        r.ue()?;
        r.skip(1)?;
    }
    let num_sets = r.ue()? as usize;
    if num_sets > 64 {
        return Err("too many short-term RPS".into());
    }
    let mut st_rps = Vec::with_capacity(num_sets);
    for i in 0..num_sets {
        let set = parse_st_rps(r, i, &st_rps)?;
        st_rps.push(set);
    }
    let long_term_refs_present = r.flag()?;
    let mut num_long_term_ref_pics_sps = 0;
    if long_term_refs_present {
        num_long_term_ref_pics_sps = r.ue()?;
        if num_long_term_ref_pics_sps > 32 {
            return Err("too many long-term pictures".into());
        }
        for _ in 0..num_long_term_ref_pics_sps {
            r.skip(log2_max_poc_lsb as usize + 1)?;
        }
    }
    let temporal_mvp = r.flag()?;
    Ok(SpsExt {
        log2_ctb_size,
        sample_adaptive_offset,
        st_rps,
        long_term_refs_present,
        num_long_term_ref_pics_sps,
        temporal_mvp,
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
    let ext = parse_pps_ext(rbsp).ok();
    Ok(Pps {
        id,
        sps_id,
        output_flag_present,
        num_extra_slice_header_bits,
        ext,
    })
}

/// Reads the whole PPS (§7.3.2.3.1) for the fields slice parsing needs.
fn parse_pps_ext(rbsp: &[u8]) -> Result<PpsExt, String> {
    let mut r = BitReader::new(rbsp);
    r.ue()?;
    r.ue()?;
    let mut p = PpsExt {
        dependent_slice_segments: r.flag()?,
        ..Default::default()
    };
    r.skip(1 + 3 + 1)?; // output_flag_present, extra bits, sign_data_hiding
    p.cabac_init_present = r.flag()?;
    p.num_ref_idx_l0_default = r.ue()? + 1;
    p.num_ref_idx_l1_default = r.ue()? + 1;
    r.se()?; // init_qp_minus26
    r.skip(2)?; // constrained_intra_pred, transform_skip_enabled
    if r.flag()? {
        r.ue()?; // diff_cu_qp_delta_depth
    }
    r.se()?;
    r.se()?;
    p.slice_chroma_qp_offsets_present = r.flag()?;
    p.weighted_pred = r.flag()?;
    p.weighted_bipred = r.flag()?;
    r.skip(1)?; // transquant_bypass
    p.tiles = r.flag()?;
    p.entropy_coding_sync = r.flag()?;
    if p.tiles {
        let cols = r.ue()?;
        let rows = r.ue()?;
        if !r.flag()? {
            for _ in 0..cols + rows {
                r.ue()?;
            }
        }
        r.skip(1)?;
    }
    p.loop_filter_across_slices = r.flag()?;
    if r.flag()? {
        p.deblocking_override_enabled = r.flag()?;
        p.deblocking_disabled = r.flag()?;
        if !p.deblocking_disabled {
            r.se()?;
            r.se()?;
        }
    }
    if r.flag()? {
        skip_scaling_list_data(&mut r)?;
    }
    p.lists_modification_present = r.flag()?;
    r.ue()?; // log2_parallel_merge_level_minus2
    p.slice_header_extension_present = r.flag()?;
    if r.flag()? {
        let range = r.flag()?;
        r.skip(7)?; // multilayer, 3d, scc, 4 reserved bits
        if range {
            if transform_skip_enabled(rbsp)? {
                r.ue()?;
            }
            r.skip(1)?; // cross_component_prediction
            p.chroma_qp_offset_list = r.flag()?;
        }
    }
    Ok(p)
}

/// Reads `transform_skip_enabled_flag` from a PPS.
fn transform_skip_enabled(rbsp: &[u8]) -> Result<bool, String> {
    let mut r = BitReader::new(rbsp);
    r.ue()?;
    r.ue()?;
    r.skip(1 + 1 + 3 + 1 + 1)?;
    r.ue()?;
    r.ue()?;
    r.se()?;
    r.skip(1)?;
    r.flag()
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

/// What a complete slice segment header parse found (see [`check_slice`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SliceCheck {
    pub first_in_picture: bool,
    pub dependent: bool,
    /// CTB address of the slice segment (0 for the first one).
    pub address: u32,
    pub slice_type: u32,
    /// Number of entry points (tiles or wavefront rows) after the first.
    pub entry_points: u32,
    /// Sum of the entry point offsets in bytes.
    pub entry_bytes: u64,
    /// Length of the header in RBSP bytes, alignment included.
    pub header_bytes: usize,
    /// Picture size in CTBs from the SPS.
    pub ctbs: (u32, u32),
    /// Whether the PPS uses wavefront parallel processing / tiles.
    pub wavefront: bool,
    pub tiles: bool,
}

/// `Ceil(Log2(n))`.
fn ceil_log2(n: u32) -> u32 {
    if n <= 1 {
        0
    } else {
        32 - (n - 1).leading_zeros()
    }
}

/// Parses a complete slice segment header (§7.3.6.1) and checks it ends with a valid
/// `byte_alignment()`. A wrong SPS or PPS almost never lines up to the alignment bit, so this
/// is a strong test that the parameter sets belong to the frames. `nal` starts with the NAL
/// header; it may be cut short after the header.
///
/// Returns `Ok(None)` when the parameter sets could not be fully read (the caller then relies
/// on the short parse).
pub fn check_slice(nal: &[u8], cfg: &HevcConfig) -> Result<Option<SliceCheck>, String> {
    if nal.len() < 3 {
        return Err("slice NAL too short".into());
    }
    let t = nal_type(nal[0]);
    let rbsp = unescape(&nal[2..]);
    let mut r = BitReader::new(&rbsp);
    let first = r.flag()?;
    if (16..=23).contains(&t) {
        r.skip(1)?;
    }
    let pps_id = r.ue()?;
    let pps = cfg
        .pps
        .iter()
        .find(|p| p.id == pps_id)
        .ok_or_else(|| format!("slice uses PPS {pps_id}, which the settings do not have"))?;
    let sps = cfg
        .sps
        .iter()
        .find(|s| s.id == pps.sps_id)
        .ok_or("PPS refers to a missing SPS")?;
    let (Some(px), Some(sx), Some(ctbs)) = (&pps.ext, &sps.ext, sps.size_in_ctbs()) else {
        return Ok(None);
    };
    let pic_ctbs = ctbs.0 * ctbs.1;
    let mut dependent = false;
    let mut address = 0;
    if !first {
        if px.dependent_slice_segments {
            dependent = r.flag()?;
        }
        address = r.u(ceil_log2(pic_ctbs))?;
        if address >= pic_ctbs {
            return Err(format!(
                "slice address {address} is outside a picture of {pic_ctbs} CTBs"
            ));
        }
    }
    let mut slice_type = 2;
    if !dependent {
        r.skip(pps.num_extra_slice_header_bits as usize)?;
        slice_type = r.ue()?;
        if slice_type > 2 {
            return Err(format!("invalid slice type {slice_type}"));
        }
        if pps.output_flag_present {
            r.skip(1)?;
        }
        if sps.separate_colour_plane {
            r.skip(2)?;
        }
        let mut temporal_mvp = false;
        let mut total_curr = 0u32;
        if !is_idr(t) {
            r.skip(sps.log2_max_poc_lsb as usize)?;
            let sets = sx.st_rps.len();
            let rps = if !r.flag()? {
                parse_st_rps(&mut r, sets, &sx.st_rps)?
            } else {
                let idx = if sets > 1 {
                    r.u(ceil_log2(sets as u32))? as usize
                } else {
                    0
                };
                sx.st_rps
                    .get(idx)
                    .cloned()
                    .ok_or("RPS index out of range")?
            };
            total_curr = rps.num_used();
            if sx.long_term_refs_present {
                let lt_sps = if sx.num_long_term_ref_pics_sps > 0 {
                    r.ue()?
                } else {
                    0
                };
                let lt_pics = r.ue()?;
                if lt_sps + lt_pics > 32 {
                    return Err("too many long-term pictures".into());
                }
                for i in 0..lt_sps + lt_pics {
                    if i < lt_sps {
                        if sx.num_long_term_ref_pics_sps > 1 {
                            r.skip(ceil_log2(sx.num_long_term_ref_pics_sps) as usize)?;
                        }
                    } else {
                        r.skip(sps.log2_max_poc_lsb as usize)?;
                        if r.flag()? {
                            total_curr += 1;
                        }
                    }
                    if r.flag()? {
                        r.ue()?;
                    }
                }
            }
            if sx.temporal_mvp {
                temporal_mvp = r.flag()?;
            }
        }
        let chroma = sps.chroma_array_type() != 0;
        let (mut sao_luma, mut sao_chroma) = (false, false);
        if sx.sample_adaptive_offset {
            sao_luma = r.flag()?;
            if chroma {
                sao_chroma = r.flag()?;
            }
        }
        if slice_type < 2 {
            let b = slice_type == 0;
            let (mut l0, mut l1) = (px.num_ref_idx_l0_default, px.num_ref_idx_l1_default);
            if r.flag()? {
                l0 = r.ue()? + 1;
                if b {
                    l1 = r.ue()? + 1;
                }
            }
            if l0 > 16 || l1 > 16 {
                return Err("too many reference indices".into());
            }
            if px.lists_modification_present && total_curr > 1 {
                let bits = ceil_log2(total_curr) as usize;
                if r.flag()? {
                    r.skip(bits * l0 as usize)?;
                }
                if b && r.flag()? {
                    r.skip(bits * l1 as usize)?;
                }
            }
            if b {
                r.skip(1)?;
            }
            if px.cabac_init_present {
                r.skip(1)?;
            }
            if temporal_mvp {
                let from_l0 = if b { r.flag()? } else { true };
                if (from_l0 && l0 > 1) || (!from_l0 && l1 > 1) {
                    r.ue()?;
                }
            }
            if (px.weighted_pred && slice_type == 1) || (px.weighted_bipred && b) {
                r.ue()?;
                if chroma {
                    r.se()?;
                }
                let lists = if b { vec![l0, l1] } else { vec![l0] };
                for n in lists {
                    let luma: Vec<bool> = (0..n).map(|_| r.flag()).collect::<Result<_, _>>()?;
                    let chr: Vec<bool> = if chroma {
                        (0..n).map(|_| r.flag()).collect::<Result<_, _>>()?
                    } else {
                        vec![false; n as usize]
                    };
                    for i in 0..n as usize {
                        if luma[i] {
                            r.se()?;
                            r.se()?;
                        }
                        if chr[i] {
                            for _ in 0..4 {
                                r.se()?;
                            }
                        }
                    }
                }
            }
            if r.ue()? > 4 {
                return Err("invalid merge candidate count".into());
            }
        }
        r.se()?; // slice_qp_delta
        if px.slice_chroma_qp_offsets_present {
            r.se()?;
            r.se()?;
        }
        if px.chroma_qp_offset_list {
            r.skip(1)?;
        }
        let mut deblock_off = px.deblocking_disabled;
        if px.deblocking_override_enabled && r.flag()? {
            deblock_off = r.flag()?;
            if !deblock_off {
                r.se()?;
                r.se()?;
            }
        }
        if px.loop_filter_across_slices && (sao_luma || sao_chroma || !deblock_off) {
            r.skip(1)?;
        }
    }
    let mut entry_points = 0;
    let mut entry_bytes = 0u64;
    if px.tiles || px.entropy_coding_sync {
        entry_points = r.ue()?;
        if entry_points > pic_ctbs {
            return Err(format!(
                "{entry_points} entry points is more than the picture has"
            ));
        }
        if entry_points > 0 {
            let len = r.ue()? + 1;
            if len > 32 {
                return Err("entry point offset length out of range".into());
            }
            for _ in 0..entry_points {
                entry_bytes += r.u(len)? as u64 + 1;
            }
        }
    }
    if px.slice_header_extension_present {
        let n = r.ue()?;
        r.skip(8 * n as usize)?;
    }
    // byte_alignment(): a one bit, then zero bits up to the byte boundary.
    if !r.flag()? {
        return Err("slice header does not end with the alignment bit".into());
    }
    while r.bit_pos() % 8 != 0 {
        if r.flag()? {
            return Err("slice header alignment bits are not zero".into());
        }
    }
    Ok(Some(SliceCheck {
        first_in_picture: first,
        dependent,
        address,
        slice_type,
        entry_points,
        entry_bytes,
        header_bytes: r.bit_pos() / 8,
        ctbs,
        wavefront: px.entropy_coding_sync,
        tiles: px.tiles,
    }))
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
