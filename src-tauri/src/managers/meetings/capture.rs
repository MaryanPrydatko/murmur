//! Capture: each track streamed to its own WAV, and the output's monitor
//! found through PulseAudio.

use anyhow::{anyhow, Context, Result};
use handy_recorder::{AudioChunk, Recorder, RecorderConfig, Sink};
use hound::{WavSpec, WavWriter};
use log::info;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use super::TrackKind;

/// Samples between header updates: a crash loses at most 5 s of a track.
const FLUSH_SAMPLES: u32 = 80_000;

/// What a track's sink shares with the meeting while it records.
#[derive(Default)]
pub(super) struct TrackFeed {
    /// When the first sample was captured.
    first_audio: OnceLock<Instant>,
}

impl TrackFeed {
    /// When the track's first sample was heard, from `started`, once it was.
    pub fn offset_ms(&self, started: Instant) -> Option<i64> {
        self.first_audio
            .get()
            .map(|at| at.saturating_duration_since(started).as_millis() as i64)
    }
}

/// Writes 16 kHz mono audio straight to a WAV as it arrives: no VAD and no
/// buffer of the whole recording, so memory stays flat however long it runs.
/// The header is brought up to date every [`FLUSH_SAMPLES`], so the file
/// reads back to then whatever happens to the process.
pub(super) struct WavSink {
    writer: WavWriter<BufWriter<File>>,
    feed: Arc<TrackFeed>,
    unflushed: u32,
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
            feed,
            unflushed: 0,
            error: None,
        })
    }

    /// Finalizes the file. The samples written.
    fn finish(self) -> Result<u32> {
        if let Some(e) = self.error {
            return Err(e.into());
        }
        let samples = self.writer.len();
        self.writer.finalize()?;
        Ok(samples)
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
        self.feed
            .first_audio
            .get_or_init(|| now.checked_sub(captured).unwrap_or(now));
        let samples = chunk.samples[..chunk.valid_frames]
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
}

pub(super) fn open_track(
    dir: &Path,
    kind: TrackKind,
    device: Option<String>,
) -> Result<TrackRecorder> {
    let file = dir.join(kind.file_name());
    let recorder = Recorder::open(RecorderConfig {
        device,
        ..RecorderConfig::speech()
    })
    .with_context(|| format!("Failed to open the {kind:?} input"))?;
    let feed = Arc::new(TrackFeed::default());
    recorder
        .start(WavSink::create(&file, Arc::clone(&feed))?)
        .map_err(|e| anyhow!(e.error).context(format!("Failed to start the {kind:?} input")))?;
    Ok(TrackRecorder {
        kind,
        file,
        feed,
        recorder,
    })
}

/// Stops the track and finalizes its WAV. Its start offset and length in ms.
pub(super) fn stop_track(track: &TrackRecorder, meeting_started: Instant) -> Result<(i64, i64)> {
    let stopped = track.recorder.stop()?;
    info!(
        "Meeting {:?} track ended: {:?}, {} frames dropped",
        track.kind, stopped.end_reason, stopped.dropped_frames
    );
    let samples = stopped.sink.finish()?;
    let start_offset_ms = track.feed.offset_ms(meeting_started).unwrap_or(0);
    Ok((start_offset_ms, i64::from(samples) * 1000 / 16_000))
}

/// The monitor source of the current default output, by its PulseAudio name.
#[cfg(target_os = "linux")]
pub(super) fn default_monitor_source() -> Result<String> {
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
pub(super) fn default_monitor_source() -> Result<String> {
    Err(anyhow!("Recording the output is only supported on Linux"))
}

/// The id of the monitor input named `monitor_source`, from `(id, is_monitor)`
/// pairs. Ids are `<host>:<name>`.
pub(super) fn monitor_device_id<'a>(
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
    fn a_flushed_then_finalized_track_keeps_every_sample() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mic.wav");
        let feed = Arc::new(TrackFeed::default());
        let mut sink = WavSink::create(&path, Arc::clone(&feed)).unwrap();
        let started = Instant::now();
        record(&mut sink);
        assert_eq!(sink.finish().unwrap(), 96_000);
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
