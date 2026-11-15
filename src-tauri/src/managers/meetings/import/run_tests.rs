use super::fixtures::*;
use super::*;
use crate::managers::history::HistoryManager;
use std::time::SystemTime;

/// 2026-09-01 10:00 UTC.
const T0_SECS: i64 = 1_788_256_800;
const DAY: i64 = 86_400;

/// Every file under `root`, with its size and modification time.
fn files(root: &Path) -> Vec<(PathBuf, u64, SystemTime)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap().flatten() {
            let meta = entry.metadata().unwrap();
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                out.push((entry.path(), meta.len(), meta.modified().unwrap()));
            }
        }
    }
    out.sort();
    out
}

struct World {
    tmp: tempfile::TempDir,
    roots: Roots,
    _wispr: Connection,
}

impl World {
    fn meetings_dir(&self) -> PathBuf {
        self.tmp.path().join("data/recordings/meetings")
    }

    fn vault(&self) -> PathBuf {
        self.tmp.path().join("vault/Meetings")
    }
}

/// Wispr and Spokenly heard one meeting, Superwhisper and Spokenly another;
/// Superwhisper alone a third and Spokenly alone a fourth. Around them, a
/// stub, a deleted meeting, a dictation and a file that was no meeting.
fn world() -> World {
    let tmp = tempfile::tempdir().unwrap();
    let roots = Roots::under(&tmp.path().join("home"));
    let wispr_db = wispr_db(&roots.wispr);
    wispr(
        &wispr_db,
        &roots.wispr,
        "wf-roadmap",
        T0_SECS * 1000,
        30,
        false,
    );
    wispr_deleted(&wispr_db, "wf-gone", (T0_SECS + 5 * DAY) * 1000);
    spokenly(
        &roots.spokenly,
        "sp-roadmap",
        T0_SECS + 40,
        30 * 60,
        true,
        false,
    );

    superwhisper(&roots.superwhisper, T0_SECS + DAY, 40, true);
    spokenly(
        &roots.spokenly,
        "sp-standup",
        T0_SECS + DAY + 30,
        39 * 60,
        true,
        false,
    );
    superwhisper(&roots.superwhisper, T0_SECS + 2 * DAY, 20, true);
    superwhisper(&roots.superwhisper, T0_SECS + 6 * DAY, 0, false);
    superwhisper_dictation(&roots.superwhisper, T0_SECS + 7 * DAY);

    spokenly(
        &roots.spokenly,
        "sp-call",
        T0_SECS + 3 * DAY,
        31,
        true,
        true,
    );
    spokenly(
        &roots.spokenly,
        "sp-podcast",
        T0_SECS + 4 * DAY,
        50 * 60,
        false,
        false,
    );
    spokenly_dictation(&roots.spokenly, "sp-dictation", T0_SECS + 60);
    World {
        tmp,
        roots,
        _wispr: wispr_db,
    }
}

fn db() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    HistoryManager::apply_migrations(&mut conn).unwrap();
    conn
}

/// What `write_meeting` adds, without an app.
fn write_json(meeting: &Meeting) -> Result<()> {
    fs::write(
        meeting.dir.join("meeting.json"),
        serde_json::to_vec_pretty(meeting)?,
    )?;
    Ok(())
}

fn import(world: &World, conn: &mut Connection, dry_run: bool) -> (ImportReport, Vec<String>) {
    let vault = world.vault();
    run(
        conn,
        &world.roots,
        &world.meetings_dir(),
        dry_run,
        Some(&vault),
        &mut write_json,
    )
    .unwrap()
}

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
        row.get(0)
    })
    .unwrap()
}

fn expected(dry_run: bool) -> ImportReport {
    let source = |source, found, stubs| SourceCount {
        source,
        found,
        unreadable: 0,
        stubs,
    };
    ImportReport {
        dry_run,
        sources: vec![
            source(Source::Wispr, 1, 0),
            source(Source::Superwhisper, 3, 1),
            source(Source::Spokenly, 4, 0),
        ],
        merged: 2,
        not_meetings: 1,
        imported: 3,
        to_transcribe: 1,
        notes: if dry_run { 0 } else { 3 },
        ..Default::default()
    }
}

#[test]
fn a_dry_run_reports_the_import_and_writes_nothing() {
    let world = world();
    let mut conn = db();
    let (report, queued) = import(&world, &mut conn, true);
    assert_eq!(report, expected(true));
    assert!(queued.is_empty());
    assert!(!world.meetings_dir().exists());
    assert!(!world.vault().exists());
    assert!(db::seen(&conn).unwrap().is_empty());
    let ledger: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'meeting_sources'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(ledger, 0);
}

