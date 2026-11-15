//! The import's own rows: `meeting_sources`, the ledger of every record it
//! consumed, and each imported meeting with its names and transcript.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashSet;

use super::{Planned, Source};
use crate::managers::meetings::Meeting;

/// No foreign key: a record stays consumed after its meeting is deleted,
/// so the meeting stays deleted.
const LEDGER: &str = "
CREATE TABLE IF NOT EXISTS meeting_sources (
    source TEXT NOT NULL,
    source_id TEXT NOT NULL,
    meeting_id TEXT NOT NULL,
    PRIMARY KEY (source, source_id)
);";

pub fn create_ledger(conn: &Connection) -> Result<()> {
    Ok(conn.execute_batch(LEDGER)?)
}

/// Every record imported before; none before the first import.
pub fn seen(conn: &Connection) -> Result<HashSet<(Source, String)>> {
    let has_ledger = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'meeting_sources'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    let query = if has_ledger {
        "SELECT source, source_id FROM meeting_sources
         UNION SELECT source, source_id FROM meetings WHERE source_id IS NOT NULL"
    } else {
        "SELECT source, source_id FROM meetings WHERE source_id IS NOT NULL"
    };
    let mut stmt = conn.prepare(query)?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut seen = HashSet::new();
    for row in rows {
        let (source, id) = row?;
        if let Some(source) = Source::parse(&source) {
            seen.insert((source, id));
        }
    }
    Ok(seen)
}

/// The meeting's row, tracks, named speakers and transcript, and a ledger
/// row for each of its records, in one transaction. A meeting still to be
/// transcribed gets its speakers and transcript when it is.
pub fn insert(conn: &mut Connection, planned: &Planned, meeting: &Meeting) -> Result<()> {
    let primary = &planned.primary;
    let status = serde_json::to_value(meeting.status)?;
    let tx = conn.transaction()?;
    tx.execute(
        "INSERT INTO meetings (id, source, source_id, title, started_at, duration_ms, status, dir)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            meeting.id,
            primary.source.as_str(),
            primary.source_id,
            primary.title,
            primary.started_at_ms.div_euclid(1000),
            meeting.duration_ms,
            status.as_str(),
            meeting.dir.to_string_lossy(),
        ],
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
    if !planned.needs_transcription() {
        for speaker in &primary.speakers {
            tx.execute(
                "INSERT INTO meeting_speakers (meeting_id, key, label) VALUES (?1, ?2, ?3)",
                params![meeting.id, speaker.key, speaker.label],
            )?;
        }
        for (idx, segment) in primary.segments.iter().enumerate() {
            tx.execute(
                "INSERT INTO meeting_segments (meeting_id, idx, t0_ms, t1_ms, speaker_key, text)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    meeting.id,
                    idx as i64,
                    segment.t0_ms,
                    segment.t1_ms,
                    segment.speaker_key,
                    segment.text,
                ],
            )?;
        }
    }
    for (source, source_id) in planned.records() {
        tx.execute(
            "INSERT INTO meeting_sources (source, source_id, meeting_id) VALUES (?1, ?2, ?3)",
            params![source.as_str(), source_id, meeting.id],
        )?;
    }
    Ok(tx.commit()?)
}

/// The meeting the planned meeting's records were imported into.
pub fn meeting_of(conn: &Connection, planned: &Planned) -> Result<Option<String>> {
    let mut stmt = conn
        .prepare("SELECT meeting_id FROM meeting_sources WHERE source = ?1 AND source_id = ?2")?;
    for (source, source_id) in planned.records() {
        let id = stmt
            .query_row(params![source.as_str(), source_id], |row| row.get(0))
            .optional()?;
        if id.is_some() {
            return Ok(id);
        }
    }
    Ok(None)
}
