//! Meetings the last run left unfinished: one still recording when the
//! process died gets its WAV headers repaired, and one mid-transcription is
//! queued again, up to [`MAX_ATTEMPTS`] times.

use anyhow::{bail, Context, Result};
use log::{error, info, warn};
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

use super::{
    meetings_dir, read_meeting, write_meeting, Meeting, MeetingManager, MeetingStatus,
    MeetingTrack, Rerun, TrackKind,
};
use crate::managers::transcription_jobs::TranscriptionJobManager;
use crate::notices::{self, Notice, NoticeCode, NoticeSource};

/// Transcriptions a meeting gets before it's left failed: one that crashes
/// the app every time must not crash it on every start.
pub(super) const MAX_ATTEMPTS: u8 = 3;

/// What an unfinished meeting needs.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Recovery {
    /// It was recording: repair its tracks, then transcribe it.
    Repair,
    /// Transcribe it again.
    Requeue,
    /// It used up its attempts.
    GiveUp,
}

pub(super) fn recovery(meeting: &Meeting) -> Option<Recovery> {
    match meeting.status {
        MeetingStatus::Recording => Some(Recovery::Repair),
        MeetingStatus::Transcribing if meeting.attempts < MAX_ATTEMPTS => Some(Recovery::Requeue),
        MeetingStatus::Transcribing => Some(Recovery::GiveUp),
        MeetingStatus::Done | MeetingStatus::Failed => None,
    }
}

