//! Who said what and when: speakers given to words, and words merged into
//! utterances and rendered as `transcript.md`. Pure, so it's tested without
//! audio.

use log::info;
use std::collections::BTreeMap;

use super::Meeting;
use crate::managers::transcription::TimedWord;

/// A pause longer than this starts a new utterance.
const MAX_WORD_GAP_MS: i64 = 1_500;

/// Without headphones the microphone hears the call from the speakers, so
/// the mic track repeats the others' words a moment after the system track.
/// A mic word is taken for that echo when the system track has the same word
/// starting up to [`ECHO_LATE_MS`] before it or [`ECHO_EARLY_MS`] after it.
/// On a real meeting without headphones 98% of the echoed words fell within
/// -136..+248 ms.
const ECHO_EARLY_MS: i64 = 150;
const ECHO_LATE_MS: i64 = 300;

/// A match is echo only with another one among this many mic words on
/// either side: echo comes in phrases, while one word both sides say at once
/// ("yeah") is as likely two people.
const ECHO_NEIGHBOURS: usize = 2;

/// With headphones there is no echo, and the few words both sides happen to
/// say at once are the user's too. So a meeting only has echo when at least
/// this share of the system track's words were matched on the mic.
const ECHO_MIN_SHARE_PCT: usize = 25;

/// Echo the mic track heard differently from the system track: in a run of
/// up to this many mic words between two echoed ones, with no pause longer
/// than [`ECHO_GAP_MS`] on either side, a filler the system track left out,
/// or a word spelled like a system word no mic word took ("them" for "him").
/// Other words in the run are the user's, said over the other side.
const ECHO_GAP_MAX_WORDS: usize = 3;
const ECHO_GAP_MS: i64 = 1_000;

/// Fillers speech recognition writes on one track and leaves out on the other.
const FILLERS: &[&str] = &[
    "uh", "uhm", "um", "umm", "er", "erm", "ah", "eh", "ehm", "hm", "hmm", "mm", "äh", "ähm",
];

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

/// A word's parts as compared for echo: split at hyphens, lower case,
/// letters and digits only.
fn echo_parts(text: &str) -> Vec<String> {
    text.split('-')
        .map(|part| {
            part.chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|part| !part.is_empty())
        .collect()
}

/// Edits (insert, delete, replace a letter) between `a` and `b`.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let replaced = diagonal + usize::from(ca != *cb);
            diagonal = row[j + 1];
            row[j + 1] = replaced.min(row[j] + 1).min(row[j + 1] + 1);
        }
    }
    row[b.len()]
}

/// Whether two spellings differ in at most half the letters of the longer.
fn spelled_alike(a: &str, b: &str) -> bool {
    edit_distance(a, b) * 2 <= a.chars().count().max(b.chars().count())
}

/// Each part of `word` and when it starts, timed by its share of the letters.
fn timed_parts(word: &TimedWord) -> Vec<(i64, String)> {
    let parts = echo_parts(&word.text);
    let letters: usize = parts.iter().map(|p| p.chars().count()).sum();
    let mut before = 0;
    parts
        .into_iter()
        .map(|part| {
            let at = word.t0_ms + (word.t1_ms - word.t0_ms) * before as i64 / letters.max(1) as i64;
            before += part.chars().count();
            (at, part)
        })
        .collect()
}

