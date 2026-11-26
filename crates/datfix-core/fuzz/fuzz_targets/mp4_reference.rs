//! The reference-clip parser: top-level box scan, `moov` tree, track summaries, sample
//! tables, codec configuration. Input: the bytes of an `.MP4`.
#![no_main]

use datfix_core::codec::VideoCodec;
use datfix_core::mp4::reference::Reference;
use datfix_core::repair::playable_mp4;
use libfuzzer_sys::fuzz_target;
use std::path::Path;

fuzz_target!(|data: &[u8]| {
    let _ = playable_mp4(data);
    let Ok(r) = Reference::from_source(Path::new("fuzz.MP4"), data) else {
        return;
    };
    for t in &r.tracks {
        let _ = r.sample_locations(t);
        if let Some(v) = &t.video {
            let _ = VideoCodec::from_config(t.codec, v.config_kind, &v.config);
        }
    }
    let _ = datfix_core::profile::strip_moov(&r.moov);
    let _ = r.moov.to_bytes();
});
