//! A finished meeting's note in the user's Obsidian vault. A note is written
//! once and never overwritten: once it's there, it's the user's.

use anyhow::{bail, Context, Result};
use chrono::{Local, TimeZone};
use log::{error, info};
use rusqlite::{params, Connection};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, Manager};

use super::store::{self, MeetingDetail, MeetingRow};
use crate::managers::history::HistoryManager;
use crate::notices::{self, Notice, NoticeCode, NoticeSource};
use crate::settings::{get_settings, AppSettings};

/// Names tried for a note before giving up: `Title.md`, `Title (2).md`, ...
const MAX_NAME_TRIES: u32 = 50;

/// Called once meeting `id` is transcribed: writes its note when a vault is
/// set and the meeting was recorded here and has none yet.
pub fn on_done(app: &AppHandle, id: &str) {
    let written = (|| -> Result<Option<PathBuf>> {
        let conn = app.state::<Arc<HistoryManager>>().get_connection()?;
        let Some(detail) = store::get(&conn, id)?.filter(|d| wants_note(&d.meeting)) else {
            return Ok(None);
        };
        let Some(folder) = notes_folder(&get_settings(app), &app.path().home_dir()?)? else {
            return Ok(None);
        };
        export(&conn, &detail, &folder).map(Some)
    })();
    match written {
        Ok(Some(path)) => {
            info!("Meeting {id}'s note is {}", path.display());
            super::emit_changed(app, id);
        }
        Ok(None) => {}
        Err(e) => {
            error!("Failed to write meeting {id}'s note: {e:#}");
            notices::raise(
                app,
                Notice::new(NoticeCode::MeetingNoteFailed, NoticeSource::Meeting)
                    .with_detail(format!("{e:#}")),
            );
        }
    }
}

/// A meeting transcribed here, as is one only Spokenly had. One imported
/// with its transcript gets its note only when asked for.
fn wants_note(meeting: &MeetingRow) -> bool {
    meeting.status.as_deref() == Some("done")
        && meeting.note_path.is_none()
        && (meeting.source == "murmur" || meeting.asr_model.is_some())
}

/// `<vault>/<notes folder>`, once a vault is set, with `~/` under `home`.
/// A vault that isn't a folder that exists is an error: the notes would
/// land in a new folder somewhere else.
pub fn notes_folder(settings: &AppSettings, home: &Path) -> Result<Option<PathBuf>> {
    let Some(typed) = settings
        .obsidian_vault_path
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return Ok(None);
    };
    let vault = match typed.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(typed),
    };
    if !vault.is_absolute() || !vault.is_dir() {
        bail!("The Obsidian vault {typed} isn't a folder that exists");
    }
    Ok(Some(vault.join(
        settings.obsidian_notes_folder.trim().trim_matches('/'),
    )))
}

/// Writes the meeting's note into `folder` and stores its path, unless the
/// note it has is still there. Never replaces a file: a name that's taken
/// by another note gets a ` (2)`, and a file with this meeting's
/// `murmur_id` is taken as its note.
pub fn export(conn: &Connection, detail: &MeetingDetail, folder: &Path) -> Result<PathBuf> {
    let meeting = &detail.meeting;
    if let Some(path) = meeting.note_path.as_deref().map(PathBuf::from) {
        if path.exists() {
            return Ok(path);
        }
    }
    fs::create_dir_all(folder).with_context(|| format!("Failed to create {}", folder.display()))?;
    // In the destination folder, so the link never crosses filesystems;
    // dot-named and not `.md`, so Obsidian never lists it.
    let temp = folder.join(format!(".{}.murmur-note", meeting.id));
    fs::write(&temp, render(detail))
        .with_context(|| format!("Failed to write {}", temp.display()))?;
    let placed = place(&temp, folder, &file_stem(meeting), &meeting.id);
    let _ = fs::remove_file(&temp);
    let path = placed?;
    conn.execute(
        "UPDATE meetings SET note_path = ?2 WHERE id = ?1",
        params![meeting.id, path.to_string_lossy()],
    )?;
    Ok(path)
}

/// Links `temp` in as `<stem>.md`, or the first free `<stem> (n).md`; a link
/// can't replace a file. A file that's already this meeting's note is kept.
fn place(temp: &Path, folder: &Path, stem: &str, id: &str) -> Result<PathBuf> {
    for n in 1..=MAX_NAME_TRIES {
        let name = match n {
            1 => format!("{stem}.md"),
            n => format!("{stem} ({n}).md"),
        };
        let path = folder.join(name);
        match fs::hard_link(temp, &path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                if note_id(&path).as_deref() == Some(id) {
                    return Ok(path);
                }
            }
            Err(e) => return Err(e).with_context(|| format!("Failed to write {}", path.display())),
        }
    }
    bail!(
        "{} has {MAX_NAME_TRIES} notes named {stem}",
        folder.display()
    )
}

/// The `murmur_id` in a note's front matter.
fn note_id(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let front = text.strip_prefix("---\n")?.split("\n---").next()?;
    front.lines().find_map(|line| {
        let value = line.strip_prefix("murmur_id:")?.trim();
        Some(value.trim_matches('"').to_string())
    })
}