/// Which of `mine` (the mic track's words, in order) are echo of `theirs`
/// (the system track's): first the ones the system track has too, each
/// system word standing for one mic word at most, near another such match
/// ([`ECHO_NEIGHBOURS`]), if there are enough of them for the meeting to
/// have echo at all ([`ECHO_MIN_SHARE_PCT`]); then the misheard ones between
/// them ([`ECHO_GAP_MAX_WORDS`]).
/// Returns how many each step found.
fn find_echo(mine: &[&TimedWord], theirs: &[&TimedWord]) -> (Vec<bool>, usize, usize) {
    // The system track's parts by start, and whether a mic word took each.
    let mut heard: Vec<(i64, String, bool)> = theirs
        .iter()
        .flat_map(|w| timed_parts(w))
        .map(|(at, part)| (at, part, false))
        .collect();
    heard.sort_by_key(|(at, _, _)| *at);

    let mut same = vec![false; mine.len()];
    for (i, word) in mine.iter().enumerate() {
        let parts = timed_parts(word);
        let mut taken: Vec<usize> = Vec::new();
        for (at, part) in &parts {
            let from = heard.partition_point(|(h, _, _)| *h < at - ECHO_LATE_MS);
            let to = heard.partition_point(|(h, _, _)| *h <= at + ECHO_EARLY_MS);
            // The earliest: echo keeps the system track's order, so a word
            // said twice in a row is matched in turn.
            let earliest =
                (from..to).find(|&k| !heard[k].2 && heard[k].1 == *part && !taken.contains(&k));
            match earliest {
                Some(k) => taken.push(k),
                None => break,
            }
        }
        if !parts.is_empty() && taken.len() == parts.len() {
            for k in taken {
                heard[k].2 = true;
            }
            same[i] = true;
        }
    }

    let matched = same.iter().filter(|&&s| s).count();
    if matched * 100 < ECHO_MIN_SHARE_PCT * theirs.len() {
        return (vec![false; mine.len()], 0, 0);
    }

    // Another match among the [`ECHO_NEIGHBOURS`] mic words on either side,
    // with no pause longer than [`MAX_WORD_GAP_MS`] to it.
    let near = |i: usize, j: usize| {
        let (a, b) = (i.min(j), i.max(j));
        same[j] && mine[b].t0_ms - mine[a].t1_ms <= MAX_WORD_GAP_MS
    };
    let mut echo: Vec<bool> = (0..mine.len())
        .map(|i| {
            same[i]
                && (i.saturating_sub(ECHO_NEIGHBOURS)..(i + ECHO_NEIGHBOURS + 1).min(mine.len()))
                    .any(|j| j != i && near(i, j))
        })
        .collect();
    let exact = echo.iter().filter(|&&e| e).count();

    // A filler the system track left out sits in a short pause between its
    // words, so a word ending just before or starting just after counts.
    let talking = |w: &TimedWord| {
        theirs
            .iter()
            .any(|t| t.t0_ms < w.t1_ms + ECHO_LATE_MS && t.t1_ms > w.t0_ms - ECHO_LATE_MS)
    };
    let misheard = |w: &TimedWord| {
        let spelled: String = echo_parts(&w.text).concat();
        let from = heard.partition_point(|(h, _, _)| *h < w.t0_ms - ECHO_LATE_MS);
        let to = heard.partition_point(|(h, _, _)| *h <= w.t1_ms + ECHO_EARLY_MS);
        !spelled.is_empty()
            && heard[from..to]
                .iter()
                .any(|(_, part, taken)| !taken && spelled_alike(&spelled, part))
    };
    let filler = |w: &TimedWord| FILLERS.contains(&echo_parts(&w.text).concat().as_str());
    let mut between = 0;
    let mut i = 0;
    while i < mine.len() {
        if echo[i] {
            i += 1;
            continue;
        }
        let start = i;
        while i < mine.len() && !echo[i] {
            i += 1;
        }
        // mine[start..i] is a run that isn't echo; i is the next echo, if any.
        let bounded = start > 0 && i < mine.len();
        if bounded
            && i - start <= ECHO_GAP_MAX_WORDS
            && mine[start].t0_ms - mine[start - 1].t1_ms <= ECHO_GAP_MS
            && mine[i].t0_ms - mine[i - 1].t1_ms <= ECHO_GAP_MS
        {
            for k in start..i {
                if (filler(mine[k]) && talking(mine[k])) || misheard(mine[k]) {
                    echo[k] = true;
                    between += 1;
                }
            }
        }
    }
    (echo, exact, between)
}

/// Drops the mic words that are echo of the system track, see [`find_echo`].
/// `mine` and `theirs` are placed on the meeting's clock.
fn drop_echo(
    mine: Vec<(Speaker, TimedWord)>,
    theirs: &[(Speaker, TimedWord)],
) -> Vec<(Speaker, TimedWord)> {
    if mine.is_empty() || theirs.is_empty() {
        return mine;
    }
    let mut mine = mine;
    mine.sort_by_key(|(_, w)| w.t0_ms);
    let mut theirs: Vec<&TimedWord> = theirs.iter().map(|(_, w)| w).collect();
    theirs.sort_by_key(|w| w.t0_ms);
    let words: Vec<&TimedWord> = mine.iter().map(|(_, w)| w).collect();
    let (echo, exact, between) = find_echo(&words, &theirs);
    if exact + between > 0 {
        info!(
            "Dropped {} of {} mic words as echo of the system track ({exact} the same, {between} between them)",
            exact + between,
            mine.len()
        );
    }
    mine.into_iter()
        .zip(echo)
        .filter_map(|(word, echo)| (!echo).then_some(word))
        .collect()
}

