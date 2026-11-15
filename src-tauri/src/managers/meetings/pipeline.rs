//! A queued meeting's transcription: every track transcribed, the system
//! track diarized, and the transcript written. Runs on the job thread.

use anyhow::{anyhow, bail, Context, Result};
use log::{error, info, warn};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, Manager};
use transcribe_cpp::{
    DiarizeExtension, DiarizeOptions, SortformerDiarizeOptions, SortformerPreset,
};

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
use crate::managers::transcription::{ChunkControl, TranscriptionManager};
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
    transcribe(app, meeting, control);
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

/// Transcribes every track and writes the transcript.
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
