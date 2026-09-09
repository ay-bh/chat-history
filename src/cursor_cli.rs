//! CLI metadata, as written by Cursor Agent's chat-session-meta.ts (schema 1).
//! The content-addressed blob store (JSON message bodies, protobuf ordering)
//! is deliberately not treated as a text transcript.

use crate::session::{
    Session, cursor_project_slug, cursor_timestamp, existing_absolute_dir, iso_date, ms_to_iso,
    mtime_iso, user_home,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
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
struct CliChat {
    dir: PathBuf,
    project: String,
    updated: i64,
    workspace_exists: bool,
    title: String,
    created: String,
    is_subagent: bool,
    /// meta.json schemaVersion label when it is not the supported 1.
    unsupported_schema: Option<String>,
    has_conversation: bool,
}

/// Why a `~/.cursor/chats/<hash>/<id>` directory is not a usable copy.
enum Skip {
    NoStore,
    BadMeta,
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
    // An unknown schema is not listed (its fields may mean something else),
    // but resume only needs `cwd`, which existing_absolute_dir still guards.
    let schema = meta.get("schemaVersion").and_then(Value::as_u64);
    let unsupported_schema =
        (schema != Some(1)).then(|| schema.map_or("(none)".to_owned(), |v| v.to_string()));
    // Cursor's own list hides a chat it marked as empty; the store itself is
    // still what `agent --resume` needs, so resume keeps working (as on
    // 0.4.0) when a transcript proves the chat happened.
    let has_conversation = meta.get("hasConversation").and_then(Value::as_bool) != Some(false);
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
        unsupported_schema,
        has_conversation,
    })
}

/// Why `resume` cannot open this session's CLI chat even though a store for
/// it exists; None when there is no store at all (or a usable one exists).
pub fn unresumable_reason(session: &Session) -> Option<String> {
    unresumable_reason_in(&chats_dir()?, session)
}

