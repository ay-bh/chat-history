use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::json;
use std::{fs, path::Path};
use tempfile::TempDir;

const ID: &str = "fedc1234-0000-4000-8000-000000000001";

fn command(tmp: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("chat-history").unwrap();
    cmd.env("HOME", tmp.path())
        .env("CLAUDE_CONFIG_DIR", tmp.path().join("claude"))
        .env("CODEX_HOME", tmp.path().join("codex"))
        .env("CURSOR_USER_DIR", tmp.path().join("cursor-user"))
        .env("NO_COLOR", "1");
    cmd
}

fn transcript(tmp: &TempDir, native_timestamp: bool) -> std::path::PathBuf {
    let path = tmp.path().join(format!(
        ".cursor/projects/test-workspace/agent-transcripts/{ID}/{ID}.jsonl"
    ));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut entry = json!({"role":"user", "message":{"content":"investigate uniquecache failure"}});
    if native_timestamp {
        entry["timestamp"] = json!(chrono::Utc::now().to_rfc3339());
    }
    fs::write(&path, format!("{entry}\n")).unwrap();
    path
}

fn cli_store(tmp: &TempDir, with_db: bool) -> std::path::PathBuf {
    let dir = tmp
        .path()
        .join(format!(".cursor/chats/workspace-hash/{ID}"));
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("meta.json"),
        json!({"schemaVersion":1, "cwd":tmp.path(),
        "title":"CLI store discovery", "createdAtMs":1788220800000i64,
        "updatedAtMs":1788307200000i64, "hasConversation":true})
        .to_string(),
    )
    .unwrap();
    if with_db {
        let conn = rusqlite::Connection::open(dir.join("store.db")).unwrap();
        conn.execute_batch("CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB);")
            .unwrap();
    }
    dir
}

fn alternate_cli_store(tmp: &TempDir, original: &Path) -> std::path::PathBuf {
    let workspace = tmp.path().join("alternate workspace");
    fs::create_dir(&workspace).unwrap();
    let dir = tmp.path().join(format!(".cursor/chats/alternate/{ID}"));
    fs::create_dir_all(&dir).unwrap();
    fs::copy(original.join("store.db"), dir.join("store.db")).unwrap();
    fs::write(
        dir.join("meta.json"),
        json!({"schemaVersion":1, "cwd":workspace, "updatedAtMs":1788393600000i64,
            "hasConversation":true})
        .to_string(),
    )
    .unwrap();
    workspace
}

#[test]
fn resume_keeps_the_resolved_workspace_when_stores_change() {
    use chat_history::session::{ResumeAction, Session, resume_command};
    const CHILD: &str = "CHAT_HISTORY_RESUME_SNAPSHOT_TEST";
    if let Some(root) = std::env::var_os(CHILD) {
        let root = std::path::PathBuf::from(root);
        let session = Session {
            source: "cursor".into(),
            id: ID.into(),
            project: root.to_str().unwrap().into(),
            ..Session::default()
        };
        let action = resume_command(&session).unwrap();
        // Deterministically simulate the original store disappearing after
        // command resolution, before the working directory is consumed.
        fs::remove_file(root.join(format!(".cursor/chats/workspace-hash/{ID}/store.db"))).unwrap();
        let ResumeAction::Exec { args, workdir, .. } = action else {
            panic!("expected exec")
        };
        assert_eq!(args.last().unwrap(), root.to_str().unwrap());
        assert_eq!(workdir, Some(root));
        return;
    }
    let tmp = TempDir::new().unwrap();
    let original = cli_store(&tmp, true);
    alternate_cli_store(&tmp, &original);
    // Isolate HOME in a subprocess rather than mutate this test runner's
    // environment while other tests run in parallel. No agent is executed.
    Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "resume_keeps_the_resolved_workspace_when_stores_change",
        ])
        .env(CHILD, tmp.path())
        .env("HOME", tmp.path())
        .env("PATH", "")
        .assert()
        .success();
}

