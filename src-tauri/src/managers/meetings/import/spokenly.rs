//! Spokenly: `History/<YYYY-MM-DD>/<id>.json`, with the audio beside it as
//! `<id>.m4a` or `<id>.wav`. Only file transcriptions can be meetings;
//! dictations are left alone.

use anyhow::Result;
use serde::Deserialize;
use std::fs;
use std::path::Path;

use super::{audio_beside, json_error, Scan, Source, SourceMeeting};
use crate::managers::meetings::store::{MeetingSegment, MeetingSpeaker};

/// Seconds from the Unix epoch to 2001-01-01, where Apple's dates start.
const APPLE_EPOCH_SECS: f64 = 978_307_200.0;

#[derive(Deserialize)]
struct Envelope {
    id: Option<String>,
    /// Seconds since 2001-01-01 UTC.
    #[serde(rename = "creationDate")]
    creation_date: f64,
    content: Content,
}

#[derive(Deserialize)]
struct Content {
    #[serde(rename = "fileTranscription")]
    file_transcription: Option<Wrapped<FileTranscription>>,
}

/// Swift's encoding of an enum case's value.
#[derive(Deserialize)]
struct Wrapped<T> {
    #[serde(rename = "_0")]
    value: T,
}

#[derive(Deserialize)]
struct FileTranscription {
    #[serde(rename = "isMeetingRecording", default)]
    is_meeting_recording: bool,
    #[serde(rename = "audioFile")]
    audio_file: Option<AudioFile>,
    state: State,
}

#[derive(Deserialize)]
struct AudioFile {
    /// In seconds.
    duration: Option<f64>,
}

#[derive(Deserialize)]
struct State {
    completed: Option<Wrapped<Completed>>,
}

#[derive(Deserialize)]
struct Completed {
    #[serde(default)]
    segments: Vec<Segment>,
}

#[derive(Deserialize)]
struct Segment {
    /// In seconds, as is `end`.
    start: f64,
    end: f64,
    #[serde(default)]
    text: String,
}

/// Every file transcription under `history`.
pub fn scan(history: &Path) -> Scan {
    let mut scan = Scan::default();
    let Ok(days) = fs::read_dir(history) else {
        return scan;
    };
    for day in days.flatten().filter(|d| d.path().is_dir()) {
        let Ok(entries) = fs::read_dir(day.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            match read(&path) {
                Ok(Some(meeting)) => scan.meetings.push(meeting),
                Ok(None) => {}
                Err(e) => scan.unreadable(&path, format!("{e:#}")),
            }
        }
    }
    scan
}

fn read(path: &Path) -> Result<Option<SourceMeeting>> {
    let envelope: Envelope = serde_json::from_slice(&fs::read(path)?).map_err(json_error)?;
    let Some(Wrapped { value: file }) = envelope.content.file_transcription else {
        return Ok(None);
    };
    // A transcription that never finished has no segments, so it's a stub.
    let segments: Vec<MeetingSegment> = file
        .state
        .completed
        .map(|c| c.value.segments)
        .unwrap_or_default()
        .into_iter()
        .map(|s| MeetingSegment {
            t0_ms: (s.start * 1000.0) as i64,
            t1_ms: (s.end * 1000.0) as i64,
            speaker_key: "them".into(),
            text: s.text.trim().to_string(),
        })
        .collect();
    let duration_ms = file
        .audio_file
        .and_then(|a| a.duration)
        .map(|secs| (secs * 1000.0) as i64)
        .or_else(|| segments.iter().map(|s| s.t1_ms).max())
        .unwrap_or(0);
    let source_id = envelope.id.unwrap_or_else(|| {
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    Ok(Some(SourceMeeting {
        source: Source::Spokenly,
        source_id,
        started_at_ms: ((envelope.creation_date + APPLE_EPOCH_SECS) * 1000.0) as i64,
        duration_ms,
        title: None,
        summary: None,
        speakers: vec![MeetingSpeaker {
            key: "them".into(),
            label: None,
        }],
        segments,
        audio: audio_beside(path),
        is_meeting: file.is_meeting_recording,
    }))
}
