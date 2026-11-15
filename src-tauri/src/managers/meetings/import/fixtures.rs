//! Folders laid out like each app keeps them, with made-up meetings.

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

pub const APPLE_EPOCH_SECS: i64 = 978_307_200;

/// A Superwhisper meeting in `recordings/<unix_secs>/`, its segment times
/// in seconds, two speakers of whom only the first was named.
pub fn superwhisper(recordings: &Path, unix_secs: i64, minutes: i64, audio: bool) -> PathBuf {
    let folder = recordings.join(unix_secs.to_string());
    fs::create_dir_all(&folder).unwrap();
    let last = (minutes * 60 - 5) as f64;
    let meta = json!({
        "kind": "meeting",
        "appVersion": "2.1.0",
        "datetime": chrono::DateTime::from_timestamp(unix_secs, 0).unwrap().to_rfc3339(),
        "duration": minutes * 60_000,
        "llmResult": ": ## Summary\n- Ana takes the release",
        "modeName": "Meeting",
        "segments": [
            {"text": " Morning all.", "speaker": 1, "start": 0.5, "end": 2.0,
             "words": [{"word": "Morning", "start": 0.5, "end": 1.0}]},
            {"text": "Hi.", "speaker": 2, "start": 2.4, "end": 3.1, "words": []},
            {"text": "Wrapping up.", "speaker": 1, "start": last, "end": last + 3.0, "words": []},
        ],
        "speakers": [
            {"name": "Ana", "number": 1},
            {"name": "Speaker 2", "number": 2},
        ],
    });
    fs::write(folder.join("meta.json"), meta.to_string()).unwrap();
    if audio {
        fs::write(folder.join("output.ogg"), b"OggS superwhisper audio").unwrap();
    }
    folder
}

/// A Superwhisper dictation, which isn't a meeting, with `segments` as
/// odd as a meeting's would be unreadable.
pub fn superwhisper_dictation(recordings: &Path, unix_secs: i64, segments: Value) {
    let folder = recordings.join(unix_secs.to_string());
    fs::create_dir_all(&folder).unwrap();
    let meta = json!({"kind": "dictation", "duration": 4000, "segments": segments});
    fs::write(folder.join("meta.json"), meta.to_string()).unwrap();
}

/// `flow.sqlite` in WAL mode. Keep the connection open, as a running Wispr
/// Flow would: its rows stay in `flow.sqlite-wal`.
pub fn wispr_db(root: &Path) -> Connection {
    fs::create_dir_all(root.join("meetings")).unwrap();
    let conn = Connection::open(root.join("flow.sqlite")).unwrap();
    let mode: String = conn
        .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    conn.execute_batch(
        "CREATE TABLE Meetings (
            id TEXT PRIMARY KEY NOT NULL,
            title TEXT,
            createdAt TEXT,
            updatedAt TEXT,
            endedAt TEXT,
            recordedMs INTEGER,
            summary TEXT,
            speakerMap TEXT,
            isDeleted INTEGER DEFAULT 0
        );",
    )
    .unwrap();
    conn
}

fn wispr_time(unix_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(unix_ms)
        .unwrap()
        .format("%Y-%m-%d %H:%M:%S%.3f +00:00")
        .to_string()
}

