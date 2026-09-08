//! CLI metadata, as written by Cursor Agent's chat-session-meta.ts (schema 1).
//! The content-addressed blob store (JSON message bodies, protobuf ordering)
//! is deliberately not treated as a text transcript.

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
pub(crate) struct CliChat {
    dir: PathBuf,
    meta: Value,
    pub(crate) project: String,
    updated: i64,
    pub(crate) workspace_exists: bool,
}

/// The single rule for what counts as a resumable CLI chat copy. Listing
/// and `resume` both go through here so they can never disagree.
fn read_copy(dir: &Path) -> Option<CliChat> {
    if !has_store(dir) {
        return None;
    }
    let meta: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("meta.json")).ok()?).ok()?;
    if meta.get("schemaVersion").and_then(Value::as_u64) != Some(1)
        || meta.get("hasConversation").and_then(Value::as_bool) == Some(false)
    {
        return None;
    }
    let project = meta
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let workspace_exists = Path::new(&project).is_absolute() && Path::new(&project).is_dir();
    let updated = meta.get("updatedAtMs").and_then(Value::as_i64).unwrap_or(0);
    Some(CliChat {
        dir: dir.to_path_buf(),
        meta,
        project,
        updated,
        workspace_exists,
    })
}

/// The same chat resumed from another directory gets a second copy. Rank
/// them so the copy that would actually reopen comes first: an existing
/// workspace, then the newest, then the path so directory iteration order
/// never decides.
fn rank(copies: &mut [CliChat]) {
    copies.sort_by(|a, b| {
        b.workspace_exists
            .cmp(&a.workspace_exists)
            .then(b.updated.cmp(&a.updated))
            .then(a.dir.cmp(&b.dir))
    });
}

/// Workspace to pass to `agent --resume` for this session, if the CLI has a
/// store for it. The CLI looks a chat up by (workspace, id), so resuming
/// from any other directory silently starts a blank chat. Prefer the copy
/// the listing showed (the session's own workspace); otherwise the
/// best-ranked copy whose workspace still exists.
pub(crate) fn resume_workspace(session: &Session) -> Option<PathBuf> {
    let chats = user_home()?.join(".cursor/chats");
    resume_workspace_in(&chats, session)
}

