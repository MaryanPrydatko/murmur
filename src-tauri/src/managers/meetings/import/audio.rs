//! An imported meeting's audio, copied into its folder: the app's own file
//! is never moved or changed.

use anyhow::{Context, Result};
use hound::{SampleFormat, WavSpec, WavWriter};
use std::fs;
use std::path::{Path, PathBuf};

use crate::audio_toolkit::decode_audio_file;

/// Copies `from` into `dir` as `audio.<its extension>`.
pub fn copy_into(from: &Path, dir: &Path) -> Result<PathBuf> {
    let extension = from.extension().and_then(|e| e.to_str()).unwrap_or("audio");
    let to = dir.join(format!("audio.{extension}"));
    fs::copy(from, &to).with_context(|| format!("Failed to copy {}", from.display()))?;
    Ok(to)
}

/// Decodes `from` into the 16 kHz mono 16-bit WAV a meeting is transcribed
/// from, at `to`. Its length in ms.
pub fn decode_to_wav(from: &Path, to: &Path) -> Result<i64> {
    let samples = decode_audio_file(from)?;
    let part = to.with_extension("wav.part");
    let spec = WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut writer = WavWriter::create(&part, spec)
        .with_context(|| format!("Failed to create {}", part.display()))?;
    for sample in &samples {
        writer.write_sample((sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
    }
    writer.finalize()?;
    fs::rename(&part, to).with_context(|| format!("Failed to write {}", to.display()))?;
    Ok(samples.len() as i64 * 1000 / 16_000)
}
