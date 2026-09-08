//! CLI metadata, as written by Cursor Agent's chat-session-meta.ts (schema 1).
//! The protobuf blob store is deliberately not treated as a text transcript.

use crate::session::{Session, cursor_timestamp, mtime_iso, user_home};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

pub(crate) fn has_store(dir: &Path) -> bool {
    fs::metadata(dir.join("store.db")).is_ok_and(|m| m.is_file() && m.len() > 0)
}

pub(crate) fn merge_cli_sessions(sessions: &mut Vec<Session>) {
    let Some(home) = user_home() else {
        return;
    };
    merge_cli_sessions_from(sessions, &home.join(".cursor/chats"));
}

/// One `~/.cursor/chats/<workspace-hash>/<id>` directory with a usable store.
struct CliChat {
    dir: PathBuf,
    meta: Value,
    project: String,
    updated: i64,
    workspace_exists: bool,
}

fn merge_cli_sessions_from(sessions: &mut Vec<Session>, root: &Path) {
    let Ok(workspaces) = fs::read_dir(root) else {
        return;
    };
    let mut copies: BTreeMap<String, Vec<CliChat>> = BTreeMap::new();
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
            let project = meta
                .get("cwd")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let workspace_exists =
                Path::new(&project).is_absolute() && Path::new(&project).is_dir();
            let updated = meta.get("updatedAtMs").and_then(Value::as_i64).unwrap_or(0);
            copies
                .entry(chat.file_name().to_string_lossy().into_owned())
                .or_default()
                .push(CliChat {
                    dir,
                    meta,
                    project,
                    updated,
                    workspace_exists,
                });
        }
    }
    for (id, mut chats) in copies {
        // The same chat resumed from another directory gets a second copy.
        // Rank them the way `resume` does (an existing workspace, then the
        // newest) so the listed copy is the one that would actually reopen.
        // Directory iteration order must not decide.
        chats.sort_by(|a, b| {
            b.workspace_exists
                .cmp(&a.workspace_exists)
                .then(b.updated.cmp(&a.updated))
        });
        let mut found = false;
        for session in sessions
            .iter_mut()
            .filter(|s| s.id.eq_ignore_ascii_case(&id))
        {
            found = true;
            // A transcript that already knows its workspace takes that copy's
            // metadata only; another copy's title or path must not replace it.
            let chat = if Path::new(&session.project).is_absolute() {
                chats.iter().find(|c| c.project == session.project)
            } else {
                chats.first()
            };
            let Some(chat) = chat else {
                continue;
            };
            if session.summary.is_empty() {
                session.summary = title_of(&chat.meta);
            }
            if Path::new(&chat.project).is_absolute() {
                session.project = chat.project.clone();
            }
            let created = cursor_timestamp(&chat.meta["createdAtMs"]);
            if !created.is_empty() {
                session.created = created;
            }
        }
        if found {
            continue;
        }
        let Some(chat) = chats.into_iter().next() else {
            continue;
        };
        let mut modified = cursor_timestamp(&chat.meta["updatedAtMs"]);
        if modified.is_empty() {
            modified = mtime_iso(&chat.dir.join("store.db")).unwrap_or_default();
        }
        let date = modified.get(..10).unwrap_or("").to_owned();
        sessions.push(Session {
            source: "cursor".into(),
            id,
            summary: title_of(&chat.meta),
            first_prompt: String::new(),
            created: cursor_timestamp(&chat.meta["createdAtMs"]),
            modified,
            date,
            messages: 0,
            branch: String::new(),
            project: chat.project,
            file: chat.dir.join("store.db").to_string_lossy().into_owned(),
            is_sidechain: chat
                .meta
                .get("isSubagent")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            also_ide: false,
        });
    }
}

fn title_of(meta: &Value) -> String {
    meta.get("title")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
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

    fn write_copy(root: &Path, hash: &str, id: &str, title: &str, cwd: &Path, updated: i64) {
        let dir = root.join(hash).join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("meta.json"),
            serde_json::json!({"schemaVersion":1, "title":title, "cwd":cwd,
                "createdAtMs":updated - 1, "updatedAtMs":updated, "hasConversation":true})
            .to_string(),
        )
        .unwrap();
        let conn = rusqlite::Connection::open(dir.join("store.db")).unwrap();
        conn.execute_batch("CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB);")
            .unwrap();
    }

    #[test]
    fn duplicate_workspace_copies_list_the_copy_that_resume_would_open() {
        // Directory iteration order is filesystem-defined (APFS hashes names),
        // so run several naming permutations: none of them may decide.
        for (old_hash, new_hash, gone_hash) in [
            ("aaaa-old", "mmmm-new", "zzzz-gone"),
            ("mmmm-old", "zzzz-new", "aaaa-gone"),
            ("zzzz-old", "aaaa-new", "mmmm-gone"),
        ] {
            let tmp = tempfile::TempDir::new().unwrap();
            let (ws_old, ws_new) = (tmp.path().join("ws-old"), tmp.path().join("ws-new"));
            fs::create_dir_all(&ws_old).unwrap();
            fs::create_dir_all(&ws_new).unwrap();
            let root = tmp.path().join("chats");
            let id = "dup-id";
            write_copy(
                &root,
                old_hash,
                id,
                "Old workspace copy",
                &ws_old,
                1_788_220_800_000,
            );
            write_copy(
                &root,
                new_hash,
                id,
                "New workspace copy",
                &ws_new,
                1_788_307_200_000,
            );
            write_copy(
                &root,
                gone_hash,
                id,
                "Newest but gone",
                Path::new("/no/such/ws"),
                1_788_393_600_000,
            );

            let mut sessions = Vec::new();
            merge_cli_sessions_from(&mut sessions, &root);
            assert_eq!(sessions.len(), 1, "{old_hash}");
            assert_eq!(sessions[0].summary, "New workspace copy", "{old_hash}");
            assert_eq!(Path::new(&sessions[0].project), ws_new, "{old_hash}");
            assert!(sessions[0].file.contains(new_hash), "{old_hash}");

            // A transcript that already knows its workspace keeps that copy.
            let mut transcript = sessions[0].clone();
            transcript.file = "/transcripts/dup-id.jsonl".into();
            transcript.summary.clear();
            transcript.project = ws_old.to_string_lossy().into_owned();
            let mut sessions = vec![transcript];
            merge_cli_sessions_from(&mut sessions, &root);
            assert_eq!(sessions.len(), 1, "{old_hash}");
            assert_eq!(sessions[0].summary, "Old workspace copy", "{old_hash}");
            assert_eq!(Path::new(&sessions[0].project), ws_old, "{old_hash}");

            // A transcript without a known workspace follows resume's choice.
            sessions[0].summary.clear();
            sessions[0].project.clear();
            merge_cli_sessions_from(&mut sessions, &root);
            assert_eq!(sessions[0].summary, "New workspace copy", "{old_hash}");
            assert_eq!(Path::new(&sessions[0].project), ws_new, "{old_hash}");
        }
    }
}
