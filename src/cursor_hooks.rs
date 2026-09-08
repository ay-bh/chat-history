//! Optional discovery via Cursor's documented `stop` hook. This registry holds
//! only locations and selected metadata, never prompts, tool output, or email.

use crate::session::{Session, mtime_iso, user_home};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Deserialize, Serialize)]
struct HookRecord {
    conversation_id: String,
    transcript_path: Option<String>,
    #[serde(default)]
    workspace_roots: Vec<String>,
    model: Option<String>,
    model_id: Option<String>,
    cursor_version: Option<String>,
}

fn registry_path() -> Option<PathBuf> {
    Some(user_home()?.join(".chat-history/cursor-hooks.db"))
}

pub fn record_hook(input: impl Read) -> Result<(), String> {
    let db = registry_path().ok_or("Cannot locate the user home directory")?;
    record_hook_at(input, &db)
}

fn record_hook_at(input: impl Read, db: &Path) -> Result<(), String> {
    const MAX_INPUT: u64 = 1024 * 1024;
    let mut raw = String::new();
    input
        .take(MAX_INPUT + 1)
        .read_to_string(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() as u64 > MAX_INPUT {
        return Err("Cursor hook input exceeds 1 MiB".into());
    }
    let record: HookRecord =
        serde_json::from_str(&raw).map_err(|e| format!("Invalid Cursor hook input: {e}"))?;
    // Null means transcripts are disabled. No registry or directory is created.
    let Some(path) = record.transcript_path.as_deref() else {
        return Ok(());
    };
    if record.conversation_id.is_empty() || record.conversation_id.len() > 256 {
        return Err("Cursor hook requires a conversation_id of 1–256 bytes".into());
    }
    if !transcript_path_supported(Path::new(path)) {
        return Err("Cursor hook transcript_path must be an absolute .jsonl or .txt path".into());
    }
    // The hook may run before Cursor flushes the transcript: register the path
    // now, and check for an actual file at discovery time.
    std::fs::create_dir_all(db.parent().ok_or("Invalid registry path")?)
        .map_err(|e| e.to_string())?;
    let conn = Connection::open(db).map_err(|e| e.to_string())?;
    conn.busy_timeout(std::time::Duration::from_secs(2))
        .map_err(|e| e.to_string())?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS transcripts (
        conversation_id TEXT NOT NULL, path TEXT NOT NULL, metadata TEXT NOT NULL,
        PRIMARY KEY (conversation_id, path));",
    )
    .map_err(|e| e.to_string())?;
    let metadata = serde_json::to_string(&record).map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO transcripts VALUES (?1, ?2, ?3)
        ON CONFLICT(conversation_id, path) DO UPDATE SET metadata = excluded.metadata",
        rusqlite::params![record.conversation_id, path, metadata],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn transcript_path_supported(path: &Path) -> bool {
    path.is_absolute()
        && matches!(
            path.extension().and_then(|s| s.to_str()),
            Some("jsonl" | "txt")
        )
}

fn read_records(db: &Path, warn: bool) -> Vec<HookRecord> {
    if !db.is_file() {
        return Vec::new();
    }
    let read = || -> Result<Vec<HookRecord>, String> {
        let conn = crate::cursor_ide::open_ro(db)
            .ok_or("cannot open it (permissions, or a lock held longer than 2s)")?;
        let mut stmt = conn
            .prepare("SELECT metadata FROM transcripts ORDER BY conversation_id, path")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        Ok(rows
            .filter_map(Result::ok)
            .filter_map(|raw| serde_json::from_str(&raw).ok())
            .collect())
    };
    read().unwrap_or_else(|error| {
        // The listing already warned; a later lookup stays quiet.
        if warn {
            eprintln!(
                "Warning: cannot read Cursor hook registry {}: {error}",
                db.display()
            );
        }
        Vec::new()
    })
}

pub(crate) fn merge_registered_transcripts(sessions: &mut Vec<Session>) {
    let Some(db) = registry_path() else {
        return;
    };
    merge_records(sessions, read_records(&db, true));
}

/// The transcript a registration refers to, as the scanner would list it: a
/// `.txt` beside `<id>/<id>.jsonl` means that jsonl (the scanner prefers it),
/// and symlinked or `/private/tmp`-style spellings resolve to one path.
fn registered_file(path: &Path) -> PathBuf {
    let mut file = path.to_path_buf();
    if path.extension().is_some_and(|e| e == "txt")
        && let (Some(stem), Some(dir)) = (path.file_stem(), path.parent())
    {
        let sibling = dir.join(stem).join(stem).with_extension("jsonl");
        if sibling.is_file() {
            file = sibling;
        }
    }
    std::fs::canonicalize(&file).unwrap_or(file)
}

fn same_transcript(listed: &str, registered: &Path) -> bool {
    Path::new(listed) == registered || std::fs::canonicalize(listed).is_ok_and(|p| p == registered)
}

fn merge_records(sessions: &mut Vec<Session>, records: Vec<HookRecord>) {
    for record in records {
        let Some(file) = record.transcript_path else {
            continue;
        };
        // Only this module writes the registry, after validation; a path
        // registered before Cursor flushed the file simply waits.
        let path = registered_file(Path::new(&file));
        if !path.is_file() {
            continue;
        }
        let project = record
            .workspace_roots
            .iter()
            .find(|p| Path::new(p).is_absolute())
            .cloned()
            .unwrap_or_default();
        // A file the scan already listed only gains the hook's workspace.
        // Any other usable file is one more copy of the conversation, listed
        // like scanned copies are; `inspect`/`resume`/`find` pick one copy.
        // Skipping by id here would hide a transcript written later at a
        // path that happens to sort after an older one.
        if let Some(session) = sessions.iter_mut().find(|s| {
            s.id.eq_ignore_ascii_case(&record.conversation_id) && same_transcript(&s.file, &path)
        }) {
            if !project.is_empty() {
                session.project = project;
            }
            continue;
        }
        let modified = mtime_iso(&path).unwrap_or_default();
        let date = modified.get(..10).unwrap_or("").to_owned();
        let first_prompt = if path.extension().is_some_and(|e| e == "txt") {
            crate::session::cursor_first_prompt_txt(&path)
        } else {
            crate::session::cursor_first_prompt_jsonl(&path)
        };
        sessions.push(Session {
            source: "cursor".into(),
            id: record.conversation_id,
            summary: String::new(),
            first_prompt,
            created: modified.clone(),
            modified,
            date,
            messages: 0,
            branch: String::new(),
            project,
            is_sidechain: path.components().any(|c| c.as_os_str() == "subagents"),
            file: path.to_string_lossy().into_owned(),
            also_ide: false,
        });
    }
}

pub(crate) fn registered_model(session: &Session) -> Option<String> {
    registered_model_in(&registry_path()?, session)
}

fn registered_model_in(db: &Path, session: &Session) -> Option<String> {
    // The same "this registration is the listed transcript" rule as merge.
    let record = read_records(db, false).into_iter().find(|r| {
        r.conversation_id.eq_ignore_ascii_case(&session.id)
            && r.transcript_path
                .as_deref()
                .is_some_and(|p| same_transcript(&session.file, &registered_file(Path::new(p))))
    })?;
    record
        .model_id
        .filter(|s| !s.is_empty())
        .or_else(|| record.model.filter(|s| !s.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_or_invalid_transcripts_do_not_create_registry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = tmp.path().join("cache/hooks.db");
        record_hook_at(
            br#"{"conversation_id":"id","transcript_path":null}"#.as_slice(),
            &db,
        )
        .unwrap();
        assert!(!db.exists());
        assert!(
            record_hook_at(
                br#"{"conversation_id":"id","transcript_path":"relative.jsonl"}"#.as_slice(),
                &db
            )
            .is_err()
        );
        assert!(!db.exists());
    }

    #[test]
    fn hooks_register_before_flush_and_deduplicate_without_storing_extra_data() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = tmp.path().join("hooks.db");
        let file = tmp.path().join("outside-default-layout.jsonl");
        let event = serde_json::json!({"conversation_id":"hook-id", "transcript_path":file,
            "workspace_roots":[tmp.path()], "model":"model-v1", "model_id":"model-v2",
            "user_email":"not-stored@example.com", "prompt":"not stored"});
        record_hook_at(event.to_string().as_bytes(), &db).unwrap();
        record_hook_at(event.to_string().as_bytes(), &db).unwrap();
        let mut sessions = Vec::new();
        merge_records(&mut sessions, read_records(&db, true));
        assert!(sessions.is_empty());
        std::fs::write(
            &file,
            "{\"role\":\"user\",\"message\":{\"content\":\"find my history\"}}\n",
        )
        .unwrap();
        merge_records(&mut sessions, read_records(&db, true));
        merge_records(&mut sessions, read_records(&db, true));
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].first_prompt, "find my history");
        let conn = Connection::open(&db).unwrap();
        let stored: String = conn
            .query_row("SELECT metadata FROM transcripts", [], |r| r.get(0))
            .unwrap();
        assert!(!stored.contains("not-stored"));
        assert!(!stored.contains("prompt"));
    }

    fn listed(file: &Path) -> Session {
        Session {
            source: "cursor".into(),
            id: "hook-id".into(),
            file: file.to_string_lossy().into_owned(),
            ..Session::default()
        }
    }

    #[test]
    fn txt_and_aliased_registrations_match_the_scanned_transcript() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = tmp.path().join("hooks.db");
        // The scanner lists `<id>/<id>.jsonl` when both it and `<id>.txt` exist.
        let real = tmp.path().join("real/agent-transcripts");
        let jsonl = real.join("hook-id/hook-id.jsonl");
        std::fs::create_dir_all(jsonl.parent().unwrap()).unwrap();
        std::fs::write(
            &jsonl,
            "{\"role\":\"user\",\"message\":{\"content\":\"q\"}}\n",
        )
        .unwrap();
        std::fs::write(real.join("hook-id.txt"), "user:\nq\n").unwrap();
        std::os::unix::fs::symlink(tmp.path().join("real"), tmp.path().join("alias")).unwrap();
        for registered in [
            real.join("hook-id.txt"),
            tmp.path()
                .join("alias/agent-transcripts/hook-id/hook-id.jsonl"),
        ] {
            let event = serde_json::json!({"conversation_id":"hook-id", "transcript_path":registered,
                "workspace_roots":[tmp.path()], "model_id":"hook-model"});
            record_hook_at(event.to_string().as_bytes(), &db).unwrap();
        }
        let mut sessions = vec![listed(&jsonl)];
        merge_records(&mut sessions, read_records(&db, true));
        assert_eq!(
            sessions.len(),
            1,
            "no duplicate row for the same transcript"
        );
        assert_eq!(Path::new(&sessions[0].project), tmp.path());
        assert_eq!(
            registered_model_in(&db, &sessions[0]).as_deref(),
            Some("hook-model")
        );
    }

    #[test]
    fn every_usable_registered_transcript_is_listed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = tmp.path().join("hooks.db");
        // Registered later but sorts first by path: ordering must not decide.
        let old = tmp.path().join("a-old.jsonl");
        let new = tmp.path().join("z-new.jsonl");
        for (file, text) in [(&old, "old question"), (&new, "new question")] {
            std::fs::write(
                file,
                format!("{{\"role\":\"user\",\"message\":{{\"content\":\"{text}\"}}}}\n"),
            )
            .unwrap();
            let event = serde_json::json!({"conversation_id":"hook-id", "transcript_path":file});
            record_hook_at(event.to_string().as_bytes(), &db).unwrap();
        }
        let mut sessions = Vec::new();
        merge_records(&mut sessions, read_records(&db, true));
        let prompts: Vec<&str> = sessions.iter().map(|s| s.first_prompt.as_str()).collect();
        assert!(prompts.contains(&"new question"), "{prompts:?}");
        assert!(prompts.contains(&"old question"), "{prompts:?}");
        merge_records(&mut sessions, read_records(&db, true));
        assert_eq!(sessions.len(), 2, "re-merging must not duplicate");
    }
}
