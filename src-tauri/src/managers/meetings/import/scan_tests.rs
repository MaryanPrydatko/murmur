use super::fixtures::*;
use super::*;
use std::fs;

/// 2026-09-01 10:00 UTC.
const T0_SECS: i64 = 1_788_256_800;

fn speakers(meeting: &SourceMeeting) -> Vec<(&str, Option<&str>)> {
    meeting
        .speakers
        .iter()
        .map(|s| (s.key.as_str(), s.label.as_deref()))
        .collect()
}

#[test]
fn reads_a_superwhisper_meeting_and_skips_its_dictations() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = superwhisper(tmp.path(), T0_SECS, 40, true);
    superwhisper_dictation(tmp.path(), T0_SECS + 9_000, serde_json::json!([]));

    let scan = superwhisper::scan(tmp.path());
    assert_eq!(scan.unreadable, Vec::<String>::new());
    let [meeting] = scan.meetings.as_slice() else {
        panic!("one meeting, got {}", scan.meetings.len());
    };
    assert_eq!(meeting.source_id, T0_SECS.to_string());
    assert_eq!(meeting.started_at_ms, T0_SECS * 1000);
    assert_eq!(meeting.duration_ms, 40 * 60_000);
    assert_eq!(speakers(meeting), [("s1", Some("Ana")), ("s2", None)]);
    let segments: Vec<_> = meeting
        .segments
        .iter()
        .map(|s| (s.t0_ms, s.speaker_key.as_str()))
        .collect();
    assert_eq!(segments, [(500, "s1"), (2_400, "s2"), (2_395_000, "s1")]);
    assert_eq!(meeting.segments[0].text, "Morning all.");
    assert_eq!(
        meeting.summary.as_deref(),
        Some("## Summary\n- Ana takes the release")
    );
    assert_eq!(meeting.audio, Some(folder.join("output.ogg")));
}

#[test]
fn reads_superwhisper_segment_times_given_in_ms() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("1788256800");
    fs::create_dir_all(&folder).unwrap();
    let meta = serde_json::json!({
        "kind": "meeting",
        "duration": 600_000,
        "segments": [{"text": "Late point.", "speaker": 0, "start": 540_000, "end": 541_500}],
        "speakers": [],
    });
    fs::write(folder.join("meta.json"), meta.to_string()).unwrap();
    let scan = superwhisper::scan(tmp.path());
    assert_eq!(scan.meetings[0].segments[0].t0_ms, 540_000);
    assert_eq!(scan.meetings[0].audio, None);
}

#[test]
fn reads_wispr_meetings_from_a_database_wispr_still_has_open() {
    let tmp = tempfile::tempdir().unwrap();
    let conn = wispr_db(tmp.path());
    wispr(&conn, tmp.path(), "wf-1", T0_SECS * 1000, 30, false);
    wispr_deleted(&conn, "wf-2", T0_SECS * 1000 + 86_400_000);
    assert!(tmp.path().join("flow.sqlite-wal").metadata().unwrap().len() > 0);

    let scan = wispr::scan(tmp.path());
    assert_eq!(scan.unreadable, Vec::<String>::new());
    let [meeting] = scan.meetings.as_slice() else {
        panic!("one meeting, got {}", scan.meetings.len());
    };
    assert_eq!(meeting.source_id, "wf-1");
    assert_eq!(meeting.title.as_deref(), Some("Roadmap sync"));
    assert_eq!(meeting.started_at_ms, T0_SECS * 1000);
    // No recordedMs: as long as its last segment.
    assert_eq!(meeting.duration_ms, 30 * 60_000);
    assert_eq!(
        speakers(meeting),
        [("s1", Some("Ana")), ("s2", Some("Bo")), ("s3", None)]
    );
    let texts: Vec<&str> = meeting.segments.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(
        texts,
        ["Let's start.", "Sounds good.", "Then again.", "Bye."]
    );
    assert_eq!(
        meeting.summary.as_deref(),
        Some("## Notes\nAna and Bo agreed; Speaker 3 left.")
    );
    assert_eq!(meeting.audio, None);
    drop(conn);
}

#[test]
fn skips_a_wispr_line_cut_off_at_the_end_but_not_in_the_middle() {
    let tmp = tempfile::tempdir().unwrap();
    let conn = wispr_db(tmp.path());
    wispr(&conn, tmp.path(), "wf-1", T0_SECS * 1000, 30, true);
    let live = tmp.path().join("meetings/wf-1/live.ndjson");
    let lines = fs::read_to_string(&live).unwrap() + "{\"segment\": 3, \"text\": \"Cut";
    fs::write(&live, &lines).unwrap();
    let scan = wispr::scan(tmp.path());
    assert_eq!(scan.unreadable, Vec::<String>::new());
    assert_eq!(scan.meetings[0].segments.len(), 4);

    fs::write(&live, lines + "\n{\"segment\": 4}\n").unwrap();
    let scan = wispr::scan(tmp.path());
    assert!(scan.meetings.is_empty());
    assert_eq!(scan.unreadable.len(), 1);
    assert!(
        scan.unreadable[0].contains("Line 6"),
        "{}",
        scan.unreadable[0]
    );
    drop(conn);
}

#[test]
fn reads_spokenly_file_transcriptions_and_leaves_dictations() {
    let tmp = tempfile::tempdir().unwrap();
    spokenly(tmp.path(), "sp-1", T0_SECS, 45, true, true);
    spokenly(tmp.path(), "sp-2", T0_SECS + 3_600, 50, false, false);
    spokenly_dictation(tmp.path(), "sp-3", T0_SECS + 60);
    let day = fs::read_dir(tmp.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::write(
        day.join("broken.json"),
        "{\"creationDate\": \"secret words\", \"content\": {\"fileTranscription\": {}}}",
    )
    .unwrap();

    let mut scan = spokenly::scan(tmp.path());
    scan.meetings.sort_by(|a, b| a.source_id.cmp(&b.source_id));
    let found: Vec<_> = scan
        .meetings
        .iter()
        .map(|m| {
            (
                m.source_id.as_str(),
                m.started_at_ms,
                m.duration_ms,
                m.is_meeting,
            )
        })
        .collect();
    assert_eq!(
        found,
        [
            ("sp-1", T0_SECS * 1000 + 250, 45_000, true),
            ("sp-2", (T0_SECS + 3_600) * 1000 + 250, 50_000, false),
        ]
    );
    assert_eq!(scan.meetings[0].audio, Some(day.join("sp-1.wav")));
    assert_eq!(scan.meetings[1].audio, Some(day.join("sp-2.m4a")));
    assert_eq!(scan.unreadable.len(), 1);
    assert!(scan.unreadable[0].contains("broken.json"));
    assert!(
        !scan.unreadable[0].contains("secret"),
        "{}",
        scan.unreadable[0]
    );
}

#[test]
fn finds_nothing_where_the_apps_never_ran() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("missing");
    for scan in [
        superwhisper::scan(&missing),
        wispr::scan(&missing),
        spokenly::scan(&missing),
    ] {
        assert!(scan.meetings.is_empty() && scan.unreadable.is_empty());
    }
}
