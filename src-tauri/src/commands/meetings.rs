use crate::managers::history::HistoryManager;
use crate::managers::meetings::store::{self, MeetingDetail, MeetingRow};
use std::sync::Arc;
use tauri::State;

/// Every meeting, newest first.
#[tauri::command]
#[specta::specta]
pub fn list_meetings(history: State<'_, Arc<HistoryManager>>) -> Result<Vec<MeetingRow>, String> {
    let conn = history.get_connection().map_err(|e| e.to_string())?;
    store::list(&conn).map_err(|e| e.to_string())
}

/// A meeting with its speakers and transcript; `None` if there's no such meeting.
#[tauri::command]
#[specta::specta]
pub fn get_meeting(
    history: State<'_, Arc<HistoryManager>>,
    id: String,
) -> Result<Option<MeetingDetail>, String> {
    let conn = history.get_connection().map_err(|e| e.to_string())?;
    store::get(&conn, &id).map_err(|e| e.to_string())
}

/// Names one of a meeting's speakers (`me`, `s1`, ...); a blank or missing
/// `label` clears the name.
#[tauri::command]
#[specta::specta]
pub fn rename_speaker(
    history: State<'_, Arc<HistoryManager>>,
    meeting_id: String,
    key: String,
    label: Option<String>,
) -> Result<(), String> {
    let conn = history.get_connection().map_err(|e| e.to_string())?;
    store::rename_speaker(&conn, &meeting_id, &key, label.as_deref()).map_err(|e| e.to_string())
}
