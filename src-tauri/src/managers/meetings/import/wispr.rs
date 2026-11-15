//! Wispr Flow: the `Meetings` table of `flow.sqlite`, and what was said in
//! `meetings/<id>/live.ndjson`. It keeps no audio.

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use super::{absent, json_error, Scan, Source, SourceMeeting, SpeakerKeys};
use crate::managers::meetings::store::MeetingSegment;

/// One line of `live.ndjson`.
#[derive(Deserialize)]
struct Line {
    segment: Option<Value>,
    #[serde(rename = "startRecordingMs")]
    start_ms: Option<i64>,
    #[serde(rename = "endRecordingMs")]
    end_ms: Option<i64>,
    #[serde(default)]
    text: String,
    speaker: Option<LineSpeaker>,
}

#[derive(Deserialize)]
struct LineSpeaker {
    id: Option<Value>,
    name: Option<String>,
}

/// `speakerMap`: who each diarized speaker is.
#[derive(Default, Deserialize)]
struct SpeakerMap {
    #[serde(default)]
    people: HashMap<String, Person>,
    #[serde(default)]
    assignments: HashMap<String, Option<Assignment>>,
}

#[derive(Deserialize)]
struct Person {
    name: Option<String>,
}

#[derive(Deserialize)]
struct Assignment {
    user: Option<String>,
    consensus: Option<String>,
}

impl SpeakerMap {
    fn name(&self, speaker: &str) -> Option<&str> {
        let assignment = self.assignments.get(speaker)?.as_ref()?;
        let person = assignment.user.as_ref().or(assignment.consensus.as_ref())?;
        self.people.get(person)?.name.as_deref()
    }
}

struct Row {
    id: String,
    title: Option<String>,
    created_at: Option<String>,
    ended_at: Option<String>,
    recorded_ms: Option<i64>,
    summary: Option<String>,
    speaker_map: Option<String>,
}

/// Removes the copy of the database when the scan ends.
struct TempCopy(PathBuf);

