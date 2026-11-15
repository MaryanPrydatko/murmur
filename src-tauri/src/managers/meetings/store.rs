//! Meetings in `history.db`, beside the transcription history. The meeting
//! folder stays the record of what was heard; these rows are what a meetings
//! list reads, and what the user adds on top (speaker names).

use anyhow::{anyhow, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;
use specta::Type;

use super::{Meeting, Utterance};

/// Applied on every open by `HistoryManager::apply_migrations`, outside its
/// numbered migrations: upstream Handy may claim the next `user_version`.
/// `source` + `source_id` keep a re-run import from duplicating a meeting;
/// a `mixed` track is imported audio with everyone on it. Times are
/// `started_at` in Unix seconds and everything else in ms.
pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meetings (
    id TEXT PRIMARY KEY,
    source TEXT NOT NULL DEFAULT 'murmur',
    source_id TEXT,
    title TEXT,
    started_at INTEGER,
    duration_ms INTEGER,
    status TEXT,
    language TEXT,
    asr_model TEXT,
    diar_model TEXT,
    dir TEXT,
    note_path TEXT,
    UNIQUE (source, source_id)
);
CREATE TABLE IF NOT EXISTS meeting_tracks (
    meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('mic', 'system', 'mixed')),
    file_name TEXT NOT NULL,
    start_offset_ms INTEGER NOT NULL,
    duration_ms INTEGER NOT NULL,
    PRIMARY KEY (meeting_id, kind)
);
CREATE TABLE IF NOT EXISTS meeting_speakers (
    meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    label TEXT,
    PRIMARY KEY (meeting_id, key)
);
CREATE TABLE IF NOT EXISTS meeting_segments (
    meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    idx INTEGER NOT NULL,
    t0_ms INTEGER NOT NULL,
    t1_ms INTEGER NOT NULL,
    speaker_key TEXT NOT NULL,
    text TEXT NOT NULL,
    PRIMARY KEY (meeting_id, idx)
);
";

#[derive(Clone, Debug, PartialEq, Serialize, Type)]
pub struct MeetingRow {
    pub id: String,
    pub source: String,
    pub source_id: Option<String>,
    pub title: Option<String>,
    pub started_at: Option<i64>,
    pub duration_ms: Option<i64>,
    /// `recording`, `transcribing`, `done` or `failed`.
    pub status: Option<String>,
    pub language: Option<String>,
    pub asr_model: Option<String>,
    pub diar_model: Option<String>,
    pub dir: Option<String>,
    pub note_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Type)]
pub struct MeetingSpeaker {
    /// `me`, `them`, or `s1`, `s2`, ... for the people on the system track.
    pub key: String,
    /// The name the user gave them.
    pub label: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Type)]
pub struct MeetingSegment {
    pub t0_ms: i64,
    pub t1_ms: i64,
    pub speaker_key: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Type)]
pub struct MeetingDetail {
    pub meeting: MeetingRow,
    pub speakers: Vec<MeetingSpeaker>,
    /// In order.
    pub segments: Vec<MeetingSegment>,
}

/// Saves the meeting and its tracks as they are now; anything set from
/// elsewhere (a title, speaker names) is kept.
pub fn save(conn: &mut Connection, meeting: &Meeting) -> Result<()> {
    let started_at = chrono::DateTime::parse_from_rfc3339(&meeting.started_at)
        .map(|at| at.timestamp())
        .ok();
    let status = serde_json::to_value(meeting.status)?;
    let tx = conn.transaction()?;
    tx.execute(
        "INSERT INTO meetings (id, started_at, duration_ms, status, language, asr_model,
             diar_model, dir)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT (id) DO UPDATE SET started_at = excluded.started_at,
             duration_ms = excluded.duration_ms, status = excluded.status,
             language = excluded.language, asr_model = excluded.asr_model,
             diar_model = excluded.diar_model, dir = excluded.dir",
        params![
            meeting.id,
            started_at,
            meeting.duration_ms,
            status.as_str(),
            meeting.language,
            meeting.asr_model,
            meeting.diar_model,
            meeting.dir.to_string_lossy(),
        ],
    )?;
    tx.execute(
        "DELETE FROM meeting_tracks WHERE meeting_id = ?1",
        [&meeting.id],
    )?;
    for track in &meeting.tracks {
        tx.execute(
            "INSERT INTO meeting_tracks (meeting_id, kind, file_name, start_offset_ms, duration_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                meeting.id,
                serde_json::to_value(track.kind)?.as_str(),
                track.file.file_name().map(|n| n.to_string_lossy()),
                track.start_offset_ms,
                track.duration_ms,
            ],
        )?;
    }
    Ok(tx.commit()?)
}

