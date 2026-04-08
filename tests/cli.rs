use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

fn isolated_cmd() -> (Command, TempDir) {
    let tmp = TempDir::new().unwrap();
    let mut cmd = Command::cargo_bin("chat-history").unwrap();
    cmd.env("CLAUDE_CONFIG_DIR", tmp.path());
    cmd.env("HOME", tmp.path());
    (cmd, tmp)
}

#[test]
fn help_output() {
    let (mut cmd, _tmp) = isolated_cmd();
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Search Claude Code"));
}

#[test]
fn no_sessions_shows_empty() {
    let (mut cmd, _tmp) = isolated_cmd();
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("No sessions found"));
}

#[test]
fn summarize_empty() {
    let (mut cmd, _tmp) = isolated_cmd();
    cmd.arg("-s")
        .assert()
        .success()
        .stdout(predicate::str::contains("No sessions found"));
}

#[test]
fn search_no_results() {
    let (mut cmd, _tmp) = isolated_cmd();
    cmd.args(["search", "nonexistent_query_xyz"])
        .assert()
        .success();
}

#[test]
fn search_json_empty() {
    let (mut cmd, _tmp) = isolated_cmd();
    cmd.args(["search", "nonexistent", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"count\""));
}

#[test]
fn inspect_missing_session() {
    let (mut cmd, _tmp) = isolated_cmd();
    cmd.args(["inspect", "nonexistent-session-id"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Session not found"));
}

#[test]
fn view_missing_session() {
    let (mut cmd, _tmp) = isolated_cmd();
    cmd.args(["view", "nonexistent-session-id"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Session not found"));
}

#[test]
fn find_missing_session() {
    let (mut cmd, _tmp) = isolated_cmd();
    cmd.args(["find", "nonexistent-session-id"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Session not found"));
}

#[test]
fn install_skill_creates_files() {
    let tmp = TempDir::new().unwrap();
    Command::cargo_bin("chat-history")
        .unwrap()
        .arg("install-skill")
        .env("HOME", tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("installed"));
    assert!(
        tmp.path()
            .join(".claude/skills/chat-history/SKILL.md")
            .exists()
    );
    assert!(
        tmp.path()
            .join(".cursor/skills/chat-history/SKILL.md")
            .exists()
    );
}

fn setup_fixture(tmp: &TempDir) {
    let project_dir = tmp.path().join("projects").join("-Users-test-project");
    fs::create_dir_all(&project_dir).unwrap();

    // Write a sessions-index.json
    let index = serde_json::json!({
        "entries": [{
            "sessionId": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            "summary": "fixture test session",
            "firstPrompt": "help me fix the build",
            "created": "2025-01-15T10:00:00Z",
            "modified": "2025-01-15T11:00:00Z",
            "messageCount": 4,
            "gitBranch": "feature-tests",
            "projectPath": "/Users/test/project",
            "fullPath": "",
            "isSidechain": false
        }, {
            "sessionId": "11111111-2222-3333-4444-555555555555",
            "summary": "docker deployment pipeline",
            "firstPrompt": "configure docker compose",
            "created": "2025-02-20T08:00:00Z",
            "modified": "2025-02-20T09:00:00Z",
            "messageCount": 8,
            "gitBranch": "main",
            "projectPath": "/Users/test/project",
            "fullPath": "",
            "isSidechain": false
        }]
    });
    fs::write(
        project_dir.join("sessions-index.json"),
        serde_json::to_string(&index).unwrap(),
    )
    .unwrap();
}

#[test]
fn list_shows_fixture_sessions() {
    let tmp = TempDir::new().unwrap();
    setup_fixture(&tmp);
    Command::cargo_bin("chat-history")
        .unwrap()
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("fixture test session"))
        .stdout(predicate::str::contains("docker deployment pipeline"))
        .stdout(predicate::str::contains("2 sessions"));
}

#[test]
fn list_verbose_shows_ids() {
    let tmp = TempDir::new().unwrap();
    setup_fixture(&tmp);
    Command::cargo_bin("chat-history")
        .unwrap()
        .arg("-v")
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
        ));
}

#[test]
fn list_summarize_groups_by_day() {
    let tmp = TempDir::new().unwrap();
    setup_fixture(&tmp);
    Command::cargo_bin("chat-history")
        .unwrap()
        .arg("-s")
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("2 sessions across 2 days"));
}

#[test]
fn search_index_finds_session() {
    let tmp = TempDir::new().unwrap();
    setup_fixture(&tmp);
    Command::cargo_bin("chat-history")
        .unwrap()
        .args(["search", "docker"])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("docker deployment pipeline"));
}

#[test]
fn search_json_format() {
    let tmp = TempDir::new().unwrap();
    setup_fixture(&tmp);
    let output = Command::cargo_bin("chat-history")
        .unwrap()
        .args(["search", "docker", "--json"])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["query"], "docker");
    assert_eq!(json["count"].as_u64().unwrap(), 1);
    let results = json["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    let first = &results[0];
    assert_eq!(first["session_id"], "11111111-2222-3333-4444-555555555555");
    assert!(
        first.get("score").is_some(),
        "result should include a score field"
    );
    assert!(
        first.get("summary").is_some(),
        "result should include a summary field"
    );
}

#[test]
fn filter_by_source_flag() {
    let tmp = TempDir::new().unwrap();
    setup_fixture(&tmp);
    // All fixture sessions are claude source
    Command::cargo_bin("chat-history")
        .unwrap()
        .args(["--source", "cursor"])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("No sessions found"));
}

