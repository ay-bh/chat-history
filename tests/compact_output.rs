//! Output sized for agents: `search --compact` prints one line per hit, and
//! `view --json` gives scripts the same bounded slice as data.
use assert_cmd::Command;
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use tempfile::TempDir;

const ID_A: &str = "aaaaaaaa-1111-2222-3333-aaaaaaaaaaaa";
const ID_B: &str = "bbbbbbbb-1111-2222-3333-bbbbbbbbbbbb";

fn write_claude(root: &Path, id: &str, records: Vec<(&str, Value)>) {
    let dir = root.join(".claude/projects/-Users-test-proj");
    fs::create_dir_all(&dir).unwrap();
    let lines: Vec<String> = records
        .into_iter()
        .enumerate()
        .map(|(i, (role, content))| {
            json!({
                "type": role,
                "cwd": "/Users/test/proj",
                "message": {"role": role, "content": content},
                "timestamp": format!("2026-09-01T10:{i:02}:00Z"),
                "uuid": format!("{id}-{i}"),
                "sessionId": id,
            })
            .to_string()
        })
        .collect();
    fs::write(dir.join(format!("{id}.jsonl")), lines.join("\n")).unwrap();
}

fn fixture() -> TempDir {
    let tmp = TempDir::new().unwrap();
    write_claude(
        tmp.path(),
        ID_A,
        vec![
            ("user", json!("why is the kestrel deploy failing")),
            (
                "assistant",
                json!([
                    {"type": "text", "text": "Checking the kestrel logs."},
                    {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "cat kestrel.log"}},
                    {"type": "tool_use", "id": "t2", "name": "Bash", "input": {"command": "cat kestrel.err"}}
                ]),
            ),
            (
                "user",
                json!([{"type": "tool_result", "tool_use_id": "t1", "content": "kestrel: connection refused"}]),
            ),
            (
                "assistant",
                json!("The kestrel deploy fails because the database\nrefuses connections."),
            ),
            ("user", json!("fix the kestrel config")),
            (
                "assistant",
                json!("Fixed the kestrel config: the port was wrong."),
            ),
        ],
    );
    write_claude(
        tmp.path(),
        ID_B,
        vec![
            ("user", json!("plan the osprey launch")),
            (
                "assistant",
                json!("The osprey launch goes out in two waves."),
            ),
        ],
    );
    tmp
}

fn cmd(tmp: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("chat-history").unwrap();
    cmd.env("HOME", tmp.path())
        .env("CLAUDE_CONFIG_DIR", tmp.path().join(".claude"))
        .env("CURSOR_USER_DIR", tmp.path().join("no-cursor"))
        .env("CHAT_HISTORY_CACHE_DIR", tmp.path().join("cache"))
        .env("CODEX_HOME", tmp.path().join(".codex"))
        .env("NO_COLOR", "1")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("CODEX_THREAD_ID")
        .env_remove("CURSOR_CONVERSATION_ID");
    cmd
}

fn stdout(tmp: &TempDir, args: &[&str]) -> String {
    let out = cmd(tmp).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn compact_search_is_one_line_per_hit() {
    let tmp = fixture();
    let out = stdout(&tmp, &["search", "database", "--compact"]);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 1, "one conversation matched:\n{out}");
    let line = lines[0];
    let fields: Vec<&str> = line.splitn(6, "  ").collect();
    assert_eq!(fields[0], "aaaaaaaa", "{line}");
    assert_eq!(fields[1].len(), 10, "a date: {line}");
    assert_eq!(&fields[2..4], ["claude", "/Users/test/proj"], "{line}");
    assert!(fields[4].starts_with('#'), "the hit's ordinal: {line}");
    assert!(
        line.contains("why is the kestrel deploy failing"),
        "title: {line}"
    );
    assert!(
        line.contains(
            "Assistant: The kestrel deploy fails because the database refuses connections."
        ),
        "{line}"
    );
    assert!(!line.contains("★"), "no scores: {line}");
}

#[test]
fn a_title_hit_is_not_repeated_as_its_excerpt() {
    let tmp = fixture();
    let out = stdout(&tmp, &["search", "osprey launch", "--compact"]);
    let line = out.lines().next().unwrap();
    assert!(line.contains("  -  plan the osprey launch"), "{line}");
    assert_eq!(line.matches("plan the osprey launch").count(), 1, "{line}");
}

#[test]
fn compact_search_lists_the_other_matches_by_ordinal() {
    let tmp = fixture();
    let json: Value =
        serde_json::from_str(&stdout(&tmp, &["search", "kestrel", "--json"])).unwrap();
    let hit = &json["results"][0];
    let others: Vec<String> = hit["additional_matches"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["ordinal"].as_u64())
        .map(|o| format!("#{o}"))
        .collect();
    assert!(!others.is_empty(), "fixture has further matches: {json}");
    let line = stdout(&tmp, &["search", "kestrel", "--compact"]);
    assert!(
        line.trim_end()
            .ends_with(&format!("(also {})", others.join(" "))),
        "{line}"
    );
}