/// Replaces the meeting's transcript. A speaker already named keeps the name.
pub fn save_transcript(conn: &mut Connection, id: &str, utterances: &[Utterance]) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM meeting_segments WHERE meeting_id = ?1", [id])?;
    for (idx, u) in utterances.iter().enumerate() {
        let key = u.speaker.key();
        tx.execute(
            "INSERT INTO meeting_speakers (meeting_id, key) VALUES (?1, ?2)
             ON CONFLICT DO NOTHING",
            params![id, key],
        )?;
        tx.execute(
            "INSERT INTO meeting_segments (meeting_id, idx, t0_ms, t1_ms, speaker_key, text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, idx as i64, u.t0_ms, u.t1_ms, key, u.text],
        )?;
    }
    Ok(tx.commit()?)
}

const MEETING_COLUMNS: &str = "id, source, source_id, title, started_at, duration_ms, status, \
    language, asr_model, diar_model, dir, note_path";

fn meeting_row(row: &Row<'_>) -> rusqlite::Result<MeetingRow> {
    Ok(MeetingRow {
        id: row.get("id")?,
        source: row.get("source")?,
        source_id: row.get("source_id")?,
        title: row.get("title")?,
        started_at: row.get("started_at")?,
        duration_ms: row.get("duration_ms")?,
        status: row.get("status")?,
        language: row.get("language")?,
        asr_model: row.get("asr_model")?,
        diar_model: row.get("diar_model")?,
        dir: row.get("dir")?,
        note_path: row.get("note_path")?,
    })
}

