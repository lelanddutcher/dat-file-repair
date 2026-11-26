//! HEVC and H.264 header parsers: `hvcC`/`avcC`, SPS, PPS, slice headers, frame checks.
//!
//! The first byte picks the mode. Slice modes parse the input against real Canon parameter
//! sets from the built-in profile bank, so fuzzing reaches deep into the slice header.
#![no_main]

use datfix_core::codec::{self, FrameAnalyzer, VideoCodec, avc, hevc};
use datfix_core::fourcc::FourCC;
use datfix_core::profile;
use libfuzzer_sys::fuzz_target;
use std::sync::OnceLock;

fn bank_codecs() -> &'static [VideoCodec] {
    static CODECS: OnceLock<Vec<VideoCodec>> = OnceLock::new();
    CODECS.get_or_init(|| {
        profile::bank()
            .iter()
            .filter_map(|p| p.to_reference().ok())
            .filter_map(|r| {
                let t = r.video_track()?;
                let v = t.video.as_ref()?;
                VideoCodec::from_config(t.codec, v.config_kind, &v.config).ok()
            })
            .collect()
    })
}

fuzz_target!(|data: &[u8]| {
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    match mode % 8 {
        0 => {
            let _ = VideoCodec::from_config(FourCC::new(b"hvc1"), Some(FourCC::new(b"hvcC")), rest);
        }
        1 => {
            let _ = VideoCodec::from_config(FourCC::new(b"avc1"), Some(FourCC::new(b"avcC")), rest);
        }
        2 => {
            let _ = hevc::parse_sps(rest);
            let _ = hevc::parse_pps(rest);
        }
        3 => {
            let _ = avc::parse_sps(rest);
            let _ = avc::parse_pps(rest);
        }
        m => {
            // Slices and whole frames against every built-in codec configuration.
            let codecs = bank_codecs();
            if codecs.is_empty() {
                return;
            }
            let c = &codecs[(m as usize + rest.first().copied().unwrap_or(0) as usize) % codecs.len()];
            match c {
                VideoCodec::Hevc(cfg) => {
                    let _ = hevc::parse_slice(rest, cfg);
                    let _ = hevc::check_slice(rest, cfg);
                }
                VideoCodec::Avc(cfg) => {
                    let _ = avc::parse_slice(rest, cfg);
                }
            }
            let size = rest.len() as u32;
            let _ = codec::verify_frame(rest, c, 0, size);
            let mut a = FrameAnalyzer::new(c.clone());
            let _ = a.analyze(rest, 0, size);
            let _ = a.analyze(rest, 0, size);
        }
    }
});
