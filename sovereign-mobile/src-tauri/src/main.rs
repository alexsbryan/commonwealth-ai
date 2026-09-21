// SPDX-License-Identifier: AGPL-3.0-or-later
// Desktop-dev shim: `cargo run` / `tauri dev` on a laptop. The real
// mobile entry is `run()`'s `tauri::mobile_entry_point` (iOS/Android).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    sovereign_mobile_lib::run();
}
