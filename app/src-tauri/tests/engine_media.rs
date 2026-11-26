//! Tests of the app's engine layer against real media. They are skipped unless these
//! environment variables are set (media never goes into git):
//!
//! - `DATFIX_APP_DAT`: a small `.DAT`, for example `samples/r5m2_64MB.DAT`.
//! - `DATFIX_APP_REFS`: a folder that holds a healthy clip from the same camera.
//! - `DATFIX_APP_OUT`: a folder to write the repaired file into.
//! - `DATFIX_APP_BIG_DAT` (optional): a multi-GB `.DAT` with a healthy clip in its own folder,
//!   used to test that cancel stops the write and leaves no temporary file.
//!
//! Run with `cargo test --release -p datfix-app -- --nocapture`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use datfix_app_lib::engine::{self, Progress};

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

#[test]
fn repairs_a_sample_through_the_app_layer() {
    let (Some(dat), Some(refs), Some(out)) = (
        env_path("DATFIX_APP_DAT"),
        env_path("DATFIX_APP_REFS"),
        env_path("DATFIX_APP_OUT"),
    ) else {
        eprintln!("skipped: set DATFIX_APP_DAT, DATFIX_APP_REFS and DATFIX_APP_OUT");
        return;
    };
    std::fs::create_dir_all(&out).unwrap();
    let (summary, index) = engine::inspect(&dat, Arc::default(), &mut |_| {}).unwrap();
    eprintln!("summary: {summary:?}");
    assert!(summary.video_frames > 0);

    let found = engine::find_reference(
        &dat,
        &index,
        &[refs],
        true,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap();
    assert!(found.ok, "{found:?}");
    eprintln!("found: {} ({} clips checked)", found.summary, found.checked);

    // The built-in settings fit the same file.
    let builtin = engine::check_builtin(&dat, &index).unwrap();
    eprintln!("builtin: {}", builtin.summary);
    assert!(builtin.ok);

    let output = engine::suggest_output(&dat, Some(&out), &[]);
    let mut stages = Vec::new();
    let outcome = engine::repair(
        &dat,
        found.path.as_deref(),
        &output,
        Some(index),
        engine::RepairChoices::default(),
        None,
        Arc::default(),
        &mut |p| {
            if let Progress::Stage { stage, .. } = p {
                stages.push(stage);
            }
        },
    )
    .unwrap();
    eprintln!("outcome: {outcome:?}");
    assert_eq!(outcome.video_frames, summary.video_frames);
    // The index read by `inspect` is reused: no second walk.
    assert_eq!(stages.len(), 3, "stages reported: {stages:?}");
    assert!(!stages.contains(&engine::Stage::Indexing));
    assert!(output.exists());
}

#[test]
fn cancel_stops_a_large_repair_and_cleans_up() {
    let (Some(dat), Some(out)) = (env_path("DATFIX_APP_BIG_DAT"), env_path("DATFIX_APP_OUT"))
    else {
        eprintln!("skipped: set DATFIX_APP_BIG_DAT and DATFIX_APP_OUT");
        return;
    };
    std::fs::create_dir_all(&out).unwrap();
    let (_, index) = engine::inspect(&dat, Arc::default(), &mut |_| {}).unwrap();
    let (dirs, recursive) = engine::auto_search_dirs(&dat);
    let found = engine::find_reference(
        &dat,
        &index,
        &dirs,
        recursive,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap();
    let output = engine::suggest_output(&dat, Some(&out), &[]);

    // Cancel two seconds into the write stage.
    let cancel = Arc::new(AtomicBool::new(false));
    let mut writing_since: Option<Instant> = None;
    let flag = cancel.clone();
    let started = Instant::now();
    let err = engine::repair(
        &dat,
        found.path.as_deref(),
        &output,
        Some(index),
        engine::RepairChoices::default(),
        None,
        cancel,
        &mut |p| {
            if let Progress::Step { stage, .. } = p
                && stage == engine::Stage::Writing
            {
                let since = *writing_since.get_or_insert_with(Instant::now);
                if since.elapsed() > Duration::from_secs(2) {
                    flag.store(true, Ordering::SeqCst);
                }
            }
        },
    )
    .unwrap_err();
    eprintln!("stopped after {:?}: {}", started.elapsed(), err.kind);
    assert!(err.is_cancelled());
    assert!(!output.exists());
    let leftovers: Vec<_> = std::fs::read_dir(&out)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".datfix-partial"))
        .collect();
    assert!(leftovers.is_empty(), "temporary file left: {leftovers:?}");
}
