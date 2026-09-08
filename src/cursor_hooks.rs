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

fn read_records(db: &Path) -> Vec<HookRecord> {
    if !db.is_file() {
        return Vec::new();
    }
    let read = || -> rusqlite::Result<Vec<HookRecord>> {
        let conn = crate::cursor_ide::open_ro(db).ok_or(rusqlite::Error::InvalidQuery)?;
        let mut stmt =
            conn.prepare("SELECT metadata FROM transcripts ORDER BY conversation_id, path")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows
            .filter_map(Result::ok)
            .filter_map(|raw| serde_json::from_str(&raw).ok())
            .collect())
    };
    read().unwrap_or_else(|error| {
        eprintln!(
            "Warning: cannot read Cursor hook registry {}: {error}",
            db.display()
        );
        Vec::new()
    })
}

pub(crate) fn merge_registered_transcripts(sessions: &mut Vec<Session>) {
    let Some(db) = registry_path() else {
        return;
    };
    merge_records(sessions, read_records(&db));
}

fn merge_records(sessions: &mut Vec<Session>, records: Vec<HookRecord>) {
    for record in records {
        let Some(file) = record.transcript_path else {
            continue;
        };
        // Only this module writes the registry, after validation; a path
        // registered before Cursor flushed the file simply waits.
        let path = Path::new(&file);
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
        if let Some(session) = sessions.iter_mut().find(|s| Path::new(&s.file) == path) {
            if session.id == record.conversation_id && !project.is_empty() {
                session.project = project;
            }
            continue;
        }
        let modified = mtime_iso(path).unwrap_or_default();
        let date = modified.get(..10).unwrap_or("").to_owned();
        let first_prompt = if path.extension().is_some_and(|e| e == "txt") {
            crate::session::cursor_first_prompt_txt(path)
        } else {
            crate::session::cursor_first_prompt_jsonl(path)
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
            file,
            also_ide: false,
        });
    }
}

pub(crate) fn registered_model(session: &Session) -> Option<String> {
    let record = read_records(&registry_path()?).into_iter().find(|r| {
        r.conversation_id == session.id && r.transcript_path.as_deref() == Some(&session.file)
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
        merge_records(&mut sessions, read_records(&db));
        assert!(sessions.is_empty());
        std::fs::write(
            &file,
            "{\"role\":\"user\",\"message\":{\"content\":\"find my history\"}}\n",
        )
        .unwrap();
        merge_records(&mut sessions, read_records(&db));
        merge_records(&mut sessions, read_records(&db));
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].first_prompt, "find my history");
        let conn = Connection::open(&db).unwrap();
        let stored: String = conn
            .query_row("SELECT metadata FROM transcripts", [], |r| r.get(0))
            .unwrap();
        assert!(!stored.contains("not-stored"));
        assert!(!stored.contains("prompt"));
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
        merge_records(&mut sessions, read_records(&db));
        let prompts: Vec<&str> = sessions.iter().map(|s| s.first_prompt.as_str()).collect();
        assert!(prompts.contains(&"new question"), "{prompts:?}");
        assert!(prompts.contains(&"old question"), "{prompts:?}");
        merge_records(&mut sessions, read_records(&db));
        assert_eq!(sessions.len(), 2, "re-merging must not duplicate");
    }
}
