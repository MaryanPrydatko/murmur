//! Superwhisper: `recordings/<unix seconds>/meta.json`, with `output.ogg`
//! beside it. Only records whose `kind` is `meeting` are meetings.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::fs;
use std::path::Path;

use super::{absent, audio_beside, json_error, list, Scan, Source, SourceMeeting, SpeakerKeys};
use crate::managers::meetings::store::MeetingSegment;

#[derive(Deserialize)]
struct Meta {
    /// In ms.
    duration: Option<f64>,
    /// UTC; the folder's name is the same time in Unix seconds.
    datetime: Option<String>,
    title: Option<String>,
    #[serde(rename = "llmResult")]
    llm_result: Option<String>,
    #[serde(default)]
    segments: Vec<Segment>,
    #[serde(default)]
    speakers: Vec<Speaker>,
}

#[derive(Deserialize)]
struct Segment {
    #[serde(default)]
    text: String,
    speaker: Option<Value>,
    start: Option<f64>,
    end: Option<f64>,
}

#[derive(Deserialize)]
struct Speaker {
    name: Option<String>,
    number: Option<Value>,
}

/// Every meeting under `recordings`.
pub fn scan(recordings: &Path) -> Scan {
    let mut scan = Scan::default();
    for folder in list(recordings, &mut scan) {
        let meta = folder.join("meta.json");
        match read(&folder, &meta) {
            Ok(Some(meeting)) => scan.meetings.push(meeting),
            Ok(None) => {}
            Err(e) => scan.unreadable(&meta, format!("{e:#}")),
        }
    }
    scan
}

fn raw_speaker(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn read(folder: &Path, meta_path: &Path) -> Result<Option<SourceMeeting>> {
    let bytes = match fs::read(meta_path) {
        Ok(bytes) => bytes,
        Err(e) if absent(&e) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    // Most records are dictations: only a meeting has to read in full.
    let record: Value = serde_json::from_slice(&bytes).map_err(json_error)?;
    if record.get("kind").and_then(Value::as_str) != Some("meeting") {
        return Ok(None);
    }
    let meta: Meta = serde_json::from_slice(&bytes).map_err(json_error)?;
    let folder_name = folder
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let started_at_ms = match folder_name.parse::<i64>() {
        Ok(secs) => secs * 1000,
        Err(_) => started_at(meta.datetime.as_deref())?,
    };

    // The units of `start` and `end` aren't written down anywhere: seconds
    // when the last end fits the duration (in ms) as seconds, else ms.
    let last_end = meta
        .segments
        .iter()
        .filter_map(|s| s.end)
        .fold(0.0_f64, f64::max);
    let duration_ms = meta.duration.unwrap_or(last_end * 1000.0);
    let scale = if last_end * 1000.0 <= duration_ms * 1.5 + 5_000.0 {
        1000.0
    } else {
        1.0
    };

    let mut keys = SpeakerKeys::default();
    let segments = meta
        .segments
        .iter()
        .map(|segment| {
            let raw = segment
                .speaker
                .as_ref()
                .map(raw_speaker)
                .unwrap_or_default();
            let name = meta
                .speakers
                .iter()
                .find(|s| s.number.as_ref().map(raw_speaker).as_deref() == Some(raw.as_str()))
                .and_then(|s| s.name.clone())
                // A speaker given by name rather than number.
                .or_else(|| {
                    segment
                        .speaker
                        .as_ref()
                        .and_then(Value::as_str)
                        .map(String::from)
                });
            MeetingSegment {
                t0_ms: (segment.start.unwrap_or(0.0) * scale) as i64,
                t1_ms: (segment.end.or(segment.start).unwrap_or(0.0) * scale) as i64,
                speaker_key: keys.key(&raw, name.as_deref()),
                text: segment.text.trim().to_string(),
            }
        })
        .collect();

    Ok(Some(SourceMeeting {
        source: Source::Superwhisper,
        source_id: folder_name,
        started_at_ms,
        duration_ms: duration_ms as i64,
        title: meta.title.filter(|t| !t.trim().is_empty()),
        summary: meta
            .llm_result
            .map(|s| s.trim().trim_start_matches(':').trim().to_string()),
        speakers: keys.speakers,
        segments,
        audio: audio_beside(&folder.join("output")),
        is_meeting: true,
    }))
}

fn started_at(datetime: Option<&str>) -> Result<i64> {
    let datetime = datetime.ok_or_else(|| anyhow!("No start time"))?;
    chrono::DateTime::parse_from_rfc3339(datetime)
        .map(|at| at.timestamp_millis())
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(datetime, "%Y-%m-%d %H:%M:%S")
                .map(|at| at.and_utc().timestamp_millis())
        })
        .with_context(|| "Unreadable start time")
}