#[test]
fn filter_by_branch_flag() {
    let tmp = TempDir::new().unwrap();
    setup_fixture(&tmp);
    Command::cargo_bin("chat-history")
        .unwrap()
        .args(["--branch", "feature-tests"])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("fixture test session"))
        .stdout(predicate::str::contains("1 sessions"));
}

#[test]
fn keyword_filter_global() {
    let tmp = TempDir::new().unwrap();
    setup_fixture(&tmp);
    Command::cargo_bin("chat-history")
        .unwrap()
        .args(["-k", "docker"])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("docker deployment pipeline"))
        .stdout(predicate::str::contains("1 sessions"));
}

#[test]
fn keyword_filter_with_search() {
    let tmp = TempDir::new().unwrap();
    setup_transcript_fixture(&tmp);
    Command::cargo_bin("chat-history")
        .unwrap()
        .args(["-k", "webpack", "search", "docker"])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("docker"));
}

#[test]
fn date_filter() {
    let tmp = TempDir::new().unwrap();
    setup_fixture(&tmp);
    Command::cargo_bin("chat-history")
        .unwrap()
        .args(["--from", "2025-02-01"])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("docker deployment pipeline"))
        .stdout(predicate::str::contains("1 sessions"));
}

fn setup_transcript_fixture(tmp: &TempDir) {
    let project_dir = tmp.path().join("projects").join("-Users-test-project");
    fs::create_dir_all(&project_dir).unwrap();

    let session_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    let jsonl_path = project_dir.join(format!("{session_id}.jsonl"));

    let messages = vec![
        serde_json::json!({"type":"user","cwd":"/Users/test/project","message":{"role":"user","content":"help me configure webpack for production builds"},"timestamp":"2025-01-15T10:00:00Z","uuid":"u1"}),
        serde_json::json!({"type":"assistant","message":{"role":"assistant","content":"I'll help you configure webpack for production. Here's the optimized configuration with code splitting and tree shaking enabled for better performance."},"timestamp":"2025-01-15T10:01:00Z","uuid":"u2"}),
        serde_json::json!({"type":"user","message":{"role":"user","content":"now add the docker deployment configuration"},"timestamp":"2025-01-15T10:02:00Z","uuid":"u3"}),
        serde_json::json!({"type":"assistant","message":{"role":"assistant","content":"Here's the Dockerfile and docker-compose.yml for deploying the application. I've included multi-stage builds to optimize the image size."},"timestamp":"2025-01-15T10:03:00Z","uuid":"u4"}),
    ];

    let jsonl: String = messages
        .iter()
        .map(|m| serde_json::to_string(m).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&jsonl_path, &jsonl).unwrap();

    let index = serde_json::json!({
        "entries": [{
            "sessionId": session_id,
            "summary": "webpack and docker config",
            "firstPrompt": "help me configure webpack",
            "created": "2025-01-15T10:00:00Z",
            "modified": "2025-01-15T10:03:00Z",
            "messageCount": 4,
            "gitBranch": "main",
            "projectPath": "/Users/test/project",
            "fullPath": jsonl_path.to_string_lossy(),
            "isSidechain": false
        }]
    });
    fs::write(
        project_dir.join("sessions-index.json"),
        serde_json::to_string(&index).unwrap(),
    )
    .unwrap();
}