fn local_start(meeting: &MeetingRow) -> Option<chrono::DateTime<Local>> {
    Local.timestamp_opt(meeting.started_at?, 0).single()
}

/// `2026-11-15 Title`, or `2026-11-15 Meeting 14-03` without a title.
fn file_stem(meeting: &MeetingRow) -> String {
    let start = local_start(meeting);
    let date = start.map_or_else(
        || meeting.id.clone(),
        |at| at.format("%Y-%m-%d").to_string(),
    );
    let title = meeting.title.as_deref().map(sanitize).unwrap_or_default();
    let title = if title.is_empty() {
        start.map_or_else(
            || "Meeting".into(),
            |at| at.format("Meeting %H-%M").to_string(),
        )
    } else {
        title
    };
    format!("{date} {title}")
}

/// A title as a file name Obsidian can link to: no path separators, no
/// characters it forbids or that break a `[[link]]`.
fn sanitize(title: &str) -> String {
    let kept: String = title
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '#' | '^' | '[' | ']' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let words: Vec<&str> = kept.split_whitespace().collect();
    let joined = words.join(" ");
    let trimmed = joined.trim_matches(|c: char| c == '.' || c == ' ');
    trimmed
        .chars()
        .take(100)
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn speaker_name(detail: &MeetingDetail, key: &str) -> String {
    let label = detail
        .speakers
        .iter()
        .find(|s| s.key == key)
        .and_then(|s| s.label.clone());
    label.unwrap_or_else(|| match key {
        "me" => "Me".into(),
        "them" => "Them".into(),
        key => match key.strip_prefix('s') {
            Some(n) => format!("Speaker {n}"),
            None => key.into(),
        },
    })
}

fn clock(ms: i64) -> String {
    let secs = ms.max(0) / 1000;
    format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

fn quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The meeting's `summary.md`, its headings kept under the note's own.
fn summary(meeting: &MeetingRow) -> Option<String> {
    let path = Path::new(meeting.dir.as_deref()?).join("summary.md");
    let text = fs::read_to_string(path).ok()?;
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let demoted: Vec<String> = text
        .lines()
        .map(
            |line| match line.strip_prefix("# ").or_else(|| line.strip_prefix("## ")) {
                Some(rest) => format!("### {rest}"),
                None => line.to_string(),
            },
        )
        .collect();
    Some(demoted.join("\n"))
}

fn render(detail: &MeetingDetail) -> String {
    let meeting = &detail.meeting;
    let mut out = String::from("---\n");
    out.push_str(&format!("murmur_id: {}\n", quoted(&meeting.id)));
    if let Some(at) = local_start(meeting) {
        out.push_str(&format!("date: {}\n", at.format("%Y-%m-%dT%H:%M:%S")));
    }
    out.push_str(&format!(
        "duration: {}\n",
        quoted(&clock(meeting.duration_ms.unwrap_or(0)))
    ));
    out.push_str("speakers:\n");
    for speaker in &detail.speakers {
        out.push_str(&format!(
            "  - {}\n",
            quoted(&speaker_name(detail, &speaker.key))
        ));
    }
    out.push_str(&format!("source: {}\n---\n\n", meeting.source));
    out.push_str("## Summary\n\n");
    out.push_str(&summary(meeting).unwrap_or_else(|| "_No summary yet._".into()));
    out.push_str("\n\n## Transcript\n");
    for segment in &detail.segments {
        out.push_str(&format!(
            "\n**{}** {} {}\n",
            speaker_name(detail, &segment.speaker_key),
            clock(segment.t0_ms),
            segment.text
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::{Meeting, MeetingStatus, Speaker, Utterance};
    use super::*;
    use std::time::SystemTime;

    fn db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        HistoryManager::apply_migrations(&mut conn).unwrap();
        conn
    }

    /// A transcribed meeting in `dir`, titled when `title` is set.
    fn transcribed(
        conn: &mut Connection,
        id: &str,
        dir: &Path,
        title: Option<&str>,
    ) -> MeetingDetail {
        let meeting = Meeting {
            id: id.into(),
            started_at: "2026-11-15T14:03:22+01:00".into(),
            duration_ms: 61_000,
            status: MeetingStatus::Done,
            dir: dir.to_path_buf(),
            tracks: Vec::new(),
            asr_model: None,
            diar_model: None,
            language: None,
            attempts: 0,
        };
        store::save(conn, &meeting).unwrap();
        let said = |t0_ms, speaker, text: &str| Utterance {
            t0_ms,
            t1_ms: t0_ms + 500,
            speaker,
            text: text.into(),
        };
        store::save_transcript(
            conn,
            id,
            &[
                said(0, Speaker::Me, "hi"),
                said(61_000, Speaker::Remote(1), "hello"),
            ],
        )
        .unwrap();
        store::rename_speaker(conn, id, "s1", Some("Ana")).unwrap();
        conn.execute(
            "UPDATE meetings SET title = ?2 WHERE id = ?1",
            params![id, title],
        )
        .unwrap();
        store::get(conn, id).unwrap().unwrap()
    }

    fn listing(folder: &Path) -> Vec<(String, u64, SystemTime)> {
        let mut files: Vec<_> = fs::read_dir(folder)
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                let meta = e.metadata().unwrap();
                (
                    e.file_name().to_string_lossy().into_owned(),
                    meta.len(),
                    meta.modified().unwrap(),
                )
            })
            .collect();
        files.sort();
        files
    }

    #[test]
    fn writes_the_note_once_and_a_rerun_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("vault/Meetings");
        let mut conn = db();
        let detail = transcribed(&mut conn, "m1", tmp.path(), Some("Sprint: planning / Q4"));
        fs::write(tmp.path().join("summary.md"), "# Decisions\n- ship it\n").unwrap();

        let path = export(&conn, &detail, &folder).unwrap();
        let stem = file_stem(&detail.meeting);
        assert!(stem.ends_with(" Sprint planning Q4"), "{stem}");
        assert_eq!(path, folder.join(format!("{stem}.md")));
        let note = fs::read_to_string(&path).unwrap();
        assert!(note.starts_with("---\nmurmur_id: \"m1\"\n"));
        assert!(note.contains("speakers:\n  - \"Me\"\n  - \"Ana\"\n"));
        assert!(note.contains("duration: \"00:01:01\"\n"));
        assert!(note.contains("## Summary\n\n### Decisions\n- ship it\n"));
        assert!(note.contains("\n**Ana** 00:01:01 hello\n"));
        assert_eq!(note_id(&path).as_deref(), Some("m1"));

        let stored = store::get(&conn, "m1").unwrap().unwrap();
        assert_eq!(stored.meeting.note_path.as_deref(), path.to_str());
        let before = listing(&folder);
        assert_eq!(export(&conn, &stored, &folder).unwrap(), path);
        assert_eq!(listing(&folder), before);
        // Even without the stored path, the note on disk is taken as its own.
        assert_eq!(export(&conn, &detail, &folder).unwrap(), path);
        assert_eq!(listing(&folder), before);
        assert!(!wants_note(&stored.meeting));
    }

    #[test]
    fn never_overwrites_another_file_of_the_same_name() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("Meetings");
        let mut conn = db();
        let detail = transcribed(&mut conn, "m1", tmp.path(), None);
        let stem = file_stem(&detail.meeting);
        assert!(stem.contains(" Meeting "), "{stem}");
        fs::create_dir_all(&folder).unwrap();
        let theirs = folder.join(format!("{stem}.md"));
        fs::write(&theirs, "my own notes").unwrap();

        let path = export(&conn, &detail, &folder).unwrap();
        assert_eq!(path, folder.join(format!("{stem} (2).md")));
        assert_eq!(fs::read_to_string(&theirs).unwrap(), "my own notes");
        let names: Vec<String> = listing(&folder).into_iter().map(|f| f.0).collect();
        assert_eq!(names.len(), 2, "no temp file is left: {names:?}");
    }

    #[test]
    fn only_a_finished_meeting_transcribed_here_gets_a_note_on_its_own() {
        let tmp = tempfile::tempdir().unwrap();
        let mut conn = db();
        let mut row = transcribed(&mut conn, "m1", tmp.path(), None).meeting;
        assert!(wants_note(&row));
        row.source = "wispr".into();
        assert!(!wants_note(&row));
        row.source = "spokenly".into();
        row.asr_model = Some("parakeet-ultra-Q8_0".into());
        assert!(wants_note(&row));
        row.source = "murmur".into();
        row.status = Some("failed".into());
        assert!(!wants_note(&row));
    }

    #[test]
    fn notes_go_nowhere_until_a_vault_is_set() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        fs::create_dir(home.join("vault")).unwrap();
        let mut settings = crate::settings::get_default_settings();
        let mut folder = |vault: &str| {
            settings.obsidian_vault_path = Some(vault.into());
            notes_folder(&settings, home).map_err(|e| e.to_string())
        };
        assert_eq!(folder("  "), Ok(None));
        let vault = home.join("vault");
        assert_eq!(
            folder(vault.to_str().unwrap()),
            Ok(Some(vault.join("Meetings")))
        );
        assert_eq!(folder("~/vault"), Ok(Some(vault.join("Meetings"))));
        // Typed as a path the vault isn't at: never a new folder elsewhere.
        for wrong in ["vault", "~/vaul", "/nowhere/vault"] {
            assert!(folder(wrong).is_err(), "{wrong}");
        }
        assert!(!home.join("vaul").exists());

        settings.obsidian_vault_path = Some(format!(" {} ", vault.display()));
        settings.obsidian_notes_folder = "/Work/Meetings/".into();
        assert_eq!(
            notes_folder(&settings, home).unwrap(),
            Some(vault.join("Work/Meetings"))
        );
        settings.obsidian_vault_path = None;
        assert_eq!(notes_folder(&settings, home).unwrap(), None);
    }
}
