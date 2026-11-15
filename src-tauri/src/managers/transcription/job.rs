//! Background transcription, always chunked: chunks are the unit of
//! progress, of yielding to dictation, and of retry.

use anyhow::Result;
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use std::ops::Range;
use std::path::PathBuf;
use std::time::Instant;
use transcribe_cpp::{DiarizeOptions, RunOptions, SpeakerSegment, TimestampKind, Transcript};

use super::{
    agreed_language, engine_error, post_process_transcription_text, real_time_factor, BatchContext,
    RunSetup, Transcription, TranscriptionManager,
};
use crate::audio_toolkit::chunking::Chunker;
use crate::engine_supervisor::EngineError;
use crate::language_detection::speech::{gather_head, SpeechHead};
use crate::managers::audio::{create_detector, create_smoothed_detector};
use crate::settings::AppSettings;

/// Tries per chunk when a dictation's cancel interrupts it.
const MAX_CHUNK_ATTEMPTS: usize = 3;

/// Audio handed to the chunker at a time: the first chunk is transcribed
/// without scoring the whole file first. Cuts match the whole-file plan (a
/// rare short blip at a feed boundary can nudge one within its pause).
const FEED_SAMPLES: usize = 60 * 16_000;

/// What a background transcription asks of whoever runs it.
pub trait ChunkControl {
    /// Called before each feed to the chunker and before each chunk is
    /// decoded. Blocks while dictation is using the engine; `false` stops the
    /// transcription.
    fn proceed(&self) -> bool;
    /// `done` of `total` samples are transcribed.
    fn progress(&self, done: usize, total: usize);
}

/// Pieces searched for the speech the language is detected from: a
/// recording that starts with silence still gets its language.
const MAX_HEAD_PIECES: usize = 10;

/// A word, in milliseconds from the start of the audio it was decoded from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimedWord {
    pub t0_ms: i64,
    pub t1_ms: i64,
    pub text: String,
}

/// Words timed from the start of the audio, and the language they're in.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TimedTranscript {
    pub words: Vec<TimedWord>,
    pub language: Option<String>,
}

/// What the chunk loop decoded, with what it ran with.
struct Decoded {
    /// Each chunk's samples in the audio, in order.
    spans: Vec<Range<usize>>,
    transcripts: Vec<Transcript>,
    settings: AppSettings,
    setup: RunSetup,
}

impl TranscriptionManager {
    /// Transcribe `audio` chunk by chunk with the selected model, one
    /// paragraph per chunk. `None` if `control` stopped it.
    pub fn transcribe_in_chunks(
        &self,
        audio: Vec<f32>,
        control: &dyn ChunkControl,
    ) -> Result<Option<Transcription>> {
        let started = Instant::now();
        let total = audio.len();
        if total == 0 {
            return Ok(Some(Transcription {
                text: String::new(),
                language: None,
            }));
        }
        self.begin_batch();
        let context = self.batch_context(&audio)?;
        let pieces = audio.chunks(FEED_SAMPLES).map(<[f32]>::to_vec);
        let Some(Decoded {
            transcripts,
            settings,
            setup,
            ..
        }) = self.decode_chunks(context, pieces, total, TimestampKind::Auto, control)?
        else {
            return Ok(None);
        };

        let language = setup.spoken_language(agreed_language(&transcripts).as_deref());
        let paragraphs: Vec<String> = transcripts
            .into_iter()
            .map(|t| {
                let output_language = setup.output_language(&settings, t.language);
                post_process_transcription_text(
                    t.text,
                    &settings,
                    setup.custom_words_prompted(),
                    &output_language,
                    &setup.languages,
                )
            })
            .filter(|p| !p.trim().is_empty())
            .collect();

        let elapsed_secs = started.elapsed().as_secs_f64();
        let audio_secs = total as f64 / 16_000.0;
        info!(
            "Background transcription of {:.1}s of audio in {} paragraphs took {:.1}s ({:.1}x real time)",
            audio_secs,
            paragraphs.len(),
            elapsed_secs,
            real_time_factor(audio_secs, elapsed_secs)
        );
        Ok(Some(Transcription {
            text: paragraphs.join("\n\n"),
            language: Some(language),
        }))
    }