fn setup_rich_transcript_fixture(tmp: &TempDir) {
    let project_dir = tmp.path().join("projects").join("-Users-test-project");
    fs::create_dir_all(&project_dir).unwrap();

    let session_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    let jsonl_path = project_dir.join(format!("{session_id}.jsonl"));

    let messages = vec![
        serde_json::json!({
            "type": "user",
            "cwd": "/Users/test/project",
            "message": {"role": "user", "content": "help me fix the authentication error in the login endpoint"},
            "timestamp": "2025-01-15T10:00:00Z",
            "uuid": "u1"
        }),
        serde_json::json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "model": "claude-sonnet-4-20250514",
                "usage": {"input_tokens": 500, "output_tokens": 200, "cache_creation_input_tokens": 100, "cache_read_input_tokens": 50},
                "content": [
                    {"type": "text", "text": "I'll read the auth handler to understand the issue."},
                    {"type": "tool_use", "id": "t1", "name": "Read", "input": {"file_path": "/src/auth/handler.rs"}},
                    {"type": "tool_result", "tool_use_id": "t1", "content": "Error: file not found /src/auth/handler.rs"},
                    {"type": "text", "text": "Let me check the correct path."},
                    {"type": "tool_use", "id": "t2", "name": "Read", "input": {"file_path": "/src/handlers/auth.rs"}},
                    {"type": "tool_result", "tool_use_id": "t2", "content": "pub fn login(req: Request) -> Response { ... }"},
                    {"type": "text", "text": "I found the bug. I decided to use JWT tokens instead of session cookies because they work better with the stateless API. I've successfully fixed the authentication handler."},
                    {"type": "tool_use", "id": "t3", "name": "Edit", "input": {"file_path": "/src/handlers/auth.rs", "new_string": "fixed code"}},
                    {"type": "tool_result", "tool_use_id": "t3", "content": "Successfully edited file"}
                ]
            },
            "timestamp": "2025-01-15T10:05:00Z",
            "uuid": "u2"
        }),
        serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": "can you also add rate limiting to prevent brute force attacks?"},
            "timestamp": "2025-01-15T10:06:00Z",
            "uuid": "u3"
        }),
        serde_json::json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "usage": {"input_tokens": 300, "output_tokens": 400, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0},
                "content": [
                    {"type": "text", "text": "I'll implement rate limiting for the login endpoint. I opted for a sliding window approach instead of fixed windows for smoother request handling."},
                    {"type": "tool_use", "id": "t4", "name": "Edit", "input": {"file_path": "/src/middleware/rate_limit.rs", "new_string": "rate limiter code"}},
                    {"type": "tool_result", "tool_use_id": "t4", "content": "Successfully created file"},
                    {"type": "text", "text": "Done. I've successfully added rate limiting middleware that blocks IPs after 5 failed attempts in a 15-minute window."}
                ]
            },
            "timestamp": "2025-01-15T10:10:00Z",
            "uuid": "u4"
        }),
    ];

    let jsonl: String = messages
        .iter()
        .map(|m| serde_json::to_string(m).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&jsonl_path, &jsonl).unwrap();

    let index = serde_json::json!({
        "entries": [{
            "sessionId": session_id,
            "summary": "fix auth and add rate limiting",
            "firstPrompt": "help me fix the authentication error",
            "created": "2025-01-15T10:00:00Z",
            "modified": "2025-01-15T10:10:00Z",
            "messageCount": 4,
            "gitBranch": "fix-auth",
            "projectPath": "/Users/test/project",
            "fullPath": jsonl_path.to_string_lossy(),
            "isSidechain": false
        }]
    });
    fs::write(
        project_dir.join("sessions-index.json"),
        serde_json::to_string(&index).unwrap(),
    )
    .unwrap();
}

