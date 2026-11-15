//! Meeting recording: the microphone and the default output's monitor, each
//! streamed to its own WAV, then transcribed into who said what and when.
//!
//! The meeting folder is the record: `recordings/meetings/<id>/` holds
//! `mic.wav`, `system.wav`, `meeting.json` and, once transcribed,
//! `transcript.md`.

use anyhow::{anyhow, Context, Result};
use handy_recorder::{AudioChunk, Recorder, RecorderConfig, Sink};
use hound::{WavSpec, WavWriter};
use log::{error, info, warn};
use serde::Serialize;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

use crate::managers::transcription::{ChunkControl, TimedWord, TranscriptionManager};
use crate::managers::transcription_jobs::TranscriptionJobManager;
use crate::notices::{self, Notice, NoticeCode, NoticeSource};
use crate::settings::get_settings;

/// A pause longer than this starts a new utterance.
const MAX_WORD_GAP_MS: i64 = 1_500;

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

    /// The microphone is the user; the output is everyone else.
    fn speaker(self) -> Speaker {
        match self {
            Self::Mic => Speaker::Me,
            Self::System => Speaker::Them,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Speaker {
    Me,
    Them,
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

/// Transcribes every track and writes the transcript. Runs on the job thread.
fn transcribe(app: &AppHandle, mut meeting: Meeting, control: &dyn ChunkControl) {
    let tm = app.state::<Arc<TranscriptionManager>>();
    let result = (|| -> Result<()> {
        let mut tracks = Vec::new();
        for track in &meeting.tracks {
            let reader = hound::WavReader::open(&track.file)?;
            let total = reader.duration() as usize;
            let samples = reader
                .into_samples::<i16>()
                .map_while(Result::ok)
                .map(|s| s as f32 / i16::MAX as f32);
            let words = tm
                .transcribe_words_in_chunks(samples, total, control)?
                .ok_or_else(|| anyhow!("Transcription was stopped"))?;
            info!("Meeting {:?} track: {} words", track.kind, words.len());
            tracks.push((track.kind.speaker(), track.start_offset_ms, words));
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

/// Interleaves each speaker's words, placed by their track's start offset,
/// into utterances: one breaks when the speaker changes or after a pause
/// longer than [`MAX_WORD_GAP_MS`].
fn merge_utterances(tracks: Vec<(Speaker, i64, Vec<TimedWord>)>) -> Vec<Utterance> {
    let mut words: Vec<(Speaker, TimedWord)> = tracks
        .into_iter()
        .flat_map(|(speaker, offset, words)| {
            words.into_iter().map(move |w| {
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
            "\n**{:?}** {} {}\n",
            u.speaker,
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

    #[test]
    fn places_each_track_by_its_start_offset() {
        let utterances = merge_utterances(vec![
            (Speaker::Me, 0, vec![word(1_000, 1_400, "hi")]),
            // The system track started 2 s in, so its 0 ms is meeting 2 s.
            (Speaker::Them, 2_000, vec![word(0, 300, "hello")]),
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
                Speaker::Me,
                0,
                vec![
                    word(0, 400, "so"),
                    word(500, 900, "yes"),
                    // 1.6 s after "yes": a new utterance.
                    word(2_500, 2_900, "anyway"),
                    word(9_000, 9_400, "right"),
                ],
            ),
            (
                Speaker::Them,
                0,
                vec![word(4_000, 4_500, "sure"), word(5_000, 5_500, "thing")],
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
            said(3_661_000, 3_662_000, Speaker::Them, "bye"),
        ];
        assert_eq!(
            render_transcript(&meeting, &utterances),
            "# Meeting 2026-11-15 14:03 (01:02:05)\n\n**Me** 00:01:23 hi\n\n**Them** 01:01:01 bye\n"
        );
    }
}
