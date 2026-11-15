//! Meetings recorded in other apps, imported once. Each app's records are
//! read into [`SourceMeeting`]s, [`plan`] folds the records of one real
//! meeting together, and every record it consumed is kept in
//! `meeting_sources`, so a second run, or a meeting deleted since, imports
//! nothing.

mod audio;
mod db;
mod spokenly;
mod superwhisper;
mod wispr;

use anyhow::{anyhow, Context, Result};
use log::{info, warn};
use rusqlite::Connection;
use serde::Serialize;
use specta::Type;
use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, Manager};

use super::store::{self, MeetingSegment, MeetingSpeaker};
use super::{notes, Meeting, MeetingManager, MeetingStatus, MeetingTrack, Rerun, TrackKind};
use crate::managers::history::HistoryManager;

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

/// Where each app keeps its meetings, under the user's home. They're Mac
/// apps: elsewhere these don't exist and an import finds nothing.
pub struct Roots {
    pub superwhisper: PathBuf,
    pub wispr: PathBuf,
    pub spokenly: PathBuf,
}

impl Roots {
    pub fn under(home: &Path) -> Self {
        let support = home.join("Library").join("Application Support");
        Self {
            superwhisper: home.join("superwhisper").join("recordings"),
            wispr: support.join("Wispr Flow"),
            spokenly: support.join("Spokenly").join("History"),
        }
    }

    fn scan(&self) -> Vec<(Source, Scan)> {
        vec![
            (Source::Wispr, wispr::scan(&self.wispr)),
            (Source::Superwhisper, superwhisper::scan(&self.superwhisper)),
            (Source::Spokenly, spokenly::scan(&self.spokenly)),
        ]
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Type)]
pub struct SourceCount {
    pub source: Source,
    pub found: u32,
    pub unreadable: u32,
    pub stubs: u32,
}

/// What an import did, or in a dry run would do. Ids and counts only.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Type)]
pub struct ImportReport {
    pub dry_run: bool,
    pub sources: Vec<SourceCount>,
    /// Records folded into another app's copy of the same meeting.
    pub merged: u32,
    pub already_imported: u32,
    /// Files Spokenly transcribed that match no meeting.
    pub not_meetings: u32,
    /// Meetings only Spokenly had, whose audio is gone.
    pub no_audio: u32,
    /// Meetings imported with their transcript.
    pub imported: u32,
    /// Of those, the ones no app kept audio for.
    pub without_audio: u32,
    /// Meetings only Spokenly had, to be transcribed here.
    pub to_transcribe: u32,
    pub failed: u32,
    pub notes: u32,
    /// What failed, by path or meeting id.
    pub errors: Vec<String>,
}

impl fmt::Display for ImportReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.dry_run {
            writeln!(f, "Dry run: nothing was written.")?;
        }
        for s in &self.sources {
            writeln!(
                f,
                "{}: {} found, {} unreadable, {} stubs",
                s.source.as_str(),
                s.found,
                s.unreadable,
                s.stubs
            )?;
        }
        writeln!(f, "merged into another app's copy: {}", self.merged)?;
        writeln!(f, "already imported: {}", self.already_imported)?;
        writeln!(
            f,
            "spokenly files that are no meeting: {}",
            self.not_meetings
        )?;
        writeln!(f, "spokenly meetings without audio: {}", self.no_audio)?;
        writeln!(f, "imported with transcript: {}", self.imported)?;
        writeln!(f, "  of which without audio: {}", self.without_audio)?;
        writeln!(f, "to transcribe: {}", self.to_transcribe)?;
        writeln!(f, "notes written: {}", self.notes)?;
        writeln!(f, "failed: {}", self.failed)?;
        for error in &self.errors {
            writeln!(f, "  {error}")?;
        }
        Ok(())
    }
}

/// Imports every meeting under `roots` not imported before into
/// `meetings_dir`, and its note into `notes_folder` when that's set.
/// `write` saves `meeting.json`. A dry run writes nothing. Returns the ids
/// of the meetings left to transcribe.
pub fn run(
    conn: &mut Connection,
    roots: &Roots,
    meetings_dir: &Path,
    dry_run: bool,
    notes_folder: Option<&Path>,
    write: &mut dyn FnMut(&Meeting) -> Result<()>,
) -> Result<(ImportReport, Vec<String>)> {
    let mut report = ImportReport {
        dry_run,
        ..Default::default()
    };
    let mut found = Vec::new();
    for (source, scan) in roots.scan() {
        report.sources.push(SourceCount {
            source,
            found: (scan.meetings.len() + scan.unreadable.len()) as u32,
            unreadable: scan.unreadable.len() as u32,
            stubs: 0,
        });
        report.errors.extend(scan.unreadable);
        found.extend(scan.meetings);
    }
    // A record that can't be read may be a meeting's best copy: imported
    // without it, the worse copy would be in the ledger for good.
    if !dry_run && !report.errors.is_empty() {
        report.failed = report.errors.len() as u32;
        report
            .errors
            .push("Nothing was imported. Fix or move the records above, then run again.".into());
        return Ok((report, Vec::new()));
    }
    if !dry_run {
        db::create_ledger(conn)?;
    }
    let mut planned = plan(found, &db::seen(conn)?);
    planned.sort_by(|a, b| a.id.cmp(&b.id));

    let mut queued = Vec::new();
    for meeting in &planned {
        match meeting.outcome {
            Outcome::Stub => {
                if let Some(count) = report
                    .sources
                    .iter_mut()
                    .find(|s| s.source == meeting.primary.source)
                {
                    count.stubs += 1;
                }
                continue;
            }
            Outcome::AlreadyImported => {
                report.already_imported += 1;
                // Imported before without a note, or transcribed since.
                if let (Some(folder), false) = (notes_folder, dry_run) {
                    if let Some(id) = db::meeting_of(conn, meeting)? {
                        write_note(conn, &id, folder, &mut report);
                    }
                }
            }
            Outcome::NotAMeeting => report.not_meetings += 1,
            Outcome::NoAudio => report.no_audio += 1,
            Outcome::Import => {}
        }
        if meeting.outcome != Outcome::Import {
            continue;
        }
        report.merged += meeting.merged.len() as u32;
        if meeting.audio.is_none() && !meeting.needs_transcription() {
            report.without_audio += 1;
        }
        if dry_run {
            match meeting.needs_transcription() {
                true => report.to_transcribe += 1,
                false => report.imported += 1,
            }
            continue;
        }
        match apply(conn, meeting, meetings_dir, write) {
            Ok(saved) if saved.status == MeetingStatus::Transcribing => {
                report.to_transcribe += 1;
                queued.push(saved.id);
            }
            Ok(saved) => {
                report.imported += 1;
                if let Some(folder) = notes_folder {
                    write_note(conn, &saved.id, folder, &mut report);
                }
            }
            Err(e) => {
                report.failed += 1;
                report.errors.push(format!("{}: {e:#}", meeting.id));
            }
        }
    }
    Ok((report, queued))
}

