//! A Claude Code transcript only ever grows. Once it is indexed, the next sync
//! resumes from the byte where the last parse stopped instead of reading the
//! file from the start, as long as the bytes before that point are the ones
//! it saw: the head of the file and the bytes just before the offset are
//! fingerprinted. Anything else (a rewrite, a shrink) parses from the start.

use assert_cmd::Command;
use serde_json::{Value, json};
use std::{fs, io::Write, path::PathBuf, thread, time::Duration};
use tempfile::TempDir;

const ID: &str = "aaaaaaaa-1111-2222-3333-aaaaaaaaaaaa";
const QUIET: Duration = Duration::from_millis(2100);

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

fn record(i: usize, role: &str, content: Value) -> String {
    json!({
        "type": role,
        "cwd": "/Users/test/proj",
        "message": {"role": role, "content": content},
        "timestamp": format!("2026-09-01T10:{:02}:00Z", i % 60),
        "uuid": format!("{ID}-{i}"),
        "sessionId": ID,
    })
    .to_string()
}

fn tool_use(id: &str, command: &str) -> Value {
    json!([{"type": "tool_use", "id": id, "name": "Bash", "input": {"command": command}}])
}

fn tool_result(id: &str, text: &str) -> Value {
    json!([{"type": "tool_result", "tool_use_id": id, "content": text}])
}

fn transcript(tmp: &TempDir) -> PathBuf {
    let dir = tmp.path().join(".claude/projects/-Users-test-proj");
    fs::create_dir_all(&dir).unwrap();
    dir.join(format!("{ID}.jsonl"))
}

/// Writes the records as JSON Lines, each terminated by a newline.
fn write(tmp: &TempDir, records: &[(&str, Value)]) -> PathBuf {
    let path = transcript(tmp);
    let mut text = String::new();
    for (i, (role, content)) in records.iter().enumerate() {
        text.push_str(&record(i, role, content.clone()));
        text.push('\n');
    }
    fs::write(&path, text).unwrap();
    path
}

