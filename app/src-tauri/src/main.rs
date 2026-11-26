//! Entry point of the desktop app. All the code lives in the library (`lib.rs`).

// On Windows, do not open a console window next to the app in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    datfix_app_lib::run();
}