/// Newest first.
pub fn list(conn: &Connection) -> Result<Vec<MeetingRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {MEETING_COLUMNS} FROM meetings ORDER BY started_at DESC, id DESC"
    ))?;
    let rows = stmt.query_map([], meeting_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<MeetingDetail>> {
    let Some(meeting) = conn
        .query_row(
            &format!("SELECT {MEETING_COLUMNS} FROM meetings WHERE id = ?1"),
            [id],
            meeting_row,
        )
        .optional()?
    else {
        return Ok(None);
    };
    let speakers = conn
        .prepare("SELECT key, label FROM meeting_speakers WHERE meeting_id = ?1 ORDER BY key")?
        .query_map([id], |row| {
            Ok(MeetingSpeaker {
                key: row.get("key")?,
                label: row.get("label")?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let segments = conn
        .prepare(
            "SELECT t0_ms, t1_ms, speaker_key, text FROM meeting_segments
             WHERE meeting_id = ?1 ORDER BY idx",
        )?
        .query_map([id], |row| {
            Ok(MeetingSegment {
                t0_ms: row.get("t0_ms")?,
                t1_ms: row.get("t1_ms")?,
                speaker_key: row.get("speaker_key")?,
                text: row.get("text")?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Some(MeetingDetail {
        meeting,
        speakers,
        segments,
    }))
}

/// Titles a meeting; a blank `title` clears it, and the list shows its date.
pub fn rename(conn: &Connection, id: &str, title: Option<&str>) -> Result<()> {
    let title = title.map(str::trim).filter(|t| !t.is_empty());
    let changed = conn.execute(
        "UPDATE meetings SET title = ?2 WHERE id = ?1",
        params![id, title],
    )?;
    if changed == 0 {
        return Err(anyhow!("No meeting {id}"));
    }
    Ok(())
}

/// Names a meeting's speaker; a blank `label` clears the name.
pub fn rename_speaker(conn: &Connection, id: &str, key: &str, label: Option<&str>) -> Result<()> {
    let label = label.map(str::trim).filter(|l| !l.is_empty());
    let changed = conn.execute(
        "UPDATE meeting_speakers SET label = ?3 WHERE meeting_id = ?1 AND key = ?2",
        params![id, key, label],
    )?;
    if changed == 0 {
        return Err(anyhow!("Meeting {id} has no speaker {key}"));
    }
    Ok(())
}

/// One of a meeting's recorded tracks and where its file is.
#[derive(Clone, Debug, PartialEq, Serialize, Type)]
pub struct MeetingAudio {
    /// `mic`, `system` or `mixed`.
    pub kind: String,
    pub path: String,
    /// When the track's first sample was heard, from the meeting's start.
    pub start_offset_ms: i64,
    pub duration_ms: i64,
}

/// The meeting's tracks, each resolved inside the meeting's folder. A track
/// whose stored name isn't a plain file name is left out.
pub fn audio(conn: &Connection, id: &str) -> Result<Vec<MeetingAudio>> {
    let mut stmt = conn.prepare(
        "SELECT m.dir, t.kind, t.file_name, t.start_offset_ms, t.duration_ms
         FROM meeting_tracks t JOIN meetings m ON m.id = t.meeting_id
         WHERE t.meeting_id = ?1 AND m.dir IS NOT NULL ORDER BY t.kind",
    )?;
    let rows = stmt.query_map([id], |row| {
        Ok((
            row.get::<_, String>("dir")?,
            row.get::<_, String>("file_name")?,
            MeetingAudio {
                kind: row.get("kind")?,
                path: String::new(),
                start_offset_ms: row.get("start_offset_ms")?,
                duration_ms: row.get("duration_ms")?,
            },
        ))
    })?;
    let mut tracks = Vec::new();
    for row in rows {
        let (dir, file_name, mut track) = row?;
        let name = std::path::Path::new(&file_name);
        if name.file_name() != Some(name.as_os_str()) {
            continue;
        }
        track.path = std::path::Path::new(&dir)
            .join(name)
            .to_string_lossy()
            .into_owned();
        tracks.push(track);
    }
    Ok(tracks)
}

/// The speakers of every meeting, for the dots in the list.
#[derive(Clone, Debug, PartialEq, Serialize, Type)]
pub struct MeetingSpeakerKeys {
    pub meeting_id: String,
    /// `me`, `them`, `s1`, ... in key order.
    pub keys: Vec<String>,
}

pub fn speaker_keys(conn: &Connection) -> Result<Vec<MeetingSpeakerKeys>> {
    let mut stmt = conn.prepare(
        "SELECT meeting_id, group_concat(key, ' ') AS keys
         FROM (SELECT meeting_id, key FROM meeting_speakers ORDER BY meeting_id, key)
         GROUP BY meeting_id",
    )?;
    let rows = stmt.query_map([], |row| {
        let keys: String = row.get("keys")?;
        Ok(MeetingSpeakerKeys {
            meeting_id: row.get("meeting_id")?,
            keys: keys.split(' ').map(str::to_owned).collect(),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The first line of a meeting's transcript that matches a search.
#[derive(Clone, Debug, PartialEq, Serialize, Type)]
pub struct MeetingHit {
    pub meeting_id: String,
    pub t0_ms: i64,
    pub text: String,
}

/// Meetings whose transcript contains `query` in any letter case, with their
/// first match. Case is folded here: SQLite's LIKE folds ASCII only.
pub fn search(conn: &Connection, query: &str) -> Result<Vec<MeetingHit>> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare("SELECT meeting_id, t0_ms, text FROM meeting_segments ORDER BY meeting_id, idx")?;
    let rows = stmt.query_map([], |row| {
        Ok(MeetingHit {
            meeting_id: row.get("meeting_id")?,
            t0_ms: row.get("t0_ms")?,
            text: row.get("text")?,
        })
    })?;
    let mut hits: Vec<MeetingHit> = Vec::new();
    for hit in rows {
        let hit = hit?;
        let seen = hits.last().is_some_and(|h| h.meeting_id == hit.meeting_id);
        if !seen && hit.text.to_lowercase().contains(&query) {
            hits.push(hit);
        }
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::super::{MeetingStatus, MeetingTrack, Speaker, TrackKind};
    use super::*;
    use crate::managers::history::HistoryManager;
    use std::path::PathBuf;

    fn db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        HistoryManager::apply_migrations(&mut conn).unwrap();
        conn
    }

    fn meeting(status: MeetingStatus) -> Meeting {
        Meeting {
            id: "2026-11-15_14-03-22".into(),
            started_at: "2026-11-15T14:03:22+01:00".into(),
            duration_ms: 61_000,
            status,
            dir: PathBuf::from("/data/recordings/meetings/2026-11-15_14-03-22"),
            tracks: vec![MeetingTrack {
                kind: TrackKind::System,
                file: PathBuf::from("/data/recordings/meetings/2026-11-15_14-03-22/system.wav"),
                start_offset_ms: 120,
                duration_ms: 60_880,
            }],
            asr_model: Some("parakeet-ultra-Q8_0".into()),
            diar_model: Some("diar_streaming_sortformer_4spk-v2.1".into()),
            language: Some("en".into()),
            attempts: 0,
        }
    }

    fn said(t0_ms: i64, speaker: Speaker, text: &str) -> Utterance {
        Utterance {
            t0_ms,
            t1_ms: t0_ms + 500,
            speaker,
            text: text.into(),
        }
    }

    #[test]
    fn reads_back_a_transcribed_meeting_with_named_speakers() {
        let mut conn = db();
        let id = "2026-11-15_14-03-22";
        save(&mut conn, &meeting(MeetingStatus::Transcribing)).unwrap();
        save(&mut conn, &meeting(MeetingStatus::Done)).unwrap();
        let utterances = [
            said(0, Speaker::Me, "hi"),
            said(1_000, Speaker::Remote(1), "hello"),
            said(2_000, Speaker::Remote(2), "hey"),
        ];
        save_transcript(&mut conn, id, &utterances).unwrap();
        rename_speaker(&conn, id, "s2", Some(" Ana ")).unwrap();
        // Transcribing again keeps the name.
        save_transcript(&mut conn, id, &utterances).unwrap();

        let detail = get(&conn, id).unwrap().unwrap();
        assert_eq!(detail.meeting.source, "murmur");
        assert_eq!(detail.meeting.status.as_deref(), Some("done"));
        assert_eq!(detail.meeting.language.as_deref(), Some("en"));
        assert_eq!(detail.meeting.started_at, Some(1_794_747_802));
        let speakers: Vec<(&str, Option<&str>)> = detail
            .speakers
            .iter()
            .map(|s| (s.key.as_str(), s.label.as_deref()))
            .collect();
        assert_eq!(speakers, [("me", None), ("s1", None), ("s2", Some("Ana"))]);
        let keys: Vec<&str> = detail
            .segments
            .iter()
            .map(|s| s.speaker_key.as_str())
            .collect();
        assert_eq!(keys, ["me", "s1", "s2"]);
        assert_eq!(list(&conn).unwrap(), [detail.meeting]);
        let tracks: i64 = conn
            .query_row("SELECT COUNT(*) FROM meeting_tracks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(tracks, 1);
    }

    #[test]
    fn a_title_survives_the_meeting_being_saved_again() {
        let mut conn = db();
        let id = "2026-11-15_14-03-22";
        save(&mut conn, &meeting(MeetingStatus::Transcribing)).unwrap();
        rename(&conn, id, Some(" Standup ")).unwrap();
        save(&mut conn, &meeting(MeetingStatus::Done)).unwrap();
        let title = |conn: &Connection| get(conn, id).unwrap().unwrap().meeting.title;
        assert_eq!(title(&conn).as_deref(), Some("Standup"));
        rename(&conn, id, Some("  ")).unwrap();
        assert_eq!(title(&conn), None);
        assert!(rename(&conn, "elsewhere", Some("x")).is_err());
    }

    #[test]
    fn renaming_a_speaker_the_meeting_lacks_fails() {
        let mut conn = db();
        save(&mut conn, &meeting(MeetingStatus::Done)).unwrap();
        assert!(rename_speaker(&conn, "2026-11-15_14-03-22", "s4", Some("Bo")).is_err());
        assert!(get(&conn, "elsewhere").unwrap().is_none());
    }

    #[test]
    fn an_import_cannot_add_the_same_meeting_twice() {
        let conn = db();
        let import =
            "INSERT INTO meetings (id, source, source_id) VALUES (?1, 'superwhisper', 'abc')";
        conn.execute(import, ["a"]).unwrap();
        assert!(conn.execute(import, ["b"]).is_err());
    }

    #[test]
    fn finds_a_meeting_by_its_transcript_and_resolves_its_tracks() {
        let mut conn = db();
        let id = "2026-11-15_14-03-22";
        save(&mut conn, &meeting(MeetingStatus::Done)).unwrap();
        let utterances = [
            said(0, Speaker::Me, "the 50% build"),
            said(1_000, Speaker::Remote(1), "Ship BUILD 215"),
            said(2_000, Speaker::Remote(1), "build again"),
            said(3_000, Speaker::Remote(1), "Привіт, Über alles"),
        ];
        save_transcript(&mut conn, id, &utterances).unwrap();

        let hits = search(&conn, "build 2").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].t0_ms, 1_000);
        assert_eq!(search(&conn, "50%").unwrap()[0].t0_ms, 0);
        assert!(search(&conn, "5_%").unwrap().is_empty());
        assert!(search(&conn, "  ").unwrap().is_empty());
        assert_eq!(search(&conn, "привіт").unwrap()[0].t0_ms, 3_000);
        assert_eq!(search(&conn, "über").unwrap()[0].t0_ms, 3_000);

        let keys = speaker_keys(&conn).unwrap();
        assert_eq!(keys[0].keys, ["me", "s1"]);

        let tracks = audio(&conn, id).unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].kind, "system");
        assert_eq!(tracks[0].start_offset_ms, 120);
        assert!(tracks[0]
            .path
            .ends_with("meetings/2026-11-15_14-03-22/system.wav"));
        conn.execute("UPDATE meeting_tracks SET file_name = '../x.wav'", [])
            .unwrap();
        assert!(audio(&conn, id).unwrap().is_empty());
    }
}
