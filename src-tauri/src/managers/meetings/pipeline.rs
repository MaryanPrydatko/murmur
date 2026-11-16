//! A queued meeting's transcription: every track transcribed, the system
//! track diarized, and the transcript written. Runs on the job thread.

use anyhow::{anyhow, bail, Context, Result};
use log::{debug, error, info, warn};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use transcribe_cpp::{
    DiarizeExtension, DiarizeOptions, SortformerDiarizeOptions, SortformerPreset,
};

use super::recovery::wav_lengths;
use super::transcript::{
    assign_speakers, merge_minor_speakers, merge_utterances, render_transcript, Turn,
};
use super::{
    meetings_dir, read_meeting, store, write_meeting, Meeting, MeetingManager, MeetingProgress,
    MeetingStatus, MeetingStep, Rerun, Speaker, TrackKind,
};
use crate::engine_supervisor::EngineError;
use crate::managers::history::HistoryManager;
use crate::managers::model::ModelManager;
use crate::managers::transcription::{ChunkControl, TimedTranscript, TranscriptionManager};
use crate::notices::{self, Notice, NoticeCode, NoticeSource};

/// Tries at diarizing when a dictation's cancel interrupts it.
const MAX_DIARIZE_ATTEMPTS: usize = 3;

/// How long the speaker step waits for a download of the speaker model
/// that's under way, and how often it looks.
const DIARIZER_WAIT: Duration = Duration::from_secs(10 * 60);
const DIARIZER_POLL: Duration = Duration::from_millis(500);

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
    crate::overlay::show_meeting_transcribing_overlay(app);
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
    // What went wrong while recording, then any track whose file ends early.
    let mut problems = app
        .state::<Arc<MeetingManager>>()
        .take_track_problems(&meeting.id);
    let mut transcribed = false;
    // Whether the transcript was written: a speakers-only rerun that can't
    // tell them apart keeps the one there is.
    let result = (|| -> Result<bool> {
        let mut tracks = Vec::new();
        // The language of the track with the most words.
        let mut language: Option<(usize, String)> = None;
        for track in &meeting.tracks {
            let step = match track.kind {
                TrackKind::Mic => MeetingStep::Mic,
                TrackKind::System | TrackKind::Mixed => MeetingStep::System,
            };
            let transcript = track_words(rerun, &meeting.dir, track.kind, || {
                transcribed = true;
                let (samples, total, short) = read_track(&track.file)?;
                if short {
                    problems
                        .cut_short
                        .push(format!("{:?}: the file ends early", track.kind));
                }
                let reporting = Reporting::new(app, &meeting.id, step, control);
                tm.transcribe_words_in_chunks(samples, total, &reporting)
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
                    emit_progress(app, &meeting.id, MeetingStep::Speakers, None);
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
                            if rerun == Rerun::Speakers {
                                return Ok(false);
                            }
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
        if transcribed {
            meeting.asr_model = tm.get_current_model();
        }
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
        Ok(true)
    })();
    meeting.status = match &result {
        Ok(true) => MeetingStatus::Done,
        Ok(false) if meeting.dir.join("transcript.md").exists() => MeetingStatus::Done,
        Ok(false) | Err(_) => MeetingStatus::Failed,
    };
    if let Err(e) = write_meeting(app, &meeting) {
        error!("Failed to save meeting {}: {e:#}", meeting.id);
    }
    match result {
        Ok(written) => {
            notices::clear(app, NoticeSource::Meeting);
            if written {
                info!("Meeting {} transcribed", meeting.id);
                super::notes::on_done(app, &meeting.id);
            } else {
                info!(
                    "Meeting {}: speakers not told apart; kept its transcript",
                    meeting.id
                );
            }
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
            problems.raise(app);
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

fn emit_progress(app: &AppHandle, id: &str, step: MeetingStep, percent: Option<u8>) {
    let progress = MeetingProgress {
        id: id.to_string(),
        step,
        percent,
    };
    match progress.emit(app) {
        Ok(()) => debug!("Meeting {id}: {step:?} {percent:?}"),
        Err(e) => warn!("Failed to emit meeting progress: {e}"),
    }
}

/// The job's control, reporting a track's progress as the meeting's.
struct Reporting<'a> {
    app: &'a AppHandle,
    id: &'a str,
    step: MeetingStep,
    control: &'a dyn ChunkControl,
    /// The last percent sent.
    percent: Cell<Option<u8>>,
}

impl<'a> Reporting<'a> {
    fn new(
        app: &'a AppHandle,
        id: &'a str,
        step: MeetingStep,
        control: &'a dyn ChunkControl,
    ) -> Self {
        Self {
            app,
            id,
            step,
            control,
            percent: Cell::new(None),
        }
    }
}

impl ChunkControl for Reporting<'_> {
    fn proceed(&self) -> bool {
        self.control.proceed()
    }

    fn progress(&self, done: usize, total: usize) {
        let percent = (done * 100 / total.max(1)).min(100) as u8;
        if self.percent.replace(Some(percent)) != Some(percent) {
            emit_progress(self.app, self.id, self.step, Some(percent));
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

/// Waits while the speaker model is still downloading, at most `wait`: a
/// meeting starts the download, and a short one is transcribed before it
/// ends. Returns once the model is `ready`, nothing is `downloading`, or the
/// time is up; fails only when `control` stopped the transcription.
fn await_diarizer(
    ready: impl Fn() -> bool,
    downloading: impl Fn() -> bool,
    control: &dyn ChunkControl,
    wait: Duration,
    poll: Duration,
) -> Result<()> {
    let started = Instant::now();
    while !ready() && downloading() && started.elapsed() < wait {
        if !control.proceed() {
            bail!("Transcription was stopped");
        }
        std::thread::sleep(poll);
    }
    Ok(())
}

/// Who spoke when on the system track, in one run over all of it, or why
/// not. Fails only when `control` stopped the transcription.
fn diarize_track(
    app: &AppHandle,
    file: &Path,
    control: &dyn ChunkControl,
) -> Result<Result<Vec<Turn>, Unseparated>> {
    let models = app.state::<Arc<ModelManager>>();
    await_diarizer(
        || models.diarizer_path().is_some(),
        || models.diarizer_downloading(),
        control,
        DIARIZER_WAIT,
        DIARIZER_POLL,
    )?;
    let Some(model) = models.diarizer_path() else {
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

    /// A job's control that counts how often it's asked, and stops when told.
    struct Asked {
        times: Cell<u32>,
        go_on: bool,
    }

    impl ChunkControl for Asked {
        fn proceed(&self) -> bool {
            self.times.set(self.times.get() + 1);
            self.go_on
        }

        fn progress(&self, _done: usize, _total: usize) {}
    }

    fn asked(go_on: bool) -> Asked {
        Asked {
            times: Cell::new(0),
            go_on,
        }
    }

    const POLL: Duration = Duration::from_millis(1);
    const LONG: Duration = Duration::from_secs(60);

    #[test]
    fn the_speaker_step_does_not_wait_for_a_model_that_is_there_or_not_coming() {
        let control = asked(true);
        let downloading = Cell::new(0);
        let count = || {
            downloading.set(downloading.get() + 1);
            true
        };
        // On disk: nothing to wait for, whatever is downloading.
        await_diarizer(|| true, count, &control, LONG, POLL).unwrap();
        assert!(downloading.get() <= 1);
        // Missing and not downloading: straight on to the fallback.
        let started = Instant::now();
        await_diarizer(|| false, || false, &control, LONG, LONG).unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(control.times.get(), 0);
    }

    #[test]
    fn the_speaker_step_waits_for_a_download_to_finish() {
        let control = asked(true);
        let polls = Cell::new(0);
        let ready = || polls.get() == 3;
        let downloading = || {
            polls.set(polls.get() + 1);
            true
        };
        await_diarizer(ready, downloading, &control, LONG, POLL).unwrap();
        assert_eq!(polls.get(), 3);
        assert_eq!(control.times.get(), 3);
    }

    #[test]
    fn the_speaker_step_gives_up_on_a_download_that_takes_too_long() {
        let control = asked(true);
        let wait = Duration::from_millis(30);
        let started = Instant::now();
        await_diarizer(|| false, || true, &control, wait, Duration::from_millis(5)).unwrap();
        let took = started.elapsed();
        assert!(took >= wait && took < Duration::from_secs(5), "{took:?}");
    }

    #[test]
    fn a_stopped_transcription_stops_waiting_for_the_speaker_model() {
        let control = asked(false);
        let e = await_diarizer(|| false, || true, &control, LONG, POLL).unwrap_err();
        assert_eq!(e.to_string(), "Transcription was stopped");
        assert_eq!(control.times.get(), 1);
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
