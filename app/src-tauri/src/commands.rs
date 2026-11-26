//! The functions the web page can call (`invoke("name", { ... })` in TypeScript).
//!
//! Each command is a thin wrapper: it moves slow work onto a background thread, keeps job
//! bookkeeping (cancel flags, cached indexes) and forwards progress to the page through a
//! Tauri `Channel`. The real work happens in `engine.rs`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, State};

use crate::engine::{
    self, AppError, AppResult, Capabilities, DatIndex, DatSummary, PartialView, Progress,
    ReferenceMatch, RepairOutcome,
};

/// Shared state, owned by Tauri and handed to every command.
#[derive(Default)]
pub struct AppState {
    /// One cancel flag per running job, keyed by an id the page chooses.
    jobs: Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// Recovery indexes of `.DAT` files already read, so a reference check does not walk a
    /// 40 GB file again.
    indexes: Mutex<HashMap<PathBuf, Arc<DatIndex>>>,
    /// Jobs still running. Closing the window waits for this to reach zero.
    running: Arc<AtomicUsize>,
}

impl AppState {
    /// Registers a job and returns its cancel flag plus a guard that unregisters it.
    fn start_job(&self, id: &str) -> (Arc<AtomicBool>, JobGuard<'_>) {
        let flag = Arc::new(AtomicBool::new(false));
        self.jobs.lock().unwrap().insert(id.into(), flag.clone());
        self.running.fetch_add(1, Ordering::SeqCst);
        let guard = JobGuard {
            state: self,
            id: id.into(),
        };
        (flag, guard)
    }

    /// Asks every running job to stop.
    pub fn cancel_all(&self) {
        for flag in self.jobs.lock().unwrap().values() {
            flag.store(true, Ordering::SeqCst);
        }
    }

    /// Number of jobs still running.
    pub fn running(&self) -> usize {
        self.running.load(Ordering::SeqCst)
    }

    fn cached_index(&self, dat: &Path) -> Option<Arc<DatIndex>> {
        self.indexes.lock().unwrap().get(dat).cloned()
    }

    fn cache_index(&self, dat: &Path, index: Arc<DatIndex>) {
        let mut map = self.indexes.lock().unwrap();
        // Keep memory bounded on large batches.
        if map.len() >= 32 {
            map.clear();
        }
        map.insert(dat.to_path_buf(), index);
    }
}

/// Removes a job from the table when the command returns, even on error.
struct JobGuard<'a> {
    state: &'a AppState,
    id: String,
}

impl Drop for JobGuard<'_> {
    fn drop(&mut self) {
        self.state.jobs.lock().unwrap().remove(&self.id);
        self.state.running.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Runs blocking work on Tauri's thread pool so the window stays responsive.
async fn background<T: Send + 'static>(
    work: impl FnOnce() -> AppResult<T> + Send + 'static,
) -> AppResult<T> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .unwrap_or_else(|e| {
            Err(AppError {
                kind: "internal",
                title: "Something went wrong inside DATfix".into(),
                message: "An internal error stopped the job.".into(),
                next_step: "Please use Copy details and report this as a bug.".into(),
                reasons: Vec::new(),
                details: format!("background task failed: {e}"),
            })
        })
}

/// Basic facts the page needs at start-up.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    /// Product name from `tauri.conf.json` (`productName`). Rename the app there.
    name: String,
    version: String,
    /// "macos", "windows" or "linux".
    os: &'static str,
    capabilities: Capabilities,
    /// "light" or "dark" when `DATFIX_DEV_THEME` forces a theme (screenshots), else null.
    theme: Option<String>,
}

#[tauri::command]
pub fn app_info(app: AppHandle) -> AppInfo {
    let info = app.package_info();
    AppInfo {
        name: info.name.clone(),
        version: info.version.to_string(),
        os: std::env::consts::OS,
        capabilities: engine::capabilities(),
        theme: std::env::var("DATFIX_DEV_THEME")
            .ok()
            .filter(|t| t == "light" || t == "dark"),
    }
}

/// `.DAT` files and `.MP4` clips found in dropped or chosen paths.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Dropped {
    dats: Vec<PathBuf>,
    clips: Vec<PathBuf>,
}

#[tauri::command]
pub async fn sort_paths(paths: Vec<PathBuf>) -> AppResult<Dropped> {
    background(move || {
        let (dats, clips) = engine::expand_dropped(&paths);
        Ok(Dropped { dats, clips })
    })
    .await
}

/// Reads a `.DAT`'s recovery index and returns a summary.
#[tauri::command]
pub async fn inspect_dat(
    state: State<'_, AppState>,
    job: String,
    dat: PathBuf,
    progress: Channel<Progress>,
) -> AppResult<DatSummary> {
    let (cancel, _guard) = state.start_job(&job);
    let path = dat.clone();
    let (summary, index) = background(move || {
        engine::inspect(&path, cancel, &mut |p| {
            let _ = progress.send(p);
        })
    })
    .await?;
    state.cache_index(&dat, index);
    Ok(summary)
}

