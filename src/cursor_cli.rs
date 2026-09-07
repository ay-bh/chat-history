//! CLI metadata, as written by Cursor Agent's chat-session-meta.ts (schema 1).
//! The protobuf blob store is deliberately not treated as a text transcript.

use crate::session::{Session, cursor_timestamp, mtime_iso, user_home};
use serde_json::Value;
use std::{fs, path::Path};

pub(crate) fn has_store(dir: &Path) -> bool {
    fs::metadata(dir.join("store.db")).is_ok_and(|m| m.is_file() && m.len() > 0)
}

pub(crate) fn merge_cli_sessions(sessions: &mut Vec<Session>) {
    let Some(home) = user_home() else {
        return;
    };
    merge_cli_sessions_from(sessions, &home.join(".cursor/chats"));
}

fn merge_cli_sessions_from(sessions: &mut Vec<Session>, root: &Path) {
    let Ok(workspaces) = fs::read_dir(root) else {
        return;
    };
    for workspace in workspaces.flatten() {
        let Ok(chats) = fs::read_dir(workspace.path()) else {
            continue;
        };
        for chat in chats.flatten() {
            let dir = chat.path();
            if !has_store(&dir) {
                continue;
            }
            let Ok(raw) = fs::read_to_string(dir.join("meta.json")) else {
                continue;
            };
            let Ok(meta) = serde_json::from_str::<Value>(&raw) else {
                continue;
            };
            if meta.get("schemaVersion").and_then(Value::as_u64) != Some(1)
                || meta.get("hasConversation").and_then(Value::as_bool) == Some(false)
            {
                continue;
            }
            let id = chat.file_name().to_string_lossy().into_owned();
            let project = meta
                .get("cwd")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let title = meta
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let created = cursor_timestamp(&meta["createdAtMs"]);
            let mut modified = cursor_timestamp(&meta["updatedAtMs"]);
            if modified.is_empty() {
                modified = mtime_iso(&dir.join("store.db")).unwrap_or_default();
            }
            let date = modified.get(..10).unwrap_or("").to_owned();
            let mut found = false;
            for session in sessions
                .iter_mut()
                .filter(|s| s.id.eq_ignore_ascii_case(&id))
            {
                found = true;
                // A copied transcript can have a different workspace. Do not
                // replace that copy's known path or metadata with another's.
                if Path::new(&session.project).is_absolute() && session.project != project {
                    continue;
                }
                if session.summary.is_empty() {
                    session.summary = title.clone();
                }
                if Path::new(&project).is_absolute() {
                    session.project = project.clone();
                }
                if !created.is_empty() {
                    session.created = created.clone();
                }
            }
            if found {
                continue;
            }
            sessions.push(Session {
                source: "cursor".into(),
                id,
                summary: title,
                first_prompt: String::new(),
                created,
                modified,
                date,
                messages: 0,
                branch: String::new(),
                project,
                file: dir.join("store.db").to_string_lossy().into_owned(),
                is_sidechain: meta
                    .get("isSubagent")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                also_ide: false,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_only_sessions_are_discovered_without_transcripts_or_workspace() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("hash/chat-id");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("meta.json"), r#"{"schemaVersion":1,"title":"CLI chat","cwd":"/missing/workspace","createdAtMs":1788220800000,"updatedAtMs":1788307200000,"hasConversation":true,"isSubagent":true}"#).unwrap();
        let mut sessions = Vec::new();
        merge_cli_sessions_from(&mut sessions, tmp.path());
        assert!(sessions.is_empty()); // metadata alone is not a conversation
        let conn = rusqlite::Connection::open(dir.join("store.db")).unwrap();
        conn.execute_batch("CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB);")
            .unwrap();
        drop(conn);
        merge_cli_sessions_from(&mut sessions, tmp.path());
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].summary, "CLI chat");
        assert_eq!(sessions[0].created, "2026-09-01T00:00:00.000Z");
        assert_eq!(sessions[0].date, "2026-09-02");
        assert!(sessions[0].is_sidechain);
        assert!(sessions[0].is_cursor_store_only());
        merge_cli_sessions_from(&mut sessions, tmp.path());
        assert_eq!(sessions.len(), 1);
    }
}