    /// Transcribe `total` samples into words timed from the start of the
    /// audio, reading `samples` a piece at a time so a long recording is
    /// never all in memory before it is cut. A language to detect is
    /// detected from the first window of speech in the first
    /// [`MAX_HEAD_PIECES`] pieces. `None` if `control` stopped it.
    pub fn transcribe_words_in_chunks(
        &self,
        mut samples: impl Iterator<Item = f32>,
        total: usize,
        control: &dyn ChunkControl,
    ) -> Result<Option<TimedTranscript>> {
        let mut pieces = std::iter::from_fn(move || {
            let piece: Vec<f32> = samples.by_ref().take(FEED_SAMPLES).collect();
            (!piece.is_empty()).then_some(piece)
        })
        .peekable();
        if pieces.peek().is_none() {
            return Ok(Some(TimedTranscript::default()));
        }
        self.begin_batch();
        let planned = self.batch_plan()?;
        let (head, speech) =
            if planned.plan.detects() && self.model_manager.language_detector_path().is_some() {
                let window = planned.settings.language_detection_window_samples();
                match create_smoothed_detector(planned.settings.vad_backend, &self.app_handle) {
                    Ok(vad) => {
                        let mut head = SpeechHead::new(vad, window);
                        let taken = gather_head(&mut pieces, &mut head, MAX_HEAD_PIECES);
                        (taken, Some(head.into_speech()))
                    }
                    Err(e) => {
                        warn!("Language detection: VAD failed: {e:#}");
                        (Vec::new(), Some(Vec::new()))
                    }
                }
            } else {
                let speech = self.plan_speech(&planned.settings, &planned.plan, &[]);
                (Vec::new(), speech)
            };
        let context = self.batch_context_with(planned, speech);
        let pieces = head.into_iter().chain(pieces);
        let Some(decoded) =
            self.decode_chunks(context, pieces, total, TimestampKind::Word, control)?
        else {
            return Ok(None);
        };
        let language = decoded
            .setup
            .spoken_language(agreed_language(&decoded.transcripts).as_deref())
            .language;
        let words = decoded
            .spans
            .iter()
            .zip(&decoded.transcripts)
            .flat_map(|(span, t)| {
                chunk_words(t, samples_to_ms(span.start), samples_to_ms(span.end))
            })
            .collect();
        Ok(Some(TimedTranscript { words, language }))
    }

    /// Who spoke when in the 16 kHz mono WAV at `audio`, `samples` long, by
    /// the diarizer at `model`, on the loaded transcription model's device.
    /// A dictation's cancel stops it like a chunk, as
    /// [`EngineError::Cancelled`].
    pub fn diarize(
        &self,
        model: PathBuf,
        options: DiarizeOptions,
        audio: PathBuf,
        samples: usize,
    ) -> std::result::Result<Vec<SpeakerSegment>, EngineError> {
        self.touch_activity();
        self.engine
            .diarize_since(self.engine.cancel_mark(), model, options, audio, samples)
    }

    /// Keeps the model from unloading, and starts loading it if it isn't.
    fn begin_batch(&self) {
        self.touch_activity();
        self.initiate_model_load(false);
    }

    /// Cuts the audio at pauses as `pieces` arrive and decodes each chunk
    /// with `context`, read once: the model can't change until the job
    /// ends. Yields to dictation through `control`. `None` if it stopped.
    fn decode_chunks(
        &self,
        context: BatchContext,
        mut pieces: impl Iterator<Item = Vec<f32>>,
        total: usize,
        timestamps: TimestampKind,
        control: &dyn ChunkControl,
    ) -> Result<Option<Decoded>> {
        let BatchContext {
            settings,
            active_model,
            setup,
            ..
        } = context;
        let run_options = RunOptions {
            timestamps,
            ..setup.run_options.clone()
        };

        let backend = settings.vad_backend;
        let app_handle = self.app_handle.clone();
        let mut chunker = Chunker::new(Box::new(move || create_detector(backend, &app_handle)));

        let mut spans = Vec::new();
        let mut transcripts: Vec<Transcript> = Vec::new();
        let mut done = 0;
        control.progress(done, total);
        loop {
            // Scoring waits out dictation too, and stops on cancel.
            if !control.proceed() {
                return Ok(None);
            }
            let (chunks, last) = match pieces.next() {
                Some(piece) => (chunker.push(piece), false),
                None => (chunker.finish(), true),
            };
            for chunk in chunks {
                let len = chunk.len();
                let mut attempts = 0;
                let transcript = loop {
                    if !control.proceed() {
                        return Ok(None);
                    }
                    attempts += 1;
                    // A fresh mark per chunk: a dictation's cancel stops only
                    // the chunk it caught, which is then tried again.
                    let mark = self.engine.cancel_mark();
                    match self.run_batch(mark, chunk.clone(), &run_options, &active_model) {
                        Ok(transcript) => break transcript,
                        Err(EngineError::Cancelled) if attempts < MAX_CHUNK_ATTEMPTS => {
                            warn!("A background chunk was cancelled; trying it again");
                        }
                        Err(e) => return Err(engine_error(e)),
                    }
                };
                transcripts.push(transcript);
                spans.push(done..done + len);
                done += len;
                control.progress(done, total);
            }
            if last {
                break;
            }
        }

        debug!("Transcribed {} chunks in the background", transcripts.len());
        Ok(Some(Decoded {
            spans,
            transcripts,
            settings,
            setup,
        }))
    }
}

