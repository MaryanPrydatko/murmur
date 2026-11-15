use super::*;

const MIN: i64 = 60_000;
/// 2026-09-01 10:00 UTC.
const T0: i64 = 1_788_256_800_000;

fn record(source: Source, id: &str, start_min: i64, duration_min: i64) -> SourceMeeting {
    SourceMeeting {
        source,
        source_id: id.into(),
        started_at_ms: T0 + start_min * MIN,
        duration_ms: duration_min * MIN,
        title: None,
        summary: None,
        speakers: vec![MeetingSpeaker {
            key: "s1".into(),
            label: None,
        }],
        segments: vec![MeetingSegment {
            t0_ms: 0,
            t1_ms: 1_000,
            speaker_key: "s1".into(),
            text: "hello".into(),
        }],
        audio: (source != Source::Wispr).then(|| PathBuf::from(format!("/src/{id}.m4a"))),
        is_meeting: true,
    }
}

fn outcomes(planned: &[Planned]) -> Vec<(&str, Outcome, Vec<&str>)> {
    planned
        .iter()
        .map(|p| {
            (
                p.primary.source_id.as_str(),
                p.outcome,
                p.merged.iter().map(|m| m.source_id.as_str()).collect(),
            )
        })
        .collect()
}

#[test]
fn folds_the_spokenly_copy_into_the_meeting_it_recorded() {
    let found = vec![
        record(Source::Spokenly, "sp-a", 1, 50),
        record(Source::Wispr, "wf-a", 0, 55),
        record(Source::Superwhisper, "sw-b", 300, 30),
        record(Source::Spokenly, "sp-b", 299, 27),
    ];
    let planned = plan(found, &HashSet::new());
    assert_eq!(
        outcomes(&planned),
        [
            ("wf-a", Outcome::Import, vec!["sp-a"]),
            ("sw-b", Outcome::Import, vec!["sp-b"]),
        ]
    );
    // Wispr keeps no audio: it comes from Spokenly, which wins over
    // Superwhisper's own.
    assert_eq!(planned[0].audio, Some(PathBuf::from("/src/sp-a.m4a")));
    assert_eq!(planned[1].audio, Some(PathBuf::from("/src/sp-b.m4a")));
    assert_eq!(planned[0].id, meeting_id(T0, Source::Wispr));
    assert!(planned[0].id.ends_with("-wf"));
    assert!(!planned[0].needs_transcription());
}

#[test]
fn merges_only_within_two_minutes_and_a_fifth_of_the_length() {
    let found = vec![
        record(Source::Wispr, "wf", 0, 100),
        // Starts too late.
        record(Source::Spokenly, "sp-late", 3, 100),
        // Too short.
        record(Source::Superwhisper, "sw-short", 0, 79),
        // Just inside both.
        record(Source::Spokenly, "sp-close", 2, 80),
    ];
    let planned = plan(found, &HashSet::new());
    assert_eq!(
        outcomes(&planned),
        [
            ("wf", Outcome::Import, vec!["sp-close"]),
            ("sw-short", Outcome::Import, vec![]),
            ("sp-late", Outcome::Import, vec![]),
        ]
    );
    assert!(planned[2].needs_transcription());
    assert!(planned[2].id.ends_with("-sp"));
}

#[test]
fn a_spokenly_record_joins_the_closest_meeting_it_matches() {
    let found = vec![
        record(Source::Wispr, "wf", 0, 40),
        record(Source::Superwhisper, "sw", 3, 40),
        record(Source::Spokenly, "sp", 2, 40),
    ];
    assert_eq!(
        outcomes(&plan(found, &HashSet::new())),
        [
            ("wf", Outcome::Import, vec![]),
            ("sw", Outcome::Import, vec!["sp"]),
        ]
    );
}

#[test]
fn every_app_that_heard_one_meeting_folds_into_it() {
    let found = vec![
        record(Source::Spokenly, "sp", 1, 40),
        record(Source::Superwhisper, "sw", 1, 40),
        record(Source::Wispr, "wf", 0, 40),
    ];
    let planned = plan(found, &HashSet::new());
    assert_eq!(
        outcomes(&planned),
        [("wf", Outcome::Import, vec!["sw", "sp"])]
    );
    let records: Vec<_> = planned[0].records().collect();
    assert_eq!(records.len(), 3);
}

#[test]
fn skips_stubs_and_spokenly_files_that_were_no_meeting() {
    let mut silent = record(Source::Superwhisper, "sw-silent", 0, 45);
    silent.segments[0].text = "  ".into();
    let mut file = record(Source::Spokenly, "sp-file", 200, 10);
    file.is_meeting = false;
    let mut lost = record(Source::Spokenly, "sp-lost", 400, 10);
    lost.audio = None;
    let mut short = record(Source::Wispr, "wf-short", 600, 0);
    short.duration_ms = 29_000;
    let planned = plan(vec![silent, file, lost, short], &HashSet::new());
    assert_eq!(
        outcomes(&planned),
        [
            ("sp-file", Outcome::NotAMeeting, vec![]),
            ("sp-lost", Outcome::NoAudio, vec![]),
            ("sw-silent", Outcome::Stub, vec![]),
            ("wf-short", Outcome::Stub, vec![]),
        ]
    );
}

#[test]
fn a_meeting_with_any_record_imported_before_is_not_imported_again() {
    let found = vec![
        record(Source::Wispr, "wf", 0, 60),
        record(Source::Spokenly, "sp", 0, 60),
    ];
    let seen = HashSet::from([(Source::Spokenly, "sp".to_string())]);
    let planned = plan(found, &seen);
    assert_eq!(
        outcomes(&planned),
        [("wf", Outcome::AlreadyImported, vec!["sp"])]
    );
}