#[test]
fn imports_each_meeting_once_with_names_from_wispr_and_audio_from_spokenly() {
    let world = world();
    let mut conn = db();
    let roadmap = meeting_id(T0_SECS * 1000, Source::Wispr);
    let standup = meeting_id((T0_SECS + DAY) * 1000, Source::Superwhisper);
    let solo = meeting_id((T0_SECS + 2 * DAY) * 1000, Source::Superwhisper);
    let call = meeting_id((T0_SECS + 3 * DAY) * 1000 + 250, Source::Spokenly);

    // A note of the user's where the roadmap's would go.
    let date = chrono::DateTime::from_timestamp(T0_SECS, 0)
        .unwrap()
        .with_timezone(&chrono::Local)
        .format("%Y-%m-%d");
    let theirs = world.vault().join(format!("{date} Roadmap sync.md"));
    fs::create_dir_all(world.vault()).unwrap();
    fs::write(&theirs, "mine").unwrap();

    let (report, queued) = import(&world, &mut conn, false);
    assert_eq!(report, expected(false));
    assert_eq!(queued, std::slice::from_ref(&call));

    let detail = store::get(&conn, &roadmap).unwrap().unwrap();
    assert_eq!(detail.meeting.source, "wispr");
    assert_eq!(detail.meeting.source_id.as_deref(), Some("wf-roadmap"));
    assert_eq!(detail.meeting.title.as_deref(), Some("Roadmap sync"));
    assert_eq!(detail.meeting.status.as_deref(), Some("done"));
    assert_eq!(detail.meeting.started_at, Some(T0_SECS));
    let names: Vec<_> = detail.speakers.iter().map(|s| s.label.as_deref()).collect();
    assert_eq!(names, [Some("Ana"), Some("Bo"), None]);
    assert_eq!(detail.segments.len(), 4);
    let dir = world.meetings_dir().join(&roadmap);
    let spokenly_audio = fs::read(
        fs::read_dir(&world.roots.spokenly)
            .unwrap()
            .flatten()
            .map(|d| d.path().join("sp-roadmap.m4a"))
            .find(|p| p.exists())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(fs::read(dir.join("audio.m4a")).unwrap(), spokenly_audio);
    assert!(fs::read_to_string(dir.join("summary.md"))
        .unwrap()
        .contains("Ana and Bo agreed"));
    let saved: Meeting =
        serde_json::from_slice(&fs::read(dir.join("meeting.json")).unwrap()).unwrap();
    assert_eq!(saved.tracks[0].kind, TrackKind::Mixed);
    assert_eq!(saved.status, MeetingStatus::Done);

    // Spokenly's audio wins over Superwhisper's own.
    assert!(world
        .meetings_dir()
        .join(&standup)
        .join("audio.m4a")
        .exists());
    assert!(world.meetings_dir().join(&solo).join("audio.ogg").exists());
    // The apps keep their own files.
    assert!(world
        .roots
        .superwhisper
        .join((T0_SECS + DAY).to_string())
        .join("output.ogg")
        .exists());

    // Only Spokenly had the call: decoded to a WAV, to be transcribed.
    let call_row = store::get(&conn, &call).unwrap().unwrap();
    assert_eq!(call_row.meeting.status.as_deref(), Some("transcribing"));
    assert!(call_row.segments.is_empty());
    let reader =
        hound::WavReader::open(world.meetings_dir().join(&call).join("mixed.wav")).unwrap();
    assert_eq!(reader.spec().sample_rate, 16_000);
    assert_eq!(reader.spec().channels, 1);
    assert_eq!(reader.duration(), 31 * 16_000);

    // Notes for the transcribed three; the user's file is left alone.
    assert_eq!(fs::read_to_string(&theirs).unwrap(), "mine");
    let note = detail_note(&conn, &roadmap);
    assert_eq!(
        note,
        world.vault().join(format!("{date} Roadmap sync (2).md"))
    );
    assert!(fs::read_to_string(&note)
        .unwrap()
        .contains("source: wispr\n"));
    assert_eq!(
        store::get(&conn, &call).unwrap().unwrap().meeting.note_path,
        None
    );

    assert_eq!(count(&conn, "meetings"), 4);
    assert_eq!(count(&conn, "meeting_sources"), 6);
}

fn detail_note(conn: &Connection, id: &str) -> PathBuf {
    PathBuf::from(
        store::get(conn, id)
            .unwrap()
            .unwrap()
            .meeting
            .note_path
            .unwrap(),
    )
}

#[test]
fn a_second_run_writes_nothing_and_a_deleted_meeting_stays_deleted() {
    let world = world();
    let mut conn = db();
    import(&world, &mut conn, false);
    let files_before: Vec<(PathBuf, u64, SystemTime)> = files(world.tmp.path());
    let rows_before: Vec<i64> = [
        "meetings",
        "meeting_segments",
        "meeting_speakers",
        "meeting_sources",
    ]
    .iter()
    .map(|t| count(&conn, t))
    .collect();

    let (again, queued) = import(&world, &mut conn, false);
    assert_eq!(
        again.imported + again.to_transcribe + again.notes + again.merged,
        0
    );
    assert_eq!(again.already_imported, 4);
    assert!(queued.is_empty());
    assert_eq!(files(world.tmp.path()), files_before);
    let rows_after: Vec<i64> = [
        "meetings",
        "meeting_segments",
        "meeting_speakers",
        "meeting_sources",
    ]
    .iter()
    .map(|t| count(&conn, t))
    .collect();
    assert_eq!(rows_after, rows_before);

    let solo = meeting_id((T0_SECS + 2 * DAY) * 1000, Source::Superwhisper);
    conn.execute("DELETE FROM meetings WHERE id = ?1", [&solo])
        .unwrap();
    let (after_delete, _) = import(&world, &mut conn, false);
    assert_eq!(after_delete.imported + after_delete.to_transcribe, 0);
    assert_eq!(after_delete.already_imported, 4);
    assert_eq!(count(&conn, "meetings"), 3);
}

#[test]
fn finds_nothing_where_the_apps_never_ran() {
    let tmp = tempfile::tempdir().unwrap();
    let mut conn = db();
    let (report, _) = run(
        &mut conn,
        &Roots::under(tmp.path()),
        &tmp.path().join("meetings"),
        true,
        None,
        &mut write_json,
    )
    .unwrap();
    assert!(report.sources.iter().all(|s| s.found == 0));
    assert_eq!(report.imported + report.to_transcribe + report.failed, 0);
}
