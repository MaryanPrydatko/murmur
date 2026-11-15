//! A queued meeting's transcription: every track transcribed, the system
//! track diarized, and the transcript written. Runs on the job thread.

use anyhow::{anyhow, bail, Context, Result};
use log::{error, info, warn};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, Manager};
use transcribe_cpp::{
    DiarizeExtension, DiarizeOptions, SortformerDiarizeOptions, SortformerPreset,
};

use super::recovery::wav_lengths;
use super::transcript::{
    assign_speakers, merge_minor_speakers, merge_utterances, render_transcript, Turn,
};
use super::{
    meetings_dir, read_meeting, store, write_meeting, Meeting, MeetingStatus, Rerun, Speaker,
    TrackKind,
};
use crate::engine_supervisor::EngineError;
use crate::managers::history::HistoryManager;
use crate::managers::model::ModelManager;
use crate::managers::transcription::{ChunkControl, TimedTranscript, TranscriptionManager};
use crate::notices::{self, Notice, NoticeCode, NoticeSource};

/// Tries at diarizing when a dictation's cancel interrupts it.
const MAX_DIARIZE_ATTEMPTS: usize = 3;

/// Why the system track's speakers weren't told apart.
enum Unseparated {
    NoModel,
    Failed(anyhow::Error),
}

/// Runs the meeting `id`'s queued transcription from its `meeting.json`,
/// counted as an attempt before it starts so a crash during it counts too.
pub(crate) fn run_job(app: &AppHandle, id: &str, rerun: Rerun, control: &dyn ChunkControl) {
    let meeting = meetings_dir(app).and_then(|dir| read_meeting(&dir.join(id)));
    let mut meeting = match meeting {
        Ok(meeting) => meeting,
        Err(e) => return fail_job(app, id, &format!("{e:#}")),
    };
    meeting.begin_attempt();
    if let Err(e) = write_meeting(app, &meeting) {
        error!("Failed to save meeting {id}: {e:#}");
    }
    info!("Meeting {id}: attempt {} ({rerun:?})", meeting.attempts);
    transcribe(app, meeting, rerun, control);
}

/// Marks the meeting `id` failed and says why, as when its job panicked.
pub(crate) fn fail_job(app: &AppHandle, id: &str, error: &str) {
    error!("Meeting {id} transcription failed: {error}");
    match meetings_dir(app).and_then(|dir| read_meeting(&dir.join(id))) {
        Ok(mut meeting) => {
            meeting.status = MeetingStatus::Failed;
            if let Err(e) = write_meeting(app, &meeting) {
                error!("Failed to save meeting {id}: {e:#}");
            }
        }
        Err(e) => error!("Failed to mark meeting {id} failed: {e:#}"),
    }
    notices::raise(
        app,
        Notice::new(
            NoticeCode::MeetingTranscriptionFailed,
            NoticeSource::Meeting,
        )
        .with_detail(error.to_string()),
    );
}

