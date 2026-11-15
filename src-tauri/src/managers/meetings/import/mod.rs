//! Meetings recorded in other apps, imported once. Each app's records are
//! read into [`SourceMeeting`]s, [`plan`] folds the records of one real
//! meeting together, and every record it consumed is kept in
//! `meeting_sources`, so a second run, or a meeting deleted since, imports
//! nothing.

mod spokenly;
mod superwhisper;
mod wispr;

use serde::Serialize;
use specta::Type;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::store::{MeetingSegment, MeetingSpeaker};

/// Shorter than this, a recording is a stub.
const MIN_DURATION_MS: i64 = 30_000;
/// Two records are one meeting when they start this close together...
const SAME_START_MS: i64 = 2 * 60_000;
/// ...and their lengths differ by at most this percentage of the longer.
const SAME_DURATION_PCT: i64 = 20;

/// In order of precedence: a meeting's names, summary and transcript come
/// from the first of these that has it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Wispr,
    Superwhisper,
    Spokenly,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wispr => "wispr",
            Self::Superwhisper => "superwhisper",
            Self::Spokenly => "spokenly",
        }
    }

    pub fn parse(source: &str) -> Option<Self> {
        [Self::Wispr, Self::Superwhisper, Self::Spokenly]
            .into_iter()
            .find(|s| s.as_str() == source)
    }

    /// The end of the meeting ids it gives.
    fn suffix(self) -> &'static str {
        match self {
            Self::Wispr => "wf",
            Self::Superwhisper => "sw",
            Self::Spokenly => "sp",
        }
    }
}

/// One meeting as another app kept it.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceMeeting {
    pub source: Source,
    pub source_id: String,
    /// Unix ms.
    pub started_at_ms: i64,
    pub duration_ms: i64,
    pub title: Option<String>,
    pub summary: Option<String>,
    /// `s1`, `s2`, ... in the order they first speak, with their names.
    pub speakers: Vec<MeetingSpeaker>,
    pub segments: Vec<MeetingSegment>,
    pub audio: Option<PathBuf>,
    /// False for a file Spokenly transcribed rather than a meeting it
    /// recorded: one that matches no meeting isn't imported.
    pub is_meeting: bool,
}

impl SourceMeeting {
    fn record(&self) -> (Source, String) {
        (self.source, self.source_id.clone())
    }

    fn is_stub(&self) -> bool {
        self.duration_ms < MIN_DURATION_MS || self.segments.iter().all(|s| s.text.trim().is_empty())
    }

    fn same_meeting(&self, other: &Self) -> bool {
        let longer = self.duration_ms.max(other.duration_ms);
        (self.started_at_ms - other.started_at_ms).abs() <= SAME_START_MS
            && (self.duration_ms - other.duration_ms).abs() * 100 <= SAME_DURATION_PCT * longer
    }
}

/// What one app's folder held.
#[derive(Debug, Default)]
pub struct Scan {
    pub meetings: Vec<SourceMeeting>,
    /// A record that failed to read, and why; never its text.
    pub unreadable: Vec<String>,
}

impl Scan {
    fn unreadable(&mut self, what: &Path, error: impl std::fmt::Display) {
        let error: String = error.to_string().chars().take(160).collect();
        self.unreadable.push(format!("{}: {error}", what.display()));
    }
}

/// A JSON record that failed to parse: where, never the value it choked
/// on, which can be what someone said.
fn json_error(e: serde_json::Error) -> anyhow::Error {
    anyhow::anyhow!(
        "{:?} error at line {} column {}",
        e.classify(),
        e.line(),
        e.column()
    )
}

/// Numbers a record's speakers `s1`, `s2`, ... in the order they first
/// speak, whatever the app called them.
#[derive(Default)]
struct SpeakerKeys {
    raw: Vec<String>,
    speakers: Vec<MeetingSpeaker>,
}

impl SpeakerKeys {
    /// The key of the speaker the app calls `raw`, named `name`.
    fn key(&mut self, raw: &str, name: Option<&str>) -> String {
        let index = match self.raw.iter().position(|r| r == raw) {
            Some(index) => index,
            None => {
                self.raw.push(raw.to_string());
                self.speakers.push(MeetingSpeaker {
                    key: format!("s{}", self.raw.len()),
                    label: None,
                });
                self.raw.len() - 1
            }
        };
        let speaker = &mut self.speakers[index];
        if speaker.label.is_none() {
            speaker.label = name.and_then(real_name);
        }
        speaker.key.clone()
    }

