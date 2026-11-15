//! Capture: each track streamed to its own WAV, and the output's monitor
//! found through PulseAudio.

use anyhow::{anyhow, Context, Result};
use handy_recorder::{AudioChunk, Recorder, RecorderConfig, Sink};
use hound::{WavSpec, WavWriter};
use log::info;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::TrackKind;

/// Writes 16 kHz mono audio straight to a WAV as it arrives: no VAD and no
/// buffer of the whole recording, so memory stays flat however long it runs.
pub(super) struct WavSink {
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

    /// Finalizes the file. When the first sample was captured, if any audio
    /// came, and the samples written.
    fn finish(self) -> Result<(Option<Instant>, u32)> {
        if let Some(e) = self.error {
            return Err(e.into());
        }
        let samples = self.writer.len();
        self.writer.finalize()?;
        Ok((self.first_audio, samples))
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

pub(super) struct TrackRecorder {
    pub kind: TrackKind,
    pub file: PathBuf,
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
    recorder
        .start(WavSink::create(&file)?)
        .map_err(|e| anyhow!(e.error).context(format!("Failed to start the {kind:?} input")))?;
    Ok(TrackRecorder {
        kind,
        file,
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
    let (first_audio, samples) = stopped.sink.finish()?;
    let start_offset_ms = first_audio
        .map(|at| at.saturating_duration_since(meeting_started).as_millis() as i64)
        .unwrap_or(0);
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