/// Writes meeting `id`'s note when it's transcribed and has none: a note
/// the user deleted stays deleted, and a deleted meeting gets none.
fn write_note(conn: &Connection, id: &str, folder: &Path, report: &mut ImportReport) {
    let note = store::get(conn, id).and_then(|detail| match detail {
        Some(d) if d.meeting.status.as_deref() == Some("done") && d.meeting.note_path.is_none() => {
            notes::export(conn, &d, folder).map(Some)
        }
        _ => Ok(None),
    });
    match note {
        Ok(Some(_)) => report.notes += 1,
        Ok(None) => {}
        Err(e) => report.errors.push(format!("{id} note: {e:#}")),
    }
}

/// Copies the meeting's audio and summary into its folder, then saves it.
fn apply(
    conn: &mut Connection,
    planned: &Planned,
    meetings_dir: &Path,
    write: &mut dyn FnMut(&Meeting) -> Result<()>,
) -> Result<Meeting> {
    let primary = &planned.primary;
    let dir = meetings_dir.join(&planned.id);
    fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    let mut duration_ms = primary.duration_ms;
    let (status, tracks) = match (&planned.audio, planned.needs_transcription()) {
        (Some(audio), true) => {
            let file = dir.join(TrackKind::Mixed.file_name());
            duration_ms = audio::decode_to_wav(audio, &file)?;
            (MeetingStatus::Transcribing, vec![(file, duration_ms)])
        }
        (None, true) => return Err(anyhow!("No audio to transcribe")),
        // Kept as it came: only a 16 kHz WAV can be transcribed again.
        (Some(audio), false) => (
            MeetingStatus::Done,
            vec![(audio::copy_into(audio, &dir)?, duration_ms)],
        ),
        (None, false) => (MeetingStatus::Done, Vec::new()),
    };
    if let Some(summary) = primary.summary.as_deref().map(str::trim) {
        if !summary.is_empty() {
            fs::write(dir.join("summary.md"), format!("{summary}\n"))?;
        }
    }
    let started_at = chrono::DateTime::from_timestamp_millis(primary.started_at_ms)
        .ok_or_else(|| anyhow!("Start time out of range"))?
        .with_timezone(&chrono::Local);
    let meeting = Meeting {
        id: planned.id.clone(),
        started_at: started_at.to_rfc3339(),
        duration_ms,
        status,
        dir,
        tracks: tracks
            .into_iter()
            .map(|(file, duration_ms)| MeetingTrack {
                kind: TrackKind::Mixed,
                file,
                start_offset_ms: 0,
                duration_ms,
            })
            .collect(),
        asr_model: None,
        diar_model: None,
        language: None,
        attempts: 0,
    };
    db::insert(conn, planned, &meeting)?;
    write(&meeting)?;
    Ok(meeting)
}

/// Imports the user's old meetings into the app's meetings, with notes
/// when `write_notes` and a vault is set. With `requeue`, those only
/// Spokenly had are queued for transcription; otherwise they wait for the
/// next start.
pub fn import(
    app: &AppHandle,
    dry_run: bool,
    write_notes: bool,
    requeue: bool,
) -> Result<ImportReport> {
    let home = app.path().home_dir()?;
    let meetings_dir = crate::portable::app_data_dir(app)?
        .join("recordings")
        .join("meetings");
    let notes_folder = match write_notes {
        true => Some(
            notes::notes_folder(&crate::settings::get_settings(app), &home)?
                .ok_or_else(|| anyhow!("Set an Obsidian vault to write notes"))?,
        ),
        false => None,
    };
    let mut conn = app.state::<Arc<HistoryManager>>().get_connection()?;
    let (report, queued) = run(
        &mut conn,
        &Roots::under(&home),
        &meetings_dir,
        dry_run,
        notes_folder.as_deref(),
        &mut |meeting| super::write_meeting(app, meeting),
    )?;
    info!(
        "Meeting import{}: {} with transcript, {} to transcribe, {} failed",
        if dry_run { " (dry run)" } else { "" },
        report.imported,
        report.to_transcribe,
        report.failed
    );
    if requeue {
        if let Some(meetings) = app.try_state::<Arc<MeetingManager>>() {
            for id in queued {
                if let Err(e) = meetings.requeue(&id, Rerun::All) {
                    warn!("Imported meeting {id} waits for the next start: {e:#}");
                }
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod plan_tests;
#[cfg(test)]
mod run_tests;
#[cfg(test)]
mod scan_tests;
