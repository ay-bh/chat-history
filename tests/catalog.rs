use assert_cmd::Command;
use rusqlite::{Connection, params};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
};
use tempfile::TempDir;

const ID: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

fn command(tmp: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("chat-history").unwrap();
    cmd.env("HOME", tmp.path())
        .env("CLAUDE_CONFIG_DIR", tmp.path().join("claude"))
        .env("CODEX_HOME", tmp.path().join("codex"))
        .env("CURSOR_USER_DIR", tmp.path().join("cursor-user"))
        .env("CHAT_HISTORY_CACHE_DIR", tmp.path().join("cache"))
        .env_remove("CHAT_HISTORY_NO_CACHE")
        .env("NO_COLOR", "1");
    cmd
}

fn output(tmp: &TempDir, args: &[&str], cached: bool) -> Vec<u8> {
    let mut cmd = command(tmp);
    cmd.args(args);
    if !cached {
        cmd.env("CHAT_HISTORY_NO_CACHE", "1");
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn same_as_uncached(tmp: &TempDir, args: &[&str]) -> String {
    let expected = output(tmp, args, false);
    // Fresh fixtures deliberately bypass the racy-stat guard. Let their
    // timestamps settle before checking persistent reuse in later processes.
    std::thread::sleep(std::time::Duration::from_millis(2100));
    assert_eq!(output(tmp, args, true), expected);
    let warm = output(tmp, args, true);
    assert_eq!(warm, expected);
    String::from_utf8(warm).unwrap()
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn claude(tmp: &TempDir, project: &str, title: &str) -> PathBuf {
    let path = tmp
        .path()
        .join(format!("claude/projects/{project}/{ID}.jsonl"));
    let user = json!({"type":"user", "cwd":tmp.path(), "gitBranch":"main",
        "message":{"role":"user", "content":"Investigate the metadata cache"}});
    let assistant = json!({"type":"assistant", "message":{"role":"assistant",
        "content":"TRANSCRIPT_BODY_MUST_NOT_BE_CACHED"}});
    let title = json!({"type":"ai-title", "aiTitle":title});
    write(&path, &format!("{user}\n{assistant}\n{title}\n"));
    path
}

#[test]
fn catalog_tracks_edits_additions_deletions_and_duplicate_paths_across_processes() {
    let tmp = TempDir::new().unwrap();
    let path = claude(&tmp, "one", "Original title");
    assert!(same_as_uncached(&tmp, &["--source", "claude", "-v"]).contains("Original title"));
    claude(&tmp, "one", "Changed title");
    let out = same_as_uncached(&tmp, &["--source", "claude", "-v"]);
    assert!(out.contains("Changed title") && !out.contains("Original title"));
    let copy = claude(&tmp, "two", "Second copy");
    let out = same_as_uncached(&tmp, &["--source", "claude", "-v"]);
    assert!(out.contains("2 sessions") && out.contains("Second copy"));
    fs::remove_file(&path).unwrap();
    let out = same_as_uncached(&tmp, &["--source", "claude", "-v"]);
    assert!(out.contains("1 sessions") && !out.contains("Changed title"));
    assert_eq!(
        same_as_uncached(&tmp, &["find", ID]).trim(),
        copy.to_str().unwrap()
    );
    assert!(
        same_as_uncached(&tmp, &["view", ID, "--plain"])
            .contains("TRANSCRIPT_BODY_MUST_NOT_BE_CACHED")
    );
    let cache = Connection::open(tmp.path().join("cache/catalog-v1.db")).unwrap();
    let entries: String = cache
        .query_row("SELECT group_concat(value) FROM metadata", [], |r| r.get(0))
        .unwrap();
    assert!(!entries.contains("TRANSCRIPT_BODY_MUST_NOT_BE_CACHED"));
}

#[test]
fn catalog_tracks_codex_prompts_and_configuration_roots() {
    let tmp = TempDir::new().unwrap();
    let path = tmp
        .path()
        .join("codex/sessions/2026/09/12/rollout-test.jsonl");
    let meta = json!({"type":"session_meta", "payload":{"id":ID,
        "timestamp":"2026-09-12T00:00:00Z", "cwd":tmp.path()}});
    let user = |text: &str| {
        json!({"type":"response_item", "payload":{
        "type":"message", "role":"user", "content":[{"type":"input_text", "text":text}]}})
    };
    write(
        &path,
        &format!("{meta}\n{}\n", user("Original Codex prompt")),
    );
    assert!(same_as_uncached(&tmp, &["--source", "codex"]).contains("Original Codex prompt"));
    write(
        &path,
        &format!("{meta}\n{}\n", user("Updated Codex prompt")),
    );
    assert!(same_as_uncached(&tmp, &["--source", "codex"]).contains("Updated Codex prompt"));
    let out = command(&tmp)
        .env("CODEX_HOME", tmp.path().join("another-profile"))
        .args(["--source", "codex"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(!String::from_utf8_lossy(&out.stdout).contains("Updated Codex prompt"));
    let cache = Connection::open(tmp.path().join("cache/catalog-v1.db")).unwrap();
    let retained: i64 = cache
        .query_row(
            "SELECT count(*) FROM metadata WHERE source='codex'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        retained, 1,
        "an empty alternate profile evicted the original profile"
    );
    assert!(same_as_uncached(&tmp, &["--source", "codex"]).contains("Updated Codex prompt"));
}

#[test]
fn ide_wal_updates_and_deletions_refresh_cached_listing() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("cursor-user/globalStorage/state.vscdb");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
        CREATE TABLE composerHeaders(composerId TEXT PRIMARY KEY, value TEXT);
        CREATE TABLE cursorDiskKV(key TEXT PRIMARY KEY, value TEXT);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO composerHeaders VALUES (?1, ?2)",
        params![
            ID,
            json!({"name":"Original IDE title", "createdAt":1789171200000i64}).to_string()
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO cursorDiskKV VALUES (?1, ?2)",
        params![
            format!("bubbleId:{ID}:one"),
            json!({"type":1, "text":"IDE user message"}).to_string()
        ],
    )
    .unwrap();
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    let modified = fs::metadata(&db).unwrap().modified().unwrap();
    assert!(same_as_uncached(&tmp, &["--source", "cursor-ide"]).contains("Original IDE title"));
    conn.execute(
        "UPDATE composerHeaders SET value=?1",
        [json!({"name":"Updated IDE title"}).to_string()],
    )
    .unwrap();
    assert_eq!(fs::metadata(&db).unwrap().modified().unwrap(), modified);
    let out = same_as_uncached(&tmp, &["--source", "cursor-ide"]);
    assert!(out.contains("Updated IDE title") && !out.contains("Original IDE title"));
    conn.execute_batch("DELETE FROM composerHeaders; DELETE FROM cursorDiskKV")
        .unwrap();
    assert!(!same_as_uncached(&tmp, &["--source", "cursor-ide"]).contains("Updated IDE title"));
}

#[test]
fn cached_cursor_metadata_keeps_store_and_workspace_checks_live() {
    let tmp = TempDir::new().unwrap();
    let first_workspace = tmp.path().join("first-workspace");
    let second_workspace = tmp.path().join("second-workspace");
    fs::create_dir(&first_workspace).unwrap();
    fs::create_dir(&second_workspace).unwrap();
    let mut stores = Vec::new();
    for (hash, workspace, updated, title) in [
        ("first", &first_workspace, 200, "Newest copy"),
        ("second", &second_workspace, 100, "Older copy"),
    ] {
        let dir = tmp.path().join(format!(".cursor/chats/{hash}/{ID}"));
        write(
            &dir.join("meta.json"),
            &json!({"schemaVersion":1,"cwd":workspace,
            "title":title,"updatedAtMs":updated,"hasConversation":true})
            .to_string(),
        );
        write(&dir.join("store.db"), "nonempty store");
        stores.push(dir);
    }
    assert!(same_as_uncached(&tmp, &["--source", "cursor"]).contains("Newest copy"));
    fs::remove_dir(&first_workspace).unwrap();
    let out = same_as_uncached(&tmp, &["--source", "cursor"]);
    assert!(out.contains("Older copy") && !out.contains("Newest copy"));
    fs::remove_file(stores[1].join("store.db")).unwrap();
    assert!(same_as_uncached(&tmp, &["--source", "cursor"]).contains("Newest copy"));
    // A schema warning must not disappear on a cache hit.
    write(
        &stores[0].join("meta.json"),
        &json!({"schemaVersion":999,"cwd":first_workspace,
        "title":"New schema title", "hasConversation":true})
        .to_string(),
    );
    for _ in 0..2 {
        let out = command(&tmp).args(["--source", "cursor"]).output().unwrap();
        assert!(out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("schemaVersion 999"));
    }
}

#[test]
fn cached_hook_registry_discovers_late_transcripts_and_later_edits() {
    let tmp = TempDir::new().unwrap();
    let transcript = tmp.path().join("outside/registered.jsonl");
    command(&tmp)
        .arg("cursor-hook")
        .write_stdin(
            json!({"conversation_id":ID,
        "transcript_path":transcript,"workspace_roots":[tmp.path()]})
            .to_string(),
        )
        .assert()
        .success();
    assert!(!same_as_uncached(&tmp, &["--source", "cursor"]).contains("Late transcript"));
    write(
        &transcript,
        &json!({"role":"user","message":{"content":"Late transcript"}}).to_string(),
    );
    assert!(same_as_uncached(&tmp, &["--source", "cursor"]).contains("Late transcript"));
    write(
        &transcript,
        &json!({"role":"user","message":{"content":"Edited transcript"}}).to_string(),
    );
    assert!(same_as_uncached(&tmp, &["--source", "cursor"]).contains("Edited transcript"));
    fs::remove_file(&transcript).unwrap();
    assert!(!same_as_uncached(&tmp, &["--source", "cursor"]).contains("Edited transcript"));
}

#[test]
fn disabled_cache_does_not_create_files_and_corrupt_cache_does_not_break_commands() {
    let tmp = TempDir::new().unwrap();
    claude(&tmp, "one", "Readable history");
    assert!(
        String::from_utf8_lossy(&output(&tmp, &["--source", "claude"], false))
            .contains("Readable history")
    );
    assert!(!tmp.path().join("cache").exists());
    write(&tmp.path().join("cache/catalog-v1.db"), "corrupt cache");
    assert!(same_as_uncached(&tmp, &["--source", "claude"]).contains("Readable history"));
}

#[test]
fn legacy_index_changes_refresh_without_caching_unbounded_prompts() {
    let tmp = TempDir::new().unwrap();
    let transcript = claude(&tmp, "one", "Transcript title");
    let index = transcript.parent().unwrap().join("sessions-index.json");
    let entry = |summary: &str| {
        json!({"entries":[{"sessionId":ID,
        "fullPath":transcript, "summary":summary,
        "firstPrompt":format!("{}PRIVATE_TAIL", "x".repeat(300)),
        "created":"2026-09-12T00:00:00Z", "projectPath":tmp.path()}]})
    };
    write(&index, &entry("Legacy title").to_string());
    assert!(same_as_uncached(&tmp, &["--source", "claude"]).contains("Legacy title"));
    write(&index, &entry("Edited legacy title").to_string());
    assert!(same_as_uncached(&tmp, &["--source", "claude"]).contains("Edited legacy title"));
    let cache = Connection::open(tmp.path().join("cache/catalog-v1.db")).unwrap();
    let entries: String = cache
        .query_row("SELECT group_concat(value) FROM metadata", [], |r| r.get(0))
        .unwrap();
    assert!(!entries.contains("PRIVATE_TAIL"));
    let sources: String = cache
        .query_row(
            "SELECT group_concat(DISTINCT source) FROM metadata",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(sources, "claude");
    fs::remove_file(&index).unwrap();
    assert!(same_as_uncached(&tmp, &["--source", "claude"]).contains("Transcript title"));
}

#[test]
fn concurrent_processes_share_the_cache_without_changing_output() {
    let tmp = TempDir::new().unwrap();
    claude(&tmp, "one", "Concurrent discovery");
    let expected = output(&tmp, &["--source", "claude"], false);
    std::thread::scope(|scope| {
        let jobs: Vec<_> = (0..4)
            .map(|_| scope.spawn(|| output(&tmp, &["--source", "claude"], true)))
            .collect();
        for job in jobs {
            assert_eq!(job.join().unwrap(), expected);
        }
    });
    assert_eq!(output(&tmp, &["--source", "claude"], true), expected);
    let cache = Connection::open(tmp.path().join("cache/catalog-v1.db")).unwrap();
    assert_eq!(
        cache
            .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
}