/// Interleaves each track's words and their speakers, placed by the track's
/// start offset, into utterances: one breaks when the speaker changes or
/// after a pause longer than [`MAX_WORD_GAP_MS`]. The mic track's echo of
/// the system track is dropped first, see [`find_echo`].
pub(super) fn merge_utterances(tracks: Vec<(i64, Vec<(Speaker, TimedWord)>)>) -> Vec<Utterance> {
    let words: Vec<(Speaker, TimedWord)> = tracks
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
    let (mine, theirs): (Vec<_>, Vec<_>) = words
        .into_iter()
        .partition(|(speaker, _)| *speaker == Speaker::Me);
    let mut words = drop_echo(mine, &theirs);
    words.extend(theirs);
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

    /// Words said one after another from `t0_ms`, each 300 ms with 100 ms
    /// between.
    fn spoken(t0_ms: i64, text: &str) -> Vec<TimedWord> {
        text.split(' ')
            .enumerate()
            .map(|(i, w)| {
                let at = t0_ms + i as i64 * 400;
                word(at, at + 300, w)
            })
            .collect()
    }

    /// The text each speaker is left with, in order.
    fn lines(utterances: &[Utterance]) -> Vec<(Speaker, &str)> {
        utterances
            .iter()
            .map(|u| (u.speaker, u.text.as_str()))
            .collect()
    }

    fn meeting_of(mic: Vec<TimedWord>, system: Vec<TimedWord>) -> Vec<Utterance> {
        merge_utterances(vec![
            (0, by(Speaker::Me, mic)),
            (0, by(Speaker::Remote(1), system)),
        ])
    }

    #[test]
    fn drops_the_mic_tracks_echo_of_the_system_track() {
        let them = spoken(1_000, "can we ship the build today");
        // The mic hears it from the speakers 80 ms later, then the user answers.
        let mut mic = spoken(1_080, "can we ship the build today");
        mic.extend(spoken(5_000, "sure after lunch"));
        assert_eq!(
            lines(&meeting_of(mic, them)),
            [
                (Speaker::Remote(1), "can we ship the build today"),
                (Speaker::Me, "sure after lunch"),
            ]
        );
    }

    #[test]
    fn places_echo_by_each_tracks_start_offset() {
        // The mic track started 500 ms before the system track, so its words
        // are 500 ms later on its own clock.
        let utterances = merge_utterances(vec![
            (0, by(Speaker::Me, spoken(1_580, "see you on monday"))),
            (
                500,
                by(Speaker::Remote(1), spoken(1_000, "see you on monday")),
            ),
        ]);
        assert_eq!(
            lines(&utterances),
            [(Speaker::Remote(1), "see you on monday")]
        );
    }

    #[test]
    fn keeps_a_lone_word_both_sides_say_at_once() {
        // With headphones there's no echo, but both may say "yeah" together.
        let them = spoken(1_000, "yeah");
        let mic = spoken(1_050, "yeah");
        assert_eq!(
            lines(&meeting_of(mic, them)),
            [(Speaker::Remote(1), "yeah"), (Speaker::Me, "yeah")]
        );
    }

    #[test]
    fn a_system_word_stands_for_one_mic_word_at_most() {
        let them = spoken(1_000, "we ship it today");
        // The echo, and the user saying "today" right after it.
        let mut mic = spoken(1_080, "we ship it today");
        mic.push(word(2_480, 2_780, "today"));
        assert_eq!(
            lines(&meeting_of(mic, them)),
            [
                (Speaker::Remote(1), "we ship it today"),
                (Speaker::Me, "today"),
            ]
        );
    }

    #[test]
    fn matches_hyphenated_words_part_by_part() {
        let them = vec![
            word(1_000, 1_300, "finish"),
            word(1_400, 2_000, "check-ins"),
        ];
        let mic = vec![
            word(1_080, 1_380, "finish"),
            word(1_480, 1_700, "check"),
            word(1_760, 2_080, "ins"),
        ];
        assert_eq!(
            lines(&meeting_of(mic, them)),
            [(Speaker::Remote(1), "finish check-ins")]
        );
    }

    #[test]
    fn drops_a_short_mishearing_between_echoed_words() {
        // The mic heard "uh" and "um" where the system track has neither.
        let them = spoken(1_000, "we ship build tomorrow");
        let mic = vec![
            word(1_080, 1_380, "we"),
            word(1_480, 1_780, "ship"),
            word(1_800, 1_850, "uh"),
            word(1_860, 1_870, "um"),
            word(1_880, 2_180, "build"),
            word(2_280, 2_580, "tomorrow"),
        ];
        assert_eq!(
            lines(&meeting_of(mic, them)),
            [(Speaker::Remote(1), "we ship build tomorrow")]
        );
    }

    #[test]
    fn drops_a_word_the_mic_heard_differently() {
        let them = spoken(1_000, "we ship him today");
        let mic = spoken(1_080, "we ship them today");
        assert_eq!(
            lines(&meeting_of(mic, them)),
            [(Speaker::Remote(1), "we ship him today")]
        );
    }

    #[test]
    fn keeps_an_interjection_said_over_the_echo() {
        let them = spoken(1_000, "so the plan for next week is to finish the build");
        let mut mic = spoken(1_080, "so the plan for next week is to finish the build");
        // The user, while the other side keeps talking.
        mic.push(word(2_850, 2_950, "yeah"));
        mic.push(word(2_960, 3_060, "exactly"));
        let kept: Vec<_> = meeting_of(mic, them)
            .into_iter()
            .filter(|u| u.speaker == Speaker::Me)
            .map(|u| u.text)
            .collect();
        assert_eq!(kept, ["yeah exactly"]);
    }

    #[test]
    fn matches_a_quickly_repeated_word_in_order() {
        let them = vec![
            word(0, 200, "okay"),
            word(250, 450, "bye"),
            word(500, 700, "bye"),
        ];
        // 136 ms late, more than half the 250 ms between the two "bye"s.
        let mic = vec![
            word(136, 336, "okay"),
            word(386, 586, "bye"),
            word(636, 836, "bye"),
        ];
        assert_eq!(
            lines(&meeting_of(mic, them)),
            [(Speaker::Remote(1), "okay bye bye")]
        );
    }

    #[test]
    fn keeps_a_phrase_both_say_at_once_in_a_meeting_without_echo() {
        // Headphones: the mic only has the user, who says goodbye with them.
        let mut them = spoken(
            0,
            "that is all from my side for this week so let us meet again on monday at ten",
        );
        them.extend(spoken(8_000, "thanks bye bye"));
        let mut mic = spoken(10_000, "i will send the notes later");
        mic.extend(spoken(8_050, "thanks bye bye"));
        let kept = meeting_of(mic, them)
            .into_iter()
            .filter(|u| u.speaker == Speaker::Me)
            .map(|u| u.text)
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(kept, "thanks bye bye i will send the notes later");
    }

    #[test]
    fn keeps_more_than_a_few_own_words_between_echoed_ones() {
        let mut them = spoken(1_000, "so the plan");
        them.push(word(2_190, 2_900, "basically"));
        them.extend(spoken(3_000, "is to ship it"));
        let mut mic = spoken(1_080, "so the plan");
        // Four words of the user's own while the other side keeps talking.
        mic.extend(vec![
            word(2_200, 2_300, "no"),
            word(2_320, 2_420, "wait"),
            word(2_440, 2_540, "hold"),
            word(2_560, 2_660, "on"),
        ]);
        mic.extend(spoken(3_080, "is to ship it"));
        let kept: Vec<_> = meeting_of(mic, them)
            .into_iter()
            .filter(|u| u.speaker == Speaker::Me)
            .map(|u| u.text)
            .collect();
        assert_eq!(kept, ["no wait hold on"]);
    }

    #[test]
    fn keeps_own_words_in_a_pause_on_the_system_track() {
        let mut them = spoken(0, "alpha beta");
        them.extend(spoken(2_700, "gamma delta"));
        let mut mic = spoken(80, "alpha beta");
        // The system track is quiet from 700 to 2,700 ms.
        mic.push(word(1_500, 1_800, "right"));
        mic.extend(spoken(2_780, "gamma delta"));
        let kept: Vec<_> = meeting_of(mic, them)
            .into_iter()
            .filter(|u| u.speaker == Speaker::Me)
            .map(|u| u.text)
            .collect();
        assert_eq!(kept, ["right"]);
    }

    #[test]
    fn keeps_own_words_after_the_last_echoed_one() {
        let them = spoken(1_000, "thanks everyone");
        let mut mic = spoken(1_080, "thanks everyone");
        mic.extend(spoken(1_900, "thanks bye"));
        let kept: Vec<_> = meeting_of(mic, them)
            .into_iter()
            .filter(|u| u.speaker == Speaker::Me)
            .map(|u| u.text)
            .collect();
        assert_eq!(kept, ["thanks bye"]);
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
