use crate::managers::history::HistoryManager;
use crate::managers::meetings::store::{
    self, MeetingAudio, MeetingDetail, MeetingHit, MeetingRow, MeetingSpeakerKeys,
};
use crate::managers::meetings::{self, MeetingManager, Rerun};
use std::sync::Arc;
use tauri::{AppHandle, State};

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

/// Titles a meeting; a blank or missing `title` clears it.
#[tauri::command]
#[specta::specta]
pub fn rename_meeting(
    app: AppHandle,
    history: State<'_, Arc<HistoryManager>>,
    id: String,
    title: Option<String>,
) -> Result<(), String> {
    let conn = history.get_connection().map_err(|e| e.to_string())?;
    store::rename(&conn, &id, title.as_deref()).map_err(|e| e.to_string())?;
    meetings::emit_changed(&app, &id);
    Ok(())
}

/// Transcribes a meeting again from its audio, speakers and all.
#[tauri::command]
#[specta::specta]
pub fn retranscribe_meeting(
    meetings: State<'_, Arc<MeetingManager>>,
    id: String,
) -> Result<(), String> {
    meetings
        .requeue(&id, Rerun::All)
        .map_err(|e| format!("{e:#}"))
}

/// Tells a meeting's speakers apart again, from the words it already has.
#[tauri::command]
#[specta::specta]
pub fn rediarize_meeting(
    meetings: State<'_, Arc<MeetingManager>>,
    id: String,
) -> Result<(), String> {
    meetings
        .requeue(&id, Rerun::Speakers)
        .map_err(|e| format!("{e:#}"))
}

/// The id of the meeting recording now, if one is.
#[tauri::command]
#[specta::specta]
pub fn active_meeting(meetings: State<'_, Arc<MeetingManager>>) -> Option<String> {
    meetings.active_id()
}

/// A meeting's recorded tracks, for playback.
#[tauri::command]
#[specta::specta]
pub fn meeting_audio(
    history: State<'_, Arc<HistoryManager>>,
    id: String,
) -> Result<Vec<MeetingAudio>, String> {
    let conn = history.get_connection().map_err(|e| e.to_string())?;
    store::audio(&conn, &id).map_err(|e| e.to_string())
}

/// Every meeting's speaker keys.
#[tauri::command]
#[specta::specta]
pub fn list_meeting_speakers(
    history: State<'_, Arc<HistoryManager>>,
) -> Result<Vec<MeetingSpeakerKeys>, String> {
    let conn = history.get_connection().map_err(|e| e.to_string())?;
    store::speaker_keys(&conn).map_err(|e| e.to_string())
}

/// Meetings whose transcript contains `query`, with the first match in each.
#[tauri::command]
#[specta::specta]
pub fn search_meetings(
    history: State<'_, Arc<HistoryManager>>,
    query: String,
) -> Result<Vec<MeetingHit>, String> {
    let conn = history.get_connection().map_err(|e| e.to_string())?;
    store::search(&conn, &query).map_err(|e| e.to_string())
}

/// Starts a meeting, or stops the one recording; does nothing if it already is
/// as asked.
#[tauri::command]
#[specta::specta]
pub fn set_meeting_recording(app: AppHandle, recording: bool) {
    crate::signal_handle::set_meeting_recording(&app, recording);
}
