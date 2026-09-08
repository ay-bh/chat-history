//! CLI metadata, as written by Cursor Agent's chat-session-meta.ts (schema 1).
//! The content-addressed blob store (JSON message bodies, protobuf ordering)
//! is deliberately not treated as a text transcript.

use crate::session::{
    Session, cursor_project_slug, cursor_timestamp, existing_absolute_dir, mtime_iso, user_home,
};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

fn chats_dir() -> Option<PathBuf> {
    Some(user_home()?.join(".cursor/chats"))
}

pub(crate) fn merge_cli_sessions(sessions: &mut Vec<Session>) {
    if let Some(root) = chats_dir() {
        merge_cli_sessions_from(sessions, &root);
    }
}

/// One `~/.cursor/chats/<workspace-hash>/<id>` directory with a usable store.
pub(crate) struct CliChat {
    dir: PathBuf,
    pub(crate) project: String,
    updated: i64,
    pub(crate) workspace_exists: bool,
    title: String,
    created: String,
    is_subagent: bool,
}

/// Why a `~/.cursor/chats/<hash>/<id>` directory is not a usable copy.
enum Skip {
    NoStore,
    BadMeta,
    Schema(Option<u64>),
    Empty,
}

/// The single rule for what counts as a resumable CLI chat copy. Listing
/// and `resume` both go through here so they can never disagree.
fn read_copy(dir: &Path) -> Result<CliChat, Skip> {
    let store = dir.join("store.db");
    if !fs::metadata(&store).is_ok_and(|m| m.is_file() && m.len() > 0) {
        return Err(Skip::NoStore);
    }
    let meta: Value = fs::read_to_string(dir.join("meta.json"))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .ok_or(Skip::BadMeta)?;
    let schema = meta.get("schemaVersion").and_then(Value::as_u64);
    if schema != Some(1) {
        return Err(Skip::Schema(schema));
    }
    if meta.get("hasConversation").and_then(Value::as_bool) == Some(false) {
        return Err(Skip::Empty);
    }
    let project = meta
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    Ok(CliChat {
        dir: dir.to_path_buf(),
        workspace_exists: existing_absolute_dir(&project).is_some(),
        updated: meta.get("updatedAtMs").and_then(Value::as_i64).unwrap_or(0),
        title: meta
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        created: cursor_timestamp(&meta["createdAtMs"]),
        is_subagent: meta
            .get("isSubagent")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        project,
    })
}

/// Why `resume` cannot open this session's CLI chat even though a store for
/// it exists; None when there is no store at all (or a usable one exists).
pub fn unresumable_reason(session: &Session) -> Option<String> {
    let (mut gone, mut schemas, mut empty) = (Vec::new(), Vec::new(), false);
    for workspace in fs::read_dir(chats_dir()?).ok()?.flatten() {
        match read_copy(&workspace.path().join(&session.id)) {
            Ok(copy) if !copy.workspace_exists => gone.push(copy.project),
            Ok(_) => return None,
            Err(Skip::Schema(v)) => schemas.push(v.map_or("missing".to_owned(), |v| v.to_string())),
            Err(Skip::Empty) => empty = true,
            Err(_) => {}
        }
    }
    if !gone.is_empty() {
        return Some(format!(
            "its workspace {} no longer exists. Cursor keys the chat by workspace and id, so recreate that directory to resume it.",
            gone.join(", ")
        ));
    }
    if !schemas.is_empty() {
        return Some(format!(
            "its meta.json uses schemaVersion {} and this version of chat-history supports 1; update chat-history.",
            schemas.join(", ")
        ));
    }
    empty.then(|| "Cursor recorded it as an empty conversation.".to_owned())
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
    resume_workspace_in(&chats_dir()?, session)
}

pub(crate) fn resume_workspace_in(chats_dir: &Path, session: &Session) -> Option<PathBuf> {
    let mut copies: Vec<CliChat> = fs::read_dir(chats_dir)
        .ok()?
        .flatten()
        .filter_map(|workspace| read_copy(&workspace.path().join(&session.id)).ok())
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
    let mut unsupported: Vec<String> = Vec::new();
    for workspace in workspaces.flatten() {
        let Ok(chats) = fs::read_dir(workspace.path()) else {
            continue;
        };
        for chat in chats.flatten() {
            match read_copy(&chat.path()) {
                Ok(copy) => copies
                    .entry(chat.file_name().to_string_lossy().into_owned())
                    .or_default()
                    .push(copy),
                Err(Skip::Schema(v)) => {
                    let v = v.map_or("missing".to_owned(), |v| v.to_string());
                    if !unsupported.contains(&v) {
                        unsupported.push(v);
                    }
                }
                Err(_) => {}
            }
        }
    }
    if !unsupported.is_empty() {
        // Distinguish "this version cannot read your chats" from "no chats".
        eprintln!(
            "Warning: skipped Cursor CLI chat stores under {} with meta.json schemaVersion {} (this version supports 1); update chat-history.",
            root.display(),
            unsupported.join(", ")
        );
    }
    for (id, mut chats) in copies {
        rank(&mut chats);
        let listed: Vec<&mut Session> = sessions
            .iter_mut()
            .filter(|s| s.id.eq_ignore_ascii_case(&id))
            .collect();
        if listed.is_empty() {
            push_store_only(sessions, id, chats.swap_remove(0));
            continue;
        }
        for session in listed {
            // A transcript's `project` is the decoded workspace path, else the
            // raw `~/.cursor/projects/<slug>`, else empty (hook-registered).
            // Take only the copy from that workspace so another copy's title
            // or path never replaces it; with no workspace evidence, follow
            // resume's choice.
            let chat = if Path::new(&session.project).is_absolute() {
                chats.iter().find(|c| c.project == session.project)
            } else if !session.project.is_empty() {
                chats
                    .iter()
                    .find(|c| cursor_project_slug(&c.project) == session.project)
            } else {
                chats.first()
            };
            let Some(chat) = chat else {
                continue;
            };
            if session.summary.is_empty() {
                session.summary = chat.title.clone();
            }
            if Path::new(&chat.project).is_absolute() {
                session.project = chat.project.clone();
            }
            if !chat.created.is_empty() {
                session.created = chat.created.clone();
            }
        }
    }
}

fn push_store_only(sessions: &mut Vec<Session>, id: String, chat: CliChat) {
    let store = chat.dir.join("store.db");
    let mut modified = cursor_timestamp(&serde_json::json!(chat.updated));
    if modified.is_empty() {
        modified = mtime_iso(&store).unwrap_or_default();
    }
    let date = modified.get(..10).unwrap_or("").to_owned();
    sessions.push(Session {
        source: "cursor".into(),
        id,
        summary: chat.title,
        first_prompt: String::new(),
        created: chat.created,
        modified,
        date,
        messages: 0,
        branch: String::new(),
        project: chat.project,
        file: store.to_string_lossy().into_owned(),
        is_sidechain: chat.is_subagent,
        also_ide: false,
    });
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
        assert_eq!(sessions[0].created, "2026-09-01T00:00:00Z");
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
            project: project.into(),
            ..Session::default()
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
    }
}
