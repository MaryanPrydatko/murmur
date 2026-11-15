//! Capture: each track streamed to its own WAV, and the output's monitor
//! found through PulseAudio. On macOS the output is recorded through the
//! recorder's tap on the default output instead.

use anyhow::{anyhow, Context, Result};
use handy_recorder::{AudioChunk, EndReason, Recorder, RecorderConfig, Sink};
use hound::{WavSpec, WavWriter};
use log::{info, warn};
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use super::recovery::repair_wav;
use super::TrackKind;

/// Samples between header updates: a crash loses at most 5 s of a track.
const FLUSH_SAMPLES: u32 = 80_000;

/// What a track's sink shares with the meeting while it records.
#[derive(Default)]
pub(super) struct TrackFeed {
    /// When the first sample was captured.
    first_audio: OnceLock<Instant>,
    /// The loudest sample since the last read, as `f32` bits: for a
    /// non-negative float those order like the float.
    peak: AtomicU32,
    /// Whether a sample since the last read was other than zero.
    heard: AtomicBool,
    /// Set once the track gave only zeros for too long while it recorded.
    withheld: AtomicBool,
}

impl TrackFeed {
    /// When the track's first sample was heard, from `started`, once it was.
    pub fn offset_ms(&self, started: Instant) -> Option<i64> {
        self.first_audio
            .get()
            .map(|at| at.saturating_duration_since(started).as_millis() as i64)
    }

    /// The loudest sample since the last call, 0 to 1.
    pub fn take_peak(&self) -> f32 {
        f32::from_bits(self.peak.swap(0, Ordering::Relaxed))
    }

    /// Whether the track had a sample other than zero since the last call.
    /// The macOS system track always has while its tap is given the audio:
    /// see [`handy_recorder::SYSTEM_OUTPUT_DEVICE`].
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn take_heard(&self) -> bool {
        self.heard.swap(false, Ordering::Relaxed)
    }

    /// Notes that the track gave only zeros for too long.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn set_withheld(&self) {
        self.withheld.store(true, Ordering::Relaxed);
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn withheld(&self) -> bool {
        self.withheld.load(Ordering::Relaxed)
    }
}

/// Writes 16 kHz mono audio straight to a WAV as it arrives: no VAD and no
/// buffer of the whole recording, so memory stays flat however long it runs.
/// The header is brought up to date every [`FLUSH_SAMPLES`], so the file
/// reads back to then whatever happens to the process.
pub(super) struct WavSink {
    writer: WavWriter<BufWriter<File>>,
    path: PathBuf,
    feed: Arc<TrackFeed>,
    unflushed: u32,
    /// Set when the track moves to another input: the next chunk is placed
    /// at the time it was captured, after silence for what was missed.
    realign: bool,
    error: Option<hound::Error>,
}

impl WavSink {
    fn create(path: &Path, feed: Arc<TrackFeed>) -> Result<Self> {
        let spec = WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        Ok(Self {
            writer: WavWriter::create(path, spec)
                .with_context(|| format!("Failed to create {}", path.display()))?,
            path: path.to_path_buf(),
            feed,
            unflushed: 0,
            realign: false,
            error: None,
        })
    }

    /// Finalizes the file. The samples it holds and, if writing it failed,
    /// why: then its header is set from what reached the disk.
    fn finish(self) -> Result<(u32, Option<String>)> {
        let samples = self.writer.len();
        let problem = match (self.error, self.writer.finalize()) {
            (None, Ok(())) => return Ok((samples, None)),
            (Some(e), _) | (None, Err(e)) => e.to_string(),
        };
        Ok((repair_wav(&self.path)?, Some(problem)))
    }

    /// Pads the track with silence up to `at`, from its first sample.
    fn pad_to(&mut self, at: Instant) -> hound::Result<()> {
        let Some(&first) = self.feed.first_audio.get() else {
            return Ok(());
        };
        let due = (at.saturating_duration_since(first).as_secs_f64() * 16_000.0) as u32;
        let missing = due.saturating_sub(self.writer.len());
        if missing > 0 {
            info!(
                "Padding a meeting track with {} ms of silence",
                missing / 16
            );
        }
        self.write(std::iter::repeat_n(0, missing as usize))
    }