/// A Wispr Flow meeting: Ana named by the user, Bo only by consensus, and
/// the first segment written twice as it was refined. Without
/// `recorded_ms`, like the early ones.
pub fn wispr(
    conn: &Connection,
    root: &Path,
    id: &str,
    start_ms: i64,
    minutes: i64,
    recorded: bool,
) {
    let ms = minutes * 60_000;
    let speaker_map = json!({
        "people": {
            "p-ana": {"name": "Ana", "origin": "calendar", "platform": "meet"},
            "p-bo": {"name": "Bo", "origin": "voice", "platform": "meet"},
            "p-cy": {"name": "Cy", "origin": "voice", "platform": "meet"},
        },
        "assignments": {
            "0": {"user": "p-ana", "consensus": "p-cy"},
            "1": {"consensus": "p-bo"},
            "2": null,
        },
    });
    conn.execute(
        "INSERT INTO Meetings (id, title, createdAt, updatedAt, endedAt, recordedMs, summary, speakerMap)
         VALUES (?1, 'Roadmap sync', ?2, ?3, ?3, ?4, ?5, ?6)",
        params![
            id,
            wispr_time(start_ms),
            wispr_time(start_ms + ms + 4_000),
            recorded.then_some(ms),
            "## Notes\n<@speaker:0> and <@speaker:1> agreed; <@speaker:2> left.",
            speaker_map.to_string(),
        ],
    )
    .unwrap();
    let folder = root.join("meetings").join(id);
    fs::create_dir_all(&folder).unwrap();
    let line = |segment: i64, start: i64, end: i64, text: &str, speaker: i64| {
        json!({
            "segment": segment,
            "startRecordingMs": start,
            "endRecordingMs": end,
            "text": text,
            "speaker": {"id": speaker, "name": null, "source": "system"},
        })
        .to_string()
    };
    let lines = [
        line(0, 1_000, 2_000, "Let's star", 0),
        line(1, 2_500, 4_000, "Sounds good.", 1),
        line(0, 1_000, 2_200, "Let's start.", 0),
        // The same segment number, later: a separate line.
        line(1, 5_000, 6_000, "Then again.", 1),
        line(2, ms - 3_000, ms, "Bye.", 2),
    ];
    fs::write(folder.join("live.ndjson"), lines.join("\n") + "\n").unwrap();
    fs::write(
        folder.join("refined.ndjson"),
        json!({"id": "r0", "timestamp": 0, "text": "Let's start.", "speaker": {"id": 0, "name": null}})
            .to_string(),
    )
    .unwrap();
}

pub fn wispr_deleted(conn: &Connection, id: &str, start_ms: i64) {
    conn.execute(
        "INSERT INTO Meetings (id, title, createdAt, recordedMs, isDeleted) VALUES (?1, 'Gone', ?2, 600000, 1)",
        params![id, wispr_time(start_ms)],
    )
    .unwrap();
}

fn spokenly_day(history: &Path, unix_secs: i64) -> PathBuf {
    let day = chrono::DateTime::from_timestamp(unix_secs, 0)
        .unwrap()
        .format("%Y-%m-%d")
        .to_string();
    let folder = history.join(day);
    fs::create_dir_all(&folder).unwrap();
    folder
}

/// What Spokenly keeps for a file it transcribed: its envelope and, when
/// `wav` is set, a real 16 kHz WAV of the whole length beside it (else an
/// `.m4a` placeholder).
pub fn spokenly(history: &Path, id: &str, unix_secs: i64, secs: u32, meeting: bool, wav: bool) {
    let folder = spokenly_day(history, unix_secs);
    let envelope = json!({
        "id": id,
        "creationDate": (unix_secs - APPLE_EPOCH_SECS) as f64 + 0.25,
        "content": {"fileTranscription": {"_0": {
            "isMeetingRecording": meeting,
            "audioFile": {"duration": f64::from(secs), "fileName": format!("{id}.m4a")},
            "state": {"completed": {"_0": {
                "modelId": "parakeet-tdt-0.6b-v3",
                "segments": [
                    {"id": 0, "start": 0.0, "end": 2.5, "text": "Speaker 1: Hello there."},
                    {"id": 1, "start": f64::from(secs) - 3.0, "end": f64::from(secs), "text": "Bye."},
                ],
            }}},
        }}},
    });
    fs::write(folder.join(format!("{id}.json")), envelope.to_string()).unwrap();
    if wav {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(folder.join(format!("{id}.wav")), spec).unwrap();
        for n in 0..secs * 16_000 {
            writer
                .write_sample(((n as f32 * 0.05).sin() * 3_000.0) as i16)
                .unwrap();
        }
        writer.finalize().unwrap();
    } else {
        fs::write(folder.join(format!("{id}.m4a")), b"spokenly m4a audio").unwrap();
    }
}

/// A Spokenly dictation, which is no meeting, and one without a date.
pub fn spokenly_dictation(history: &Path, id: &str, unix_secs: i64) {
    let folder = spokenly_day(history, unix_secs);
    let envelope = json!({
        "id": id,
        "creationDate": (unix_secs - APPLE_EPOCH_SECS) as f64,
        "content": {"dictation": {"_0": {"text": "note to self", "duration": 3.0}}},
    });
    let undated = json!({"id": format!("{id}-undated"), "content": {"dictation": null}});
    fs::write(
        folder.join(format!("{id}-undated.json")),
        undated.to_string(),
    )
    .unwrap();
    fs::write(folder.join(format!("{id}.json")), envelope.to_string()).unwrap();
}