#[test]
fn deep_search_with_transcript() {
    let tmp = TempDir::new().unwrap();
    setup_transcript_fixture(&tmp);
    Command::cargo_bin("chat-history")
        .unwrap()
        .args(["search", "webpack production", "--deep"])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("webpack"));
}

#[test]
fn inspect_session_with_transcript() {
    let tmp = TempDir::new().unwrap();
    setup_transcript_fixture(&tmp);
    Command::cargo_bin("chat-history")
        .unwrap()
        .args(["inspect", "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("webpack and docker config"));
}

#[test]
fn inspect_last_session() {
    let tmp = TempDir::new().unwrap();
    setup_transcript_fixture(&tmp);
    Command::cargo_bin("chat-history")
        .unwrap()
        .args(["inspect", "--last"])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("webpack and docker config"));
}

#[test]
fn view_session_with_transcript() {
    let tmp = TempDir::new().unwrap();
    setup_transcript_fixture(&tmp);
    Command::cargo_bin("chat-history")
        .unwrap()
        .args(["view", "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("webpack"));
}

#[test]
fn view_plain_output() {
    let tmp = TempDir::new().unwrap();
    setup_transcript_fixture(&tmp);
    let output = Command::cargo_bin("chat-history")
        .unwrap()
        .args(["view", "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", "--plain"])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    // Plain output should not have ANSI codes
    assert!(!stdout.contains("\x1b["));
    assert!(stdout.contains("You:") || stdout.contains("Claude:"));
}

#[test]
fn view_with_tools_flag() {
    let tmp = TempDir::new().unwrap();
    setup_rich_transcript_fixture(&tmp);
    let output = Command::cargo_bin("chat-history")
        .unwrap()
        .args(["view", "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", "--tools"])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("tools:"), "should display tool names");
    assert!(stdout.contains("Read"), "should show Read tool");
    assert!(stdout.contains("Edit"), "should show Edit tool");
}

#[test]
fn view_last_session() {
    let tmp = TempDir::new().unwrap();
    setup_transcript_fixture(&tmp);
    let output = Command::cargo_bin("chat-history")
        .unwrap()
        .args(["view", "--last"])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("webpack"),
        "view --last should show transcript content"
    );
    assert!(
        stdout.contains("docker"),
        "view --last should show all messages"
    );
}

#[test]
fn export_creates_file() {
    let tmp = TempDir::new().unwrap();
    setup_transcript_fixture(&tmp);
    let out_file = tmp.path().join("export.md");
    Command::cargo_bin("chat-history")
        .unwrap()
        .args([
            "export",
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            "-o",
            out_file.to_str().unwrap(),
        ])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Exported to"));
    assert!(out_file.exists());
    let content = fs::read_to_string(&out_file).unwrap();
    assert!(content.contains("webpack"));
    assert!(content.contains("## You"));
    assert!(content.contains("## Assistant"));
}

#[test]
fn find_session_prints_path() {
    let tmp = TempDir::new().unwrap();
    setup_transcript_fixture(&tmp);
    Command::cargo_bin("chat-history")
        .unwrap()
        .args(["find", "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(".jsonl"));
}

#[test]
fn search_by_uuid() {
    let tmp = TempDir::new().unwrap();
    setup_transcript_fixture(&tmp);
    Command::cargo_bin("chat-history")
        .unwrap()
        .args(["search", "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("webpack"));
}

#[test]
fn no_color_env_disables_colors() {
    let tmp = TempDir::new().unwrap();
    setup_fixture(&tmp);
    let output = Command::cargo_bin("chat-history")
        .unwrap()
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(!stdout.contains("\x1b["));
}

// --- Tests using the rich transcript fixture (tool_use, errors, metadata) ---

#[test]
fn inspect_shows_tools_and_files() {
    let tmp = TempDir::new().unwrap();
    setup_rich_transcript_fixture(&tmp);
    let output = Command::cargo_bin("chat-history")
        .unwrap()
        .args(["inspect", "--last"])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("fix auth and add rate limiting"),
        "inspect should show the session summary"
    );
    assert!(
        stdout.contains("messages:"),
        "inspect should show message count"
    );
    assert!(stdout.contains("duration:"), "inspect should show duration");
    assert!(
        stdout.contains("Tools Used"),
        "inspect should list tools used"
    );
    assert!(stdout.contains("Read"), "inspect should show Read tool");
    assert!(stdout.contains("Edit"), "inspect should show Edit tool");
    assert!(
        stdout.contains("Files Touched"),
        "inspect should list files"
    );
    assert!(
        stdout.contains("auth.rs"),
        "inspect should show touched files"
    );
}

#[test]
fn inspect_shows_accomplishments_and_decisions() {
    let tmp = TempDir::new().unwrap();
    setup_rich_transcript_fixture(&tmp);
    let output = Command::cargo_bin("chat-history")
        .unwrap()
        .args(["inspect", "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("Accomplishments"),
        "inspect should extract accomplishments from assistant messages"
    );
    assert!(
        stdout.contains("Errors Encountered"),
        "inspect should extract error patterns"
    );
}

#[test]
fn inspect_shows_model_and_tokens() {
    let tmp = TempDir::new().unwrap();
    setup_rich_transcript_fixture(&tmp);
    let output = Command::cargo_bin("chat-history")
        .unwrap()
        .args(["inspect", "--last"])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("claude-sonnet-4"),
        "inspect should show the model name"
    );
    assert!(
        stdout.contains("tokens:"),
        "inspect should show token count"
    );
}

#[test]
fn deep_search_json_combined() {
    let tmp = TempDir::new().unwrap();
    setup_rich_transcript_fixture(&tmp);
    let output = Command::cargo_bin("chat-history")
        .unwrap()
        .args(["search", "authentication error", "--deep", "--json"])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["query"], "authentication error");
    let results = json["results"].as_array().unwrap();
    assert!(
        !results.is_empty(),
        "deep --json should find transcript matches"
    );
    let first = &results[0];
    assert_eq!(first["session_id"], "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");
    assert!(
        first["snippet"].as_str().unwrap().len() > 0,
        "snippet should have content"
    );
    assert!(
        first.get("tools").is_some(),
        "result should include tools array"
    );
    assert!(
        first.get("files").is_some(),
        "result should include files array"
    );
}

#[test]
fn deep_search_finds_tool_output_content() {
    let tmp = TempDir::new().unwrap();
    setup_rich_transcript_fixture(&tmp);
    Command::cargo_bin("chat-history")
        .unwrap()
        .args(["search", "rate limiting brute force", "--deep"])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("rate limiting"));
}

#[test]
fn export_preserves_all_messages() {
    let tmp = TempDir::new().unwrap();
    setup_rich_transcript_fixture(&tmp);
    let out_file = tmp.path().join("export.md");
    Command::cargo_bin("chat-history")
        .unwrap()
        .args([
            "export",
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            "-o",
            out_file.to_str().unwrap(),
        ])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .assert()
        .success();
    let content = fs::read_to_string(&out_file).unwrap();
    assert!(
        content.contains("# fix auth and add rate limiting"),
        "export should start with summary heading"
    );
    assert!(
        content.contains("**Source:** claude"),
        "export should include source metadata"
    );
    assert!(
        content.contains("**Branch:** fix-auth"),
        "export should include branch metadata"
    );
    let you_count = content.matches("## You").count();
    let assistant_count = content.matches("## Assistant").count();
    assert_eq!(you_count, 2, "should export both user messages");
    assert_eq!(assistant_count, 2, "should export both assistant messages");
    let you_pos = content.find("## You").unwrap();
    let assistant_pos = content.find("## Assistant").unwrap();
    assert!(
        you_pos < assistant_pos,
        "user message should come before assistant message"
    );
}

#[test]
fn malformed_jsonl_handled_gracefully() {
    let tmp = TempDir::new().unwrap();
    let project_dir = tmp.path().join("projects").join("-Users-test-project");
    fs::create_dir_all(&project_dir).unwrap();

    let session_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    let jsonl_path = project_dir.join(format!("{session_id}.jsonl"));
    fs::write(
        &jsonl_path,
        "not valid json\n{broken\n\n{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hello\"},\"timestamp\":\"2025-01-15T10:00:00Z\",\"uuid\":\"u1\"}\n",
    ).unwrap();

    let index = serde_json::json!({
        "entries": [{
            "sessionId": session_id,
            "summary": "session with corrupt lines",
            "firstPrompt": "hello",
            "created": "2025-01-15T10:00:00Z",
            "modified": "2025-01-15T10:00:00Z",
            "messageCount": 1,
            "gitBranch": "main",
            "projectPath": "/Users/test/project",
            "fullPath": jsonl_path.to_string_lossy(),
            "isSidechain": false
        }]
    });
    fs::write(
        project_dir.join("sessions-index.json"),
        serde_json::to_string(&index).unwrap(),
    )
    .unwrap();

    Command::cargo_bin("chat-history")
        .unwrap()
        .args(["view", session_id])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .stdout(predicate::str::contains("hello"));
}

fn setup_cursor_fixture(tmp: &TempDir) {
    let project_dir = tmp
        .path()
        .join(".cursor")
        .join("projects")
        .join("Users-test-myapp");
    let transcripts_dir = project_dir.join("agent-transcripts");
    fs::create_dir_all(&transcripts_dir).unwrap();

    let session_id = "cccccccc-dddd-eeee-ffff-000000000000";
    let txt_content = "Session Title\nuser: refactor the database module to use connection pooling\nassistant: I'll refactor the database module to use connection pooling with r2d2. This will improve performance under concurrent load.\nuser: also add retry logic for transient failures\nassistant: Done. I've added exponential backoff retry logic for transient database errors like connection timeouts.\n";
    fs::write(
        transcripts_dir.join(format!("{session_id}.txt")),
        txt_content,
    )
    .unwrap();
}

#[test]
fn cursor_sessions_listed() {
    let tmp = TempDir::new().unwrap();
    setup_cursor_fixture(&tmp);
    let output = Command::cargo_bin("chat-history")
        .unwrap()
        .env("HOME", tmp.path())
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success());
    assert!(
        stdout.contains("1 sessions"),
        "should discover cursor session from .txt file"
    );
    assert!(
        stdout.contains("refactor the database module"),
        "should show cursor session's first prompt"
    );
}

#[test]
fn cursor_session_view() {
    let tmp = TempDir::new().unwrap();
    setup_cursor_fixture(&tmp);
    let output = Command::cargo_bin("chat-history")
        .unwrap()
        .args(["view", "--last", "--plain"])
        .env("HOME", tmp.path())
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success());
    assert!(
        stdout.contains("connection pooling"),
        "should display cursor transcript content"
    );
    assert!(
        stdout.contains("retry logic"),
        "should display all messages from cursor transcript"
    );
}

