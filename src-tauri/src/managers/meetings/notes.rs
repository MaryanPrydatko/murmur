//! A finished meeting's note in the Obsidian vault.

use tauri::AppHandle;

/// Called once the meeting in `id` is transcribed.
pub fn on_done(_app: &AppHandle, _id: &str) {}
