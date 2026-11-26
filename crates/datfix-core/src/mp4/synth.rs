//! Building track templates from scratch, byte for byte the way Canon writes them.
//!
//! A *hybrid* reference keeps the video track of a healthy clip (the part that holds the
//! decoder settings) and adds the tracks the `.DAT` needs but the clip lacks: PCM audio,
//! timecode and Canon metadata. Those tracks carry no decoder secrets, so they can be written
//! from the conventions observed in healthy files:
//!
//! | Track | Sample entry | Notes |
//! |---|---|---|
//! | 16-bit stereo PCM (CAEP) | `twos`, version 0, 2 ch, 16 bit, 48 kHz | `stsz` fixed 4, 24024 samples per chunk |
//! | 24-bit mono PCM (XFVC) | `ipcm` + `pcmC` (big-endian, 24 bit) | `stsz` fixed 3, one track per channel |
//! | timecode | `tmcd`, flags 3 (drop frame, 24 h), timescale/frame duration of the video | one 4-byte sample per frame |
//! | Canon metadata | `cndm` (16-byte entry), handler `meta`, `tref cdsc` to the video | one sample per frame |
//!
//! Every template has a minimal sample table so it parses like a real track; the writer
//! rebuilds the tables anyway.

use crate::fourcc::FourCC;
use crate::layout::{Family, FrameRate};

use super::atom::Atom;
use super::bytes::PutBe;
use super::tables;

/// ISO-639 code `und` packed the way Canon writes it in `mdhd`.
const LANGUAGE: u16 = 0x15c7;

fn full(version: u8, flags: u32) -> Vec<u8> {
    ((u32::from(version) << 24) | flags).to_be_bytes().to_vec()
}

/// The `ftyp` box Canon writes for a family.
pub fn ftyp(family: Family, video_codec: FourCC) -> Vec<u8> {
    let brands: [&[u8; 4]; 4] = match family {
        Family::C => [b"XFVC", b"XFVC", b"mp42", b"iso2"],
        _ => [
            b"mp42",
            b"mp42",
            if video_codec.0 == *b"avc1" {
                b"avc1"
            } else {
                b"hvc1"
            },
            b"CAEP",
        ],
    };
    let mut p = Vec::new();
    p.extend_from_slice(brands[0]);
    p.put_u32(1); // minor version
    for b in &brands[1..] {
        p.extend_from_slice(*b);
    }
    Atom::leaf(b"ftyp", p).to_bytes()
}

/// `mvhd` version 0 with zero times and duration.
pub fn mvhd(timescale: u32, next_track_id: u32) -> Atom {
    let mut p = full(0, 0);
    p.put_u32(0);
    p.put_u32(0);
    p.put_u32(timescale);
    p.put_u32(0);
    p.put_u32(0x0001_0000); // rate 1.0
    p.put_u16(0x0100); // volume 1.0
    p.extend_from_slice(&[0; 10]);
    for v in [0x10000u32, 0, 0, 0, 0x10000, 0, 0, 0, 0x4000_0000] {
        p.put_u32(v);
    }
    p.extend_from_slice(&[0; 24]);
    p.put_u32(next_track_id);
    Atom::leaf(b"mvhd", p)
}

/// `tkhd` version 0, flags 7 (enabled, in movie, in preview).
fn tkhd(track_id: u32, alternate_group: u16, volume: u16, size: (u32, u32)) -> Atom {
    let mut p = full(0, 7);
    p.put_u32(0);
    p.put_u32(0);
    p.put_u32(track_id);
    p.put_u32(0); // reserved
    p.put_u32(0); // duration
    p.extend_from_slice(&[0; 8]);
    p.put_u16(0); // layer
    p.put_u16(alternate_group);
    p.put_u16(volume);
    p.put_u16(0);
    for v in [0x10000u32, 0, 0, 0, 0x10000, 0, 0, 0, 0x4000_0000] {
        p.put_u32(v);
    }
    p.put_u32(size.0 << 16);
    p.put_u32(size.1 << 16);
    Atom::leaf(b"tkhd", p)
}

