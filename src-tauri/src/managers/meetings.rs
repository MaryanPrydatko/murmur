//! Meeting recording: the microphone and the default output's monitor, each
//! streamed to its own WAV, then transcribed into who said what and when.
//!
//! The meeting folder is the record: `recordings/meetings/<id>/` holds
//! `mic.wav`, `system.wav`, `meeting.json` and, once transcribed,
//! `transcript.md`.

use anyhow::{anyhow, bail, Context, Result};
use handy_recorder::{AudioChunk, Recorder, RecorderConfig, Sink};
use hound::{WavSpec, WavWriter};
use log::{error, info, warn};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use transcribe_cpp::{
    DiarizeExtension, DiarizeOptions, SortformerDiarizeOptions, SortformerPreset,
};

use crate::engine_supervisor::EngineError;
use crate::managers::model::ModelManager;
use crate::managers::transcription::{ChunkControl, TimedWord, TranscriptionManager};
use crate::managers::transcription_jobs::TranscriptionJobManager;
use crate::notices::{self, Notice, NoticeCode, NoticeSource};
use crate::settings::get_settings;

/// A pause longer than this starts a new utterance.
const MAX_WORD_GAP_MS: i64 = 1_500;

/// A diarized speaker with less of the track's speech than this percentage
/// is taken for the diarizer splitting off part of someone else. On two real
/// meetings Sortformer added one or two such speakers, with 1 and 10 turns
/// against about 200 for each real one (10 of ~410 turns is 2.4%).
const MIN_SPEAKER_SHARE_PCT: i64 = 3;

/// Tries at diarizing when a dictation's cancel interrupts it.
const MAX_DIARIZE_ATTEMPTS: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MeetingStatus {
    Recording,
    Transcribing,
    Done,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Mic,
    System,
}

impl TrackKind {
    fn file_name(self) -> &'static str {
        match self {
            Self::Mic => "mic.wav",
            Self::System => "system.wav",
        }
    }
}

/// The microphone is the user; the output is everyone else, told apart when
/// the diarizer ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speaker {
    Me,
    /// The output, not told apart.
    Them,
    /// One person on the output, numbered from 1 in the order they first
    /// spoke.
    Remote(u8),
}

impl Speaker {
    fn name(self) -> String {
        match self {
            Self::Me => "Me".into(),
            Self::Them => "Them".into(),
            Self::Remote(n) => format!("Speaker {n}"),
        }
    }
}

/// A stretch of the system track one diarized speaker talks in, in ms from
/// the track's start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Turn {
    t0_ms: i64,
    t1_ms: i64,
    speaker: u8,
}

impl Turn {
    fn len(&self) -> i64 {
        self.t1_ms - self.t0_ms
    }

    fn overlap(&self, t0_ms: i64, t1_ms: i64) -> i64 {
        (self.t1_ms.min(t1_ms) - self.t0_ms.max(t0_ms)).max(0)
    }