#[test]
fn cursor_session_deep_search() {
    let tmp = TempDir::new().unwrap();
    setup_cursor_fixture(&tmp);
    Command::cargo_bin("chat-history")
        .unwrap()
        .args(["search", "connection pooling", "--deep"])
        .env("HOME", tmp.path())
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("pooling"));
}

#[test]
fn timeframe_excludes_empty_timestamp_messages() {
    let tmp = TempDir::new().unwrap();
    // Claude session with timestamps + Cursor session without timestamps
    setup_transcript_fixture(&tmp);
    setup_cursor_fixture(&tmp);

    // Without timeframe: deep search should find Cursor results
    let output = Command::cargo_bin("chat-history")
        .unwrap()
        .args(["search", "database", "--deep"])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("database"),
        "without timeframe, cursor results should appear: {stdout}"
    );

    // With timeframe: empty-timestamp Cursor messages should be excluded
    let output = Command::cargo_bin("chat-history")
        .unwrap()
        .args(["search", "database", "--deep", "--timeframe", "today"])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        !stdout.contains("connection pooling"),
        "with timeframe, empty-timestamp cursor messages should be excluded: {stdout}"
    );
}

#[test]
fn local_flag_works_with_search() {
    let tmp = TempDir::new().unwrap();

    // Create two projects with different names
    let proj_a_dir = tmp.path().join("projects").join("-Users-test-alpha");
    fs::create_dir_all(&proj_a_dir).unwrap();
    let session_a = "aaaa1111-2222-3333-4444-555555555555";
    let jsonl_a = proj_a_dir.join(format!("{session_a}.jsonl"));
    let msg_a = serde_json::json!({"type":"user","cwd":"/Users/test/alpha","message":{"role":"user","content":"deploy the authentication service to production"},"timestamp":"2025-01-15T10:00:00Z","uuid":"u1"});
    fs::write(&jsonl_a, serde_json::to_string(&msg_a).unwrap()).unwrap();
    let index_a = serde_json::json!({
        "entries": [{
            "sessionId": session_a,
            "summary": "deploy auth to prod",
            "firstPrompt": "deploy the authentication service",
            "created": "2025-01-15T10:00:00Z",
            "modified": "2025-01-15T10:00:00Z",
            "messageCount": 1,
            "gitBranch": "main",
            "projectPath": "/Users/test/alpha",
            "fullPath": jsonl_a.to_string_lossy(),
            "isSidechain": false
        }]
    });
    fs::write(
        proj_a_dir.join("sessions-index.json"),
        serde_json::to_string(&index_a).unwrap(),
    )
    .unwrap();

    let proj_b_dir = tmp.path().join("projects").join("-Users-test-beta");
    fs::create_dir_all(&proj_b_dir).unwrap();
    let session_b = "bbbb1111-2222-3333-4444-555555555555";
    let jsonl_b = proj_b_dir.join(format!("{session_b}.jsonl"));
    let msg_b = serde_json::json!({"type":"user","cwd":"/Users/test/beta","message":{"role":"user","content":"deploy the notification service to staging"},"timestamp":"2025-01-15T10:00:00Z","uuid":"u2"});
    fs::write(&jsonl_b, serde_json::to_string(&msg_b).unwrap()).unwrap();
    let index_b = serde_json::json!({
        "entries": [{
            "sessionId": session_b,
            "summary": "deploy notifications",
            "firstPrompt": "deploy the notification service",
            "created": "2025-01-15T10:00:00Z",
            "modified": "2025-01-15T10:00:00Z",
            "messageCount": 1,
            "gitBranch": "main",
            "projectPath": "/Users/test/beta",
            "fullPath": jsonl_b.to_string_lossy(),
            "isSidechain": false
        }]
    });
    fs::write(
        proj_b_dir.join("sessions-index.json"),
        serde_json::to_string(&index_b).unwrap(),
    )
    .unwrap();

    // Without -L: both projects appear
    let output = Command::cargo_bin("chat-history")
        .unwrap()
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("2 sessions"),
        "without -L both sessions should appear: {stdout}"
    );

    // With --project filter on search: only matching project
    let output = Command::cargo_bin("chat-history")
        .unwrap()
        .args(["search", "deploy", "--deep", "--project", "alpha"])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("authentication"),
        "search --project alpha should find alpha session: {stdout}"
    );
    assert!(
        !stdout.contains("notification"),
        "search --project alpha should not find beta session: {stdout}"
    );
}

#[test]
fn mixed_sources_both_listed() {
    let tmp = TempDir::new().unwrap();
    setup_fixture(&tmp);
    setup_cursor_fixture(&tmp);
    let output = Command::cargo_bin("chat-history")
        .unwrap()
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env("HOME", tmp.path())
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("CC"),
        "should show Claude sessions with CC tag"
    );
    assert!(
        stdout.contains("CR"),
        "should show Cursor sessions with CR tag"
    );
    assert!(
        stdout.contains("3 sessions"),
        "should list all sessions from both sources"
    );
}