/// Transcribes every track, or with [`Rerun::Speakers`] reuses the words
/// each already has, and writes the transcript.
fn transcribe(app: &AppHandle, mut meeting: Meeting, rerun: Rerun, control: &dyn ChunkControl) {
    let tm = app.state::<Arc<TranscriptionManager>>();
    let mut unseparated = None;
    let mut cut_short = Vec::new();
    let result = (|| -> Result<()> {
        let mut tracks = Vec::new();
        // The language of the track with the most words.
        let mut language: Option<(usize, String)> = None;
        for track in &meeting.tracks {
            let transcript = track_words(rerun, &meeting.dir, track.kind, || {
                let (samples, total, short) = read_track(&track.file)?;
                if short {
                    cut_short.push(format!("{:?}: the file ends early", track.kind));
                }
                tm.transcribe_words_in_chunks(samples, total, control)
            })?
            .ok_or_else(|| anyhow!("Transcription was stopped"))?;
            let track_words = transcript.words;
            if let Some(spoken) = transcript.language {
                if language
                    .as_ref()
                    .is_none_or(|(most, _)| track_words.len() > *most)
                {
                    language = Some((track_words.len(), spoken));
                }
            }
            info!(
                "Meeting {:?} track: {} words",
                track.kind,
                track_words.len()
            );
            let speakers = match track.kind {
                TrackKind::Mic => vec![Speaker::Me; track_words.len()],
                // Nobody spoke, so there's no one to tell apart.
                TrackKind::System | TrackKind::Mixed if track_words.is_empty() => Vec::new(),
                TrackKind::System | TrackKind::Mixed => {
                    match diarize_track(app, &track.file, control)? {
                        Ok(turns) => {
                            meeting.diar_model = Some(crate::catalog::DIARIZER_CATALOG_ID.into());
                            assign_speakers(&track_words, &merge_minor_speakers(turns))
                        }
                        Err(why) => {
                            if let Unseparated::Failed(e) = &why {
                                warn!("Meeting {} diarization failed: {e:#}", meeting.id);
                            }
                            unseparated = Some(why);
                            vec![Speaker::Them; track_words.len()]
                        }
                    }
                }
            };
            tracks.push((
                track.start_offset_ms,
                speakers.into_iter().zip(track_words).collect(),
            ));
        }
        meeting.asr_model = tm.get_current_model();
        meeting.language = language.map(|(_, language)| language);
        let utterances = merge_utterances(tracks);
        let path = meeting.dir.join("transcript.md");
        std::fs::write(&path, render_transcript(&meeting, &utterances))
            .with_context(|| format!("Failed to write {}", path.display()))?;
        let history = app.state::<Arc<HistoryManager>>();
        if let Err(e) = history
            .get_connection()
            .and_then(|mut conn| store::save_transcript(&mut conn, &meeting.id, &utterances))
        {
            error!("Failed to store meeting {}'s transcript: {e:#}", meeting.id);
        }
        Ok(())
    })();
    meeting.status = match &result {
        Ok(()) => MeetingStatus::Done,
        Err(_) => MeetingStatus::Failed,
    };
    if let Err(e) = write_meeting(app, &meeting) {
        error!("Failed to save meeting {}: {e:#}", meeting.id);
    }
    match result {
        Ok(()) => {
            info!("Meeting {} transcribed", meeting.id);
            notices::clear(app, NoticeSource::Meeting);
            super::notes::on_done(app, &meeting.id);
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
            if !cut_short.is_empty() {
                notices::raise(
                    app,
                    Notice::new(NoticeCode::MeetingTrackIncomplete, NoticeSource::Meeting)
                        .with_detail(cut_short.join("; ")),
                );
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

/// Where a track's words are kept, so its speakers can be told apart again
/// without transcribing it again.
fn words_path(dir: &Path, kind: TrackKind) -> PathBuf {
    let kind = serde_json::to_value(kind)
        .ok()
        .and_then(|k| k.as_str().map(str::to_string))
        .unwrap_or_default();
    dir.join(format!("words.{kind}.json"))
}

/// A track's words: the ones kept from its last transcription for
/// [`Rerun::Speakers`], else from `transcribe`, then kept. `None` if the
/// transcription was stopped.
fn track_words(
    rerun: Rerun,
    dir: &Path,
    kind: TrackKind,
    transcribe: impl FnOnce() -> Result<Option<TimedTranscript>>,
) -> Result<Option<TimedTranscript>> {
    let path = words_path(dir, kind);
    if rerun == Rerun::Speakers {
        match std::fs::read(&path).map(|json| serde_json::from_slice(&json)) {
            Ok(Ok(kept)) => {
                info!("Reusing the {kind:?} track's words");
                return Ok(Some(kept));
            }
            Ok(Err(e)) => warn!("Transcribing again: {} won't parse: {e}", path.display()),
            Err(e) => info!("Transcribing again: no {}: {e}", path.display()),
        }
    }
    let transcript = transcribe()?;
    if let Some(transcript) = &transcript {
        let partial = path.with_extension("json.partial");
        if let Err(e) = serde_json::to_vec(transcript)
            .map_err(anyhow::Error::from)
            .and_then(|json| Ok(std::fs::write(&partial, json)?))
            .and_then(|()| Ok(std::fs::rename(&partial, &path)?))
        {
            warn!("Failed to keep the {kind:?} track's words: {e:#}");
        }
    }
    Ok(transcript)
}

/// A track's samples, read as they're needed, how many there are, and
/// whether the file holds fewer than its header says.
fn read_track(file: &Path) -> Result<(impl Iterator<Item = f32>, usize, bool)> {
    let (said, held) = wav_lengths(file)?;
    if held < said {
        warn!("{} holds {held} of its {said} samples", file.display());
    }
    let reader = hound::WavReader::open(file)
        .with_context(|| format!("Failed to open {}", file.display()))?;
    let total = said.min(held) as usize;
    let samples = reader
        .into_samples::<i16>()
        .map_while(Result::ok)
        .map(|s| s as f32 / i16::MAX as f32);
    Ok((samples, total, held < said))
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
    // The worker reads the track itself; the header's length sizes its deadline.
    let samples = match hound::WavReader::open(file) {
        Ok(reader) => reader.duration() as usize,
        Err(e) => return Ok(Err(Unseparated::Failed(e.into()))),
    };
    let mut attempts = 0;
    loop {
        if !control.proceed() {
            bail!("Transcription was stopped");
        }
        attempts += 1;
        let started = Instant::now();
        match tm.diarize(model.clone(), options.clone(), file.to_path_buf(), samples) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managers::transcription::TimedWord;

    fn said() -> TimedTranscript {
        TimedTranscript {
            words: vec![TimedWord {
                t0_ms: 0,
                t1_ms: 400,
                text: "hello".into(),
            }],
            language: Some("en".into()),
        }
    }

    #[test]
    fn telling_speakers_apart_again_reuses_the_kept_words() {
        let dir = tempfile::tempdir().unwrap();
        let first = track_words(Rerun::All, dir.path(), TrackKind::System, || {
            Ok(Some(said()))
        });
        assert_eq!(first.unwrap(), Some(said()));
        assert!(dir.path().join("words.system.json").exists());

        let again = track_words(Rerun::Speakers, dir.path(), TrackKind::System, || {
            panic!("transcribed again")
        });
        assert_eq!(again.unwrap(), Some(said()));
    }

    #[test]
    fn a_track_without_kept_words_is_transcribed() {
        let dir = tempfile::tempdir().unwrap();
        let mut ran = false;
        let words = track_words(Rerun::Speakers, dir.path(), TrackKind::Mixed, || {
            ran = true;
            Ok(Some(said()))
        });
        assert_eq!(words.unwrap(), Some(said()));
        assert!(ran);
        assert!(dir.path().join("words.mixed.json").exists());
        // Everything again transcribes again, whatever is kept.
        let mut ran = false;
        track_words(Rerun::All, dir.path(), TrackKind::Mixed, || {
            ran = true;
            Ok(None)
        })
        .unwrap();
        assert!(ran);
    }
}
