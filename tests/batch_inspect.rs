//! `inspect` takes several sessions at once, `--brief` cuts each to a few
//! lines, and accomplishments come from how each turn ended, not from any
//! sentence that happens to say "fixed" or "created".
use assert_cmd::Command;
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use tempfile::TempDir;

const ID_A: &str = "aaaaaaaa-1111-2222-3333-aaaaaaaaaaaa";
const ID_B: &str = "bbbbbbbb-1111-2222-3333-bbbbbbbbbbbb";

fn claude_record(id: &str, i: usize, role: &str, content: Value) -> String {
    json!({
        "type": role,
        "cwd": "/Users/test/proj",
        "message": {"role": role, "content": content},
        "timestamp": format!("2026-09-0{}T10:{i:02}:00Z", if id == ID_A { 1 } else { 2 }),
        "uuid": format!("{id}-{i}"),
        "sessionId": id,
    })
    .to_string()
}

fn write_claude(root: &Path, id: &str, records: Vec<(&str, Value)>) {
    let dir = root.join(".claude/projects/-Users-test-proj");
    fs::create_dir_all(&dir).unwrap();
    let lines: Vec<String> = records
        .into_iter()
        .enumerate()
        .map(|(i, (role, content))| claude_record(id, i, role, content))
        .collect();
    fs::write(dir.join(format!("{id}.jsonl")), lines.join("\n")).unwrap();
}

