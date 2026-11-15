//! A finished meeting's note in the user's Obsidian vault.

use tauri::AppHandle;

/// Called once meeting `id` is transcribed.
pub fn on_done(_app: &AppHandle, _id: &str) {}
