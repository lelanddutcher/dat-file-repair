//! A small, stable-Rust stand-in for the cargo-fuzz targets (crates/datfix-core/fuzz).
//!
//! Mutates synthetic `.DAT` files and reference clips with a fixed-seed generator and runs
//! them through the same entry points as the fuzz targets. Any panic fails the test. It runs
//! in a few seconds so CI catches regressions without a nightly toolchain; the real fuzzing
//! runs are longer and find more.
//!
//! `DATFIX_SMOKE_ITERS` raises the iteration count (default 300 per target).

mod common;

use std::path::Path;

use datfix_core::codec::{self, FrameAnalyzer, VideoCodec};
use datfix_core::fourcc::FourCC;
use datfix_core::mp4::reference::Reference;
use datfix_core::repair::{self, ReferenceSource, RepairOptions};
use datfix_core::{inspect, layout, repi, timeline};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Applies a few random edits of the kinds libFuzzer uses: flip bytes, write interesting
/// integers, cut, duplicate a block, insert zeros.
fn mutate(rng: &mut Rng, input: &[u8]) -> Vec<u8> {
    let mut d = input.to_vec();
    for _ in 0..1 + rng.below(6) {
        if d.is_empty() {
            d.push(rng.next() as u8);
            continue;
        }
        let at = rng.below(d.len());
        match rng.below(6) {
            0 => d[at] ^= 1 << rng.below(8),
            1 => {
                let v: [u32; 6] = [0, 1, 0x7fff_ffff, 0xffff_ffff, 0x8000_0000, 0x0100_0000];
                let b = v[rng.below(v.len())].to_le_bytes();
                for (i, x) in b.iter().enumerate() {
                    if at + i < d.len() {
                        d[at + i] = *x;
                    }
                }
            }
            2 => d.truncate(at),
            3 => {
                let len = rng.below(256).min(d.len() - at);
                let block = d[at..at + len].to_vec();
                let to = rng.below(d.len());
                d.splice(to..to, block);
            }
            4 => {
                let n = rng.below(4096);
                d.splice(at..at, std::iter::repeat_n(0u8, n));
            }
            _ => d[at] = rng.next() as u8,
        }
    }
    d
}

fn iters() -> usize {
    std::env::var("DATFIX_SMOKE_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300)
}

#[test]
fn walker_and_layout_survive_mutated_dats() {
    let seeds = [common::dat(2, 0), common::dat(3, 2), common::dat(2, 4)];
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for i in 0..iters() {
        let d = mutate(&mut rng, &seeds[i % seeds.len()]);
        let Ok(index) = repi::walk(&d, &mut |_| true) else {
            continue;
        };
        let lay = layout::analyze(&d, &index, "A_0002D054H241221_134042C0_LAD04.DAT");
        let _ = timeline::build(&d, &index, &lay);
        let _ = inspect::inspect_index(Path::new("x.DAT"), &d, &index);
    }
}

#[test]
fn reference_parser_survives_mutated_clips() {
    let seeds = [
        common::reference(0, &common::dat(1, 0)[16..]),
        common::reference(2, &common::dat(2, 2)[16..]),
    ];
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    for i in 0..iters() {
        let d = mutate(&mut rng, &seeds[i % seeds.len()]);
        let _ = repair::playable_mp4(&d);
        let Ok(r) = Reference::from_source(Path::new("x.MP4"), &d) else {
            continue;
        };
        for t in &r.tracks {
            let _ = r.sample_locations(t);
            if let Some(v) = &t.video {
                let _ = VideoCodec::from_config(t.codec, v.config_kind, &v.config);
            }
        }
    }
}

#[test]
fn codec_parsers_survive_mutated_headers() {
    let hvcc = common::hvcc();
    let cfg =
        VideoCodec::from_config(FourCC::new(b"hvc1"), Some(FourCC::new(b"hvcC")), &hvcc).unwrap();
    let frames: Vec<Vec<u8>> = common::GOP
        .iter()
        .map(|&(t, poc)| common::frame(t, poc, 80))
        .collect();
    let mut rng = Rng(0xdead_beef_cafe_f00d);
    for i in 0..iters() * 10 {
        let h = mutate(&mut rng, &hvcc);
        let _ = VideoCodec::from_config(FourCC::new(b"hvc1"), Some(FourCC::new(b"hvcC")), &h);
        let _ = VideoCodec::from_config(FourCC::new(b"avc1"), Some(FourCC::new(b"avcC")), &h);
        let f = mutate(&mut rng, &frames[i % frames.len()]);
        let size = f.len() as u32;
        let _ = codec::verify_frame(f.as_slice(), &cfg, 0, size);
        let mut a = FrameAnalyzer::new(cfg.clone());
        let _ = a.analyze(f.as_slice(), 0, size);
    }
}

#[test]
fn repair_plan_survives_mutated_dats_and_references() {
    let reference = common::reference(2, &common::dat(2, 2)[16..]);
    let seed = common::dat(3, 2);
    let mut rng = Rng(0x0123_4567_89ab_cdef);
    let path = Path::new("A_0002D054H241221_134042C0_LAD04.DAT");
    let opts = RepairOptions::default();
    for i in 0..iters() {
        let d = mutate(&mut rng, &seed);
        let r = if i % 3 == 0 {
            mutate(&mut rng, &reference)
        } else {
            reference.clone()
        };
        let Ok(r) = Reference::from_source(Path::new("ref.MP4"), &r) else {
            continue;
        };
        if repair::classify_input(path, d.as_slice()).is_err() {
            continue;
        }
        let Ok(index) = repi::walk(&d, &mut |_| true) else {
            continue;
        };
        if let Ok(plan) = repair::plan_repair(
            path,
            d.as_slice(),
            &index,
            &ReferenceSource::Builtin,
            Some(&r),
            None,
            &opts,
            &mut |_| {},
        ) {
            assert!(plan.copy_start <= plan.copy_end && plan.copy_end <= d.len() as u64);
        }
    }
}
