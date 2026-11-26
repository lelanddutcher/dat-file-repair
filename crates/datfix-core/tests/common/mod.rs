//! Builders for synthetic test media: a tiny HEVC-like stream, a Canon-style `.DAT`, and a
//! matching "healthy" reference MP4. The video is not decodable (slices hold filler bytes),
//! but every header the repair reads is real.

#![allow(dead_code)]

use datfix_core::mp4::atom::Atom;
use datfix_core::mp4::tables;

/// Writes bits most-significant first.
#[derive(Default)]
pub struct BitWriter {
    bytes: Vec<u8>,
    bits: usize,
}

impl BitWriter {
    pub fn bit(&mut self, b: bool) {
        if self.bits % 8 == 0 {
            self.bytes.push(0);
        }
        if b {
            *self.bytes.last_mut().unwrap() |= 0x80 >> (self.bits % 8);
        }
        self.bits += 1;
    }
    pub fn u(&mut self, n: u32, v: u32) {
        for i in (0..n).rev() {
            self.bit((v >> i) & 1 == 1);
        }
    }
    pub fn ue(&mut self, v: u32) {
        let x = v + 1;
        let len = 32 - x.leading_zeros();
        self.u(len - 1, 0);
        self.u(len, x);
    }
    /// rbsp_trailing_bits, then the bytes.
    pub fn finish(mut self) -> Vec<u8> {
        self.bit(true);
        while self.bits % 8 != 0 {
            self.bit(false);
        }
        self.bytes
    }
}

