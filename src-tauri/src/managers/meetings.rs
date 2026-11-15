//! Meeting recording: the microphone and the default output's monitor (on
//! macOS, a tap on the default output), each streamed to its own WAV, then
//! transcribed into who said what and when.
//!
//! The meeting folder is the record: `recordings/meetings/<id>/` holds
//! `mic.wav`, `system.wav`, `meeting.json` and, once transcribed,
//! `transcript.md`. `history.db` holds the same meeting for listing, and
//! the names the user gives its speakers (see [`store`]).

mod capture;
pub mod import;
pub mod notes;
mod pipeline;
mod recovery;
pub mod store;
mod transcript;

pub(crate) use pipeline::{fail_job, run_job};
pub use recovery::reconcile;
pub use transcript::{Speaker, Utterance};

use anyhow::{anyhow, bail, Context, Result};
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use tauri_specta::Event;

use crate::managers::history::HistoryManager;
use crate::managers::transcription_jobs::TranscriptionJobManager;
use crate::notices::{self, Notice, NoticeCode, NoticeSource};
use crate::settings::get_settings;
use capture::{
    default_monitor_source, monitor_input, open_recorder, open_track, stop_track, TrackFeed,
    TrackRecorder,
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
    /// Missing from meetings recorded before tracks had a length.
    #[serde(default)]
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

impl Meeting {
    /// Counts a transcription starting.
    fn begin_attempt(&mut self) {
        self.attempts = self.attempts.saturating_add(1);
        self.status = MeetingStatus::Transcribing;
    }
}

/// A meeting's folder or row changed: its status, tracks, title or
/// transcript.
#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct MeetingChanged {
    pub id: String,
}

/// What a meeting's transcription is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingStep {
    /// Transcribing the microphone.
    Mic,
    /// Transcribing the output, or imported audio.
    System,
    /// Telling the output's speakers apart, which reports no percent.
    Speakers,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct MeetingProgress {
    pub id: String,
    pub step: MeetingStep,
    pub percent: Option<u8>,
}

/// The loudest sample of each track since the last, 0 to 1, sent 30 times
/// a second while a meeting records.
#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct MeetingLevels {
    pub mic: f32,
    pub system: f32,
}

pub(crate) fn emit_changed(app: &AppHandle, id: &str) {
    if let Err(e) = (MeetingChanged { id: id.to_string() }).emit(app) {
        warn!("Failed to emit a meeting change: {e}");
    }
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
    /// Set when it stops, which ends its ticker and output watcher.
    ended: Arc<AtomicBool>,
}

/// Records one meeting at a time, beside dictation: its recorders are its
/// own, so a dictation opens the microphone again alongside them.
pub struct MeetingManager {
    app: AppHandle,
    /// Held through a start or stop, so the two never overlap; `active`
    /// is only held for moments, never while PulseAudio is asked anything.
    toggling: Mutex<()>,
    active: Mutex<Option<ActiveMeeting>>,
    /// What went wrong with the tracks of meetings stopped this run, by
    /// meeting id, for their transcription to warn about again after it
    /// clears the meeting's notices.
    track_problems: Mutex<HashMap<String, TrackProblems>>,
    /// Read by the tray without waiting on a start or stop in progress.
    recording: AtomicBool,
}

impl MeetingManager {
    pub fn new(app: &AppHandle) -> Self {
        Self {
            app: app.clone(),
            toggling: Mutex::new(()),
            active: Mutex::new(None),
            track_problems: Mutex::default(),
            recording: AtomicBool::new(false),
        }
    }

    pub fn is_recording(&self) -> bool {
        self.recording.load(Ordering::Acquire)
    }

