//! The `repi` walker and everything that reads a walked index: shapes, layout, timeline,
//! inspection summary. Input: the bytes of a `.DAT`.
#![no_main]

use datfix_core::{inspect, layout, repi, timeline};
use libfuzzer_sys::fuzz_target;
use std::path::Path;

fuzz_target!(|data: &[u8]| {
    let Ok(index) = repi::walk(data, &mut |_| true) else {
        return;
    };
    let _ = index.shapes();
    let lay = layout::analyze(data, &index, "A_0002D054H241221_134042C0_LAD04.DAT");
    let _ = lay.describe();
    let _ = timeline::build(data, &index, &lay);
    let _ = inspect::inspect_index(Path::new("fuzz.DAT"), data, &index);
    for g in &index.groups {
        assert!(g.complete <= g.declared);
        assert!(g.complete_end() <= data.len() as u64);
    }
});
