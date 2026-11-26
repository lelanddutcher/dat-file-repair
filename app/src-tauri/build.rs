//! Tauri's build step: reads `tauri.conf.json`, embeds the app icon and the permission
//! manifest (`capabilities/`) into the binary.

fn main() {
    tauri_build::build()
}