/// Recovers every unfinished meeting but the one recording now. Run once at
/// start, off the main thread.
pub fn reconcile(app: &AppHandle) {
    let dir = match meetings_dir(app) {
        Ok(dir) => dir,
        Err(e) => return error!("Failed to find the meetings folder: {e:#}"),
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let active = app.state::<Arc<MeetingManager>>().active_id();
    for entry in entries.flatten() {
        let folder = entry.path();
        if !folder.join("meeting.json").exists() {
            continue;
        }
        let mut meeting = match read_meeting(&folder) {
            Ok(meeting) => meeting,
            Err(e) => {
                warn!("Skipping meeting {}: {e:#}", folder.display());
                continue;
            }
        };
        if active.as_deref() == Some(meeting.id.as_str()) {
            continue;
        }
        let Some(recovery) = recovery(&meeting) else {
            continue;
        };
        info!("Recovering meeting {}: {recovery:?}", meeting.id);
        let id = meeting.id.clone();
        let jobs = app.state::<Arc<TranscriptionJobManager>>();
        let result = match recovery {
            Recovery::Repair => jobs.queue_meeting(&id, Rerun::All, || {
                repair_meeting(&mut meeting, &folder);
                write_meeting(app, &meeting)
            }),
            Recovery::Requeue => jobs.queue_meeting(&id, Rerun::All, || Ok(())),
            Recovery::GiveUp => {
                meeting.status = MeetingStatus::Failed;
                notices::raise(
                    app,
                    Notice::new(
                        NoticeCode::MeetingTranscriptionFailed,
                        NoticeSource::Meeting,
                    )
                    .with_detail(format!(
                        "Stopped after {} tries at transcribing meeting {id}",
                        meeting.attempts
                    )),
                );
                write_meeting(app, &meeting)
            }
        };
        if let Err(e) = result {
            error!("Failed to recover meeting {id}: {e:#}");
        }
    }
}

/// Repairs the tracks of a meeting that was recording when the process
/// died and gives it the length they have. A meeting from before tracks
/// were listed while recording gets the WAVs in its folder.
pub(super) fn repair_meeting(meeting: &mut Meeting, folder: &Path) {
    if meeting.tracks.is_empty() {
        meeting.tracks = [TrackKind::Mic, TrackKind::System]
            .into_iter()
            .map(|kind| MeetingTrack {
                kind,
                file: folder.join(kind.file_name()),
                start_offset_ms: 0,
                duration_ms: 0,
            })
            .filter(|track| track.file.exists())
            .collect();
    }
    meeting
        .tracks
        .retain_mut(|track| match repair_wav(&track.file) {
            Ok(samples) => {
                track.duration_ms = i64::from(samples) * 1000 / 16_000;
                true
            }
            Err(e) => {
                warn!(
                    "Dropping meeting {}'s {:?} track: {e:#}",
                    meeting.id, track.kind
                );
                false
            }
        });
    meeting.duration_ms = meeting
        .tracks
        .iter()
        .map(|t| t.start_offset_ms + t.duration_ms)
        .max()
        .unwrap_or(0);
    meeting.status = MeetingStatus::Transcribing;
}

/// Sets a 16-bit mono WAV's sizes from the file's length, as a process that
/// died before finalizing it never did. Returns its samples.
pub(super) fn repair_wav(path: &Path) -> Result<u32> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("Failed to open {}", path.display()))?;
    let mut riff = [0u8; 12];
    file.read_exact(&mut riff)?;
    if &riff[..4] != b"RIFF" || &riff[8..] != b"WAVE" {
        bail!("{} is not a WAV", path.display());
    }
    let len = file.metadata()?.len();
    let mut at = 12u64;
    loop {
        let mut header = [0u8; 8];
        file.seek(SeekFrom::Start(at))?;
        file.read_exact(&mut header)
            .with_context(|| format!("{} has no data chunk", path.display()))?;
        let size = u64::from(u32::from_le_bytes(header[4..].try_into()?));
        if &header[..4] == b"data" {
            let data = at + 8;
            // Whole samples only.
            let bytes = (len - data.min(len)) & !1;
            let bytes = u32::try_from(bytes).context("The track is over 4 GB")?;
            file.set_len(data + u64::from(bytes))?;
            file.seek(SeekFrom::Start(at + 4))?;
            file.write_all(&bytes.to_le_bytes())?;
            file.seek(SeekFrom::Start(4))?;
            file.write_all(&(data as u32 - 8 + bytes).to_le_bytes())?;
            file.sync_all()?;
            return Ok(bytes / 2);
        }
        at += 8 + size + (size & 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn meeting(status: MeetingStatus, attempts: u8) -> Meeting {
        Meeting {
            id: "2026-11-15_14-03-22".into(),
            started_at: "2026-11-15T14:03:22+01:00".into(),
            duration_ms: 0,
            status,
            dir: PathBuf::new(),
            tracks: Vec::new(),
            asr_model: None,
            diar_model: None,
            language: None,
            attempts,
        }
    }

    /// A finalized 16 kHz mono WAV of `samples` samples.
    fn wav(path: &Path, samples: usize) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..samples {
            writer.write_sample(i as i16).unwrap();
        }
        writer.finalize().unwrap();
    }

    /// Zeroes the RIFF and data sizes, as `WavWriter::create` leaves them.
    fn unfinalize(path: &Path) {
        let mut bytes = std::fs::read(path).unwrap();
        let data = bytes.windows(4).position(|w| w == b"data").unwrap();
        bytes[4..8].fill(0);
        bytes[data + 4..data + 8].fill(0);
        std::fs::write(path, bytes).unwrap();
    }

    fn samples(path: &Path) -> Vec<i16> {
        hound::WavReader::open(path)
            .unwrap()
            .into_samples()
            .map(Result::unwrap)
            .collect()
    }

    #[test]
    fn repair_restores_a_zero_length_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mic.wav");
        wav(&path, 1_000);
        unfinalize(&path);
        assert_eq!(hound::WavReader::open(&path).unwrap().duration(), 0);
        assert_eq!(repair_wav(&path).unwrap(), 1_000);
        assert_eq!(samples(&path), (0..1_000).collect::<Vec<i16>>());
    }

    #[test]
    fn repair_drops_half_a_sample() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mic.wav");
        wav(&path, 10);
        unfinalize(&path);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&[7])
            .unwrap();
        assert_eq!(repair_wav(&path).unwrap(), 10);
        assert_eq!(samples(&path).len(), 10);
        // Repairing a sound file changes nothing.
        assert_eq!(repair_wav(&path).unwrap(), 10);
    }

    #[test]
    fn a_meeting_cut_off_while_recording_is_repaired_from_its_folder() {
        let dir = tempfile::tempdir().unwrap();
        wav(&dir.path().join("mic.wav"), 16_000);
        wav(&dir.path().join("system.wav"), 32_000);
        unfinalize(&dir.path().join("system.wav"));
        // Written before meeting.json listed the tracks while recording.
        let mut cut_off = meeting(MeetingStatus::Recording, 0);
        assert_eq!(recovery(&cut_off), Some(Recovery::Repair));
        repair_meeting(&mut cut_off, dir.path());
        let tracks: Vec<(TrackKind, i64)> = cut_off
            .tracks
            .iter()
            .map(|t| (t.kind, t.duration_ms))
            .collect();
        assert_eq!(
            tracks,
            [(TrackKind::Mic, 1_000), (TrackKind::System, 2_000)]
        );
        assert_eq!(cut_off.duration_ms, 2_000);
        assert_eq!(cut_off.status, MeetingStatus::Transcribing);
    }

    #[test]
    fn a_meeting_that_keeps_crashing_is_requeued_then_given_up() {
        let mut crashing = meeting(MeetingStatus::Transcribing, 0);
        let mut requeued = 0;
        while recovery(&crashing) == Some(Recovery::Requeue) {
            requeued += 1;
            // The job counts the attempt before it runs, then the app dies.
            crashing.begin_attempt();
        }
        assert_eq!(requeued, MAX_ATTEMPTS);
        assert_eq!(recovery(&crashing), Some(Recovery::GiveUp));
        assert_eq!(recovery(&meeting(MeetingStatus::Done, 0)), None);
        assert_eq!(recovery(&meeting(MeetingStatus::Failed, 5)), None);
    }
}
