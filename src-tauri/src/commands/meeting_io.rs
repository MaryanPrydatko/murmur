//! Meetings in and out of Murmur: notes into the Obsidian vault, and
//! meetings from the apps used before it.

use crate::managers::history::HistoryManager;
use crate::managers::meetings::import::{self, ImportReport};
use crate::managers::meetings::{notes, store};
use crate::settings::{get_settings, write_settings};
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};

/// Imports the meetings Superwhisper, Wispr Flow and Spokenly kept that
/// weren't imported before; with `dry_run`, only says what it would do.
/// With `write_notes`, each imported transcript also gets its note.
#[tauri::command]
#[specta::specta]
pub async fn import_meetings(
    app: AppHandle,
    dry_run: bool,
    write_notes: bool,
) -> Result<ImportReport, String> {
    tauri::async_runtime::spawn_blocking(move || import::import(&app, dry_run, write_notes, true))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("{e:#}"))
}

/// Writes a transcribed meeting's note into the vault, unless its note is
/// there already. The note's path.
#[tauri::command]
#[specta::specta]
pub fn export_meeting_note(
    app: AppHandle,
    history: State<'_, Arc<HistoryManager>>,
    id: String,
) -> Result<String, String> {
    let home = app.path().home_dir().map_err(|e| e.to_string())?;
    let folder = notes::notes_folder(&get_settings(&app), &home)
        .map_err(|e| format!("{e:#}"))?
        .ok_or_else(|| "Set an Obsidian vault first".to_string())?;
    let conn = history.get_connection().map_err(|e| e.to_string())?;
    let detail = store::get(&conn, &id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("No meeting {id}"))?;
    if detail.meeting.status.as_deref() != Some("done") {
        return Err(format!("Meeting {id} isn't transcribed yet"));
    }
    notes::export(&conn, &detail, &folder)
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
#[specta::specta]
pub fn change_obsidian_vault_path_setting(
    app: AppHandle,
    path: Option<String>,
) -> Result<(), String> {
    let mut settings = get_settings(&app);
    settings.obsidian_vault_path = path.filter(|p| !p.trim().is_empty());
    write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_obsidian_notes_folder_setting(app: AppHandle, folder: String) -> Result<(), String> {
    let mut settings = get_settings(&app);
    settings.obsidian_notes_folder = folder;
    write_settings(&app, settings);
    Ok(())
}