    /// 0 inside the turn.
    fn distance(&self, at_ms: i64) -> i64 {
        (self.t0_ms - at_ms).max(at_ms - self.t1_ms).max(0)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct MeetingTrack {
    pub kind: TrackKind,
    pub file: PathBuf,
    /// When the track's first sample was heard, from the meeting's start.
    pub start_offset_ms: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Meeting {
    /// The local start time, `2026-11-15_14-03-22`; also the folder name.
    pub id: String,
    /// RFC 3339, local time.
    pub started_at: String,
    pub duration_ms: i64,
    pub status: MeetingStatus,
    pub dir: PathBuf,
    pub tracks: Vec<MeetingTrack>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Utterance {
    pub t0_ms: i64,
    pub t1_ms: i64,
    pub speaker: Speaker,
    pub text: String,
}

/// Writes 16 kHz mono audio straight to a WAV as it arrives: no VAD and no
/// buffer of the whole recording, so memory stays flat however long it runs.
struct WavSink {
    writer: WavWriter<BufWriter<File>>,
    /// When the first chunk's first sample was captured.
    first_audio: Option<Instant>,
    error: Option<hound::Error>,
}

impl WavSink {
    fn create(path: &Path) -> Result<Self> {
        let spec = WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        Ok(Self {
            writer: WavWriter::create(path, spec)
                .with_context(|| format!("Failed to create {}", path.display()))?,
            first_audio: None,
            error: None,
        })
    }

    /// Finalizes the file. When the first sample was captured, if any audio came.
    fn finish(self) -> Result<Option<Instant>> {
        if let Some(e) = self.error {
            return Err(e.into());
        }
        self.writer.finalize()?;
        Ok(self.first_audio)
    }
}

impl Sink for WavSink {
    fn process_chunk(&mut self, chunk: AudioChunk<'_>) {
        if self.error.is_some() {
            return;
        }
        if self.first_audio.is_none() {
            let captured = Duration::from_millis(
                chunk.valid_frames as u64 * 1000 / u64::from(chunk.sample_rate.max(1)),
            );
            let now = Instant::now();
            self.first_audio = Some(now.checked_sub(captured).unwrap_or(now));
        }
        for &sample in &chunk.samples[..chunk.valid_frames] {
            let sample = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            if let Err(e) = self.writer.write_sample(sample) {
                self.error = Some(e);
                return;
            }
        }
    }
}

struct TrackRecorder {
    kind: TrackKind,
    file: PathBuf,
    recorder: Recorder<WavSink>,
}

struct ActiveMeeting {
    meeting: Meeting,
    started: Instant,
    tracks: Vec<TrackRecorder>,
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
        let meeting = Meeting {
            id,
            started_at: now.to_rfc3339(),
            duration_ms: 0,
            status: MeetingStatus::Recording,
            dir,
            tracks: Vec::new(),
        };
        write_meeting(&meeting)?;
        notices::clear(&self.app, NoticeSource::Meeting);
        info!("Meeting {} recording", meeting.id);
        Ok(ActiveMeeting {
            meeting,
            started,
            tracks,
        })
    }

    fn stop(&self, active: ActiveMeeting) -> Result<()> {
        let ActiveMeeting {
            mut meeting,
            started,
            tracks,
        } = active;
        meeting.duration_ms = started.elapsed().as_millis() as i64;
        let mut failure = None;
        for track in tracks {
            match stop_track(&track, started) {
                Ok(start_offset_ms) => meeting.tracks.push(MeetingTrack {
                    kind: track.kind,
                    file: track.file,
                    start_offset_ms,
                }),
                Err(e) => failure = Some(e.context(format!("{:?} track", track.kind))),
            }
            if let Err(e) = track.recorder.close() {
                warn!("Failed to close a meeting recorder: {e}");
            }
        }
        if let Some(e) = failure {
            meeting.status = MeetingStatus::Failed;
            write_meeting(&meeting)?;
            return Err(e);
        }
        meeting.status = MeetingStatus::Transcribing;
        write_meeting(&meeting)?;
        info!(
            "Meeting {} stopped after {:.1}s; queued for transcription",
            meeting.id,
            meeting.duration_ms as f64 / 1000.0
        );
        let app = self.app.clone();
        self.app
            .state::<Arc<TranscriptionJobManager>>()
            .queue_meeting(Box::new(move |control| transcribe(&app, meeting, control)));
        Ok(())
    }
}

fn open_track(dir: &Path, kind: TrackKind, device: Option<String>) -> Result<TrackRecorder> {
    let file = dir.join(kind.file_name());
    let recorder = Recorder::open(RecorderConfig {
        device,
        ..RecorderConfig::speech()
    })
    .with_context(|| format!("Failed to open the {kind:?} input"))?;
    recorder
        .start(WavSink::create(&file)?)
        .map_err(|e| anyhow!(e.error).context(format!("Failed to start the {kind:?} input")))?;
    Ok(TrackRecorder {
        kind,
        file,
        recorder,
    })
}

/// Stops the track and finalizes its WAV. Its start offset in ms.
fn stop_track(track: &TrackRecorder, meeting_started: Instant) -> Result<i64> {
    let stopped = track.recorder.stop()?;
    info!(
        "Meeting {:?} track ended: {:?}, {} frames dropped",
        track.kind, stopped.end_reason, stopped.dropped_frames
    );
    let first_audio = stopped.sink.finish()?;
    Ok(first_audio
        .map(|at| at.saturating_duration_since(meeting_started).as_millis() as i64)
        .unwrap_or(0))
}

/// Why the system track's speakers weren't told apart.
enum Unseparated {
    NoModel,
    Failed(anyhow::Error),
}

/// Transcribes every track and writes the transcript. Runs on the job thread.
fn transcribe(app: &AppHandle, mut meeting: Meeting, control: &dyn ChunkControl) {
    let tm = app.state::<Arc<TranscriptionManager>>();
    let mut unseparated = None;
    let result = (|| -> Result<()> {
        let mut tracks = Vec::new();
        for track in &meeting.tracks {
            let (samples, total) = read_track(&track.file)?;
            let track_words = tm
                .transcribe_words_in_chunks(samples, total, control)?
                .ok_or_else(|| anyhow!("Transcription was stopped"))?;
            info!(
                "Meeting {:?} track: {} words",
                track.kind,
                track_words.len()
            );
            let speakers = match track.kind {
                TrackKind::Mic => vec![Speaker::Me; track_words.len()],
                // Nobody spoke, so there's no one to tell apart.
                TrackKind::System if track_words.is_empty() => Vec::new(),
                TrackKind::System => match diarize_track(app, &track.file, control)? {
                    Ok(turns) => assign_speakers(&track_words, &merge_minor_speakers(turns)),
                    Err(why) => {
                        if let Unseparated::Failed(e) = &why {
                            warn!("Meeting {} diarization failed: {e:#}", meeting.id);
                        }
                        unseparated = Some(why);
                        vec![Speaker::Them; track_words.len()]
                    }
                },
            };
            tracks.push((
                track.start_offset_ms,
                speakers.into_iter().zip(track_words).collect(),
            ));
        }
        let utterances = merge_utterances(tracks);
        let path = meeting.dir.join("transcript.md");
        std::fs::write(&path, render_transcript(&meeting, &utterances))
            .with_context(|| format!("Failed to write {}", path.display()))
    })();
    meeting.status = match &result {
        Ok(()) => MeetingStatus::Done,
        Err(_) => MeetingStatus::Failed,
    };
    if let Err(e) = write_meeting(&meeting) {
        error!("Failed to save meeting {}: {e:#}", meeting.id);
    }
    match result {
        Ok(()) => {
            info!("Meeting {} transcribed", meeting.id);
            notices::clear(app, NoticeSource::Meeting);
            let notice = Notice::new(
                NoticeCode::MeetingSpeakersNotSeparated,
                NoticeSource::Meeting,
            );
            match unseparated {
                Some(Unseparated::NoModel) => notices::raise(app, notice),
                Some(Unseparated::Failed(e)) => {
                    notices::raise(app, notice.with_detail(format!("{e:#}")))
                }
                None => {}
            }
        }
        Err(e) => {
            error!("Meeting {} transcription failed: {e:#}", meeting.id);
            notices::raise(
                app,
                Notice::new(
                    NoticeCode::MeetingTranscriptionFailed,
                    NoticeSource::Meeting,
                )
                .with_detail(format!("{e:#}")),
            );
        }
    }
}

/// A track's samples, read as they're needed, and how many there are.
fn read_track(file: &Path) -> Result<(impl Iterator<Item = f32>, usize)> {
    let reader = hound::WavReader::open(file)
        .with_context(|| format!("Failed to open {}", file.display()))?;
    let total = reader.duration() as usize;
    let samples = reader
        .into_samples::<i16>()
        .map_while(Result::ok)
        .map(|s| s as f32 / i16::MAX as f32);
    Ok((samples, total))
}

/// Who spoke when on the system track, in one run over all of it, or why
/// not. Fails only when `control` stopped the transcription.
fn diarize_track(
    app: &AppHandle,
    file: &Path,
    control: &dyn ChunkControl,
) -> Result<Result<Vec<Turn>, Unseparated>> {
    let Some(model) = app.state::<Arc<ModelManager>>().diarizer_path() else {
        return Ok(Err(Unseparated::NoModel));
    };
    let tm = app.state::<Arc<TranscriptionManager>>();
    let options = DiarizeOptions {
        family: Some(DiarizeExtension::Sortformer(SortformerDiarizeOptions {
            preset: Some(SortformerPreset::VeryHighLatency),
        })),
    };
    let mut attempts = 0;
    loop {
        if !control.proceed() {
            bail!("Transcription was stopped");
        }
        attempts += 1;
        // Read again for each try rather than keep a second copy of the track.
        let pcm: Vec<f32> = match read_track(file) {
            Ok((samples, _)) => samples.collect(),
            Err(e) => return Ok(Err(Unseparated::Failed(e))),
        };
        let started = Instant::now();
        match tm.diarize(model.clone(), options.clone(), pcm) {
            Ok(segments) => {
                info!(
                    "Diarized the system track in {} ms: {} turns",
                    started.elapsed().as_millis(),
                    segments.len()
                );
                return Ok(Ok(segments
                    .into_iter()
                    .filter(|s| s.t1_ms > s.t0_ms && s.speaker_id >= 1)
                    .map(|s| Turn {
                        t0_ms: s.t0_ms,
                        t1_ms: s.t1_ms,
                        speaker: s.speaker_id as u8,
                    })
                    .collect()));
            }
            Err(EngineError::Cancelled) if attempts < MAX_DIARIZE_ATTEMPTS => {
                warn!("Diarization was cancelled; trying it again");
            }
            Err(e) => return Ok(Err(Unseparated::Failed(e.into()))),
        }
    }
}

/// Merges each speaker with under [`MIN_SPEAKER_SHARE_PCT`] of the speech
/// into the main speaker their turn overlaps most, or else the nearest one,
/// then numbers speakers from 1 in the order they first speak.
fn merge_minor_speakers(mut turns: Vec<Turn>) -> Vec<Turn> {
    let mut talk: BTreeMap<u8, i64> = BTreeMap::new();
    for turn in &turns {
        *talk.entry(turn.speaker).or_default() += turn.len();
    }
    let total: i64 = talk.values().sum();
    let is_minor = |speaker: u8| talk[&speaker] * 100 < MIN_SPEAKER_SHARE_PCT * total;
    for (speaker, ms) in &talk {
        info!(
            "Diarized speaker {speaker}: {:.1}s, {:.1}% of the speech",
            *ms as f64 / 1000.0,
            *ms as f64 * 100.0 / total.max(1) as f64
        );
    }

    let main: Vec<Turn> = turns
        .iter()
        .filter(|t| !is_minor(t.speaker))
        .copied()
        .collect();
    for turn in turns.iter_mut().filter(|t| is_minor(t.speaker)) {
        let overlapping = main
            .iter()
            .max_by_key(|m| m.overlap(turn.t0_ms, turn.t1_ms))
            .filter(|m| m.overlap(turn.t0_ms, turn.t1_ms) > 0);
        let nearest = || {
            main.iter()
                .min_by_key(|m| m.distance(turn.t0_ms).min(m.distance(turn.t1_ms)))
        };
        if let Some(into) = overlapping.or_else(nearest) {
            turn.speaker = into.speaker;
        }
    }

    turns.sort_by_key(|t| (t.t0_ms, t.t1_ms));
    let mut numbers: Vec<u8> = Vec::new();
    for turn in &mut turns {
        let index = match numbers.iter().position(|&s| s == turn.speaker) {
            Some(index) => index,
            None => {
                numbers.push(turn.speaker);
                numbers.len() - 1
            }
        };
        turn.speaker = index as u8 + 1;
    }
    turns
}

/// Each word's speaker: the turn it overlaps most, or else the nearest one.
/// Turns can overlap and come in any order. Without turns, it's [`Speaker::Them`].
fn assign_speakers(words: &[TimedWord], turns: &[Turn]) -> Vec<Speaker> {
    words
        .iter()
        .map(|w| {
            let middle = (w.t0_ms + w.t1_ms) / 2;
            turns
                .iter()
                .max_by_key(|t| (t.overlap(w.t0_ms, w.t1_ms), -t.distance(middle)))
                .map_or(Speaker::Them, |t| Speaker::Remote(t.speaker))
        })
        .collect()
}

fn write_meeting(meeting: &Meeting) -> Result<()> {
    let path = meeting.dir.join("meeting.json");
    std::fs::write(&path, serde_json::to_vec_pretty(meeting)?)
        .with_context(|| format!("Failed to write {}", path.display()))
}

/// The monitor source of the current default output, by its PulseAudio name.
#[cfg(target_os = "linux")]
fn default_monitor_source() -> Result<String> {
    use std::net::Shutdown;
    use std::os::unix::net::UnixStream;

    let path = pulseaudio::socket_path_from_env().ok_or_else(|| anyhow!("No PulseAudio server"))?;
    let socket = UnixStream::connect(path)?;
    // The client's reactor thread only ends when its socket does.
    let shutdown = socket.try_clone()?;
    let cookie = pulseaudio::cookie_path_from_env().and_then(|p| std::fs::read(p).ok());
    let sink = pulseaudio::Client::new_unix(c"murmur-meeting", socket, cookie).and_then(|client| {
        tauri::async_runtime::block_on(
            client.sink_info_by_name(pulseaudio::protocol::DEFAULT_SINK.to_owned()),
        )
    });
    let _ = shutdown.shutdown(Shutdown::Both);
    let name = sink
        .context("Failed to read the default output")?
        .monitor_source_name
        .ok_or_else(|| anyhow!("The default output has no monitor"))?;
    Ok(name.to_string_lossy().into_owned())
}

#[cfg(not(target_os = "linux"))]
fn default_monitor_source() -> Result<String> {
    Err(anyhow!("Recording the output is only supported on Linux"))
}

/// The id of the monitor input named `monitor_source`, from `(id, is_monitor)`
/// pairs. Ids are `<host>:<name>`.
fn monitor_device_id<'a>(
    devices: impl IntoIterator<Item = (&'a str, bool)>,
    monitor_source: &str,
) -> Option<&'a str> {
    devices
        .into_iter()
        .filter(|&(_, is_monitor)| is_monitor)
        .map(|(id, _)| id)
        .find(|id| id.split_once(':').map(|(_, name)| name) == Some(monitor_source))
}

/// Interleaves each track's words and their speakers, placed by the track's
/// start offset, into utterances: one breaks when the speaker changes or
/// after a pause longer than [`MAX_WORD_GAP_MS`].
fn merge_utterances(tracks: Vec<(i64, Vec<(Speaker, TimedWord)>)>) -> Vec<Utterance> {
    let mut words: Vec<(Speaker, TimedWord)> = tracks
        .into_iter()
        .flat_map(|(offset, words)| {
            words.into_iter().map(move |(speaker, w)| {
                (
                    speaker,
                    TimedWord {
                        t0_ms: w.t0_ms + offset,
                        t1_ms: w.t1_ms + offset,
                        text: w.text,
                    },
                )
            })
        })
        .collect();
    words.sort_by_key(|(_, w)| w.t0_ms);

    let mut utterances: Vec<Utterance> = Vec::new();
    for (speaker, word) in words {
        match utterances.last_mut() {
            Some(last) if last.speaker == speaker && word.t0_ms - last.t1_ms <= MAX_WORD_GAP_MS => {
                last.text.push(' ');
                last.text.push_str(&word.text);
                last.t1_ms = last.t1_ms.max(word.t1_ms);
            }
            _ => utterances.push(Utterance {
                t0_ms: word.t0_ms,
                t1_ms: word.t1_ms,
                speaker,
                text: word.text,
            }),
        }
    }
    utterances
}

fn clock(ms: i64) -> String {
    let secs = ms.max(0) / 1000;
    format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

fn render_transcript(meeting: &Meeting, utterances: &[Utterance]) -> String {
    let date = chrono::DateTime::parse_from_rfc3339(&meeting.started_at)
        .map(|at| at.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| meeting.id.clone());
    let mut out = format!("# Meeting {date} ({})\n", clock(meeting.duration_ms));
    for u in utterances {
        out.push_str(&format!(
            "\n**{}** {} {}\n",
            u.speaker.name(),
            clock(u.t0_ms),
            u.text
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(t0_ms: i64, t1_ms: i64, text: &str) -> TimedWord {
        TimedWord {
            t0_ms,
            t1_ms,
            text: text.to_string(),
        }
    }

    fn said(t0_ms: i64, t1_ms: i64, speaker: Speaker, text: &str) -> Utterance {
        Utterance {
            t0_ms,
            t1_ms,
            speaker,
            text: text.to_string(),
        }
    }

    #[test]
    fn picks_the_default_outputs_monitor_among_inputs() {
        let devices = [
            ("pulseaudio:alsa_input.usb-HyperX-00.mono-fallback", false),
            (
                "pulseaudio:alsa_output.pci-0000_01_00.1.hdmi-stereo.monitor",
                true,
            ),
            (
                "pulseaudio:alsa_output.usb-HyperX-00.iec958-stereo.monitor",
                true,
            ),
        ];
        assert_eq!(
            monitor_device_id(devices, "alsa_output.usb-HyperX-00.iec958-stereo.monitor"),
            Some("pulseaudio:alsa_output.usb-HyperX-00.iec958-stereo.monitor")
        );
    }

    #[test]
    fn finds_no_monitor_when_the_default_output_has_none_listed() {
        let devices = [
            // Same name, but not a monitor: never record a microphone as "Them".
            (
                "pulseaudio:alsa_output.usb-HyperX-00.iec958-stereo.monitor",
                false,
            ),
            (
                "pulseaudio:alsa_output.pci-0000_01_00.1.hdmi-stereo.monitor",
                true,
            ),
        ];
        assert_eq!(
            monitor_device_id(devices, "alsa_output.usb-HyperX-00.iec958-stereo.monitor"),
            None
        );
    }

    /// One track's words, all said by `speaker`.
    fn by(speaker: Speaker, words: Vec<TimedWord>) -> Vec<(Speaker, TimedWord)> {
        words.into_iter().map(|w| (speaker, w)).collect()
    }

    fn turn(t0_ms: i64, t1_ms: i64, speaker: u8) -> Turn {
        Turn {
            t0_ms,
            t1_ms,
            speaker,
        }
    }

    #[test]
    fn places_each_track_by_its_start_offset() {
        let utterances = merge_utterances(vec![
            (0, by(Speaker::Me, vec![word(1_000, 1_400, "hi")])),
            // The system track started 2 s in, so its 0 ms is meeting 2 s.
            (2_000, by(Speaker::Them, vec![word(0, 300, "hello")])),
        ]);
        assert_eq!(
            utterances,
            vec![
                said(1_000, 1_400, Speaker::Me, "hi"),
                said(2_000, 2_300, Speaker::Them, "hello"),
            ]
        );
    }

    #[test]
    fn breaks_utterances_on_speaker_change_and_long_pauses() {
        let utterances = merge_utterances(vec![
            (
                0,
                by(
                    Speaker::Me,
                    vec![
                        word(0, 400, "so"),
                        word(500, 900, "yes"),
                        // 1.6 s after "yes": a new utterance.
                        word(2_500, 2_900, "anyway"),
                        word(9_000, 9_400, "right"),
                    ],
                ),
            ),
            (
                0,
                by(
                    Speaker::Them,
                    vec![word(4_000, 4_500, "sure"), word(5_000, 5_500, "thing")],
                ),
            ),
        ]);
        assert_eq!(
            utterances,
            vec![
                said(0, 900, Speaker::Me, "so yes"),
                said(2_500, 2_900, Speaker::Me, "anyway"),
                said(4_000, 5_500, Speaker::Them, "sure thing"),
                said(9_000, 9_400, Speaker::Me, "right"),
            ]
        );
    }

    #[test]
    fn renders_a_titled_transcript_with_speaker_and_time() {
        let meeting = Meeting {
            id: "2026-11-15_14-03-22".into(),
            started_at: "2026-11-15T14:03:22+01:00".into(),
            duration_ms: 3_725_000,
            status: MeetingStatus::Done,
            dir: PathBuf::new(),
            tracks: Vec::new(),
        };
        let utterances = [
            said(83_000, 84_000, Speaker::Me, "hi"),
            said(3_661_000, 3_662_000, Speaker::Remote(2), "bye"),
            said(3_663_000, 3_664_000, Speaker::Them, "ok"),
        ];
        assert_eq!(
            render_transcript(&meeting, &utterances),
            "# Meeting 2026-11-15 14:03 (01:02:05)\n\n**Me** 00:01:23 hi\n\n\
             **Speaker 2** 01:01:01 bye\n\n**Them** 01:01:03 ok\n"
        );
    }

    #[test]
    fn gives_each_word_the_turn_it_overlaps_most() {
        // Turns come grouped by speaker and can overlap.
        let turns = [
            turn(0, 2_100, 1),
            turn(5_000, 6_000, 1),
            turn(1_900, 4_000, 2),
        ];
        let words = [
            word(100, 500, "hello"),
            // 2000..2300 overlaps speaker 1 by 100 ms, speaker 2 by 300 ms.
            word(2_000, 2_300, "well"),
            word(5_200, 5_600, "yes"),
        ];
        assert_eq!(
            assign_speakers(&words, &turns),
            [Speaker::Remote(1), Speaker::Remote(2), Speaker::Remote(1)]
        );
    }

    #[test]
    fn gives_a_word_between_turns_the_nearest_one() {
        let turns = [turn(0, 1_000, 1), turn(3_000, 4_000, 2)];
        let words = [word(1_200, 1_400, "um"), word(2_600, 2_800, "so")];
        assert_eq!(
            assign_speakers(&words, &turns),
            [Speaker::Remote(1), Speaker::Remote(2)]
        );
    }

    #[test]
    fn words_without_turns_are_them() {
        assert_eq!(assign_speakers(&[word(0, 300, "hi")], &[]), [Speaker::Them]);
    }

    #[test]
    fn merges_a_speaker_with_too_little_speech_into_the_one_it_overlaps() {
        let turns = vec![
            turn(0, 50_000, 1),
            turn(50_000, 100_000, 2),
            // 1 s of 101 s: under 3%, and inside speaker 2's turn.
            turn(60_000, 61_000, 3),
        ];
        assert_eq!(
            merge_minor_speakers(turns),
            [
                turn(0, 50_000, 1),
                turn(50_000, 100_000, 2),
                turn(60_000, 61_000, 2)
            ]
        );
    }

    #[test]
    fn merges_a_lone_minor_turn_into_the_nearest_speaker() {
        let turns = vec![
            turn(0, 40_000, 1),
            turn(60_000, 100_000, 2),
            // Overlaps nobody; 4 s from speaker 2, 15 s from speaker 1.
            turn(55_000, 56_000, 3),
        ];
        assert_eq!(merge_minor_speakers(turns)[1], turn(55_000, 56_000, 2));
    }

    #[test]
    fn keeps_a_quiet_speaker_above_the_threshold() {
        // 4 s of 104 s is 3.8%.
        let turns = vec![
            turn(0, 50_000, 1),
            turn(50_000, 100_000, 2),
            turn(100_000, 104_000, 3),
        ];
        let speakers: Vec<u8> = merge_minor_speakers(turns)
            .iter()
            .map(|t| t.speaker)
            .collect();
        assert_eq!(speakers, [1, 2, 3]);
    }

    #[test]
    fn numbers_speakers_in_the_order_they_first_speak() {
        // The diarizer's ids are its own; Speaker 1 is whoever talks first.
        let turns = vec![
            turn(10_000, 20_000, 4),
            turn(0, 10_000, 2),
            turn(20_000, 30_000, 2),
        ];
        assert_eq!(
            merge_minor_speakers(turns),
            [
                turn(0, 10_000, 1),
                turn(10_000, 20_000, 2),
                turn(20_000, 30_000, 1)
            ]
        );
    }

    #[test]
    fn breaks_an_utterance_where_the_system_speaker_changes() {
        let system = vec![
            (Speaker::Remote(1), word(0, 400, "how")),
            (Speaker::Remote(1), word(500, 900, "are")),
            (Speaker::Remote(2), word(1_000, 1_400, "fine")),
        ];
        assert_eq!(
            merge_utterances(vec![(0, system)]),
            [
                said(0, 900, Speaker::Remote(1), "how are"),
                said(1_000, 1_400, Speaker::Remote(2), "fine"),
            ]
        );
    }
}