fn mdhd(timescale: u32) -> Atom {
    let mut p = full(0, 0);
    p.put_u32(0);
    p.put_u32(0);
    p.put_u32(timescale);
    p.put_u32(0);
    p.put_u16(LANGUAGE);
    p.put_u16(0);
    Atom::leaf(b"mdhd", p)
}

/// `hdlr` with an empty name, as Canon writes it (33 bytes).
fn hdlr(handler: &[u8; 4]) -> Atom {
    let mut p = full(0, 0);
    p.put_u32(0);
    p.extend_from_slice(handler);
    p.extend_from_slice(&[0; 12]);
    p.push(0);
    Atom::leaf(b"hdlr", p)
}

fn dinf() -> Atom {
    let mut dref = full(0, 0);
    dref.put_u32(1);
    dref.extend(Atom::leaf(b"url ", full(0, 1)).to_bytes());
    Atom::container(b"dinf", vec![Atom::leaf(b"dref", dref)])
}

/// A sample table with one sample entry and placeholder tables.
fn stbl(entry: Vec<u8>, delta: u32, fixed_size: u32, per_chunk: u32) -> Atom {
    let mut stsd = full(0, 0);
    stsd.put_u32(1);
    stsd.extend(entry);
    Atom::container(
        b"stbl",
        vec![
            Atom::leaf(b"stsd", stsd),
            tables::build_stts(&[(1, delta)]),
            tables::build_stsc(&[per_chunk]),
            tables::build_stsz_fixed(fixed_size, 0),
            tables::build_co64(&[]),
        ],
    )
}

/// Sample entry header: size, type, 6 reserved bytes, data reference index 1.
fn entry_header(kind: &[u8; 4], body_len: usize) -> Vec<u8> {
    let mut e = Vec::new();
    e.put_u32((16 + body_len) as u32);
    e.extend_from_slice(kind);
    e.extend_from_slice(&[0; 6]);
    e.put_u16(1);
    e
}

/// Sound sample entry version 0.
fn sound_entry(kind: &[u8; 4], channels: u16, bits: u16, extra: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&[0; 8]); // version, revision, vendor
    body.put_u16(channels);
    body.put_u16(bits);
    body.put_u32(0); // compression id, packet size
    body.put_u32(48_000 << 16);
    body.extend_from_slice(extra);
    let mut e = entry_header(kind, body.len());
    e.extend(body);
    e
}

/// The PCM layouts Canon uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PcmKind {
    /// `twos`: 16-bit big-endian stereo (CAEP bodies).
    Twos,
    /// `ipcm`: 24-bit big-endian mono, one track per channel (XFVC bodies).
    Ipcm24,
}

impl PcmKind {
    /// Bytes per audio frame.
    pub fn frame_bytes(self) -> u32 {
        match self {
            PcmKind::Twos => 4,
            PcmKind::Ipcm24 => 3,
        }
    }

    pub fn codec(self) -> &'static str {
        match self {
            PcmKind::Twos => "twos 16-bit stereo",
            PcmKind::Ipcm24 => "ipcm 24-bit mono",
        }
    }
}

/// The `stsd` sample entry for a PCM track.
pub fn pcm_entry(kind: PcmKind) -> Vec<u8> {
    match kind {
        PcmKind::Twos => sound_entry(b"twos", 2, 16, &[]),
        PcmKind::Ipcm24 => {
            // pcmC: version/flags 0, format_flags 0 (big-endian), PCM_sample_size 24.
            let mut pcmc = full(0, 0);
            pcmc.push(0);
            pcmc.push(24);
            sound_entry(b"ipcm", 1, 24, &Atom::leaf(b"pcmC", pcmc).to_bytes())
        }
    }
}