#[test]
fn compact_search_prints_nothing_when_nothing_matches() {
    let tmp = fixture();
    let out = cmd(&tmp)
        .args(["search", "albatross", "--compact"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("No results"));
}

#[test]
fn compact_and_json_cannot_be_combined() {
    let tmp = fixture();
    let out = cmd(&tmp)
        .args(["search", "kestrel", "--compact", "--json"])
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn search_json_lists_each_tool_once() {
    let tmp = fixture();
    let json: Value = serde_json::from_str(&stdout(
        &tmp,
        &["search", "kestrel logs", "--json", "--group-by", "message"],
    ))
    .unwrap();
    let hit = json["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["snippet"].as_str().unwrap_or("").contains("Checking"))
        .expect("the narration message is a hit");
    assert_eq!(hit["tools"], json!(["Bash"]), "{hit}");
}

#[test]
fn view_json_gives_messages_as_data() {
    let tmp = fixture();
    let json: Value = serde_json::from_str(&stdout(&tmp, &["view", "aaaaaaaa", "--json"])).unwrap();
    assert_eq!(json["session_id"], ID_A);
    let messages = json["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 6, "{json}");
    assert_eq!(messages[0]["ordinal"], 0);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"], "why is the kestrel deploy failing");
    assert_eq!(messages[0]["timestamp"], "2026-09-01T10:00:00Z");
    assert_eq!(messages[2]["role"], "tool");
    assert_eq!(messages[1]["tools"], json!(["Bash", "Bash"]));
}

#[test]
fn view_json_honours_the_bounded_selection() {
    let tmp = fixture();
    let json: Value = serde_json::from_str(&stdout(
        &tmp,
        &[
            "view",
            "aaaaaaaa",
            "--json",
            "--around",
            "3",
            "-C",
            "1",
            "--role",
            "user,assistant",
        ],
    ))
    .unwrap();
    let ordinals: Vec<u64> = json["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["ordinal"].as_u64().unwrap())
        .collect();
    assert_eq!(ordinals, [1, 3, 4], "{json}");
    let clipped: Value = serde_json::from_str(&stdout(
        &tmp,
        &[
            "view",
            "aaaaaaaa",
            "--json",
            "--max-chars",
            "10",
            "--head",
            "1",
        ],
    ))
    .unwrap();
    let content = clipped["messages"][0]["content"].as_str().unwrap();
    assert!(content.chars().count() < 40, "{content}");
    assert_eq!(clipped["messages"][0]["truncated"], true);
}

#[test]
fn view_json_and_plain_cannot_be_combined() {
    let tmp = fixture();
    let out = cmd(&tmp)
        .args(["view", "aaaaaaaa", "--json", "--plain"])
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn view_json_with_no_grep_match_is_still_json() {
    let tmp = fixture();
    let out = stdout(&tmp, &["view", "aaaaaaaa", "--json", "--grep", "albatross"]);
    let json: Value = serde_json::from_str(&out).expect("valid JSON");
    assert_eq!(json["messages"], json!([]), "{json}");
    assert_eq!(json["message_count"], 6);
}

#[test]
fn a_long_first_prompt_hit_keeps_what_the_title_cuts() {
    let tmp = fixture();
    let prompt = "plan the heron migration across every regional warehouse and \
                  its loading docks, then check the zanzibar route";
    write_claude(
        tmp.path(),
        "cccccccc-1111-2222-3333-cccccccccccc",
        vec![("user", json!(prompt)), ("assistant", json!("Planned it."))],
    );
    let out = stdout(&tmp, &["search", "zanzibar", "--compact"]);
    let line = out.lines().find(|l| l.starts_with("cccccccc")).unwrap();
    assert!(line.contains("zanzibar"), "{line}");
}

#[test]
fn compact_marks_metadata_only_sessions() {
    let tmp = fixture();
    let id = "dddddddd-1111-2222-3333-dddddddddddd";
    let dir = tmp
        .path()
        .join(format!(".cursor/chats/workspace-hash/{id}"));
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("meta.json"),
        json!({"schemaVersion": 1, "cwd": "/Users/test/proj",
               "title": "Kingfisher store notes", "createdAtMs": 1788220800000i64,
               "updatedAtMs": 1788307200000i64, "hasConversation": true})
        .to_string(),
    )
    .unwrap();
    rusqlite::Connection::open(dir.join("store.db"))
        .unwrap()
        .execute_batch("CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB);")
        .unwrap();
    let out = stdout(&tmp, &["search", "kingfisher", "--compact"]);
    let line = out.lines().find(|l| l.starts_with("dddddddd")).expect(&out);
    assert!(line.contains("[metadata only]"), "{line}");
}
