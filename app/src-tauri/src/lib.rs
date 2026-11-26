//! The DATfix desktop app.
//!
//! How the pieces fit:
//!
//! - `engine.rs` calls the repair engine (`datfix-core`) and turns its results into plain-words
//!   types for the page. It and its child module `engine/card.rs` (card recovery) are the only
//!   files that use `datfix-core`.
//! - `commands.rs` holds the functions the web page calls. Slow work runs on a background
//!   thread; progress goes back to the page through a Tauri `Channel`.
//! - `run()` below builds the window and wires everything together.
//!
//! The app's name lives in one place: `productName` in `tauri.conf.json`. The window title and
//! the page heading are read from it at start-up.

// `AppError` carries several strings for the page, so clippy calls it "large". Errors are rare
// and end up as JSON for the UI; boxing them would only add noise.
#![allow(clippy::result_large_err)]

pub mod commands;
pub mod engine;

use std::time::{Duration, Instant};

use tauri::{Manager, WindowEvent};

use commands::AppState;

/// Starts the app. Called from `main.rs`.
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::default())
        .setup(|app| {
            let window = app
                .get_webview_window("main")
                .expect("tauri.conf.json defines a window labelled \"main\"");
            window.set_title(&app.package_info().name)?;
            // Test hook for screenshots: DATFIX_DEV_THEME=dark or light forces the theme.
            match std::env::var("DATFIX_DEV_THEME").as_deref() {
                Ok("dark") => window.set_theme(Some(tauri::Theme::Dark))?,
                Ok("light") => window.set_theme(Some(tauri::Theme::Light))?,
                _ => {}
            }
            // Test hook: keep a script-driven window in front, so the web view keeps drawing
            // (macOS stops painting hidden windows) and screenshots show the current screen.
            if std::env::var_os("DATFIX_DEV_AUTORUN").is_some() {
                window.set_always_on_top(true)?;
                let _ = window.set_focus();
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window during a repair: stop the job first, so the engine can delete
            // its temporary file, then quit.
            if let WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<AppState>();
                if state.running() == 0 {
                    return;
                }
                api.prevent_close();
                state.cancel_all();
                let app = window.app_handle().clone();
                std::thread::spawn(move || {
                    let state = app.state::<AppState>();
                    let deadline = Instant::now() + Duration::from_secs(15);
                    while state.running() > 0 && Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    app.exit(0);
                });
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::sort_paths,
            commands::inspect_dat,
            commands::check_reference,
            commands::check_builtin,
            commands::find_reference,
            commands::suggest_output,
            commands::repair_dat,
            commands::find_partials,
            commands::remove_partials,
            commands::cancel_job,
            commands::list_cards,
            commands::image_card,
            commands::scan_card,
            commands::card_prepare_repair,
            commands::card_save,
            commands::dev_autorun,
            commands::dev_report,
        ])
        .run(tauri::generate_context!())
        .expect("error while running the DATfix app");
}
