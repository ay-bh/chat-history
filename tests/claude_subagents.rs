//! Claude Code keeps each subagent's transcript beside its session:
//! `<project>/<session>/subagents/agent-<id>.jsonl`, with a `.meta.json`
//! describing the task. `--sidechains` lists and searches them.

use assert_cmd::Command;
use predicates::prelude::*;
use std::{fs, path::Path};
use tempfile::TempDir;

const SESSION: &str = "d3752d92-9fae-432f-9f9f-10e4066cc131";

fn cmd(home: &Path) -> Command {
    let mut c = Command::cargo_bin("chat-history").unwrap();
    c.env("HOME", home)
        .env("CLAUDE_CONFIG_DIR", home.join(".claude"))
        .env_remove("CODEX_HOME")
        .env_remove("USERPROFILE")
        .env_remove("CODEX_THREAD_ID")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("CURSOR_CONVERSATION_ID")
        .env("CHAT_HISTORY_CACHE_DIR", home.join("cache"))
        .env("NO_COLOR", "1");
    c
}

fn line(kind: &str, text: &str, sidechain: bool, agent: Option<&str>) -> String {
    let content = if kind == "user" {
        serde_json::json!(text)
    } else {
        serde_json::json!([{ "type": "text", "text": text }])
    };
    let mut v = serde_json::json!({
        "type": kind,
        "sessionId": SESSION,
        "isSidechain": sidechain,
        "cwd": "/work/proj",
        "gitBranch": "main",
        "timestamp": "2026-10-30T08:00:00.000Z",
        "uuid": format!("{kind}-{text:.6}"),
        "message": { "role": kind, "content": content },
    });
    if let Some(a) = agent {
        v["agentId"] = serde_json::json!(a);
    }
    v.to_string()
}

fn fixture(h: &Path) {
    let proj = h.join(".claude/projects/-work-proj");
    let subagents = proj.join(SESSION).join("subagents");
    fs::create_dir_all(&subagents).unwrap();
    fs::write(
        proj.join(format!("{SESSION}.jsonl")),
        [
            line("user", "review the release", false, None),
            line("assistant", "Started a reviewer.", false, None),
        ]
        .join("\n"),
    )
    .unwrap();
    fs::write(
        subagents.join("agent-aef1e734424f66eaa.jsonl"),
        [
            line(
                "user",
                "check the platypus migration",
                true,
                Some("aef1e734424f66eaa"),
            ),
            line(
                "assistant",
                "The platypus migration is safe.",
                true,
                Some("aef1e734424f66eaa"),
            ),
        ]
        .join("\n"),
    )
    .unwrap();
    fs::write(
        subagents.join("agent-aef1e734424f66eaa.meta.json"),
        r#"{"agentType":"general-purpose","description":"Final review of the release","name":"reviewer"}"#,
    )
    .unwrap();
}

#[test]
fn subagent_transcripts_are_listed_and_searched_with_sidechains() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    fixture(h);
    cmd(h)
        .args(["--source", "claude"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 sessions"));
    cmd(h)
        .args(["--source", "claude", "--sidechains"])
        .assert()
        .success()
        .stdout(predicate::str::contains("2 sessions"))
        .stdout(predicate::str::contains("Final review of the release"));
    cmd(h)
        .args(["search", "platypus", "--sidechains", "--compact"])
        .assert()
        .success()
        .stdout(predicate::str::contains("agent-ae"));
    cmd(h)
        .args(["view", "agent-aef1e734424f66eaa", "--sidechains", "--plain"])
        .assert()
        .success()
        .stdout(predicate::str::contains("migration is safe"));
}

#[test]
fn a_search_from_inside_a_session_leaves_out_its_subagents() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    fixture(h);
    cmd(h)
        .env("CLAUDE_CODE_SESSION_ID", SESSION)
        .args(["search", "platypus", "--sidechains", "--compact"])
        .assert()
        .success()
        .stdout(predicate::str::contains("agent-ae").not());
}