fn cli_store_with(tmp: &TempDir, hash: &str, meta: serde_json::Value) -> std::path::PathBuf {
    let dir = tmp.path().join(format!(".cursor/chats/{hash}/{ID}"));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("meta.json"), meta.to_string()).unwrap();
    let conn = rusqlite::Connection::open(dir.join("store.db")).unwrap();
    conn.execute_batch("CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB);")
        .unwrap();
    dir
}

/// A fake `agent` on PATH so resume can exec without launching Cursor.
fn agent_shim(tmp: &TempDir) -> String {
    use std::os::unix::fs::PermissionsExt;
    let bin = tmp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let shim = bin.join("agent");
    fs::write(&shim, "#!/bin/sh\nprintf 'SHIM workspace: %s\\n' \"$4\"\n").unwrap();
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
    format!("{}:/usr/bin:/bin", bin.display())
}

#[test]
fn resume_explains_a_missing_workspace_instead_of_the_sidebar_hint() {
    let tmp = TempDir::new().unwrap();
    let gone = tmp.path().join("deleted-workspace");
    cli_store_with(
        &tmp,
        "h",
        json!({"schemaVersion":1, "cwd":gone, "title":"Gone workspace chat",
        "createdAtMs":1788220800000i64, "updatedAtMs":1788307200000i64, "hasConversation":true}),
    );
    command(&tmp)
        .args(["--source", "cursor"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Gone workspace chat"));
    command(&tmp)
        .args(["resume", ID])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no longer exists"))
        .stderr(predicate::str::contains(gone.to_str().unwrap()))
        .stderr(predicate::str::contains("No Agent CLI chat store").not())
        .stdout(predicate::str::contains("Cursor IDE UI").not());
}

#[test]
fn unsupported_meta_schema_warns_and_is_used_anyway() {
    let tmp = TempDir::new().unwrap();
    cli_store_with(
        &tmp,
        "h",
        json!({"schemaVersion":2, "cwd":tmp.path(), "title":"schema two",
        "createdAtMs":1788220800000i64, "updatedAtMs":1788307200000i64, "hasConversation":true}),
    );
    command(&tmp)
        .args(["--source", "cursor"])
        .assert()
        .success()
        .stdout(predicate::str::contains("schema two"))
        .stderr(predicate::str::contains("schemaVersion 2"));
    let path = agent_shim(&tmp);
    command(&tmp)
        .args(["resume", ID])
        .env("PATH", &path)
        .assert()
        .success()
        .stdout(predicate::str::contains("SHIM workspace:"));
}

#[test]
fn store_only_row_with_a_header_only_ide_composer_still_resumes() {
    let tmp = TempDir::new().unwrap();
    cli_store(&tmp, true);
    let db = tmp.path().join("cursor-user/globalStorage/state.vscdb");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("CREATE TABLE composerHeaders (composerId TEXT PRIMARY KEY, createdAt INTEGER, lastUpdatedAt INTEGER, isSubagent INTEGER, value TEXT);
        CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value BLOB);").unwrap();
    conn.execute(
        "INSERT INTO composerHeaders VALUES (?1, 1, 1, 0, '{}')",
        [ID],
    )
    .unwrap();
    drop(conn);
    let path = agent_shim(&tmp);
    command(&tmp)
        .args(["resume", ID])
        .env("PATH", &path)
        .assert()
        .success()
        .stdout(predicate::str::contains("SHIM workspace:"))
        .stdout(predicate::str::contains("IDE chats cannot be resumed").not());
}

#[test]
fn last_notes_a_skipped_newer_metadata_only_session() {
    let tmp = TempDir::new().unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    cli_store_with(
        &tmp,
        "h",
        json!({"schemaVersion":1, "cwd":tmp.path(), "title":"newest, metadata only",
        "createdAtMs":now, "updatedAtMs":now + 60_000, "hasConversation":true}),
    );
    let other = "11111111-2222-4000-8000-333333333333";
    let old = tmp.path().join(format!(
        ".cursor/projects/w/agent-transcripts/{other}/{other}.jsonl"
    ));
    fs::create_dir_all(old.parent().unwrap()).unwrap();
    fs::write(
        &old,
        "{\"role\":\"user\",\"message\":{\"content\":\"older readable transcript\"}}\n",
    )
    .unwrap();
    command(&tmp)
        .args(["--source", "cursor", "view", "--last", "--plain"])
        .assert()
        .success()
        .stdout(predicate::str::contains("older readable transcript"))
        .stderr(predicate::str::contains("metadata only"))
        .stderr(predicate::str::contains(ID));
}

#[test]
fn cli_chat_indexed_by_the_ide_still_resumes() {
    // Transcript + CLI store + an IDE composer for the same id (the IDE can
    // index CLI chats): the store wins, never the sidebar hint.
    let tmp = TempDir::new().unwrap();
    transcript(&tmp, false);
    cli_store(&tmp, true);
    let db = tmp.path().join("cursor-user/globalStorage/state.vscdb");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("CREATE TABLE composerHeaders (composerId TEXT PRIMARY KEY, createdAt INTEGER, lastUpdatedAt INTEGER, isSubagent INTEGER, value TEXT);
        CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value BLOB);").unwrap();
    conn.execute(
        "INSERT INTO composerHeaders VALUES (?1, 1, 1, 0, '{}')",
        [ID],
    )
    .unwrap();
    conn.execute("INSERT INTO cursorDiskKV VALUES (?1, ?2)", rusqlite::params![format!("bubbleId:{ID}:b"),
        json!({"type":1, "bubbleId":"b", "text":"investigate uniquecache failure", "createdAt":"2026-09-01T00:00:00Z"}).to_string()]).unwrap();
    drop(conn);
    command(&tmp)
        .assert()
        .success()
        .stdout(predicate::str::contains("cursor-ide"));
    let path = agent_shim(&tmp);
    command(&tmp)
        .args(["resume", ID])
        .env("PATH", &path)
        .assert()
        .success()
        .stdout(predicate::str::contains("SHIM workspace:"))
        .stdout(predicate::str::contains("IDE chats cannot be resumed").not());
}

#[test]
fn metadata_only_rows_match_a_timeframe_search_by_activity_time() {
    let tmp = TempDir::new().unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    cli_store_with(
        &tmp,
        "h",
        json!({"schemaVersion":1, "cwd":tmp.path(), "title":"discovery probe title",
        "createdAtMs":now - 1000, "updatedAtMs":now, "hasConversation":true}),
    );
    // The title entry carries Cursor's own activity time, like every source.
    command(&tmp)
        .args([
            "search",
            "discovery probe",
            "--deep",
            "--json",
            "--timeframe",
            "today",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(ID))
        .stdout(predicate::str::contains("\"metadata_only\": true"));
}

#[test]
fn metadata_only_message_names_the_resume_blocker() {
    let tmp = TempDir::new().unwrap();
    let gone = tmp.path().join("deleted-workspace");
    cli_store_with(
        &tmp,
        "h",
        json!({"schemaVersion":1, "cwd":gone, "title":"Gone",
        "createdAtMs":1788220800000i64, "updatedAtMs":1788307200000i64, "hasConversation":true}),
    );
    command(&tmp)
        .args(["inspect", ID])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Only metadata is available"))
        .stderr(predicate::str::contains("no longer exists"))
        .stderr(predicate::str::contains("Use `chat-history resume").not());
}

fn ide_db_with_bubble(tmp: &TempDir, with_bubble: bool) {
    let db = tmp.path().join("cursor-user/globalStorage/state.vscdb");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("CREATE TABLE composerHeaders (composerId TEXT PRIMARY KEY, createdAt INTEGER, lastUpdatedAt INTEGER, isSubagent INTEGER, value TEXT);
        CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value BLOB);").unwrap();
    conn.execute(
        "INSERT INTO composerHeaders VALUES (?1, 1, 1, 0, '{}')",
        [ID],
    )
    .unwrap();
    if with_bubble {
        conn.execute("INSERT INTO cursorDiskKV VALUES (?1, ?2)", rusqlite::params![format!("bubbleId:{ID}:b"),
            json!({"type":1, "bubbleId":"b", "text":"investigate uniquecache failure", "createdAt":"2026-09-01T00:00:00Z"}).to_string()]).unwrap();
    }
}

#[test]
fn store_only_chat_with_ide_bubbles_still_resumes() {
    // No transcript, but a CLI store and IDE bubbles: the row lists as
    // cursor-ide, and the store still wins for resume.
    let tmp = TempDir::new().unwrap();
    cli_store(&tmp, true);
    ide_db_with_bubble(&tmp, true);
    command(&tmp)
        .assert()
        .success()
        .stdout(predicate::str::contains("cursor-ide"));
    let path = agent_shim(&tmp);
    command(&tmp)
        .args(["resume", ID])
        .env("PATH", &path)
        .assert()
        .success()
        .stdout(predicate::str::contains("SHIM workspace:"));
}

#[test]
fn ide_indexed_transcript_with_an_unusable_store_gets_the_reason() {
    let tmp = TempDir::new().unwrap();
    transcript(&tmp, false);
    ide_db_with_bubble(&tmp, false);
    let gone = tmp.path().join("deleted-workspace");
    cli_store_with(
        &tmp,
        "h",
        json!({"schemaVersion":1, "cwd":gone,
        "createdAtMs":1788220800000i64, "updatedAtMs":1788307200000i64, "hasConversation":true}),
    );
    command(&tmp)
        .args(["resume", ID])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no longer exists"))
        // The reason comes first; the sidebar pointer still follows for an
        // IDE-indexed chat, since the IDE may open it.
        .stdout(predicate::str::contains("Cursor IDE UI"));
}

#[test]
fn uuid_lookup_respects_timeframe() {
    // (The JSON envelope echoes the query, so assert on the session_id field.)
    let hit = format!("\"session_id\": \"{ID}\"");
    // Timestamped today: found by id with and without the window.
    let tmp = TempDir::new().unwrap();
    transcript(&tmp, true);
    for args in [
        vec!["search", ID, "--deep", "--json"],
        vec!["search", ID, "--deep", "--json", "--timeframe", "today"],
    ] {
        command(&tmp)
            .args(args)
            .assert()
            .success()
            .stdout(predicate::str::contains(hit.as_str()));
    }
    // No message times but active today: the session's own activity time
    // satisfies the window, like the index title entry.
    let tmp = TempDir::new().unwrap();
    let path = transcript(&tmp, false);
    command(&tmp)
        .args(["search", ID, "--deep", "--json", "--timeframe", "today"])
        .assert()
        .success()
        .stdout(predicate::str::contains(hit.as_str()));
    // Last active years ago: not found by id in the window...
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_577_836_800))
        .unwrap();
    command(&tmp)
        .args(["search", ID, "--deep", "--json", "--timeframe", "today"])
        .assert()
        .success()
        .stdout(predicate::str::contains(hit.as_str()).not());
    // ...unless a recent conversation quotes the id: content search still runs.
    let other = "11111111-2222-4000-8000-333333333333";
    let recent = tmp.path().join(format!(
        ".cursor/projects/w/agent-transcripts/{other}/{other}.jsonl"
    ));
    fs::create_dir_all(recent.parent().unwrap()).unwrap();
    fs::write(
        &recent,
        json!({"role":"user", "timestamp": chrono::Utc::now().to_rfc3339(),
        "message":{"content": format!("please look at session {ID}")}})
        .to_string()
            + "\n",
    )
    .unwrap();
    command(&tmp)
        .args(["search", ID, "--deep", "--json", "--timeframe", "today"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            format!("\"session_id\": \"{other}\"").as_str(),
        ));
}

#[test]
fn empty_conversation_flag_hides_a_store_only_row_but_never_blocks_resume() {
    let tmp = TempDir::new().unwrap();
    cli_store_with(
        &tmp,
        "h",
        json!({"schemaVersion":1, "cwd":tmp.path(), "title":"flagged empty",
        "createdAtMs":1788220800000i64, "updatedAtMs":1788307200000i64, "hasConversation":false}),
    );
    // Alone, the store lists nothing (Cursor hides it too)...
    command(&tmp)
        .args(["--source", "cursor"])
        .assert()
        .success()
        .stdout(predicate::str::contains("flagged empty").not());
    // ...but a transcript proves the chat happened, and the store resumes it.
    transcript(&tmp, false);
    let path = agent_shim(&tmp);
    command(&tmp)
        .args(["resume", ID])
        .env("PATH", &path)
        .assert()
        .success()
        .stdout(predicate::str::contains("SHIM workspace:"));
}

#[test]
fn uuid_outside_the_timeframe_says_so() {
    let tmp = TempDir::new().unwrap();
    let path = transcript(&tmp, false);
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_577_836_800))
        .unwrap();
    command(&tmp)
        .args(["search", ID, "--deep", "--timeframe", "today"])
        .assert()
        .success()
        .stderr(predicate::str::contains("no activity in the --timeframe"));
    // The note is stderr, so JSON consumers get it too without corrupting stdout.
    command(&tmp)
        .args(["search", ID, "--deep", "--json", "--timeframe", "today"])
        .assert()
        .success()
        .stderr(predicate::str::contains("no activity in the --timeframe"));
}

