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
    for args in [
        vec![
            "search",
            "uniquecache",
            "--deep",
            "--json",
            "--timeframe",
            "today",
        ],
        vec![
            "--source",
            "cursor",
            "search",
            "uniquecache",
            "--deep",
            "--json",
            "--timeframe",
            "today",
        ],
    ] {
        command(&tmp)
            .args(args)
            .assert()
            .success()
            .stdout(predicate::str::contains("investigate uniquecache failure"));
    }
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
