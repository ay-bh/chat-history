//! The search index is disposable but long-lived: sessions come and go, and
//! SQLite only ever adds the pages they free to a free list. Compaction gives
//! the space back without the search path paying for it.

use assert_cmd::Command;
use serde_json::json;
use std::{fs, path::Path, thread, time::Duration};
use tempfile::TempDir;

const ID_A: &str = "aaaaaaaa-1111-2222-3333-aaaaaaaaaaaa";
const ID_B: &str = "bbbbbbbb-1111-2222-3333-bbbbbbbbbbbb";

fn command(tmp: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("chat-history").unwrap();
    cmd.env("HOME", tmp.path())
        .env("CLAUDE_CONFIG_DIR", tmp.path().join(".claude"))
        .env("CODEX_HOME", tmp.path().join("codex"))
        .env("CURSOR_USER_DIR", tmp.path().join("cursor-user"))
        .env("CHAT_HISTORY_CACHE_DIR", tmp.path().join("cache"))
        .env("NO_COLOR", "1");
    cmd
}

fn write_claude(root: &Path, id: &str, turns: usize, word: &str) {
    let dir = root.join(".claude/projects/-Users-test-proj");
    fs::create_dir_all(&dir).unwrap();
    let filler = format!("{word} ").repeat(400);
    let lines: Vec<String> = (0..turns)
        .map(|i| {
            let role = if i % 2 == 0 { "user" } else { "assistant" };
            json!({
                "type": role,
                "cwd": "/Users/test/proj",
                "message": {"role": role, "content": format!("{filler} turn {i}")},
                "timestamp": format!("2026-09-01T10:{:02}:00Z", i % 60),
                "uuid": format!("{id}-{i}"),
                "sessionId": id,
            })
            .to_string()
        })
        .collect();
    fs::write(dir.join(format!("{id}.jsonl")), lines.join("\n")).unwrap();
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

fn pragma(tmp: &TempDir, name: &str) -> i64 {
    let conn = rusqlite::Connection::open(tmp.path().join("cache/search-v3.db")).unwrap();
    conn.query_row(&format!("PRAGMA {name}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn maintain_gives_freed_pages_back_and_the_index_keeps_working() {
    let tmp = TempDir::new().unwrap();
    write_claude(tmp.path(), ID_A, 200, "albatross");
    write_claude(tmp.path(), ID_B, 6, "bittern");
    thread::sleep(Duration::from_millis(2100));
    assert!(search(&tmp, "albatross").contains(ID_A), "indexed");
    assert!(search(&tmp, "bittern").contains(ID_B), "indexed");

    // A large session disappears: its pages are freed, not returned.
    fs::remove_file(
        tmp.path()
            .join(format!(".claude/projects/-Users-test-proj/{ID_A}.jsonl")),
    )
    .unwrap();
    assert!(!search(&tmp, "albatross").contains(ID_A), "removed");
    assert!(pragma(&tmp, "freelist_count") > 0, "pages were freed");
    assert!(
        !tmp.path().join("cache/search-v3.db.maintained").exists(),
        "a small index is not compacted on its own"
    );

    command(&tmp)
        .args(["maintain", "--cache-dir"])
        .arg(tmp.path().join("cache"))
        .assert()
        .success();
    assert_eq!(pragma(&tmp, "freelist_count"), 0, "space given back");
    assert!(
        tmp.path().join("cache/search-v3.db.maintained").exists(),
        "the run is recorded so it is not repeated for a day"
    );
    assert!(search(&tmp, "bittern").contains(ID_B), "still searchable");
    let conn = rusqlite::Connection::open(tmp.path().join("cache/search-v3.db")).unwrap();
    conn.execute(
        "INSERT INTO passages_fts(passages_fts) VALUES ('integrity-check')",
        [],
    )
    .unwrap();
}