/// Returns the cached index, or reads it now.
async fn index_for(state: &AppState, dat: &Path, job: &str) -> AppResult<Arc<DatIndex>> {
    if let Some(i) = state.cached_index(dat) {
        return Ok(i);
    }
    let (cancel, _guard) = state.start_job(job);
    let path = dat.to_path_buf();
    let (_, index) = background(move || engine::inspect(&path, cancel, &mut |_| {})).await?;
    state.cache_index(dat, index.clone());
    Ok(index)
}

/// Checks one clip against a `.DAT`.
#[tauri::command]
pub async fn check_reference(
    state: State<'_, AppState>,
    job: String,
    dat: PathBuf,
    clip: PathBuf,
) -> AppResult<ReferenceMatch> {
    let index = index_for(&state, &dat, &job).await?;
    background(move || engine::check_reference_file(&dat, &index, &clip)).await
}

/// Picks DATfix's built-in settings for a `.DAT`.
#[tauri::command]
pub async fn check_builtin(
    state: State<'_, AppState>,
    job: String,
    dat: PathBuf,
) -> AppResult<ReferenceMatch> {
    let index = index_for(&state, &dat, &job).await?;
    background(move || engine::check_builtin(&dat, &index)).await
}

/// Searches for a matching clip, then falls back to the built-in settings. With `dir`,
/// searches that folder and its subfolders; without it, searches the `.DAT`'s own folder, or
/// the whole card when the `.DAT` is on one.
#[tauri::command]
pub async fn find_reference(
    state: State<'_, AppState>,
    job: String,
    dat: PathBuf,
    dir: Option<PathBuf>,
    progress: Channel<Progress>,
) -> AppResult<ReferenceMatch> {
    let index = index_for(&state, &dat, &job).await?;
    let (cancel, _guard) = state.start_job(&job);
    background(move || {
        let (dirs, recursive) = match dir {
            Some(d) => (vec![d], true),
            None => engine::auto_search_dirs(&dat),
        };
        engine::find_reference(&dat, &index, &dirs, recursive, &cancel, &mut |p| {
            let _ = progress.send(p);
        })
    })
    .await
}

/// A free output path for a `.DAT` (never an existing file).
#[tauri::command]
pub fn suggest_output(dat: PathBuf, dir: Option<PathBuf>, taken: Vec<PathBuf>) -> PathBuf {
    engine::suggest_output(&dat, dir.as_deref(), &taken)
}

/// Repairs one `.DAT`. `reference` is a clip, or null for the built-in settings. Progress
/// streams through `progress`; the result is the return value.
#[tauri::command]
pub async fn repair_dat(
    state: State<'_, AppState>,
    job: String,
    dat: PathBuf,
    reference: Option<PathBuf>,
    output: PathBuf,
    progress: Channel<Progress>,
) -> AppResult<RepairOutcome> {
    // The index read when the file was added; the engine checks it still fits the file.
    let index = state.cached_index(&dat);
    let (cancel, _guard) = state.start_job(&job);
    background(move || {
        engine::repair(
            &dat,
            reference.as_deref(),
            &output,
            index,
            cancel,
            &mut |p| {
                let _ = progress.send(p);
            },
        )
    })
    .await
}

/// Lists temporary files left by killed repairs in the given folders.
#[tauri::command]
pub async fn find_partials(dirs: Vec<PathBuf>) -> AppResult<Vec<PartialView>> {
    background(move || Ok(engine::find_partials(&dirs))).await
}

/// Deletes temporary files found by `find_partials`. Returns the bytes freed.
#[tauri::command]
pub async fn remove_partials(paths: Vec<PathBuf>) -> AppResult<u64> {
    background(move || engine::remove_partials(&paths)).await
}

/// Asks a running job to stop. Unknown ids are ignored (the job already finished).
#[tauri::command]
pub fn cancel_job(state: State<'_, AppState>, job: String) {
    if let Some(flag) = state.jobs.lock().unwrap().get(&job) {
        flag.store(true, Ordering::SeqCst);
    }
}

/// Test hook: when the environment variable `DATFIX_DEV_AUTORUN` holds JSON such as
/// `{"dats": ["/x/A.DAT"], "reference": "/refs", "outDir": "/out", "cancelAfterMs": 3000}`,
/// the page adds those files and presses Repair by itself. Used to drive the real UI from a
/// script, because the app's web view cannot be clicked by test tools. Unset in normal use.
#[tauri::command]
pub fn dev_autorun() -> Option<serde_json::Value> {
    let raw = std::env::var("DATFIX_DEV_AUTORUN").ok()?;
    serde_json::from_str(&raw).ok()
}

/// Test hook: the page prints its final state here so a script can check it.
#[tauri::command]
pub fn dev_report(line: String) {
    if std::env::var_os("DATFIX_DEV_AUTORUN").is_some() {
        println!("DATFIX_DEV_REPORT {line}");
    }
}