/// A PCM audio track.
pub fn pcm_trak(track_id: u32, kind: PcmKind) -> Atom {
    let minf = Atom::container(
        b"minf",
        vec![
            Atom::leaf(b"smhd", vec![0; 8]),
            dinf(),
            stbl(pcm_entry(kind), 1, kind.frame_bytes(), 24_024),
        ],
    );
    Atom::container(
        b"trak",
        vec![
            tkhd(track_id, 1, 0x0100, (0, 0)),
            Atom::container(b"mdia", vec![mdhd(48_000), hdlr(b"soun"), minf]),
        ],
    )
}

/// An MPEG-4 descriptor: tag, then the length in Canon's fixed 4-byte form.
fn descriptor(tag: u8, body: &[u8]) -> Vec<u8> {
    let n = body.len() as u32;
    let mut d = vec![
        tag,
        0x80 | (n >> 21) as u8 & 0x7f,
        0x80 | (n >> 14) as u8 & 0x7f,
        0x80 | (n >> 7) as u8 & 0x7f,
        n as u8 & 0x7f,
    ];
    d.extend_from_slice(body);
    d
}

/// AAC-LC AudioSpecificConfig (ISO 14496-3 1.6.2.1): object type 2, the sampling-frequency
/// index, the channel configuration, no extensions.
pub fn aac_lc_config(sample_rate: u32, channels: u16) -> Option<[u8; 2]> {
    const RATES: [u32; 13] = [
        96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025,
        8_000, 7_350,
    ];
    let index = RATES.iter().position(|&r| r == sample_rate)? as u16;
    if !(1..=7).contains(&channels) {
        return None;
    }
    let v = (2u16 << 11) | (index << 7) | (channels << 3);
    Some(v.to_be_bytes())
}

/// The `mp4a` sample entry Canon writes: version 0 sound entry plus an `esds` with a
/// 4096-byte buffer and the bitrate as both maximum and average.
pub fn mp4a_entry(channels: u16, sample_rate: u32, bitrate: u32) -> Option<Vec<u8>> {
    let asc = aac_lc_config(sample_rate, channels)?;
    let mut dc = vec![0x40, 0x15, 0x00, 0x10, 0x00]; // MPEG-4 audio, audio stream, buffer 4096
    dc.put_u32(bitrate);
    dc.put_u32(bitrate);
    dc.extend(descriptor(0x05, &asc));
    let mut es = vec![0x00, 0x00, 0x00]; // ES_ID 0, no flags
    es.extend(descriptor(0x04, &dc));
    es.extend(descriptor(0x06, &[0x02]));
    let mut esds = full(0, 0);
    esds.extend(descriptor(0x03, &es));
    let mut body = Vec::new();
    body.extend_from_slice(&[0; 8]); // version, revision, vendor
    body.put_u16(channels);
    body.put_u16(16);
    body.put_u32(0); // compression id, packet size
    body.put_u32(sample_rate << 16);
    body.extend(Atom::leaf(b"esds", esds).to_bytes());
    let mut e = entry_header(b"mp4a", body.len());
    e.extend(body);
    Some(e)
}

/// An AAC audio track (1024 samples per frame).
pub fn aac_trak(track_id: u32, channels: u16, sample_rate: u32, bitrate: u32) -> Option<Atom> {
    let minf = Atom::container(
        b"minf",
        vec![
            Atom::leaf(b"smhd", vec![0; 8]),
            dinf(),
            stbl(mp4a_entry(channels, sample_rate, bitrate)?, 1024, 0, 1),
        ],
    );
    Some(Atom::container(
        b"trak",
        vec![
            tkhd(track_id, 1, 0x0100, (0, 0)),
            Atom::container(b"mdia", vec![mdhd(sample_rate), hdlr(b"soun"), minf]),
        ],
    ))
}