impl Drop for TempCopy {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Every meeting Wispr Flow keeps under `root`.
pub fn scan(root: &Path) -> Scan {
    let mut scan = Scan::default();
    let db = root.join("flow.sqlite");
    match fs::metadata(&db) {
        Ok(_) => {}
        Err(e) if absent(&e) => return scan,
        Err(e) => {
            scan.unreadable(&db, e);
            return scan;
        }
    }
    match rows(&db) {
        Ok(rows) => {
            for row in rows {
                let ndjson = root.join("meetings").join(&row.id).join("live.ndjson");
                match read(row, &ndjson) {
                    Ok(meeting) => scan.meetings.push(meeting),
                    Err(e) => scan.unreadable(&ndjson, format!("{e:#}")),
                }
            }
        }
        Err(e) => scan.unreadable(&db, format!("{e:#}")),
    }
    scan
}

/// The meetings not deleted, read from a copy: Wispr Flow may be running,
/// with recent writes still in `flow.sqlite-wal`.
fn rows(db: &Path) -> Result<Vec<Row>> {
    let copy = TempCopy(std::env::temp_dir().join(format!(
        "murmur-wispr-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    )));
    fs::create_dir_all(&copy.0)?;
    for suffix in ["", "-wal", "-shm"] {
        let from = PathBuf::from(format!("{}{suffix}", db.display()));
        if from.is_file() {
            fs::copy(&from, copy.0.join(format!("flow.sqlite{suffix}")))
                .with_context(|| format!("Failed to copy {}", from.display()))?;
        }
    }
    let conn = Connection::open_with_flags(
        copy.0.join("flow.sqlite"),
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let mut stmt = conn.prepare(
        "SELECT id, title, createdAt, endedAt, recordedMs, summary, speakerMap
         FROM Meetings WHERE COALESCE(isDeleted, 0) = 0",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(Row {
                id: row.get(0)?,
                title: row.get(1)?,
                created_at: row.get(2)?,
                ended_at: row.get(3)?,
                recorded_ms: row.get(4)?,
                summary: row.get(5)?,
                speaker_map: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// `2026-08-19 14:31:42.633 +00:00`, in Unix ms.
fn timestamp_ms(at: &str) -> Option<i64> {
    chrono::DateTime::parse_from_str(at, "%Y-%m-%d %H:%M:%S%.f %:z")
        .or_else(|_| chrono::DateTime::parse_from_rfc3339(at))
        .ok()
        .map(|at| at.timestamp_millis())
}

fn raw_id(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn read(row: Row, ndjson: &Path) -> Result<SourceMeeting> {
    let started_at_ms = row
        .created_at
        .as_deref()
        .and_then(timestamp_ms)
        .with_context(|| format!("Meeting {} has no readable start", row.id))?;
    let map: SpeakerMap = match row.speaker_map.as_deref() {
        Some(json) if !json.trim().is_empty() => serde_json::from_str(json)
            .map_err(json_error)
            .context("Unreadable speakerMap")?,
        _ => SpeakerMap::default(),
    };

    // A segment can be written again as it's refined: keep its last line.
    // Keyed on the start too, in case `segment` numbers something coarser.
    let mut lines: BTreeMap<(String, i64), Line> = BTreeMap::new();
    let live = match fs::read_to_string(ndjson) {
        Ok(live) => live,
        Err(e) if absent(&e) => String::new(),
        Err(e) => return Err(e.into()),
    };
    for (n, text) in live.lines().enumerate() {
        if text.trim().is_empty() {
            continue;
        }
        let line: Line = serde_json::from_str(text)
            .map_err(json_error)
            .with_context(|| format!("Line {} is unreadable", n + 1))?;
        let key = line
            .segment
            .as_ref()
            .map_or_else(|| format!("line {n}"), raw_id);
        lines.insert((key, line.start_ms.unwrap_or(0)), line);
    }
    let mut lines: Vec<Line> = lines.into_values().collect();
    lines.sort_by_key(|l| (l.start_ms.unwrap_or(0), l.end_ms.unwrap_or(0)));

    let mut keys = SpeakerKeys::default();
    let segments: Vec<MeetingSegment> = lines
        .iter()
        .map(|line| {
            let raw = line
                .speaker
                .as_ref()
                .and_then(|s| s.id.as_ref())
                .map(raw_id)
                .unwrap_or_default();
            let name = map
                .name(&raw)
                .map(String::from)
                .or_else(|| line.speaker.as_ref().and_then(|s| s.name.clone()));
            let t0_ms = line.start_ms.unwrap_or(0);
            MeetingSegment {
                t0_ms,
                t1_ms: line.end_ms.unwrap_or(t0_ms),
                speaker_key: keys.key(&raw, name.as_deref()),
                text: line.text.trim().to_string(),
            }
        })
        .collect();

    let ended_ms = row.ended_at.as_deref().and_then(timestamp_ms);
    let duration_ms = row
        .recorded_ms
        .or_else(|| segments.iter().map(|s| s.t1_ms).max())
        .or_else(|| ended_ms.map(|end| end - started_at_ms))
        .unwrap_or(0);
    let summary = row.summary.map(|summary| {
        let mut out = String::new();
        let mut rest = summary.as_str();
        while let Some(at) = rest.find("<@speaker:") {
            out.push_str(&rest[..at]);
            let tail = &rest[at + "<@speaker:".len()..];
            match tail.find('>') {
                Some(end) => {
                    let raw = &tail[..end];
                    let name = map
                        .name(raw)
                        .map(String::from)
                        .or_else(|| keys.name(raw))
                        .unwrap_or_else(|| format!("Speaker {raw}"));
                    out.push_str(&name);
                    rest = &tail[end + 1..];
                }
                None => {
                    out.push_str(&rest[at..]);
                    rest = "";
                }
            }
        }
        out.push_str(rest);
        out
    });

    Ok(SourceMeeting {
        source: Source::Wispr,
        source_id: row.id,
        started_at_ms,
        duration_ms,
        title: row.title.filter(|t| !t.trim().is_empty()),
        summary,
        speakers: keys.speakers,
        segments,
        audio: None,
        is_meeting: true,
    })
}
