//! Cursor keeps every conversation in one shared database that it rewrites
//! every few seconds while open. A search must not re-hash every conversation's
//! text each time that happens: a conversation whose cheap shape (row count,
//! newest row, header) is unchanged keeps the digest it already has.

use assert_cmd::Command;
use serde_json::{Value, json};
use std::{fs, path::PathBuf, thread, time::Duration};
use tempfile::TempDir;

const ID: &str = "fedc1234-0000-4000-8000-00000000c0de";
const OLD_MS: i64 = 1_735_689_600_000; // 2025-01-01, long before any test runs
const QUIET: Duration = Duration::from_millis(2100);

fn command(tmp: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("chat-history").unwrap();
    cmd.env("HOME", tmp.path())
        .env("CLAUDE_CONFIG_DIR", tmp.path().join("claude"))
        .env("CODEX_HOME", tmp.path().join("codex"))
        .env("CURSOR_USER_DIR", tmp.path().join("cursor-user"))
        .env("CHAT_HISTORY_CACHE_DIR", tmp.path().join("cache"))
        .env("NO_COLOR", "1");
    cmd
}

fn db_path(tmp: &TempDir) -> PathBuf {
    tmp.path().join("cursor-user/globalStorage/state.vscdb")
}

/// One IDE conversation with one user bubble, last updated at `updated_ms`.
fn ide_db(tmp: &TempDir, text: &str, updated_ms: i64) -> rusqlite::Connection {
    let db = db_path(tmp);
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE composerHeaders (composerId TEXT PRIMARY KEY, createdAt INTEGER, lastUpdatedAt INTEGER, isSubagent INTEGER, value TEXT);
         CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value BLOB);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO composerHeaders VALUES (?1, ?2, ?3, 0, '{}')",
        rusqlite::params![ID, OLD_MS, updated_ms],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO cursorDiskKV VALUES (?1, ?2)",
        rusqlite::params![
            format!("bubbleId:{ID}:b1"),
            json!({"type":1, "bubbleId":"b1", "text":text, "createdAt":"2025-01-01T00:00:00Z"})
                .to_string()
        ],
    )
    .unwrap();
    conn
}

fn search(tmp: &TempDir, query: &str) -> String {
    let out = command(tmp)
        .args(["search", query, "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn stored_stamp(tmp: &TempDir) -> Value {
    let conn = rusqlite::Connection::open(tmp.path().join("cache/search-v3.db")).unwrap();
    let raw: String = conn
        .query_row(
            "SELECT fingerprint FROM sessions WHERE key LIKE ?1",
            [format!("[\"cursor-ide\",\"{ID}\"%")],
            |r| r.get(0),
        )
        .unwrap();
    serde_json::from_str(&raw).unwrap()
}

fn set_verified(tmp: &TempDir, verified_ms: u64) {
    let conn = rusqlite::Connection::open(tmp.path().join("cache/search-v3.db")).unwrap();
    conn.execute(
        "UPDATE sessions SET fingerprint = json_set(fingerprint, '$.cursor.verified', ?2) WHERE key LIKE ?1",
        rusqlite::params![format!("[\"cursor-ide\",\"{ID}\"%"), verified_ms],
    )
    .unwrap();
}

#[test]
fn an_unrelated_change_to_the_cursor_database_keeps_a_conversation_digest() {
    let tmp = TempDir::new().unwrap();
    let conn = ide_db(&tmp, "investigate uniquefirst failure", OLD_MS);
    thread::sleep(QUIET);
    assert!(search(&tmp, "uniquefirst").contains(ID), "indexed");
    let before = stored_stamp(&tmp);
    let verified = before["cursor"]["verified"]
        .as_u64()
        .expect("the stamp records when the digest was hashed from content");
    assert!(before["cursor"]["shape"].is_string(), "{before}");

    // Cursor writes unrelated state into the same database all the time.
    conn.execute(
        "INSERT INTO cursorDiskKV VALUES ('workbench.panel.state', 'unrelated')",
        [],
    )
    .unwrap();
    thread::sleep(QUIET);
    assert!(search(&tmp, "uniquefirst").contains(ID));
    let after = stored_stamp(&tmp);
    assert_ne!(
        after["cursor"]["observed"], before["cursor"]["observed"],
        "the database stamp moved on"
    );
    assert_eq!(
        after["cursor"]["verified"].as_u64(),
        Some(verified),
        "an unchanged conversation keeps its digest instead of being hashed again: {after}"
    );
}

#[test]
fn a_new_bubble_in_a_quiet_conversation_is_indexed_on_the_next_search() {
    let tmp = TempDir::new().unwrap();
    let conn = ide_db(&tmp, "investigate uniquefirst failure", OLD_MS);
    thread::sleep(QUIET);
    assert!(search(&tmp, "uniquefirst").contains(ID), "indexed");
    conn.execute(
        "INSERT INTO cursorDiskKV VALUES (?1, ?2)",
        rusqlite::params![
            format!("bubbleId:{ID}:b2"),
            json!({"type":1, "bubbleId":"b2", "text":"then uniquesecond question", "createdAt":"2025-01-02T00:00:00Z"}).to_string()
        ],
    )
    .unwrap();
    thread::sleep(QUIET);
    assert!(
        search(&tmp, "uniquesecond").contains(ID),
        "new bubble found"
    );
}

#[test]
fn an_active_conversation_is_always_hashed_from_its_text() {
    let tmp = TempDir::new().unwrap();
    let now_ms = chrono::Utc::now().timestamp_millis();
    let conn = ide_db(&tmp, "investigate uniquefifth failure", now_ms);
    thread::sleep(QUIET);
    assert!(search(&tmp, "uniquefifth").contains(ID), "indexed");
    // Same byte length, same row, same header: the shape cannot tell.
    conn.execute(
        "UPDATE cursorDiskKV SET value = replace(value, 'uniquefifth', 'uniquesixth') WHERE key = ?1",
        [format!("bubbleId:{ID}:b1")],
    )
    .unwrap();
    thread::sleep(QUIET);
    assert!(
        search(&tmp, "uniquesixth").contains(ID),
        "a conversation updated recently is re-read every time its database changes"
    );
}

#[test]
fn a_shape_preserving_edit_to_a_cold_conversation_is_caught_by_the_periodic_check() {
    let tmp = TempDir::new().unwrap();
    let conn = ide_db(&tmp, "investigate uniquethird failure", OLD_MS);
    thread::sleep(QUIET);
    assert!(search(&tmp, "uniquethird").contains(ID), "indexed");
    conn.execute(
        "UPDATE cursorDiskKV SET value = replace(value, 'uniquethird', 'uniquefour2') WHERE key = ?1",
        [format!("bubbleId:{ID}:b1")],
    )
    .unwrap();
    // Pretend the digest was last hashed from content long ago.
    set_verified(&tmp, 1);
    thread::sleep(QUIET);
    assert!(
        search(&tmp, "uniquefour2").contains(ID),
        "a stale digest is re-hashed once its periodic check is due"
    );
    assert!(
        stored_stamp(&tmp)["cursor"]["verified"].as_u64().unwrap() > 1,
        "the check time moves forward"
    );
}