fn unresumable_reason_in(chats_dir: &Path, session: &Session) -> Option<String> {
    let (mut gone, mut blank, mut bad_meta) = (BTreeSet::new(), false, false);
    for workspace in fs::read_dir(chats_dir).ok()?.flatten() {
        match read_copy(&workspace.path().join(&session.id)) {
            Ok(copy) if copy.workspace_exists => return None,
            Ok(copy) if copy.project.is_empty() => blank = true,
            Ok(copy) => {
                gone.insert(copy.project);
            }
            Err(Skip::BadMeta) => bad_meta = true,
            Err(Skip::NoStore) => {}
        }
    }
    if !gone.is_empty() {
        return Some(format!(
            "its workspace {} no longer exists. Cursor keys the chat by workspace and id, so recreate that directory to resume it.",
            gone.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    if blank {
        return Some("its meta.json records no workspace directory.".to_owned());
    }
    bad_meta.then(|| "its meta.json is missing or unreadable.".to_owned())
}

/// Two spellings of one directory (symlinks, `/private/tmp` vs `/tmp`).
pub fn same_workspace(a: &str, b: &str) -> bool {
    // A relative value is an undecoded Cursor slug; it must never resolve
    // against the current directory.
    a == b
        || (Path::new(a).is_absolute()
            && Path::new(b).is_absolute()
            && fs::canonicalize(a)
                .ok()
                .is_some_and(|a| fs::canonicalize(b).ok() == Some(a)))
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
    let chosen = copies
        .iter()
        .find(|copy| same_workspace(&copy.project, &session.project))
        .or(copies.first())?;
    Some(PathBuf::from(&chosen.project))
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
            if let Ok(copy) = read_copy(&chat.path()) {
                if let Some(v) = &copy.unsupported_schema
                    && !unsupported.contains(v)
                {
                    unsupported.push(v.clone());
                }
                copies
                    .entry(chat.file_name().to_string_lossy().into_owned())
                    .or_default()
                    .push(copy);
            }
        }
    }
    if !unsupported.is_empty() {
        // Every field is read defensively, so use the stores anyway; say so,
        // since a newer Cursor may have changed what they mean.
        eprintln!(
            "Warning: Cursor CLI chat stores under {} have meta.json schemaVersion {} (this version was written for 1); using them anyway. Update chat-history if their titles or times look wrong.",
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
            // Nothing else proves the chat happened: list it only if Cursor
            // itself would (the store may still be resumable).
            if chats.iter().any(|c| c.has_conversation) {
                push_store_only(sessions, id, chats.swap_remove(0));
            }
            continue;
        }
        for session in listed {
            // A transcript's `project` is the decoded workspace path, else the
            // raw `~/.cursor/projects/<slug>`, else empty (hook-registered).
            // Take only the copy from that workspace so another copy's title
            // or path never replaces it; with no workspace evidence, follow
            // resume's choice.
            let chat = if Path::new(&session.project).is_absolute() {
                chats
                    .iter()
                    .find(|c| same_workspace(&c.project, &session.project))
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
    let mut modified = ms_to_iso(chat.updated);
    if modified.is_empty() {
        modified = mtime_iso(&store).unwrap_or_default();
    }
    let date = iso_date(&modified);
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
    fn unresumable_reason_skips_blank_workspaces_and_repeats() {
        let tmp = tempfile::TempDir::new().unwrap();
        let chats = tmp.path().join("chats");
        let id = "dup-id";
        let gone = tmp.path().join("gone");
        write_copy(&chats, "a", id, "", &gone, 1);
        write_copy(&chats, "b", id, "", &gone, 2);
        write_copy(&chats, "c", id, "", Path::new(""), 3);
        let reason = unresumable_reason_in(&chats, &session_for(id, "")).unwrap();
        assert_eq!(
            reason.matches(gone.to_str().unwrap()).count(),
            1,
            "{reason}"
        );
        assert!(!reason.contains("workspace  no"), "{reason}");
        assert!(unresumable_reason_in(&chats, &session_for("other", "")).is_none());
    }

    #[test]
    fn a_relative_slug_never_resolves_against_the_current_directory() {
        // `src` exists relative to the package root where tests run.
        let here = std::env::current_dir().unwrap().join("src");
        assert!(!same_workspace("src", here.to_str().unwrap()));
        assert!(same_workspace(
            here.to_str().unwrap(),
            here.to_str().unwrap()
        ));
    }

    #[test]
    fn a_newer_empty_copy_does_not_hide_a_real_conversation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let chats = tmp.path().join("chats");
        let (ws_a, ws_b) = (tmp.path().join("a"), tmp.path().join("b"));
        fs::create_dir_all(&ws_a).unwrap();
        fs::create_dir_all(&ws_b).unwrap();
        write_copy(&chats, "a", "id", "Real chat", &ws_a, 1);
        write_copy(&chats, "b", "id", "", &ws_b, 2);
        let meta = chats.join("b/id/meta.json");
        let flagged = fs::read_to_string(&meta)
            .unwrap()
            .replace("\"hasConversation\":true", "\"hasConversation\":false");
        fs::write(&meta, flagged).unwrap();
        let mut sessions = Vec::new();
        merge_cli_sessions_from(&mut sessions, &chats);
        assert_eq!(sessions.len(), 1);
    }

    #[test]
    fn a_store_without_readable_meta_is_explained() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("chats/h/id");
        fs::create_dir_all(&dir).unwrap();
        let conn = rusqlite::Connection::open(dir.join("store.db")).unwrap();
        conn.execute_batch("CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB);")
            .unwrap();
        drop(conn);
        let reason =
            unresumable_reason_in(&tmp.path().join("chats"), &session_for("id", "")).unwrap();
        assert!(reason.contains("meta.json"), "{reason}");
        fs::write(dir.join("meta.json"), "{ not json").unwrap();
        let reason =
            unresumable_reason_in(&tmp.path().join("chats"), &session_for("id", "")).unwrap();
        assert!(reason.contains("meta.json"), "{reason}");
    }

    #[test]
    fn workspace_paths_match_after_canonicalization() {
        // macOS temp dirs live under /var -> /private/var; a listed project may
        // carry either spelling and must still match the store's cwd.
        let tmp = tempfile::TempDir::new().unwrap();
        let chats = tmp.path().join("chats");
        let ws = tmp.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        let canonical = fs::canonicalize(&ws).unwrap();
        write_copy(&chats, "h", "id", "Titled", &ws, 5);
        let listed = session_for("id", canonical.to_str().unwrap());
        assert_eq!(resume_workspace_in(&chats, &listed), Some(ws.clone()));
        let mut sessions = vec![listed];
        merge_cli_sessions_from(&mut sessions, &chats);
        assert_eq!(sessions[0].summary, "Titled");
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
