//! The whole repair up to the moment bytes would be written: input checks, index walk,
//! built-in profile choice, track mapping, timeline, frame analysis, `moov` building.
//!
//! Input: the bytes of a `.DAT`, or `REF1` + u32 BE length + a reference MP4 + the `.DAT`.
//! With a reference, the fuzzer can mutate both the clip and the recording.
#![no_main]

use datfix_core::mp4::reference::Reference;
use datfix_core::repair::{self, ReferenceSource, RepairOptions};
use datfix_core::repi;
use libfuzzer_sys::fuzz_target;
use std::path::Path;

fuzz_target!(|data: &[u8]| {
    // No ffmpeg trial decodes while fuzzing.
    unsafe { std::env::set_var("PATH", "") };
    let path = Path::new("A_0002D054H241221_134042C0_LAD04.DAT");
    let (reference, data) = match data.strip_prefix(b"REF1") {
        Some(rest) if rest.len() >= 4 => {
            let n = u32::from_be_bytes(rest[..4].try_into().unwrap()) as usize;
            let rest = &rest[4..];
            if n > rest.len() {
                return;
            }
            let Ok(r) = Reference::from_source(Path::new("ref.MP4"), &rest[..n]) else {
                return;
            };
            (Some(r), &rest[n..])
        }
        _ => (None, data),
    };
    if repair::classify_input(path, data).is_err() {
        return;
    }
    let Ok(index) = repi::walk(data, &mut |_| true) else {
        return;
    };
    let opts = RepairOptions::default();
    if let Ok(plan) = repair::plan_repair(
        path,
        data,
        &index,
        &ReferenceSource::Builtin,
        reference.as_ref(),
        Some(1_734_800_000),
        &opts,
        &mut |_| {},
    ) {
        assert!(plan.copy_end >= plan.copy_start);
        assert!(plan.copy_end <= data.len() as u64);
        let _ = plan.output_size();
    }
});