    fn lock(&self) -> MutexGuard<'_, Option<ActiveMeeting>> {
        self.active.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// What went wrong with the tracks of the meeting `id` while it
    /// recorded, once.
    pub(crate) fn take_track_problems(&self, id: &str) -> TrackProblems {
        self.track_problems
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id)
            .unwrap_or_default()
    }

    /// The id of the meeting recording now.
    pub fn active_id(&self) -> Option<String> {
        self.lock().as_ref().map(|a| a.meeting.id.clone())
    }

    /// Stops a meeting that's recording as the app quits and leaves it for
    /// the next start to transcribe: the transcription engine may already
    /// be going away. Safe to call more than once.
    pub fn finish_for_exit(&self) {
        let Some(active) = self.lock().take() else {
            return;
        };
        self.recording.store(false, Ordering::Release);
        let id = active.meeting.id.clone();
        match self.finalize(active) {
            Ok(_) => info!("Meeting {id} stopped on exit; it's transcribed on the next start"),
            Err(e) => error!("Failed to stop meeting {id} on exit: {e:#}"),
        }
    }

    /// Starts a meeting, or stops the one recording and queues its
    /// transcription. Opens devices, so call it off the main thread.
    pub fn toggle(&self) {
        self.switch(None);
    }

    /// Like [`Self::toggle`], but only towards `recording`: a stop asked for
    /// twice doesn't start a meeting.
    pub fn set_recording(&self, recording: bool) {
        self.switch(Some(recording));
    }

    fn switch(&self, want: Option<bool>) {
        let _toggling = self.toggling.lock().unwrap_or_else(PoisonError::into_inner);
        if want == Some(self.is_recording()) {
            return;
        }
        let result = if self.is_recording() {
            let active = self.lock().take();
            self.recording.store(false, Ordering::Release);
            // Gone already if the app is quitting.
            active.map_or(Ok(()), |active| self.stop(active))
        } else {
            self.start().map(|active| {
                *self.lock() = Some(active);
                self.recording.store(true, Ordering::Release);
            })
        };
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
    /// Its tracks must be 16 kHz mono 16-bit WAV, as Murmur records them.
    /// Starts its count of attempts over.
    pub fn requeue(&self, id: &str, rerun: Rerun) -> Result<()> {
        if self.lock().as_ref().is_some_and(|a| a.meeting.id == id) {
            bail!("Meeting {id} is still recording");
        }
        let mut meeting = read_meeting(&meetings_dir(&self.app)?.join(id))?;
        for track in &meeting.tracks {
            check_track(&track.file)?;
        }
        info!("Meeting {id} queued again ({rerun:?})");
        self.app
            .state::<Arc<TranscriptionJobManager>>()
            .queue_meeting(id, rerun, || {
                meeting.status = MeetingStatus::Transcribing;
                meeting.attempts = 0;
                write_meeting(&self.app, &meeting)
            })
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

    /// Moves the system track of the recording meeting `id` to the output
    /// `monitor_source` now plays to. `false` if `id` stopped recording.
    fn move_output(&self, id: &str, monitor_source: &str) -> Result<bool> {
        let next = open_recorder(TrackKind::System, Some(monitor_input(monitor_source)?))?;
        let mut guard = self.lock();
        let Some(active) = guard.as_mut().filter(|a| a.meeting.id == id) else {
            return Ok(false);
        };
        let track = active
            .tracks
            .iter_mut()
            .find(|t| t.kind == TrackKind::System)
            .ok_or_else(|| anyhow!("Meeting {id} has no system track"))?;
        let previous = track.move_to(next)?;
        drop(guard);
        if let Err(e) = previous.close() {
            warn!("Failed to close the previous output's recorder: {e}");
        }
        info!("Meeting {id}: recording the output from {monitor_source}");
        Ok(true)
    }

    fn start(&self) -> Result<ActiveMeeting> {
        let monitor_source = default_monitor_source()?;
        let system = monitor_input(&monitor_source)?;
        // Like dictation: the selected microphone by name, else the default.
        let mic = get_settings(&self.app)
            .selected_microphone
            .and_then(|name| {
                handy_recorder::list_input_devices()
                    .ok()?
                    .into_iter()
                    .find(|d| d.name == name && !d.is_monitor)
                    .map(|d| d.id)
            });
        info!("Meeting devices: mic {mic:?}, system {system}");

        let now = chrono::Local::now();
        let id = now.format("%Y-%m-%d_%H-%M-%S").to_string();
        let dir = meetings_dir(&self.app)?.join(&id);
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
        let feeds = tracks
            .iter()
            .map(|t| (t.kind, Arc::clone(&t.feed)))
            .collect();
        let (app, id, until) = (self.app.clone(), meeting.id.clone(), Arc::clone(&ended));
        std::thread::Builder::new()
            .name("meeting-ticker".into())
            .spawn(move || tick(&app, &id, feeds, started, &until))?;
        let (app, id, until) = (self.app.clone(), meeting.id.clone(), Arc::clone(&ended));
        std::thread::Builder::new()
            .name("meeting-output".into())
            .spawn(move || follow_output(&app, &id, monitor_source, &until))?;
        Ok(ActiveMeeting {
            meeting,
            started,
            tracks,
            ended,
        })
    }

    fn stop(&self, active: ActiveMeeting) -> Result<()> {
        let meeting = self.finalize(active)?;
        info!(
            "Meeting {} stopped after {:.1}s; queued for transcription",
            meeting.id,
            meeting.duration_ms as f64 / 1000.0
        );
        // Before the job can start, so the overlay can show its progress.
        crate::overlay::set_meeting_recording(&self.app, false);
        self.app
            .state::<Arc<TranscriptionJobManager>>()
            .queue_meeting(&meeting.id, Rerun::All, || Ok(()))
    }

    /// Stops the recorders, finalizes the tracks and saves the meeting as
    /// waiting to be transcribed. A track cut short keeps what it got, with
    /// a warning; the meeting only fails without a track left.
    fn finalize(&self, active: ActiveMeeting) -> Result<Meeting> {
        let ActiveMeeting {
            mut meeting,
            started,
            tracks,
            ended,
        } = active;
        ended.store(true, Ordering::Release);
        meeting.duration_ms = started.elapsed().as_millis() as i64;
        meeting.tracks.clear();
        let (mut failure, mut problems) = (None, TrackProblems::default());
        for track in tracks {
            match stop_track(&track, started) {
                Ok(stopped) => {
                    if let Some(problem) = stopped.problem {
                        warn!(
                            "Meeting {} {:?} track cut short: {problem}",
                            meeting.id, track.kind
                        );
                        problems
                            .cut_short
                            .push(format!("{:?}: {problem}", track.kind));
                    }
                    #[cfg(target_os = "macos")]
                    if track.kind == TrackKind::System && track.feed.withheld() {
                        problems.system_withheld = true;
                    }
                    meeting.tracks.push(MeetingTrack {
                        kind: track.kind,
                        file: track.file,
                        start_offset_ms: stopped.start_offset_ms,
                        duration_ms: stopped.duration_ms,
                    });
                }
                Err(e) => failure = Some(e.context(format!("{:?} track", track.kind))),
            }
            if let Err(e) = track.recorder.close() {
                warn!("Failed to close a meeting recorder: {e}");
            }
        }
        if meeting.tracks.is_empty() {
            meeting.status = MeetingStatus::Failed;
            write_meeting(&self.app, &meeting)?;
            return Err(failure.unwrap_or_else(|| anyhow!("The meeting has no tracks")));
        }
        if let Some(e) = &failure {
            problems.cut_short.push(format!("{e:#}"));
        }
        meeting.status = MeetingStatus::Transcribing;
        write_meeting(&self.app, &meeting)?;
        problems.raise(&self.app);
        self.track_problems
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(meeting.id.clone(), problems);
        Ok(meeting)
    }
}

/// What went wrong with a meeting's tracks.
#[derive(Default)]
pub(crate) struct TrackProblems {
    /// Which tracks were cut short, and why.
    pub cut_short: Vec<String>,
    /// macOS gave the system track only zeros for a stretch.
    pub system_withheld: bool,
}

impl TrackProblems {
    /// Warns about them. A meeting has one notice at a time: a track cut
    /// short comes before a system track macOS withheld.
    pub(crate) fn raise(&self, app: &AppHandle) {
        let code = if !self.cut_short.is_empty() {
            NoticeCode::MeetingTrackIncomplete
        } else if self.system_withheld {
            NoticeCode::MeetingSystemAudioWithheld
        } else {
            return;
        };
        let detail = (!self.cut_short.is_empty()).then(|| self.cut_short.join("; "));
        notices::raise(
            app,
            Notice::new(code, NoticeSource::Meeting).with_detail(detail),
        );
    }
}
/// Where each meeting's folder is.
pub(crate) fn meetings_dir(app: &AppHandle) -> Result<PathBuf> {
    Ok(crate::portable::app_data_dir(app)?
        .join("recordings")
        .join("meetings"))
}

/// Fails unless `file` is a WAV a meeting can be transcribed from.
fn check_track(file: &Path) -> Result<()> {
    let spec = hound::WavReader::open(file)
        .with_context(|| format!("Failed to open {}", file.display()))?
        .spec();
    if spec.sample_rate != 16_000
        || spec.channels != 1
        || spec.bits_per_sample != 16
        || spec.sample_format != hound::SampleFormat::Int
    {
        bail!(
            "{} is {} Hz, {} channel, {}-bit audio; a meeting track must be 16 kHz mono 16-bit WAV",
            file.display(),
            spec.sample_rate,
            spec.channels,
            spec.bits_per_sample
        );
    }
    Ok(())
}

/// How often a recording meeting's ticker runs.
const TICK: Duration = Duration::from_millis(1000 / 30);

/// How long a meeting's system track may give only zeros before the user is
/// warned: long enough to answer macOS the first time it asks. macOS gives a
/// tap it didn't allow, or an output it can't tap, silence and no error.
/// Nothing checks when the meeting stops, so one shorter than this gets no
/// warning, even with its whole system track withheld.
#[cfg(target_os = "macos")]
const SYSTEM_AUDIO_DUE: Duration = Duration::from_secs(10);

/// Runs while the meeting `id` records: sends the tracks' levels, and
/// saves each track's start offset once its audio arrives, so a meeting
/// that never stops can still be lined up.
fn tick(
    app: &AppHandle,
    id: &str,
    feeds: Vec<(TrackKind, Arc<TrackFeed>)>,
    started: Instant,
    ended: &AtomicBool,
) {
    let mut known = vec![false; feeds.len()];
    let mut sent = 0u64;
    let mut next = Instant::now();
    // When the system track last had a sample other than zero, and whether
    // the user was warned since.
    #[cfg(target_os = "macos")]
    let (mut system_heard, mut system_warned) = (started, false);
    while !ended.load(Ordering::Acquire) {
        #[cfg(target_os = "macos")]
        for (_, feed) in feeds.iter().filter(|(kind, _)| *kind == TrackKind::System) {
            if feed.take_heard() {
                system_heard = Instant::now();
                system_warned = false;
            } else if !system_warned && system_heard.elapsed() >= SYSTEM_AUDIO_DUE {
                system_warned = true;
                feed.set_withheld();
                warn!("Meeting {id}: no system audio for {SYSTEM_AUDIO_DUE:?}");
                notices::raise(
                    app,
                    Notice::new(
                        NoticeCode::MeetingSystemAudioWithheld,
                        NoticeSource::Meeting,
                    ),
                );
            }
        }
        let mut levels = MeetingLevels {
            mic: 0.0,
            system: 0.0,
        };
        for (kind, feed) in &feeds {
            match kind {
                TrackKind::Mic => levels.mic = feed.take_peak(),
                TrackKind::System | TrackKind::Mixed => levels.system = feed.take_peak(),
            }
        }
        match levels.emit(app) {
            Ok(()) => sent += 1,
            Err(e) => debug!("Failed to emit meeting levels: {e}"),
        }
        let mut arrived = false;
        for ((_, feed), known) in feeds.iter().zip(&mut known) {
            if !*known && feed.offset_ms(started).is_some() {
                *known = true;
                arrived = true;
            }
        }
        if arrived {
            app.state::<Arc<MeetingManager>>().save_offsets(id);
        }
        next += TICK;
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
    }
    debug!("Meeting {id}: sent {sent} level updates");
}

/// How often a recording meeting checks where the output plays.
const OUTPUT_POLL: Duration = Duration::from_secs(2);

/// Runs while the meeting `id` records: when the default output changes,
/// moves the system track to it. A check that fails changes nothing.
fn follow_output(app: &AppHandle, id: &str, mut monitor_source: String, ended: &AtomicBool) {
    loop {
        let wait = Instant::now();
        while wait.elapsed() < OUTPUT_POLL {
            if ended.load(Ordering::Acquire) {
                return;
            }
            std::thread::sleep(TICK);
        }
        let now = match default_monitor_source() {
            Ok(now) if now != monitor_source => now,
            Ok(_) => continue,
            Err(e) => {
                debug!("Meeting {id}: couldn't check the default output: {e:#}");
                continue;
            }
        };
        info!("Meeting {id}: the default output is now {now}");
        match app.state::<Arc<MeetingManager>>().move_output(id, &now) {
            Ok(true) => monitor_source = now,
            Ok(false) => return,
            Err(e) => warn!("Meeting {id}: failed to follow the output to {now}: {e:#}"),
        }
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
    emit_changed(app, &meeting.id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(path: &Path, sample_rate: u32, channels: u16) {
        let spec = hound::WavSpec {
            channels,
            sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for _ in 0..channels {
            writer.write_sample(0i16).unwrap();
        }
        writer.finalize().unwrap();
    }

    #[test]
    fn only_16_khz_mono_tracks_can_be_transcribed_again() {
        let dir = tempfile::tempdir().unwrap();
        let ok = dir.path().join("mic.wav");
        let stereo = dir.path().join("mixed.wav");
        wav(&ok, 16_000, 1);
        wav(&stereo, 44_100, 2);
        assert!(check_track(&ok).is_ok());
        let e = check_track(&stereo).unwrap_err().to_string();
        assert!(e.contains("44100 Hz, 2 channel"), "{e}");
        assert!(check_track(&dir.path().join("system.wav")).is_err());
    }

    #[test]
    fn meeting_json_without_the_newer_fields_still_reads() {
        let dir = tempfile::tempdir().unwrap();
        let json = format!(
            r#"{{"id":"2026-11-15_14-03-22","started_at":"2026-11-15T14:03:22+01:00",
            "duration_ms":1000,"status":"done","dir":{:?},
            "tracks":[{{"kind":"mic","file":"mic.wav","start_offset_ms":0}}]}}"#,
            dir.path()
        );
        std::fs::write(dir.path().join("meeting.json"), json).unwrap();
        let meeting = read_meeting(dir.path()).unwrap();
        assert_eq!((meeting.attempts, meeting.language.as_deref()), (0, None));
        write_meeting_json(&meeting).unwrap();
        assert_eq!(
            read_meeting(dir.path()).unwrap().status,
            MeetingStatus::Done
        );
    }
}