pub(crate) fn resume_workspace_in(chats_dir: &Path, session: &Session) -> Option<PathBuf> {
    let mut copies: Vec<CliChat> = fs::read_dir(chats_dir)
        .ok()?
        .flatten()
        .filter_map(|workspace| read_copy(&workspace.path().join(&session.id)))
        .filter(|copy| copy.workspace_exists)
        .collect();
    rank(&mut copies);
    copies
        .iter()
        .find(|copy| copy.project == session.project)
        .or(copies.first())
        .map(|copy| PathBuf::from(&copy.project))
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
            if let Some(copy) = read_copy(&chat.path()) {
                copies
                    .entry(chat.file_name().to_string_lossy().into_owned())
                    .or_default()
                    .push(copy);
            }
        }
    }
    for (id, mut chats) in copies {
        rank(&mut chats);
        let mut found = false;
        for session in sessions
            .iter_mut()
            .filter(|s| s.id.eq_ignore_ascii_case(&id))
        {
            found = true;
            // A transcript takes only the copy from its own workspace: by
            // absolute path when the slug resolved, else by the project slug
            // in its file path. Another copy's title or path must not replace
            // it. A transcript with no workspace evidence at all (a
            // hook-registered path) follows resume's choice.
            let chat = if Path::new(&session.project).is_absolute() {
                chats.iter().find(|c| c.project == session.project)
            } else if let Some(slug) = transcript_project_slug(&session.file) {
                chats
                    .iter()
                    .find(|c| cursor_project_slug(&c.project) == slug)
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

/// Cursor names `~/.cursor/projects/<slug>` by replacing every run of
/// non-alphanumeric characters in the workspace path with one hyphen
/// (matches all 204 resolved transcripts on a real install).
fn cursor_project_slug(workspace: &str) -> String {
    let mut slug = String::with_capacity(workspace.len());
    for c in workspace.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    slug.trim_end_matches('-').to_owned()
}

/// The `<slug>` component of a transcript stored under `.cursor/projects`.
fn transcript_project_slug(file: &str) -> Option<String> {
    let mut components = Path::new(file).components().peekable();
    while let Some(component) = components.next() {
        if component.as_os_str() == ".cursor" && components.next()?.as_os_str() == "projects" {
            return components
                .next()
                .map(|slug| slug.as_os_str().to_string_lossy().into_owned());
        }
    }
    None
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

            // Cursor writes one transcript per workspace the chat ran in. When
            // the slug did not resolve, each transcript keeps its own copy.
            let transcript_in = |ws: &Path| {
                let slug = cursor_project_slug(ws.to_str().unwrap());
                let mut s = sessions[0].clone();
                s.file =
                    format!("/home/u/.cursor/projects/{slug}/agent-transcripts/{id}/{id}.jsonl");
                s.project = slug;
                s.summary.clear();
                s
            };
            let mut sessions = vec![transcript_in(&ws_old), transcript_in(&ws_new)];
            merge_cli_sessions_from(&mut sessions, &root);
            assert_eq!(sessions.len(), 2, "{old_hash}");
            assert_eq!(sessions[0].summary, "Old workspace copy", "{old_hash}");
            assert_eq!(Path::new(&sessions[0].project), ws_old, "{old_hash}");
            assert_eq!(sessions[1].summary, "New workspace copy", "{old_hash}");
            assert_eq!(Path::new(&sessions[1].project), ws_new, "{old_hash}");

            // An unresolvable slug with several copies is left alone rather
            // than stamped with another workspace's path.
            let mut stranger = sessions[0].clone();
            stranger.file = format!(
                "/home/u/.cursor/projects/some-Doc-6ac763d/agent-transcripts/{id}/{id}.jsonl"
            );
            stranger.project = "some-Doc-6ac763d".into();
            stranger.summary.clear();
            let mut sessions = vec![stranger];
            merge_cli_sessions_from(&mut sessions, &root);
            assert_eq!(sessions[0].project, "some-Doc-6ac763d", "{old_hash}");
            assert!(sessions[0].summary.is_empty(), "{old_hash}");
        }
    }

    fn session_for(id: &str, project: &str) -> Session {
        Session {
            source: "cursor".into(),
            id: id.into(),
            summary: String::new(),
            first_prompt: String::new(),
            created: String::new(),
            modified: String::new(),
            date: String::new(),
            messages: 0,
            branch: String::new(),
            project: project.into(),
            file: String::new(),
            is_sidechain: false,
            also_ide: false,
        }
    }

    #[test]
    fn resume_prefers_the_listed_workspace_then_the_newest_existing_one() {
        let tmp = tempfile::TempDir::new().unwrap();
        let chats = tmp.path().join("chats");
        let (ws_old, ws_new) = (tmp.path().join("ws-old"), tmp.path().join("ws-new"));
        fs::create_dir_all(&ws_old).unwrap();
        fs::create_dir_all(&ws_new).unwrap();
        let id = "cc9ae34e-117f-435c-9a83-f8958c7b09e1";
        write_copy(&chats, "aaaa", id, "", &ws_old, 1);
        write_copy(&chats, "bbbb", id, "", &ws_new, 2);
        write_copy(&chats, "cccc", id, "", Path::new("/no/such/workspace"), 3); // newest, gone
        write_copy(&chats, "dddd", id, "", &ws_old, 4);
        fs::remove_file(chats.join("dddd").join(id).join("store.db")).unwrap();

        // The row the user is looking at wins, even though another copy is newer.
        let listed_old = session_for(id, ws_old.to_str().unwrap());
        assert_eq!(
            resume_workspace_in(&chats, &listed_old),
            Some(ws_old.clone())
        );
        // No usable copy for the listed workspace: the best existing one.
        let listed_gone = session_for(id, "/no/such/workspace");
        assert_eq!(
            resume_workspace_in(&chats, &listed_gone),
            Some(ws_new.clone())
        );
        let slug_only = session_for(id, "some-slug");
        assert_eq!(
            resume_workspace_in(&chats, &slug_only),
            Some(ws_new.clone())
        );
        assert_eq!(
            resume_workspace_in(&chats, &session_for("other-id", "")),
            None
        );
        assert_eq!(
            resume_workspace_in(&tmp.path().join("missing"), &listed_old),
            None
        );
        // Listing ranks the same copies with the same rule.
        let mut sessions = Vec::new();
        merge_cli_sessions_from(&mut sessions, &chats);
        assert_eq!(sessions.len(), 1);
        assert_eq!(Path::new(&sessions[0].project), ws_new);
    }

    #[test]
    fn project_slug_matches_cursor_naming() {
        assert_eq!(
            cursor_project_slug("/Users/me/Documents/GitHub/chat-history"),
            "Users-me-Documents-GitHub-chat-history"
        );
        assert_eq!(
            cursor_project_slug("/private/tmp/claude-501/-Users-me/x y"),
            "private-tmp-claude-501-Users-me-x-y"
        );
        assert_eq!(
            transcript_project_slug(
                "/home/u/.cursor/projects/Users-me-app/agent-transcripts/i/i.jsonl"
            )
            .as_deref(),
            Some("Users-me-app")
        );
        assert_eq!(transcript_project_slug("/elsewhere/i.jsonl"), None);
    }
}