    fn write(&mut self, samples: impl IntoIterator<Item = i16>) -> hound::Result<()> {
        for sample in samples {
            self.writer.write_sample(sample)?;
            self.unflushed += 1;
        }
        if self.unflushed >= FLUSH_SAMPLES {
            self.writer.flush()?;
            self.unflushed = 0;
        }
        Ok(())
    }
}

impl Sink for WavSink {
    fn process_chunk(&mut self, chunk: AudioChunk<'_>) {
        if self.error.is_some() {
            return;
        }
        let captured = Duration::from_millis(
            chunk.valid_frames as u64 * 1000 / u64::from(chunk.sample_rate.max(1)),
        );
        let now = Instant::now();
        let at = now.checked_sub(captured).unwrap_or(now);
        self.feed.first_audio.get_or_init(|| at);
        if std::mem::take(&mut self.realign) {
            if let Err(e) = self.pad_to(at) {
                self.error = Some(e);
                return;
            }
        }
        let audio = &chunk.samples[..chunk.valid_frames];
        let peak = audio
            .iter()
            .fold(0.0f32, |peak, s| peak.max(s.abs().min(1.0)));
        self.feed.peak.fetch_max(peak.to_bits(), Ordering::Relaxed);
        if peak > 0.0 {
            self.feed.heard.store(true, Ordering::Relaxed);
        }
        let samples = audio
            .iter()
            .map(|&sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
        if let Err(e) = self.write(samples) {
            self.error = Some(e);
        }
    }
}

pub(super) struct TrackRecorder {
    pub kind: TrackKind,
    pub file: PathBuf,
    pub feed: Arc<TrackFeed>,
    pub recorder: Recorder<WavSink>,
    /// Set when `recorder`'s input fails while it's open.
    pub failed: Arc<AtomicBool>,
}

/// An input opened for a track, and the flag set if it fails while open.
pub(super) type OpenedInput = (Recorder<WavSink>, Arc<AtomicBool>);

impl TrackRecorder {
    /// Moves the track to `next`, an input opened and not started, after
    /// silence for the time the move took. Returns the input it was on, to
    /// close. If `next` won't start, the track stays where it was. The
    /// input it leaves may have failed: its recording is kept all the same.
    pub fn move_to(&mut self, (next, failed): OpenedInput) -> Result<Recorder<WavSink>> {
        let mut sink = self.recorder.stop()?.sink;
        if let Err(e) = sink.pad_to(Instant::now()) {
            sink.error = Some(e);
        }
        sink.realign = true;
        match next.start(sink) {
            Ok(()) => {
                self.failed = failed;
                Ok(std::mem::replace(&mut self.recorder, next))
            }
            Err(failed) => {
                let error = anyhow!(failed.error).context("Failed to start the new output");
                if let Err(e) = self.recorder.start(failed.sink) {
                    warn!(
                        "Failed to go back to the {:?} input: {}",
                        self.kind, e.error
                    );
                }
                Err(error)
            }
        }
    }
}

/// Opens `device`, or the default input, for a meeting track.
pub(super) fn open_recorder(kind: TrackKind, device: Option<String>) -> Result<OpenedInput> {
    let failed = Arc::new(AtomicBool::new(false));
    let recorder = Recorder::open_with_failure_handler(
        RecorderConfig {
            device,
            ..RecorderConfig::speech()
        },
        {
            let failed = Arc::clone(&failed);
            move |_| failed.store(true, Ordering::Release)
        },
    )
    .with_context(|| format!("Failed to open the {kind:?} input"))?;
    Ok((recorder, failed))
}

pub(super) fn open_track(
    dir: &Path,
    kind: TrackKind,
    device: Option<String>,
) -> Result<TrackRecorder> {
    let file = dir.join(kind.file_name());
    let (recorder, failed) = open_recorder(kind, device)?;
    let feed = Arc::new(TrackFeed::default());
    recorder
        .start(WavSink::create(&file, Arc::clone(&feed))?)
        .map_err(|e| anyhow!(e.error).context(format!("Failed to start the {kind:?} input")))?;
    Ok(TrackRecorder {
        kind,
        file,
        feed,
        recorder,
        failed,
    })
}

/// A track, stopped and finalized.
pub(super) struct StoppedTrack {
    pub start_offset_ms: i64,
    pub duration_ms: i64,
    /// Why part of it is missing, if it is.
    pub problem: Option<String>,
}

/// Stops the track and finalizes its WAV. A track the input or the disk
/// cut short keeps what it got.
pub(super) fn stop_track(track: &TrackRecorder, meeting_started: Instant) -> Result<StoppedTrack> {
    let (samples, problem) = match track.recorder.stop() {
        Ok(stopped) => {
            info!(
                "Meeting {:?} track ended: {:?}, {} frames dropped",
                track.kind, stopped.end_reason, stopped.dropped_frames
            );
            let ended = match stopped.end_reason {
                EndReason::StopCalled => None,
                EndReason::RecorderFailed(e) => Some(format!("The input failed: {e}")),
                EndReason::SinkPanicked(message) => Some(message),
            };
            let (samples, problem) = stopped.sink.finish()?;
            (samples, problem.or(ended))
        }
        // The sink is gone with what it hadn't written: keep what reached
        // the disk.
        Err(e) => (repair_wav(&track.file)?, Some(e.to_string())),
    };
    Ok(StoppedTrack {
        start_offset_ms: track.feed.offset_ms(meeting_started).unwrap_or(0),
        duration_ms: i64::from(samples) * 1000 / 16_000,
        problem,
    })
}

/// The id of the input that records `monitor_source`.
#[cfg(not(target_os = "macos"))]
pub(super) fn monitor_input(monitor_source: &str) -> Result<String> {
    let devices = handy_recorder::list_input_devices()?;
    monitor_device_id(
        devices.iter().map(|d| (d.id.as_str(), d.is_monitor)),
        monitor_source,
    )
    .map(str::to_string)
    .ok_or_else(|| anyhow!("No input device for the output monitor {monitor_source}"))
}

/// The input that records what the Mac plays: the recorder's tap on the
/// default output, whichever that is when it's opened. `_output` only tells
/// [`default_monitor_source`]'s callers when to open it again.
#[cfg(target_os = "macos")]
pub(super) fn monitor_input(_output: &str) -> Result<String> {
    let os = objc2_foundation::NSProcessInfo::processInfo().operatingSystemVersion();
    require_tap((os.majorVersion, os.minorVersion))?;
    Ok(handy_recorder::SYSTEM_OUTPUT_DEVICE.to_string())
}

/// The first macOS that has the tap (`AudioHardwareCreateProcessTap`).
#[cfg(target_os = "macos")]
const TAP_MIN_MACOS: (isize, isize) = (14, 2);

/// Core Audio is weak-linked for the sake of an older macOS, where the tap
/// isn't there to call: say so instead of opening it.
#[cfg(target_os = "macos")]
fn require_tap(macos: (isize, isize)) -> Result<()> {
    if macos < TAP_MIN_MACOS {
        anyhow::bail!(
            "Meetings need macOS {}.{} or newer, this Mac has {}.{}",
            TAP_MIN_MACOS.0,
            TAP_MIN_MACOS.1,
            macos.0,
            macos.1
        );
    }
    Ok(())
}

/// How long PulseAudio gets to say what the default output is.
const PULSE_TIMEOUT: Duration = Duration::from_secs(2);

/// The monitor source of the current default output, by its PulseAudio
/// name; on macOS, the default output itself, by its Core Audio id. Asked on
/// a thread of its own, so a server that never answers costs
/// [`PULSE_TIMEOUT`], not a stuck caller.
pub(super) fn default_monitor_source() -> Result<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("pulse-default-output".into())
        .spawn(move || {
            let _ = tx.send(query_default_monitor());
        })?;
    rx.recv_timeout(PULSE_TIMEOUT)
        .map_err(|_| anyhow!("PulseAudio didn't say what the default output is"))?
}