/// The `tmcd` sample entry for a frame rate.
pub fn tmcd_entry(rate: FrameRate) -> Vec<u8> {
    let ntsc = rate.frame_duration == 1001;
    let mut body = Vec::new();
    body.put_u32(0); // reserved
    body.put_u32(if ntsc { 3 } else { 2 }); // drop frame (NTSC), 24-hour maximum
    body.put_u32(rate.timescale);
    body.put_u32(rate.frame_duration);
    body.push(rate.nominal() as u8);
    body.push(0);
    let mut e = entry_header(b"tmcd", body.len());
    e.extend(body);
    e
}

/// A timecode track: one 4-byte frame counter per video frame.
pub fn tmcd_trak(track_id: u32, rate: FrameRate, per_chunk: u32) -> Atom {
    let minf = Atom::container(
        b"minf",
        vec![
            Atom::leaf(b"nmhd", full(0, 0)),
            dinf(),
            stbl(tmcd_entry(rate), rate.frame_duration, 4, per_chunk),
        ],
    );
    Atom::container(
        b"trak",
        vec![
            tkhd(track_id, 0, 0, (0, 0)),
            Atom::container(b"mdia", vec![mdhd(rate.timescale), hdlr(b"tmcd"), minf]),
        ],
    )
}

/// A Canon metadata track (`cndm`), one `sample_size`-byte record per video frame.
pub fn cndm_trak(
    track_id: u32,
    video_track_id: u32,
    rate: FrameRate,
    sample_size: u32,
    per_chunk: u32,
) -> Atom {
    let mut cdsc = Vec::new();
    cdsc.put_u32(12);
    cdsc.extend_from_slice(b"cdsc");
    cdsc.put_u32(video_track_id);
    let minf = Atom::container(
        b"minf",
        vec![
            Atom::leaf(b"nmhd", full(0, 0)),
            dinf(),
            stbl(
                entry_header(b"cndm", 0),
                rate.frame_duration,
                sample_size,
                per_chunk,
            ),
        ],
    );
    Atom::container(
        b"trak",
        vec![
            tkhd(track_id, 0, 0, (0, 0)),
            Atom::leaf(b"tref", cdsc),
            Atom::container(b"mdia", vec![mdhd(rate.timescale), hdlr(b"meta"), minf]),
        ],
    )
}

/// Sets the track id inside a `trak`'s `tkhd`.
pub fn set_track_id(trak: &mut Atom, id: u32) -> Result<(), String> {
    let tkhd = trak.find_mut(&[b"tkhd"]).ok_or("no tkhd")?;
    let super::atom::Body::Leaf(p) = &mut tkhd.body else {
        return Err("tkhd is not a leaf".into());
    };
    let at = if p.first() == Some(&1) { 20 } else { 12 };
    p.get_mut(at..at + 4)
        .ok_or("tkhd too short")?
        .copy_from_slice(&id.to_be_bytes());
    Ok(())
}

/// A `udta` holding only the maker and model strings (no location, no owner).
pub fn udta_model(model: &str) -> Atom {
    let string_box = |kind: &[u8; 4], s: &str| {
        let mut p = Vec::new();
        p.put_u32(0); // version/flags
        p.put_u16(LANGUAGE);
        p.extend_from_slice(s.as_bytes());
        p.push(0);
        Atom::leaf(kind, p)
    };
    Atom::container(
        b"udta",
        vec![string_box(b"manu", "Canon"), string_box(b"modl", model)],
    )
}