fn append(path: &PathBuf, text: &str) {
    let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(text.as_bytes()).unwrap();
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

fn stamp(tmp: &TempDir) -> Value {
    let conn = rusqlite::Connection::open(tmp.path().join("cache/search-v3.db")).unwrap();
    let raw: String = conn
        .query_row(
            "SELECT fingerprint FROM sessions WHERE key LIKE ?1",
            [format!("[\"claude\",\"{ID}\"%")],
            |r| r.get(0),
        )
        .unwrap();
    serde_json::from_str(&raw).unwrap()
}

fn stored_ids(tmp: &TempDir) -> Vec<i64> {
    let conn = rusqlite::Connection::open(tmp.path().join("cache/search-v3.db")).unwrap();
    let mut stmt = conn
        .prepare("SELECT id FROM messages WHERE session_key LIKE ?1 ORDER BY ordinal")
        .unwrap();
    stmt.query_map([format!("%{ID}%")], |r| r.get(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

#[test]
fn an_appended_transcript_is_parsed_from_where_the_last_parse_stopped() {
    let tmp = TempDir::new().unwrap();
    let path = write(
        &tmp,
        &[
            ("user", json!("find the flamingo notes")),
            ("assistant", json!("The flamingo notes are in docs.")),
        ],
    );
    thread::sleep(QUIET);
    assert!(search(&tmp, "flamingo").contains(ID), "indexed");
    let first = stamp(&tmp);
    let len = fs::metadata(&path).unwrap().len();
    assert_eq!(
        first["tail"]["offset"].as_u64(),
        Some(len),
        "the stamp remembers where the parse stopped: {first}"
    );
    let ids = stored_ids(&tmp);

    append(
        &path,
        &format!("{}\n", record(2, "user", json!("now the ibisquill deploy"))),
    );
    append(
        &path,
        &format!(
            "{}\n",
            record(3, "assistant", json!("Deploying ibisquill."))
        ),
    );
    thread::sleep(QUIET);
    assert!(search(&tmp, "ibisquill").contains(ID), "new messages found");
    let second = stamp(&tmp);
    assert_eq!(
        second["tail"]["offset"].as_u64(),
        Some(fs::metadata(&path).unwrap().len()),
        "the offset moved to the new end"
    );
    let after = stored_ids(&tmp);
    assert_eq!(&after[..ids.len()], &ids[..], "earlier rows kept their ids");
    assert_eq!(after.len(), ids.len() + 2, "two messages appended");
}

#[test]
fn a_torn_last_line_is_retried_once_it_is_complete() {
    let tmp = TempDir::new().unwrap();
    let path = write(&tmp, &[("user", json!("the pelican question"))]);
    thread::sleep(QUIET);
    assert!(search(&tmp, "pelican").contains(ID), "indexed");
    let whole = record(1, "assistant", json!("The heronbeak answer."));
    let cut = whole.len() - 12;
    append(&path, &whole[..cut]);
    thread::sleep(QUIET);
    assert!(
        !search(&tmp, "heronbeak").contains(ID),
        "a half-written line is not indexed"
    );
    let before = stamp(&tmp)["tail"]["offset"].as_u64().unwrap();
    assert!(
        before < fs::metadata(&path).unwrap().len(),
        "the offset stays before the torn line"
    );
    append(&path, &format!("{}\n", &whole[cut..]));
    thread::sleep(QUIET);
    assert!(
        search(&tmp, "heronbeak").contains(ID),
        "the completed line is indexed"
    );
}

#[test]
fn a_rewritten_transcript_is_parsed_from_the_start_again() {
    let tmp = TempDir::new().unwrap();
    let path = write(
        &tmp,
        &[
            ("user", json!("about the albatross")),
            ("assistant", json!("Albatross facts.")),
        ],
    );
    thread::sleep(QUIET);
    assert!(search(&tmp, "albatross").contains(ID), "indexed");
    // Claude Code can rewrite a transcript (compaction, an edited line).
    let rewritten = format!(
        "{}\n{}\n",
        record(0, "user", json!("about the cormorant")),
        record(1, "assistant", json!("Cormorant facts."))
    );
    fs::write(&path, rewritten).unwrap();
    thread::sleep(QUIET);
    assert!(search(&tmp, "cormorant").contains(ID), "new content found");
    assert!(
        !search(&tmp, "albatross").contains(ID),
        "the old content is gone"
    );
    assert_eq!(
        stamp(&tmp)["tail"]["offset"].as_u64(),
        Some(fs::metadata(&path).unwrap().len())
    );
}

#[test]
fn a_tool_result_for_a_history_command_before_the_offset_stays_out_of_the_index() {
    // The parser marks the output of chat-history's own commands so it is not
    // indexed (it echoes other sessions). The call may be before the offset
    // and its result after: the resume must still connect them.
    let tmp = TempDir::new().unwrap();
    let path = write(
        &tmp,
        &[
            ("user", json!("find the kingfisher notes")),
            (
                "assistant",
                tool_use("tu1", "chat-history search \"kingfisher\" --json"),
            ),
        ],
    );
    thread::sleep(QUIET);
    assert!(search(&tmp, "kingfisher").contains(ID), "indexed");
    append(
        &path,
        &format!(
            "{}\n",
            record(
                2,
                "user",
                tool_result(
                    "tu1",
                    "{\"results\": [{\"snippet\": \"sandpiperecho notes\"}]}"
                )
            )
        ),
    );
    append(
        &path,
        &format!("{}\n", record(3, "assistant", json!("Found them."))),
    );
    thread::sleep(QUIET);
    assert!(
        search(&tmp, "Found").contains(ID),
        "the later reply is indexed"
    );
    assert!(
        !search(&tmp, "sandpiperecho").contains(ID),
        "the history command's output is still left out"
    );
}

#[test]
fn an_assistant_reply_to_a_warmup_before_the_offset_is_still_skipped() {
    let tmp = TempDir::new().unwrap();
    let path = write(
        &tmp,
        &[
            ("user", json!("about the osprey")),
            ("assistant", json!("Osprey facts.")),
            ("user", json!("warmup")),
        ],
    );
    thread::sleep(QUIET);
    assert!(search(&tmp, "osprey").contains(ID), "indexed");
    append(
        &path,
        &format!(
            "{}\n",
            record(3, "assistant", json!("Warmed up, say gannetreply."))
        ),
    );
    append(
        &path,
        &format!("{}\n", record(4, "user", json!("thanks, puffinword"))),
    );
    thread::sleep(QUIET);
    assert!(
        search(&tmp, "puffinword").contains(ID),
        "the later message is indexed"
    );
    assert!(
        !search(&tmp, "gannetreply").contains(ID),
        "the reply to a warm-up is skipped, as a full parse skips it"
    );
}

#[test]
fn a_same_length_edit_deep_inside_a_transcript_waits_for_a_parse_from_the_start() {
    // The resume trusts the head of the file and the bytes before the offset,
    // like every log shipper does. An edit that changes neither and keeps the
    // length is therefore seen only when something makes the next parse start
    // from byte 0, here a shorter rewrite. This documents the trade-off.
    let tmp = TempDir::new().unwrap();
    let mut records: Vec<(&str, Value)> = Vec::new();
    for i in 0..24 {
        let role = if i % 2 == 0 { "user" } else { "assistant" };
        let text = format!("turn {i} about the lapwing and nothing else, repeated for length");
        records.push((role, json!(text)));
    }
    records[12].1 = json!("turn 12 about the dunlinone marsh bird, repeated for length");
    let path = write(&tmp, &records);
    thread::sleep(QUIET);
    assert!(search(&tmp, "dunlinone").contains(ID), "indexed");
    let len = fs::metadata(&path).unwrap().len();
    let text = fs::read_to_string(&path).unwrap();
    let edited = text.replace("dunlinone", "dunlintwo");
    assert_eq!(edited.len(), text.len());
    fs::write(&path, &edited).unwrap();
    append(
        &path,
        &format!("{}\n", record(24, "user", json!("and a new line"))),
    );
    thread::sleep(QUIET);
    let rows = search(&tmp, "dunlintwo");
    assert!(
        !rows.contains(ID) && search(&tmp, "dunlinone").contains(ID),
        "the edit sits past the head and before the pre-offset window: the resume keeps the stored rows"
    );
    assert!(
        stamp(&tmp)["tail"]["offset"].as_u64().unwrap() > len,
        "only the appended line was read"
    );
    // A rewrite that shortens the file fails the offset check: parse from 0.
    fs::write(
        &path,
        edited.lines().take(20).collect::<Vec<_>>().join("\n") + "\n",
    )
    .unwrap();
    thread::sleep(QUIET);
    assert!(
        search(&tmp, "dunlintwo").contains(ID),
        "seen after the parse from the start"
    );
}