    /// The name of the speaker the app calls `raw`, or `Speaker n`.
    fn name(&self, raw: &str) -> Option<String> {
        let index = self.raw.iter().position(|r| r == raw)?;
        let speaker = &self.speakers[index];
        Some(
            speaker
                .label
                .clone()
                .unwrap_or_else(|| format!("Speaker {}", index + 1)),
        )
    }
}

/// A name someone gave, not an app's `Speaker 2`.
fn real_name(name: &str) -> Option<String> {
    let name = name.trim();
    let generic = name
        .strip_prefix("Speaker ")
        .is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()));
    (!name.is_empty() && !generic).then(|| name.to_string())
}

/// The audio file beside `stem` (`<stem>.m4a`, ...), if there is one.
fn audio_beside(stem: &Path) -> Option<PathBuf> {
    ["m4a", "wav", "ogg", "opus", "mp3", "aac", "flac"]
        .into_iter()
        .map(|ext| stem.with_extension(ext))
        .find(|path| path.is_file())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Too short, or nobody said anything.
    Stub,
    /// One of its records was imported before.
    AlreadyImported,
    /// A file Spokenly transcribed that matches no meeting.
    NotAMeeting,
    /// Only Spokenly has it, without its audio.
    NoAudio,
    Import,
}

/// What one real meeting becomes.
#[derive(Clone, Debug, PartialEq)]
pub struct Planned {
    /// `<local start>-<wf|sw|sp>`, by its primary record.
    pub id: String,
    /// Where its names, summary and transcript come from.
    pub primary: SourceMeeting,
    /// The other records of the same meeting.
    pub merged: Vec<SourceMeeting>,
    /// Spokenly's recording, else Superwhisper's.
    pub audio: Option<PathBuf>,
    pub outcome: Outcome,
}

impl Planned {
    /// Only Spokenly had it, so it's transcribed here from its audio.
    pub fn needs_transcription(&self) -> bool {
        self.primary.source == Source::Spokenly
    }

    pub fn records(&self) -> impl Iterator<Item = (Source, String)> + '_ {
        std::iter::once(&self.primary)
            .chain(&self.merged)
            .map(SourceMeeting::record)
    }
}

/// The id a meeting started at `started_at_ms` gets from `source`.
pub fn meeting_id(started_at_ms: i64, source: Source) -> String {
    let start = chrono::DateTime::from_timestamp_millis(started_at_ms)
        .unwrap_or_default()
        .with_timezone(&chrono::Local);
    format!("{}-{}", start.format("%Y-%m-%d_%H-%M-%S"), source.suffix())
}

/// One entry per real meeting in `found`, and one per stub. `seen` is every
/// record imported before. Pure: its output is the dry run.
pub fn plan(found: Vec<SourceMeeting>, seen: &HashSet<(Source, String)>) -> Vec<Planned> {
    let (stubs, mut real): (Vec<_>, Vec<_>) = found.into_iter().partition(SourceMeeting::is_stub);
    real.sort_by_key(|r| (r.source, r.started_at_ms));

    // Each group's first record is its primary: sorting by source put the
    // highest precedence first.
    let mut groups: Vec<Vec<SourceMeeting>> = Vec::new();
    for record in real {
        let closest = groups
            .iter_mut()
            .filter(|g| g.iter().all(|r| r.source != record.source) && g[0].same_meeting(&record))
            .min_by_key(|g| (g[0].started_at_ms - record.started_at_ms).abs());
        match closest {
            Some(group) => group.push(record),
            None => groups.push(vec![record]),
        }
    }

    let stubs = stubs.into_iter().map(|stub| Planned {
        id: meeting_id(stub.started_at_ms, stub.source),
        audio: None,
        primary: stub,
        merged: Vec::new(),
        outcome: Outcome::Stub,
    });
    let meetings = groups.into_iter().map(|mut group| {
        let primary = group.remove(0);
        let audio = [Source::Spokenly, Source::Superwhisper]
            .into_iter()
            .find_map(|source| {
                std::iter::once(&primary)
                    .chain(&group)
                    .find(|r| r.source == source)
                    .and_then(|r| r.audio.clone())
            });
        let mut planned = Planned {
            id: meeting_id(primary.started_at_ms, primary.source),
            primary,
            merged: group,
            audio,
            outcome: Outcome::Import,
        };
        planned.outcome = if planned.records().any(|r| seen.contains(&r)) {
            Outcome::AlreadyImported
        } else if planned.needs_transcription() && !planned.primary.is_meeting {
            Outcome::NotAMeeting
        } else if planned.needs_transcription() && planned.audio.is_none() {
            Outcome::NoAudio
        } else {
            Outcome::Import
        };
        planned
    });
    meetings.chain(stubs).collect()
}

#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod plan_tests;
#[cfg(test)]
mod scan_tests;
