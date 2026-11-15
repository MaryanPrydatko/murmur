//! Meeting recording: the microphone and the default output's monitor, each
//! streamed to its own WAV, then transcribed into who said what and when.
//!
//! The meeting folder is the record: `recordings/meetings/<id>/` holds
//! `mic.wav`, `system.wav`, `meeting.json` and, once transcribed,
//! `transcript.md`. `history.db` holds the same meeting for listing, and
//! the names the user gives its speakers (see [`store`]).

mod capture;
pub mod import;
pub mod notes;
mod pipeline;
pub mod store;
mod transcript;

pub use transcript::{Speaker, Utterance};

use anyhow::{anyhow, Context, Result};
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

use crate::managers::history::HistoryManager;
use crate::managers::transcription_jobs::TranscriptionJobManager;
use crate::notices::{self, Notice, NoticeCode, NoticeSource};
use crate::settings::get_settings;
use capture::{
    default_monitor_source, monitor_device_id, open_track, stop_track, TrackFeed, TrackRecorder,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeetingStatus {
    Recording,
    Transcribing,
    Done,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Mic,
    System,
    /// Imported audio with everyone on it, told apart like the system track.
    Mixed,
}

impl TrackKind {
    fn file_name(self) -> &'static str {
        match self {
            Self::Mic => "mic.wav",
            Self::System => "system.wav",
            Self::Mixed => "mixed.wav",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MeetingTrack {
    pub kind: TrackKind,
    pub file: PathBuf,
    /// When the track's first sample was heard, from the meeting's start.
    pub start_offset_ms: i64,
    pub duration_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Meeting {
    /// The local start time, `2026-11-15_14-03-22`; also the folder name.
    pub id: String,
    /// RFC 3339, local time.
    pub started_at: String,
    pub duration_ms: i64,
    pub status: MeetingStatus,
    pub dir: PathBuf,
    pub tracks: Vec<MeetingTrack>,
    /// The models it was transcribed and diarized with, once it was.
    pub asr_model: Option<String>,
    pub diar_model: Option<String>,
    /// The language of the track with the most words, once transcribed.
    #[serde(default)]
    pub language: Option<String>,
    /// Transcriptions started, counted before each runs so a crash counts.
    #[serde(default)]
    pub attempts: u8,
}

/// What [`MeetingManager::requeue`] runs again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rerun {
    /// Every track transcribed and diarized again.
    All,
    /// Only the speakers, from the words each track already has.
    Speakers,
}

struct ActiveMeeting {
    meeting: Meeting,
    started: Instant,
    tracks: Vec<TrackRecorder>,
    /// Set when it stops, which ends its ticker.
    ended: Arc<AtomicBool>,
}

/// Records one meeting at a time, beside dictation: its recorders are its
/// own, so a dictation opens the microphone again alongside them.
pub struct MeetingManager {
    app: AppHandle,
    active: Mutex<Option<ActiveMeeting>>,
    /// Read by the tray without waiting on a start or stop in progress.
    recording: AtomicBool,
}

impl MeetingManager {
    pub fn new(app: &AppHandle) -> Self {
        Self {
            app: app.clone(),
            active: Mutex::new(None),
            recording: AtomicBool::new(false),
        }
    }

    pub fn is_recording(&self) -> bool {
        self.recording.load(Ordering::Acquire)
    }

    fn lock(&self) -> MutexGuard<'_, Option<ActiveMeeting>> {
        self.active.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Starts a meeting, or stops the one recording and queues its
    /// transcription. Opens devices, so call it off the main thread.
    pub fn toggle(&self) {
        let mut active = self.lock();
        let result = match active.take() {
            Some(meeting) => self.stop(meeting),
            None => self.start().map(|meeting| *active = Some(meeting)),
        };
        self.recording.store(active.is_some(), Ordering::Release);
        drop(active);
        if let Err(e) = result {
            error!("Meeting recording failed: {e:#}");
            notices::raise(
                &self.app,
                Notice::new(NoticeCode::MeetingRecordingFailed, NoticeSource::Meeting)
                    .with_detail(format!("{e:#}")),
            );
        }
        crate::tray::update_tray_menu(&self.app);
    }

    /// Queues the meeting in `id`'s folder to be transcribed again from its
    /// `meeting.json`.
    pub fn requeue(&self, id: &str, rerun: Rerun) -> Result<()> {
        Err(anyhow!(
            "Can't transcribe meeting {id} again yet ({rerun:?})"
        ))
    }

    /// Writes the offsets of the tracks whose audio has arrived into the
    /// recording meeting `id`'s `meeting.json`.
    fn save_offsets(&self, id: &str) {
        let mut active = self.lock();
        let Some(active) = active.as_mut().filter(|a| a.meeting.id == id) else {
            return;
        };
        for track in &active.tracks {
            if let Some(listed) = active
                .meeting
                .tracks
                .iter_mut()
                .find(|t| t.kind == track.kind)
            {
                listed.start_offset_ms = track.feed.offset_ms(active.started).unwrap_or(0);
            }
        }
        if let Err(e) = write_meeting(&self.app, &active.meeting) {
            warn!("Failed to save meeting {id}'s track offsets: {e:#}");
        }
    }

    fn start(&self) -> Result<ActiveMeeting> {
        let devices = handy_recorder::list_input_devices()?;
        let monitor_source = default_monitor_source()?;
        let system = monitor_device_id(
            devices.iter().map(|d| (d.id.as_str(), d.is_monitor)),
            &monitor_source,
        )
        .ok_or_else(|| anyhow!("No input device for the output monitor {monitor_source}"))?
        .to_string();
        // Like dictation: the selected microphone by name, else the default.
        let mic = get_settings(&self.app)
            .selected_microphone
            .and_then(|name| {
                devices
                    .iter()
                    .find(|d| d.name == name && !d.is_monitor)
                    .map(|d| d.id.clone())
            });
        info!("Meeting devices: mic {mic:?}, system {system}");

        let now = chrono::Local::now();
        let id = now.format("%Y-%m-%d_%H-%M-%S").to_string();
        let dir = crate::portable::app_data_dir(&self.app)?
            .join("recordings")
            .join("meetings")
            .join(&id);
        std::fs::create_dir_all(dir.parent().expect("the meetings folder"))?;
        std::fs::create_dir(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;

        let started = Instant::now();
        let tracks = [(TrackKind::Mic, mic), (TrackKind::System, Some(system))]
            .into_iter()
            .map(|(kind, device)| open_track(&dir, kind, device))
            .collect::<Result<Vec<_>>>();
        let tracks = match tracks {
            Ok(tracks) => tracks,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(e);
            }
        };
        // Listed from the start, so a meeting that never stops is still found.
        let listed = tracks
            .iter()
            .map(|track| MeetingTrack {
                kind: track.kind,
                file: track.file.clone(),
                start_offset_ms: 0,
                duration_ms: 0,
            })
            .collect();
        let meeting = Meeting {
            id,
            started_at: now.to_rfc3339(),
            duration_ms: 0,
            status: MeetingStatus::Recording,
            dir,
            tracks: listed,
            asr_model: None,
            diar_model: None,
            language: None,
            attempts: 0,
        };
        write_meeting(&self.app, &meeting)?;
        notices::clear(&self.app, NoticeSource::Meeting);
        info!("Meeting {} recording", meeting.id);
        let ended = Arc::new(AtomicBool::new(false));
        let feeds = tracks.iter().map(|t| Arc::clone(&t.feed)).collect();
        let (app, id, until) = (self.app.clone(), meeting.id.clone(), Arc::clone(&ended));
        std::thread::Builder::new()
            .name("meeting-ticker".into())
            .spawn(move || tick(&app, &id, feeds, started, &until))?;
        Ok(ActiveMeeting {
            meeting,
            started,
            tracks,
            ended,
        })
    }

    fn stop(&self, active: ActiveMeeting) -> Result<()> {
        let ActiveMeeting {
            mut meeting,
            started,
            tracks,
            ended,
        } = active;
        ended.store(true, Ordering::Release);
        meeting.duration_ms = started.elapsed().as_millis() as i64;
        meeting.tracks.clear();
        let mut failure = None;
        for track in tracks {
            match stop_track(&track, started) {
                Ok((start_offset_ms, duration_ms)) => meeting.tracks.push(MeetingTrack {
                    kind: track.kind,
                    file: track.file,
                    start_offset_ms,
                    duration_ms,
                }),
                Err(e) => failure = Some(e.context(format!("{:?} track", track.kind))),
            }
            if let Err(e) = track.recorder.close() {
                warn!("Failed to close a meeting recorder: {e}");
            }
        }
        if let Some(e) = failure {
            meeting.status = MeetingStatus::Failed;
            write_meeting(&self.app, &meeting)?;
            return Err(e);
        }
        meeting.status = MeetingStatus::Transcribing;
        write_meeting(&self.app, &meeting)?;
        info!(
            "Meeting {} stopped after {:.1}s; queued for transcription",
            meeting.id,
            meeting.duration_ms as f64 / 1000.0
        );
        let app = self.app.clone();
        self.app
            .state::<Arc<TranscriptionJobManager>>()
            .queue_meeting(Box::new(move |control| {
                pipeline::transcribe(&app, meeting, control)
            }));
        Ok(())
    }
}
/// How often a recording meeting's ticker runs.
const TICK: Duration = Duration::from_millis(1000 / 30);

/// Runs while the meeting `id` records: saves each track's start offset
/// once its audio arrives, so a meeting that never stops can still be lined
/// up.
fn tick(
    app: &AppHandle,
    id: &str,
    feeds: Vec<Arc<TrackFeed>>,
    started: Instant,
    ended: &AtomicBool,
) {
    let mut known = vec![false; feeds.len()];
    while !ended.load(Ordering::Acquire) {
        let mut arrived = false;
        for (feed, known) in feeds.iter().zip(&mut known) {
            if !*known && feed.offset_ms(started).is_some() {
                *known = true;
                arrived = true;
            }
        }
        if arrived {
            app.state::<Arc<MeetingManager>>().save_offsets(id);
        }
        std::thread::sleep(TICK);
    }
}

/// The meeting in `dir`, from its `meeting.json`.
pub(crate) fn read_meeting(dir: &Path) -> Result<Meeting> {
    let path = dir.join("meeting.json");
    let json =
        std::fs::read(&path).with_context(|| format!("Failed to read {}", path.display()))?;
    serde_json::from_slice(&json).with_context(|| format!("Failed to parse {}", path.display()))
}

/// Replaces `meeting.json` whole: written aside and renamed, so a crash
/// mid-write never leaves half a record.
fn write_meeting_json(meeting: &Meeting) -> Result<()> {
    let path = meeting.dir.join("meeting.json");
    let partial = meeting.dir.join("meeting.json.partial");
    std::fs::write(&partial, serde_json::to_vec_pretty(meeting)?)
        .and_then(|()| std::fs::rename(&partial, &path))
        .with_context(|| format!("Failed to write {}", path.display()))
}

/// Writes `meeting.json`, then the meeting's row. The folder is the record:
/// a row that fails to save is only logged.
pub(crate) fn write_meeting(app: &AppHandle, meeting: &Meeting) -> Result<()> {
    write_meeting_json(meeting)?;
    let history = app.state::<Arc<HistoryManager>>();
    if let Err(e) = history
        .get_connection()
        .and_then(|mut conn| store::save(&mut conn, meeting))
    {
        error!("Failed to store meeting {}: {e:#}", meeting.id);
    }
    Ok(())
}