fn samples_to_ms(samples: usize) -> i64 {
    (samples as u64 * 1000 / 16_000) as i64
}

/// A chunk's words, moved from chunk time to audio time. A model without
/// word times gives its segments, or else the chunk's whole text as one word
/// spanning the chunk.
fn chunk_words(transcript: &Transcript, start_ms: i64, end_ms: i64) -> Vec<TimedWord> {
    // The vocabulary-echo filter clears only the text.
    if transcript.text.trim().is_empty() {
        return Vec::new();
    }
    let timed = |t0_ms: i64, t1_ms: i64, text: &str| TimedWord {
        t0_ms: start_ms + t0_ms,
        t1_ms: start_ms + t1_ms,
        text: text.trim().to_string(),
    };
    let words: Vec<TimedWord> = if !transcript.words.is_empty() {
        transcript
            .words
            .iter()
            .map(|w| timed(w.t0_ms, w.t1_ms, &w.text))
            .collect()
    } else {
        transcript
            .segments
            .iter()
            .map(|s| timed(s.t0_ms, s.t1_ms, &s.text))
            .collect()
    };
    let words: Vec<TimedWord> = words.into_iter().filter(|w| !w.text.is_empty()).collect();
    if words.is_empty() {
        return vec![TimedWord {
            t0_ms: start_ms,
            t1_ms: end_ms,
            text: transcript.text.trim().to_string(),
        }];
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;
    use transcribe_cpp::{Segment, Word};

    fn word(t0_ms: i64, t1_ms: i64, text: &str) -> Word {
        Word {
            t0_ms,
            t1_ms,
            text: text.to_string(),
            ..Default::default()
        }
    }

    fn timed(t0_ms: i64, t1_ms: i64, text: &str) -> TimedWord {
        TimedWord {
            t0_ms,
            t1_ms,
            text: text.to_string(),
        }
    }

    #[test]
    fn words_move_to_audio_time_by_the_chunk_start() {
        let transcript = Transcript {
            text: "hello there".into(),
            words: vec![word(0, 400, " hello"), word(450, 900, " there")],
            ..Default::default()
        };
        assert_eq!(
            chunk_words(&transcript, 30_000, 60_000),
            vec![
                timed(30_000, 30_400, "hello"),
                timed(30_450, 30_900, "there")
            ]
        );
    }

    #[test]
    fn a_model_without_word_times_falls_back_to_segments_then_the_chunk() {
        let segmented = Transcript {
            text: "one two".into(),
            segments: vec![Segment {
                t0_ms: 100,
                t1_ms: 900,
                text: "one two".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            chunk_words(&segmented, 1_000, 5_000),
            vec![timed(1_100, 1_900, "one two")]
        );

        let untimed = Transcript {
            text: " one two ".into(),
            ..Default::default()
        };
        assert_eq!(
            chunk_words(&untimed, 1_000, 5_000),
            vec![timed(1_000, 5_000, "one two")]
        );
    }

    #[test]
    fn a_chunk_whose_text_was_dropped_has_no_words() {
        let echoed = Transcript {
            text: String::new(),
            words: vec![word(0, 400, "custom")],
            ..Default::default()
        };
        assert!(chunk_words(&echoed, 0, 5_000).is_empty());
    }
}