/// Inserts emulation prevention bytes.
pub fn escape(rbsp: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut zeros = 0;
    for &b in rbsp {
        if zeros >= 2 && b <= 3 {
            out.push(3);
            zeros = 0;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

fn nal(t: u8, rbsp: &[u8]) -> Vec<u8> {
    let mut v = vec![t << 1, 1];
    v.extend(escape(rbsp));
    v
}

/// SPS: 64x64, 4:2:0, 8-bit, 8-bit POC LSB.
pub fn sps() -> Vec<u8> {
    let mut w = BitWriter::default();
    w.u(4, 0); // vps id
    w.u(3, 0); // max sub layers - 1
    w.u(1, 1); // temporal id nesting
    w.u(8, 0x01); // profile space, tier, profile idc = 1 (Main)
    w.u(32, 0x6000_0000); // compatibility flags
    w.u(32, 0x9000_0000); // progressive, frame only, constraint flags...
    w.u(16, 0);
    w.u(8, 93); // level
    w.ue(0); // sps id
    w.ue(1); // chroma 4:2:0
    w.ue(64);
    w.ue(64);
    w.u(1, 0); // no conformance window
    w.ue(0);
    w.ue(0);
    w.ue(4); // log2_max_poc_lsb - 4
    nal(33, &w.finish())
}

pub fn pps() -> Vec<u8> {
    let mut w = BitWriter::default();
    w.ue(0);
    w.ue(0);
    w.u(1, 0);
    w.u(1, 0);
    w.u(3, 0);
    nal(34, &w.finish())
}

/// `hvcC` payload holding the SPS and PPS above.
pub fn hvcc() -> Vec<u8> {
    let mut v = vec![
        1, 1, 0x60, 0, 0, 0, 0x90, 0, 0, 0, 0, 0, 93, 0xf0, 0, 0xfc, 0xfd, 0xf8, 0xf8, 0, 0,
    ];
    v.push(0x0f); // lengthSizeMinusOne = 3
    v.push(2); // arrays
    for (t, n) in [(33u8, sps()), (34u8, pps())] {
        v.push(0x80 | t);
        v.extend_from_slice(&1u16.to_be_bytes());
        v.extend_from_slice(&(n.len() as u16).to_be_bytes());
        v.extend(n);
    }
    v
}

/// One frame (sample): a 4-byte length plus a slice NAL with filler after the header.
/// `nal_type` 19 = IDR, 1 = TRAIL_R, 0 = TRAIL_N. `size` is the total sample size.
pub fn frame(nal_type: u8, poc_lsb: u32, size: usize) -> Vec<u8> {
    let mut w = BitWriter::default();
    w.u(1, 1); // first slice segment
    if (16..=23).contains(&nal_type) {
        w.u(1, 0);
    }
    w.ue(0); // pps id
    w.ue(if nal_type == 19 {
        2
    } else if nal_type == 1 {
        1
    } else {
        0
    });
    if nal_type != 19 {
        w.u(8, poc_lsb);
    }
    let mut n = nal(nal_type, &w.finish());
    assert!(size >= n.len() + 4);
    n.resize(size - 4, 0x55);
    let mut s = ((size - 4) as u32).to_be_bytes().to_vec();
    s.extend(n);
    s
}

/// A `repi` group header followed by the samples.
pub fn group(kind: &[u8; 4], fixed: u32, samples: &[Vec<u8>]) -> Vec<u8> {
    let n = samples.len() as u32;
    let header_len: u32 = if fixed > 0 { 20 } else { 20 + 4 * n };
    let mut v = b"repi".to_vec();
    v.extend_from_slice(&header_len.to_le_bytes()[..3]);
    v.push(0x83);
    v.extend_from_slice(kind);
    v.extend_from_slice(&fixed.to_le_bytes());
    v.extend_from_slice(&n.to_le_bytes());
    if fixed == 0 {
        for s in samples {
            v.extend_from_slice(&(s.len() as u32).to_le_bytes());
        }
    }
    for s in samples {
        v.extend_from_slice(s);
    }
    v
}

/// Decode-order GOP pattern: (nal type, POC LSB). Display order is 0..6.
pub const GOP: [(u8, u32); 6] = [(19, 0), (1, 3), (0, 1), (0, 2), (1, 5), (0, 4)];

/// Builds a `.DAT` with `intervals` groups of [tmcd, lpcm x pcm_streams, vide x 6].
/// Each interval is 6 frames at 25 fps = 0.24 s = 11520 audio frames at 48 kHz.
pub fn dat(intervals: usize, pcm_streams: usize) -> Vec<u8> {
    let mut d = vec![
        0, 0, 0, 0, b'm', b'd', b'a', b't', 0, 0, 0, 8, b'w', b'i', b'd', b'e',
    ];
    for i in 0..intervals {
        let tc: Vec<Vec<u8>> = (0..6)
            .map(|f| ((i * 6 + f) as u32).to_be_bytes().to_vec())
            .collect();
        d.extend(group(b"tmcd", 4, &tc));
        for c in 0..pcm_streams {
            let pcm: Vec<Vec<u8>> = (0..11520)
                .map(|k| vec![c as u8, (k >> 8) as u8, k as u8])
                .collect();
            d.extend(group(b"lpcm", 3, &pcm));
        }
        // POC continues across intervals; IDR only in the first one, CRA-like restarts are
        // not needed for the test.
        let frames: Vec<Vec<u8>> = GOP
            .iter()
            .enumerate()
            .map(|(k, &(t, lsb))| {
                let t = if i > 0 && t == 19 { 1 } else { t };
                let lsb = (lsb + 6 * i as u32) % 256;
                frame(t, lsb, 100 + 7 * k)
            })
            .collect();
        d.extend(group(b"vide", 0, &frames));
    }
    d
}

fn full(version: u8, flags: u32) -> Vec<u8> {
    ((u32::from(version) << 24) | flags).to_be_bytes().to_vec()
}

fn mvhd(timescale: u32) -> Atom {
    let mut p = full(0, 0);
    p.extend_from_slice(&[0; 8]);
    p.extend_from_slice(&timescale.to_be_bytes());
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // rate
    p.extend_from_slice(&0x0100u16.to_be_bytes()); // volume
    p.extend_from_slice(&[0; 10]);
    for v in [0x10000u32, 0, 0, 0, 0x10000, 0, 0, 0, 0x4000_0000] {
        p.extend_from_slice(&v.to_be_bytes());
    }
    p.extend_from_slice(&[0; 24]);
    p.extend_from_slice(&10u32.to_be_bytes()); // next track id
    Atom::leaf(b"mvhd", p)
}

fn tkhd(id: u32) -> Atom {
    let mut p = full(0, 7);
    p.extend_from_slice(&[0; 8]);
    p.extend_from_slice(&id.to_be_bytes());
    p.extend_from_slice(&[0; 8]); // reserved + duration
    p.extend_from_slice(&[0; 60]);
    Atom::leaf(b"tkhd", p)
}

fn mdhd(timescale: u32) -> Atom {
    let mut p = full(0, 0);
    p.extend_from_slice(&[0; 8]);
    p.extend_from_slice(&timescale.to_be_bytes());
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&[0x55, 0xc4, 0, 0]);
    Atom::leaf(b"mdhd", p)
}

fn hdlr(kind: &[u8; 4]) -> Atom {
    let mut p = full(0, 0);
    p.extend_from_slice(&[0; 4]);
    p.extend_from_slice(kind);
    p.extend_from_slice(&[0; 13]);
    Atom::leaf(b"hdlr", p)
}

fn stsd(entry_kind: &[u8; 4], body: Vec<u8>) -> Atom {
    let mut p = full(0, 0);
    p.extend_from_slice(&1u32.to_be_bytes());
    p.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
    p.extend_from_slice(entry_kind);
    p.extend(body);
    Atom::leaf(b"stsd", p)
}

fn video_entry() -> Vec<u8> {
    let mut e = vec![0; 6];
    e.extend_from_slice(&1u16.to_be_bytes());
    e.extend_from_slice(&[0; 16]);
    e.extend_from_slice(&64u16.to_be_bytes());
    e.extend_from_slice(&64u16.to_be_bytes());
    e.extend_from_slice(&0x0048_0000u32.to_be_bytes());
    e.extend_from_slice(&0x0048_0000u32.to_be_bytes());
    e.extend_from_slice(&[0; 4]);
    e.extend_from_slice(&1u16.to_be_bytes());
    e.extend_from_slice(&[0; 32]);
    e.extend_from_slice(&0x18u16.to_be_bytes());
    e.extend_from_slice(&0xffffu16.to_be_bytes());
    e.extend(Atom::leaf(b"hvcC", hvcc()).to_bytes());
    e
}

fn ipcm_entry() -> Vec<u8> {
    let mut e = vec![0; 6];
    e.extend_from_slice(&1u16.to_be_bytes());
    e.extend_from_slice(&[0; 8]);
    e.extend_from_slice(&1u16.to_be_bytes()); // channels
    e.extend_from_slice(&24u16.to_be_bytes());
    e.extend_from_slice(&[0; 4]);
    e.extend_from_slice(&(48000u32 << 16).to_be_bytes());
    let mut pcmc = full(0, 0);
    pcmc.extend_from_slice(&[0, 24]);
    e.extend(Atom::leaf(b"pcmC", pcmc).to_bytes());
    e
}

fn tmcd_entry() -> Vec<u8> {
    let mut e = vec![0; 6];
    e.extend_from_slice(&1u16.to_be_bytes());
    e.extend_from_slice(&[0; 4]);
    e.extend_from_slice(&0u32.to_be_bytes());
    e.extend_from_slice(&25u32.to_be_bytes());
    e.extend_from_slice(&1u32.to_be_bytes());
    e.extend_from_slice(&[25, 0]);
    e
}

fn trak(
    id: u32,
    handler: &[u8; 4],
    timescale: u32,
    entry: (&[u8; 4], Vec<u8>),
    delta: u32,
    size: u32,
) -> Atom {
    let stbl = Atom::container(
        b"stbl",
        vec![
            stsd(entry.0, entry.1),
            tables::build_stts_constant(1, delta),
            tables::build_stsc(&[1]),
            if size > 0 {
                tables::build_stsz_fixed(size, 1)
            } else {
                tables::build_stsz(&[100])
            },
            tables::build_co64(&[0]),
        ],
    );
    let minf = Atom::container(b"minf", vec![stbl]);
    let mdia = Atom::container(b"mdia", vec![mdhd(timescale), hdlr(handler), minf]);
    Atom::container(b"trak", vec![tkhd(id), mdia])
}

/// A reference MP4: video (hvc1, 25 fps), `pcm_tracks` mono ipcm tracks, tmcd. Its `mdat`
/// holds `mdat_payload` so the walker can read its repi layout.
pub fn reference(pcm_tracks: usize, mdat_payload: &[u8]) -> Vec<u8> {
    reference_at(pcm_tracks, mdat_payload, 1000)
}

/// Like [`reference`], with the video sample duration in 1/25000 s (1000 = 25 fps).
pub fn reference_at(pcm_tracks: usize, mdat_payload: &[u8], video_delta: u32) -> Vec<u8> {
    let mut children = vec![
        mvhd(1000),
        trak(1, b"vide", 25000, (b"hvc1", video_entry()), video_delta, 0),
    ];
    for i in 0..pcm_tracks {
        children.push(trak(
            2 + i as u32,
            b"soun",
            48000,
            (b"ipcm", ipcm_entry()),
            1,
            3,
        ));
    }
    children.push(trak(9, b"tmcd", 25000, (b"tmcd", tmcd_entry()), 1000, 4));
    // Mark the video as Long GOP like Canon's files.
    let v = children[1].find_mut(&[b"mdia", b"minf", b"stbl"]).unwrap();
    v.children_mut().push(tables::build_ctts(&[1000]));
    v.children_mut().push(tables::build_stss(&[1]));

    let mut f = Atom::leaf(b"ftyp", b"XFVC\0\0\0\x01XFVCmp42iso2".to_vec()).to_bytes();
    f.extend(Atom::container(b"moov", children).to_bytes());
    f.extend_from_slice(&1u32.to_be_bytes());
    f.extend_from_slice(b"mdat");
    f.extend_from_slice(&(16 + mdat_payload.len() as u64).to_be_bytes());
    f.extend_from_slice(mdat_payload);
    f
}