#[cfg(target_os = "linux")]
fn query_default_monitor() -> Result<String> {
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

#[cfg(target_os = "macos")]
fn query_default_monitor() -> Result<String> {
    use rodio::cpal::traits::{DeviceTrait, HostTrait};

    let output = crate::audio_toolkit::get_cpal_host()
        .default_output_device()
        .ok_or_else(|| anyhow!("No output device"))?;
    let id = output.id().context("Failed to read the default output")?;
    Ok(id.to_string())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn query_default_monitor() -> Result<String> {
    Err(anyhow!(
        "Recording the output is only supported on Linux and macOS"
    ))
}

/// The id of the monitor input named `monitor_source`, from `(id, is_monitor)`
/// pairs. Ids are `<host>:<name>`.
#[cfg_attr(target_os = "macos", allow(dead_code))]
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

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(samples: &[f32]) -> AudioChunk<'_> {
        AudioChunk {
            samples,
            sample_rate: 16_000,
            channels: 1,
            valid_frames: samples.len(),
        }
    }

    /// 60 chunks of 100 ms: 96,000 samples, one flush in.
    fn record(sink: &mut WavSink) {
        let audio = vec![0.25; 1_600];
        for _ in 0..60 {
            sink.process_chunk(chunk(&audio));
        }
    }

    #[test]
    fn a_track_the_process_never_finalized_reads_back_to_its_last_flush() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mic.wav");
        let mut sink = WavSink::create(&path, Arc::default()).unwrap();
        record(&mut sink);
        // As if the process were killed: nothing finalizes the file.
        std::mem::forget(sink);
        let reader = hound::WavReader::open(&path).unwrap();
        assert_eq!(reader.duration(), 80_000);
        assert_eq!(
            reader.into_samples::<i16>().map_while(Result::ok).count(),
            80_000
        );
    }

    #[test]
    fn a_track_moved_to_another_output_stays_in_time() {
        let dir = tempfile::tempdir().unwrap();
        let feed = Arc::new(TrackFeed::default());
        let mut sink = WavSink::create(&dir.path().join("system.wav"), Arc::clone(&feed)).unwrap();
        let tenth = vec![0.5; 1_600];
        sink.process_chunk(chunk(&tenth));
        // The move: the old output stops, the new one takes a while to start.
        std::thread::sleep(Duration::from_millis(300));
        sink.pad_to(Instant::now()).unwrap();
        sink.realign = true;
        std::thread::sleep(Duration::from_millis(200));
        sink.process_chunk(chunk(&tenth));
        let heard = Instant::now().saturating_duration_since(*feed.first_audio.get().unwrap());
        let (samples, problem) = sink.finish().unwrap();
        assert_eq!(problem, None);
        // Silence fills the time between, so the track is as long as it ran.
        let expected = (heard.as_secs_f64() * 16_000.0) as u32;
        assert!(samples.abs_diff(expected) < 160, "{samples} vs {expected}");
        assert!(samples >= 8_000);
    }

    /// An input on `mic`, as [`open_recorder`] opens one.
    fn fake_input(mic: &handy_recorder::testing::FakeMic) -> OpenedInput {
        let failed = Arc::new(AtomicBool::new(false));
        let handler = {
            let failed = Arc::clone(&failed);
            Box::new(move |_| failed.store(true, Ordering::Release))
        };
        let recorder = mic
            .open(
                RecorderConfig::speech(),
                Some(handler),
                Duration::from_secs(2),
            )
            .unwrap();
        (recorder, failed)
    }

    fn wait_for(what: &str, done: impl Fn() -> bool) {
        let started = Instant::now();
        while !done() {
            assert!(started.elapsed() < Duration::from_secs(5), "{what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_track_whose_input_failed_goes_on_from_a_new_one() {
        use handy_recorder::testing::{FakeError, FakeMic};

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("system.wav");
        let feed = Arc::new(TrackFeed::default());
        let headset = FakeMic::new(16_000, 1);
        let (recorder, failed) = fake_input(&headset);
        recorder
            .start(WavSink::create(&file, Arc::clone(&feed)).unwrap())
            .unwrap();
        let mut track = TrackRecorder {
            kind: TrackKind::System,
            file,
            feed,
            recorder,
            failed,
        };
        let started = Instant::now();
        let tenth = vec![0.5; 1_600];
        assert!(headset.push(&tenth));
        wait_for("the first audio", || track.feed.take_heard());

        // The headset's stream ends under the track, as when its sample
        // rate changes: nothing reaches the file until it's opened again.
        assert!(headset.report_error(FakeError::Other));
        wait_for("the failure", || track.failed.load(Ordering::Acquire));
        std::thread::sleep(Duration::from_millis(300));

        let again = FakeMic::new(16_000, 1);
        let previous = track.move_to(fake_input(&again)).unwrap();
        assert!(!track.failed.load(Ordering::Acquire));
        previous.close().unwrap();
        assert!(again.push(&tenth));
        wait_for("audio from the new input", || track.feed.take_heard());

        let stopped = std::thread::scope(|scope| {
            let stopping = scope.spawn(|| stop_track(&track, started));
            // A stop waits for the audio thread, which this test is.
            while !stopping.is_finished() {
                again.push(&[0.0; 16]);
                std::thread::sleep(Duration::from_millis(5));
            }
            stopping.join().unwrap().unwrap()
        });
        assert_eq!(stopped.problem, None);
        // Both tenths, and silence for the time without an input.
        assert!(stopped.duration_ms >= 400, "{}", stopped.duration_ms);
        let loud = hound::WavReader::open(&track.file)
            .unwrap()
            .into_samples::<i16>()
            .filter(|s| *s.as_ref().unwrap() != 0)
            .count();
        assert_eq!(loud, 3_200);
    }

    #[test]
    fn a_flushed_then_finalized_track_keeps_every_sample() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mic.wav");
        let feed = Arc::new(TrackFeed::default());
        let mut sink = WavSink::create(&path, Arc::clone(&feed)).unwrap();
        let started = Instant::now();
        record(&mut sink);
        assert_eq!(sink.finish().unwrap(), (96_000, None));
        let samples: Vec<i16> = hound::WavReader::open(&path)
            .unwrap()
            .into_samples()
            .map(Result::unwrap)
            .collect();
        assert_eq!(samples.len(), 96_000);
        assert!(samples
            .iter()
            .all(|&s| s == (0.25 * i16::MAX as f32) as i16));
        assert!(feed.offset_ms(started).is_some());
        assert_eq!(feed.take_peak(), 0.25);
        assert_eq!(feed.take_peak(), 0.0, "read once");
    }

    #[test]
    fn a_track_is_heard_once_a_sample_is_not_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("system.wav");
        let feed = Arc::new(TrackFeed::default());
        let mut sink = WavSink::create(&path, Arc::clone(&feed)).unwrap();
        sink.process_chunk(chunk(&[0.0; 1_600]));
        assert!(!feed.take_heard(), "only zeros: macOS withheld the audio");
        // What an allowed tap hears when nothing plays: the recorder's own
        // output, too quiet to reach the 16-bit file.
        sink.process_chunk(chunk(&[1.0e-5; 1_600]));
        assert!(feed.take_heard());
        assert!(!feed.take_heard(), "read once");
        assert_eq!(sink.finish().unwrap(), (3_200, None));
        assert!(hound::WavReader::open(&path)
            .unwrap()
            .into_samples::<i16>()
            .all(|s| s.unwrap() == 0));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_macos_without_the_tap_is_told_which_one_has_it() {
        for old in [(13, 6), (14, 0), (14, 1)] {
            let error = require_tap(old).unwrap_err().to_string();
            assert!(error.starts_with("Meetings need macOS 14.2 or newer"));
        }
        for new in [(14, 2), (14, 6), (15, 0), (26, 1)] {
            assert!(require_tap(new).is_ok());
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
}