/// Reads the model string from a `udta` (`modl`), if present.
pub fn model_of(udta: &Atom) -> Option<String> {
    let payload = match &udta.body {
        super::atom::Body::Leaf(p) => p.clone(),
        super::atom::Body::Container(c) => {
            let mut v = Vec::new();
            c.iter().for_each(|a| a.write_to(&mut v));
            v
        }
    };
    let children = super::atom::parse_atoms(&payload).ok()?;
    let modl = children.iter().find(|a| a.kind.0 == *b"modl")?.payload()?;
    let text = modl.get(6..)?;
    let end = text.iter().position(|&b| b == 0).unwrap_or(text.len());
    Some(String::from_utf8_lossy(&text[..end]).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    // Sample entries copied from healthy camera files (stsd payload after the entry count).
    #[test]
    fn entries_match_camera_files() {
        // R3 LAD37313.MP4, twos.
        assert_eq!(
            pcm_entry(PcmKind::Twos),
            hex("0000002474776f73000000000000000100000000000000000002001000000000bb800000")
        );
        // R5 Mark II A_0053C605…MP4, ipcm with pcmC.
        assert_eq!(
            pcm_entry(PcmKind::Ipcm24),
            hex(
                "000000326970636d000000000000000100000000000000000001001800000000bb8000000000000e70636d43000000000018"
            )
        );
        // tmcd at 59.94p (R3) and 119.88p (R5 Mark II).
        let r = |ts| FrameRate {
            timescale: ts,
            frame_duration: 1001,
        };
        assert_eq!(
            tmcd_entry(r(60_000)),
            hex("00000022746d6364000000000000000100000000000000030000ea60000003e93c00")
        );
        assert_eq!(
            tmcd_entry(r(120_000)),
            hex("00000022746d6364000000000000000100000000000000030001d4c0000003e97800")
        );
        assert_eq!(
            entry_header(b"cndm", 0),
            hex("00000010636e646d0000000000000001")
        );
        // hdlr and mdhd shapes.
        assert_eq!(
            hdlr(b"soun").payload().unwrap(),
            &hex("0000000000000000736f756e00000000000000000000000000")[..]
        );
        assert_eq!(mdhd(48_000).payload().unwrap().len(), 24);
    }

    #[test]
    fn ftyp_matches_camera_files() {
        assert_eq!(
            ftyp(Family::C, FourCC::new(b"hvc1")),
            hex("0000001c667479705846564300000001584656436d70343269736f32")
        );
        assert_eq!(
            ftyp(Family::A, FourCC::new(b"hvc1")),
            hex("0000001c667479706d703432000000016d7034326876633143414550")
        );
    }

    #[test]
    fn mp4a_matches_camera_files() {
        // R5, AAC-LC 48 kHz stereo 256 kb/s (profile R5-hevc-4096x2160-59.94p-…-mp4a).
        let canon = hex(
            "000000576d703461000000000000000100000000000000000002001000000000bb80000000000033\
             65736473000000000380808022000000048080801440150010000003e8000003e80005808080\
             021190068080800102",
        );
        assert_eq!(mp4a_entry(2, 48_000, 256_000).unwrap(), canon);
        assert_eq!(aac_lc_config(48_000, 1), Some([0x11, 0x88]));
        assert_eq!(aac_lc_config(47_000, 2), None);
        let t = aac_trak(3, 2, 48_000, 256_000).unwrap();
        let r = crate::mp4::reference::Reference::from_parts(
            std::path::Path::new("x"),
            0,
            ftyp(Family::BPrime, FourCC::new(b"hvc1")),
            Atom::container(b"moov", vec![mvhd(48_000, 4), t]),
        )
        .unwrap();
        let a = &r.tracks[0];
        assert_eq!(a.codec, FourCC::new(b"mp4a"));
        assert_eq!((a.timescale, a.sample_delta), (48_000, 1024));
        assert_eq!(a.audio.as_ref().unwrap().channels, 2);
    }

    #[test]
    fn model_round_trip() {
        let u = udta_model("Canon EOS R5");
        assert_eq!(model_of(&u).as_deref(), Some("Canon EOS R5"));
        // Camera bytes for `modl`.
        assert_eq!(
            u.children()[1].payload().unwrap(),
            &hex("0000000015c743616e6f6e20454f5320523500")[..]
        );
    }
}