fn slug_of(path: &Path) -> String {
    let mut slug = String::new();
    for c in path.to_string_lossy().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    slug.trim_end_matches('-').to_owned()
}

#[test]
fn cli_store_pins_the_workspace_before_the_hook_fills_it() {
    // A dotted directory name makes the slug undecodable from the filesystem,
    // so only the CLI store (by slug) or the hook (by IDE root) can supply
    // the workspace; the store's exact cwd must win over the window root.
    let tmp = TempDir::new().unwrap();
    let ws = tmp.path().join("my.app");
    fs::create_dir_all(&ws).unwrap();
    let file = tmp.path().join(format!(
        ".cursor/projects/{}/agent-transcripts/{ID}/{ID}.jsonl",
        slug_of(&ws)
    ));
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(
        &file,
        "{\"role\":\"user\",\"message\":{\"content\":\"dotted workspace chat\"}}\n",
    )
    .unwrap();
    cli_store_with(
        &tmp,
        "h",
        json!({"schemaVersion":1, "cwd":ws, "title":"Dotted workspace",
        "createdAtMs":1788220800000i64, "updatedAtMs":1788307200000i64, "hasConversation":true}),
    );
    command(&tmp)
        .arg("cursor-hook")
        .write_stdin(
            json!({"conversation_id":ID, "transcript_path":file, "workspace_roots":[tmp.path()]})
                .to_string(),
        )
        .assert()
        .success();
    command(&tmp)
        .args(["--source", "cursor", "-v"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Dotted workspace"))
        .stdout(predicate::str::contains("DIR: ~/my.app"));
}

#[test]
fn a_hook_transcript_supersedes_the_store_only_row_it_arrives_after() {
    let tmp = TempDir::new().unwrap();
    cli_store(&tmp, true);
    let file = tmp.path().join("elsewhere.jsonl");
    fs::write(
        &file,
        "{\"role\":\"user\",\"message\":{\"content\":\"registered later\"}}\n",
    )
    .unwrap();
    command(&tmp)
        .arg("cursor-hook")
        .write_stdin(json!({"conversation_id":ID, "transcript_path":file}).to_string())
        .assert()
        .success();
    command(&tmp)
        .args(["--source", "cursor"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 sessions"))
        .stdout(predicate::str::contains("CLI store discovery"))
        .stdout(predicate::str::contains("metadata only").not());
}

#[test]
fn native_cursor_timestamps_enable_timeframe_search() {
    let tmp = TempDir::new().unwrap();
    transcript(&tmp, true);
    command(&tmp)
        .args([
            "search",
            "uniquecache",
            "--deep",
            "--json",
            "--timeframe",
            "today",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("investigate uniquecache failure"));
}

#[cfg(unix)]
#[test]
fn resume_arguments_cwd_and_fallback_note_use_the_same_workspace() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = TempDir::new().unwrap();
    let original = cli_store(&tmp, true);
    let alternate = alternate_cli_store(&tmp, &original);
    let file = transcript(&tmp, false);
    // Give the existing transcript its workspace through the public hook.
    command(&tmp)
        .arg("cursor-hook")
        .write_stdin(
            json!({"conversation_id":ID, "transcript_path":file,
            "workspace_roots":[tmp.path()]})
            .to_string(),
        )
        .assert()
        .success();
    let bin = tmp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let shim = bin.join("agent");
    fs::write(&shim, "#!/bin/sh\nprintf 'SHIM workspace: %s\\n' \"$4\"\nprintf 'SHIM cwd: %s\\n' \"$(pwd -P)\"\n").unwrap();
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
    for fallback in [false, true] {
        if fallback {
            fs::remove_file(original.join("store.db")).unwrap();
        }
        let expected = if fallback {
            alternate.as_path()
        } else {
            tmp.path()
        };
        let output = command(&tmp)
            .args(["resume", ID])
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .assert()
            .success()
            .stdout(predicate::str::contains(format!(
                "SHIM workspace: {}\n",
                expected.display()
            )))
            .stdout(predicate::str::contains(format!(
                "SHIM cwd: {}\n",
                expected.canonicalize().unwrap().display()
            )));
        if fallback {
            output.stderr(predicate::str::contains(
                "Note: no resumable Agent CLI store in ~; resuming in ~/alternate workspace",
            ));
        } else {
            output.stderr(predicate::str::contains("Note:").not());
        }
    }
}

#[test]
fn paired_ide_timestamps_enable_search_without_losing_transcript_content() {
    let tmp = TempDir::new().unwrap();
    let path = transcript(&tmp, false);
    let content = fs::read(&path).unwrap();
    let db = tmp.path().join("cursor-user/globalStorage/state.vscdb");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("CREATE TABLE composerHeaders (composerId TEXT PRIMARY KEY, createdAt INTEGER, lastUpdatedAt INTEGER, isSubagent INTEGER, value TEXT);
        CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value BLOB);").unwrap();
    let now = chrono::Utc::now();
    conn.execute(
        "INSERT INTO composerHeaders VALUES (?1, ?2, ?2, 0, '{}')",
        rusqlite::params![ID, now.timestamp_millis()],
    )
    .unwrap();
    let bubble = json!({"type":1, "bubbleId":"bubble", "text":"investigate uniquecache failure", "createdAt":now.to_rfc3339()});
    conn.execute(
        "INSERT INTO cursorDiskKV VALUES (?1, ?2)",
        rusqlite::params![format!("bubbleId:{ID}:bubble"), bubble.to_string()],
    )
    .unwrap();
    drop(conn);
    command(&tmp)
        .args([
            "--source",
            "cursor",
            "search",
            "uniquecache",
            "--deep",
            "--json",
            "--timeframe",
            "today",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("investigate uniquecache failure"));
    // The same hit must score the same with and without --timeframe: recovered
    // timestamps feed recency scoring in both cases.
    let score = |args: &[&str]| -> f64 {
        let out = command(&tmp).args(args).output().unwrap().stdout;
        serde_json::from_slice::<serde_json::Value>(&out).unwrap()["results"][0]["score"]
            .as_f64()
            .unwrap()
    };
    assert_eq!(
        score(&[
            "--source",
            "cursor",
            "search",
            "uniquecache",
            "--deep",
            "--json"
        ]),
        score(&[
            "--source",
            "cursor",
            "search",
            "uniquecache",
            "--deep",
            "--json",
            "--timeframe",
            "today"
        ])
    );
    command(&tmp)
        .args(["view", ID, "--plain"])
        .assert()
        .success()
        .stdout(predicate::str::contains("investigate uniquecache failure"));
    assert_eq!(fs::read(path).unwrap(), content);
}

#[test]
fn unsupported_cursor_schema_warns_without_corrupting_json() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("cursor-user/globalStorage/state.vscdb");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch("CREATE TABLE unsupported (value TEXT);")
        .unwrap();
    drop(conn);
    transcript(&tmp, true);
    let output = command(&tmp)
        .args(["search", "uniquecache", "--deep", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(parsed["count"].as_u64().unwrap() > 0);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unsupported Cursor history database")
    );
}

#[test]
fn metadata_only_cursor_sessions_are_visible_and_cannot_export_an_empty_transcript() {
    let tmp = TempDir::new().unwrap();
    cli_store(&tmp, true);
    command(&tmp)
        .args(["--source", "cursor"])
        .assert()
        .success()
        .stdout(predicate::str::contains("CLI store discovery"))
        .stdout(predicate::str::contains("[metadata only]"));
    for verb in ["inspect", "view"] {
        command(&tmp)
            .args([verb, ID])
            .assert()
            .failure()
            .stderr(predicate::str::contains("Only metadata is available"));
        // `--last` over a list that is entirely metadata-only must not
        // claim "Session not found" for rows the listing just showed.
        command(&tmp)
            .args(["--source", "cursor", verb, "--last"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("Only metadata is available"));
    }
    let output = tmp.path().join("existing.md");
    fs::write(&output, "existing user export").unwrap();
    command(&tmp)
        .args(["export", ID, "-o"])
        .arg(&output)
        .assert()
        .failure();
    assert_eq!(fs::read_to_string(output).unwrap(), "existing user export");
}

#[test]
fn cli_metadata_does_not_duplicate_or_replace_a_readable_transcript() {
    let tmp = TempDir::new().unwrap();
    let path = transcript(&tmp, false);
    cli_store(&tmp, true);
    command(&tmp)
        .args(["--source", "cursor"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 sessions"))
        .stdout(predicate::str::contains("metadata only").not());
    command(&tmp)
        .args(["find", ID])
        .assert()
        .success()
        .stdout(predicate::str::contains(path.to_str().unwrap()));
    command(&tmp)
        .args(["view", ID, "--plain"])
        .assert()
        .success()
        .stdout(predicate::str::contains("investigate uniquecache failure"));
}

#[test]
fn metadata_without_store_cannot_launch_cursor() {
    let tmp = TempDir::new().unwrap();
    transcript(&tmp, false);
    cli_store(&tmp, false);
    command(&tmp)
        .args(["resume", ID])
        .assert()
        .failure()
        .stderr(predicate::str::contains("No Agent CLI chat store"));
}

#[test]
fn cursor_hook_is_opt_in_fail_open_and_discovers_nonstandard_paths() {
    let tmp = TempDir::new().unwrap();
    command(&tmp)
        .args(["cursor-hook"])
        .write_stdin("invalid json")
        .assert()
        .success()
        .stdout("{}\n")
        .stderr(predicate::str::contains("Invalid Cursor hook input"));
    assert!(!tmp.path().join(".cursor/skills").exists());
    assert!(!tmp.path().join(".chat-history").exists());
    let file = tmp.path().join("nonstandard.jsonl");
    let hook = json!({"conversation_id":ID, "transcript_path":file, "workspace_roots":[tmp.path()],
        "model":"legacy-model", "model_id":"hook-model", "hook_event_name":"stop", "cursor_version":"test"});
    command(&tmp)
        .args(["cursor-hook"])
        .write_stdin(hook.to_string())
        .assert()
        .success()
        .stdout("{}\n");
    // Cursor may flush after the stop hook returns.
    fs::write(
        &file,
        "{\"role\":\"user\",\"message\":{\"content\":\"nonstandard transcript discovery\"}}\n",
    )
    .unwrap();
    command(&tmp)
        .args(["--source", "cursor"])
        .assert()
        .success()
        .stdout(predicate::str::contains("nonstandard transcript discovery"));
    command(&tmp)
        .args(["inspect", ID])
        .assert()
        .success()
        .stdout(predicate::str::contains("hook-model"));
    assert!(!Path::new(&tmp.path().join(".cursor/hooks.json")).exists());
}
