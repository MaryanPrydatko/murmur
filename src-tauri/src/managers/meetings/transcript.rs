//! Who said what and when: speakers given to words, and words merged into
//! utterances and rendered as `transcript.md`. Pure, so it's tested without
//! audio.

use log::info;
use std::collections::BTreeMap;

use super::Meeting;
use crate::managers::transcription::TimedWord;

/// A pause longer than this starts a new utterance.
const MAX_WORD_GAP_MS: i64 = 1_500;

/// A diarized speaker with less of the track's speech than this percentage
/// is taken for the diarizer splitting off part of someone else. On two real
/// meetings Sortformer added one or two such speakers, with 1 and 10 turns
/// against about 200 for each real one (10 of ~410 turns is 2.4%).
const MIN_SPEAKER_SHARE_PCT: i64 = 3;

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
    /// What it's stored as: `me`, `them` or `s1`, `s2`, ...
    pub fn key(self) -> String {
        match self {
            Self::Me => "me".into(),
            Self::Them => "them".into(),
            Self::Remote(n) => format!("s{n}"),
        }
    }

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
pub(super) struct Turn {
    pub t0_ms: i64,
    pub t1_ms: i64,
    pub speaker: u8,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Utterance {
    pub t0_ms: i64,
    pub t1_ms: i64,
    pub speaker: Speaker,
    pub text: String,
}

/// Merges each speaker with under [`MIN_SPEAKER_SHARE_PCT`] of the speech
/// into the main speaker their turn overlaps most, or else the nearest one,
/// then numbers speakers from 1 in the order they first speak.
pub(super) fn merge_minor_speakers(mut turns: Vec<Turn>) -> Vec<Turn> {
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
pub(super) fn assign_speakers(words: &[TimedWord], turns: &[Turn]) -> Vec<Speaker> {
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

/// Interleaves each track's words and their speakers, placed by the track's
/// start offset, into utterances: one breaks when the speaker changes or
/// after a pause longer than [`MAX_WORD_GAP_MS`].
pub(super) fn merge_utterances(tracks: Vec<(i64, Vec<(Speaker, TimedWord)>)>) -> Vec<Utterance> {
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

pub(super) fn render_transcript(meeting: &Meeting, utterances: &[Utterance]) -> String {
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
    use super::super::MeetingStatus;
    use super::*;
    use std::path::PathBuf;

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
            asr_model: None,
            diar_model: None,
            language: None,
            attempts: 0,
            wrap_up: false,
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