/// A: two turns. The first ends in a reply with a table, code and a link;
/// on the way the assistant narrates, runs a shell loop that mentions
/// "fixed", and reads a tool result. B: one short turn.
fn fixture() -> TempDir {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write_claude(
        tmp.path(),
        ID_A,
        vec![
            ("user", json!("fix the flaky deploy")),
            (
                "assistant",
                json!([
                    {"type": "text", "text": "Let me look at the deploy logs first."},
                    {"type": "tool_use", "id": "t1", "name": "Bash",
                     "input": {"command": "for f in fixed/*.log; do grep -c created $f; done"}}
                ]),
            ),
            (
                "user",
                json!([{"type": "tool_result", "tool_use_id": "t1",
                        "content": "Successfully created 3 files"}]),
            ),
            (
                "assistant",
                json!(
                    "Fixed the flaky deploy by retrying the health check.\n\n\
                       | step | status |\n|---|---|\n| deploy | fixed |\n\n\
                       ```sh\nfixed=1\n```\n\
                       Created [PR #41](https://github.com/acme/web/pull/41) for review."
                ),
            ),
            ("user", json!("also update the docs")),
            (
                "assistant",
                json!(
                    "Done.\n\nUpdated the README with the new retry flag, and noted it in the changelog."
                ),
            ),
        ],
    );
    write_claude(
        tmp.path(),
        ID_B,
        vec![
            ("user", json!("rename the heron module")),
            (
                "assistant",
                json!([
                    {"type": "tool_use", "id": "r1", "name": "Read",
                     "input": {"file_path": format!("{}/.claude/skills/rename/SKILL.md", root.display())}},
                    {"type": "tool_use", "id": "r2", "name": "Edit",
                     "input": {"file_path": "/Users/test/proj/src/heron.rs"}},
                    {"type": "tool_use", "id": "r3", "name": "Write",
                     "input": {"file_path": "/private/tmp/claude-501/x/scratchpad/check.py"}},
                    {"type": "tool_use", "id": "r4", "name": "Glob",
                     "input": {"path": "/Users/test/proj"}}
                ]),
            ),
            (
                "assistant",
                json!("> Renamed heron to egret in all 4 files."),
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
        .env("NO_COLOR", "1");
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

fn section<'a>(out: &'a str, heading: &str) -> Vec<&'a str> {
    out.lines()
        .skip_while(|l| l.trim() != heading)
        .skip(1)
        .take_while(|l| !l.trim().is_empty())
        .map(str::trim)
        .collect()
}

#[test]
fn accomplishments_are_how_each_turn_ended() {
    let tmp = fixture();
    let out = stdout(&tmp, &["inspect", "aaaaaaaa"]);
    assert_eq!(
        section(&out, "Accomplishments:"),
        [
            "✓ Fixed the flaky deploy by retrying the health check.",
            "✓ Updated the README with the new retry flag, and noted it in the changelog.",
        ],
        "{out}"
    );
}

#[test]
fn accomplishments_skip_tool_calls_code_and_tables() {
    let tmp = fixture();
    let out = stdout(&tmp, &["inspect", "aaaaaaaa"]);
    let acc = section(&out, "Accomplishments:").join("\n");
    for noise in [
        "[Tool:",
        "for f in",
        "fixed=1",
        "| deploy",
        "Successfully created",
        "Let me look",
    ] {
        assert!(!acc.contains(noise), "{noise:?} in accomplishments:\n{acc}");
    }
}

#[test]
fn inspect_takes_several_sessions() {
    let tmp = fixture();
    let out = stdout(&tmp, &["inspect", "aaaaaaaa", "bbbbbbbb"]);
    let a = out.find(ID_A).expect("A inspected");
    let b = out.find(ID_B).expect("B inspected");
    assert!(a < b, "sessions print in the order given:\n{out}");
    assert!(
        out.contains("✓ Renamed heron to egret in all 4 files."),
        "{out}"
    );
}

#[test]
fn brief_is_a_few_lines_per_session() {
    let tmp = fixture();
    let out = stdout(&tmp, &["inspect", "aaaaaaaa", "bbbbbbbb", "--brief"]);
    let blocks: Vec<&str> = out.trim().split("\n\n").collect();
    assert_eq!(blocks.len(), 2, "one block per session:\n{out}");
    let a = blocks[0];
    assert!(a.lines().next().unwrap().contains("aaaaaaaa"), "{a}");
    assert!(a.contains("Asked: fix the flaky deploy"), "{a}");
    assert!(
        a.contains(
            "Outcome: Updated the README with the new retry flag, and noted it in the changelog."
        ),
        "the last turn's ending, without the bare \"Done.\":\n{a}"
    );
    assert!(
        !a.contains("Tools Used") && !a.contains("Accomplishments"),
        "{a}"
    );
    assert!(a.lines().count() <= 6, "{a}");
    assert!(
        blocks[1].contains("Outcome: Renamed heron to egret in all 4 files."),
        "{out}"
    );
}

#[test]
fn brief_files_are_the_projects_not_the_agents() {
    let tmp = fixture();
    let out = stdout(&tmp, &["inspect", "bbbbbbbb", "--brief"]);
    assert!(out.contains("  Files: src/heron.rs\n"), "{out}");
}

#[test]
fn brief_works_with_last() {
    let tmp = fixture();
    let out = stdout(&tmp, &["inspect", "--last", "--brief"]);
    assert!(
        out.contains("bbbbbbbb") && !out.contains("aaaaaaaa"),
        "{out}"
    );
}

#[test]
fn an_unknown_id_in_a_batch_does_not_hide_the_others() {
    let tmp = fixture();
    let out = cmd(&tmp)
        .args(["inspect", "aaaaaaaa", "ffffffff", "bbbbbbbb", "--brief"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stdout.contains("aaaaaaaa") && stdout.contains("bbbbbbbb"),
        "{stdout}"
    );
    assert!(stderr.contains("ffffffff"), "{stderr}");
    assert!(
        !out.status.success(),
        "a missing session still fails the command"
    );
}

const ID_C: &str = "cccccccc-1111-2222-3333-cccccccccccc";

#[test]
fn a_trailing_done_or_lead_in_does_not_hide_the_outcome() {
    let tmp = fixture();
    write_claude(
        tmp.path(),
        ID_C,
        vec![
            ("user", json!("audit the egret config")),
            (
                "assistant",
                json!(
                    "The egret config sets two conflicting timeouts, so requests fail after 5 seconds."
                ),
            ),
            ("assistant", json!("Here are the findings.")),
            ("assistant", json!("Done.")),
        ],
    );
    let out = stdout(&tmp, &["inspect", "cccccccc", "--brief"]);
    assert!(
        out.contains("Outcome: The egret config sets two conflicting timeouts, so requests fail after 5 seconds."),
        "{out}"
    );
}

#[test]
fn brief_files_list_the_projects_first() {
    let tmp = fixture();
    write_claude(
        tmp.path(),
        ID_C,
        vec![
            ("user", json!("compare with the other project")),
            (
                "assistant",
                json!([
                    {"type": "tool_use", "id": "o1", "name": "Read",
                     "input": {"file_path": "/Users/test/another-project/notes.md"}},
                    {"type": "tool_use", "id": "o2", "name": "Edit",
                     "input": {"file_path": "/Users/test/proj/src/egret.rs"}}
                ]),
            ),
            (
                "assistant",
                json!("Copied the retry settings from the other project."),
            ),
        ],
    );
    let out = stdout(&tmp, &["inspect", "cccccccc", "--brief"]);
    assert!(
        out.contains("  Files: src/egret.rs, /Users/test/another-project/notes.md\n"),
        "{out}"
    );
}
