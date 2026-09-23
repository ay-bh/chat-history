//! Messages say where they came from: a person, the assistant, or a tool.
//! Tool output ranks below conversation text, chat-history's own output is
//! never indexed, the calling session is left out of its own searches, and
//! long transcripts are read to the end.
use assert_cmd::Command;
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use tempfile::TempDir;

const ID_A: &str = "aaaaaaaa-1111-2222-3333-aaaaaaaaaaaa";
const ID_B: &str = "bbbbbbbb-1111-2222-3333-bbbbbbbbbbbb";
const ID_C: &str = "cccccccc-1111-2222-3333-cccccccccccc";
const ID_D: &str = "dddddddd-1111-2222-3333-dddddddddddd";
const CODEX_ID: &str = "019a0000-0000-7000-8000-00000000c0de";

fn claude_record(id: &str, i: usize, role: &str, content: Value) -> String {
    json!({
        "type": role,
        "cwd": "/Users/test/proj",
        "message": {"role": role, "content": content},
        "timestamp": format!("2026-09-01T10:{i:02}:00Z"),
        "uuid": format!("{id}-{i}"),
        "sessionId": id,
    })
    .to_string()
}

fn tool_use(id: &str, command: &str) -> Value {
    json!([{"type": "tool_use", "id": id, "name": "Bash", "input": {"command": command}}])
}

fn tool_result(id: &str, text: &str) -> Value {
    json!([{"type": "tool_result", "tool_use_id": id, "content": text}])
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

/// A: a person asks, the assistant runs chat-history (whose output is an
/// echo of other sessions) and a shell command whose output is a tool result.
/// C: a term only in tool output. D: the same term in conversation text.
fn fixture() -> TempDir {
    let tmp = TempDir::new().unwrap();
    write_claude(
        tmp.path(),
        ID_A,
        vec![
            ("user", json!("find the flamingo deploy notes")),
            (
                "assistant",
                tool_use("tu1", "chat-history search \"flamingo deploy\" --json"),
            ),
            (
                "user",
                tool_result("tu1", "{\"results\": [{\"snippet\": \"ibisquill notes\"}]}"),
            ),
            ("assistant", tool_use("tu2", "cat deploy.log")),
            (
                "user",
                tool_result("tu2", "pelican stacktrace: connection refused at deploy"),
            ),
            (
                "assistant",
                json!("The pelican failure is a refused connection."),
            ),
            ("user", json!("thanks, that settles it")),
        ],
    );
    write_claude(
        tmp.path(),
        ID_B,
        vec![
            ("user", json!("plan the pelican rollout")),
            (
                "assistant",
                json!("The pelican rollout goes out in three waves."),
            ),
        ],
    );
    write_claude(
        tmp.path(),
        ID_C,
        vec![
            ("user", json!("run the linter")),
            ("assistant", tool_use("tu3", "make lint")),
            (
                "user",
                tool_result("tu3", &"heronbeak warning: unused import\n".repeat(6)),
            ),
            ("assistant", json!("Lint finished.")),
        ],
    );
    write_claude(
        tmp.path(),
        ID_D,
        vec![
            (
                "user",
                json!("why does the service keep failing on startup"),
            ),
            (
                "assistant",
                json!("The heronbeak loader runs before the network is up, so it fails."),
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

fn run(mut cmd: Command, args: &[&str]) -> String {
    let out = cmd.args(args).output().unwrap();
    assert!(
        out.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn stdout(tmp: &TempDir, args: &[&str]) -> String {
    run(cmd(tmp), args)
}

fn search_with(cmd: Command, query: &str, extra: &[&str]) -> Value {
    let mut args = vec!["search", query, "--json"];
    args.extend_from_slice(extra);
    serde_json::from_str(&run(cmd, &args)).unwrap()
}

fn search(tmp: &TempDir, query: &str) -> Value {
    search_with(cmd(tmp), query, &[])
}

fn hits(out: &Value) -> Vec<(String, String, Option<u64>)> {
    let mut all = Vec::new();
    for r in out["results"].as_array().unwrap() {
        let sid = r["session_id"].as_str().unwrap().to_owned();
        all.push((
            sid.clone(),
            r["role"].as_str().unwrap().to_owned(),
            r["ordinal"].as_u64(),
        ));
        for m in r["additional_matches"].as_array().unwrap() {
            all.push((
                sid.clone(),
                m["role"].as_str().unwrap().to_owned(),
                m["ordinal"].as_u64(),
            ));
        }
    }
    all
}

fn session_ids(out: &Value) -> Vec<String> {
    out["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["session_id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn tool_results_are_labelled_tool_not_you() {
    let tmp = fixture();
    let out = stdout(&tmp, &["view", ID_A, "--plain"]);
    assert!(
        out.contains("Tool: pelican stacktrace: connection refused"),
        "{out}"
    );
    assert!(!out.contains("[Tool Result]"), "{out}");
    assert!(out.contains("You: find the flamingo deploy notes"), "{out}");
    assert!(!out.contains("You: pelican"), "{out}");
}

#[test]
fn search_reports_the_tool_role_for_tool_output_hits() {
    let tmp = fixture();
    let found = hits(&search(&tmp, "stacktrace"));
    assert_eq!(found, vec![(ID_A.to_owned(), "tool".to_owned(), Some(4))]);
}

#[test]
fn chat_history_output_is_viewable_but_never_indexed() {
    let tmp = fixture();
    let out = search(&tmp, "ibisquill");
    assert!(out["results"].as_array().unwrap().is_empty(), "{out}");
    // Still part of the transcript.
    let view = stdout(&tmp, &["view", ID_A, "--plain"]);
    assert!(
        view.contains("Tool: {\"results\": [{\"snippet\": \"ibisquill"),
        "{view}"
    );
    // Ordinals after the skipped echo still match `view`.
    let found = hits(&search(&tmp, "refused"));
    assert!(
        found.contains(&(ID_A.to_owned(), "tool".to_owned(), Some(4))),
        "{found:?}"
    );
}

#[test]
fn tool_output_ranks_below_conversation_text() {
    let tmp = fixture();
    let out = search(&tmp, "heronbeak");
    assert_eq!(
        session_ids(&out),
        vec![ID_D.to_owned(), ID_C.to_owned()],
        "{out}"
    );
}

#[test]
fn the_calling_claude_session_is_left_out_of_search() {
    let tmp = fixture();
    let all = session_ids(&search(&tmp, "pelican"));
    assert!(
        all.contains(&ID_A.to_owned()) && all.contains(&ID_B.to_owned()),
        "{all:?}"
    );

    let mut inside = cmd(&tmp);
    inside.env("CLAUDE_CODE_SESSION_ID", ID_A);
    let without = session_ids(&search_with(inside, "pelican", &[]));
    assert_eq!(without, vec![ID_B.to_owned()]);

    // Asking for the session by id still finds it.
    let mut inside = cmd(&tmp);
    inside.env("CLAUDE_CODE_SESSION_ID", ID_A);
    let direct = session_ids(&search_with(inside, ID_A, &[]));
    assert_eq!(direct, vec![ID_A.to_owned()]);
}

#[test]
fn the_calling_session_is_left_out_of_legacy_search_too() {
    let tmp = fixture();
    let mut inside = cmd(&tmp);
    inside.env("CLAUDE_CODE_SESSION_ID", ID_A);
    let out = search_with(inside, "pelican", &["--engine", "legacy", "--deep"]);
    assert!(!session_ids(&out).contains(&ID_A.to_owned()), "{out}");
}

#[test]
fn view_role_keeps_only_the_named_roles() {
    let tmp = fixture();
    let out = stdout(&tmp, &["view", ID_A, "--plain", "--role", "user"]);
    assert_eq!(
        out,
        "[#0] You: find the flamingo deploy notes\n\n…\n\n[#6] You: thanks, that settles it\n\n"
    );
    let out = stdout(&tmp, &["view", ID_A, "--plain", "--role", "user,assistant"]);
    assert!(!out.contains("] Tool: "), "{out}");
    assert!(out.contains("[#5] Claude: The pelican failure"), "{out}");
    let out = stdout(&tmp, &["view", ID_A, "--plain", "--role", "tool"]);
    assert!(out.starts_with("[#2] Tool: "), "{out}");
    assert!(out.contains("[#4] Tool: pelican stacktrace"), "{out}");
}

#[test]
fn view_role_combines_with_grep_and_around() {
    let tmp = fixture();
    // --grep only looks at the selected roles.
    let out = stdout(
        &tmp,
        &[
            "view",
            ID_A,
            "--plain",
            "--role",
            "user",
            "--grep",
            "pelican|thanks",
        ],
    );
    assert_eq!(out, "[#6] You: thanks, that settles it\n\n");
    // Context around a hit counts selected messages, so `--role user -C 1`
    // around the tool hit #4 shows the person's turns on either side.
    let out = stdout(
        &tmp,
        &[
            "view", ID_A, "--plain", "--role", "user", "--around", "4", "-C", "1",
        ],
    );
    assert_eq!(
        out,
        "[#0] You: find the flamingo deploy notes\n\n…\n\n[#6] You: thanks, that settles it\n\n"
    );
}

#[test]
fn view_role_rejects_unknown_roles() {
    let tmp = fixture();
    cmd(&tmp)
        .args(["view", ID_A, "--plain", "--role", "human"])
        .assert()
        .code(2);
}

#[test]
fn inspect_counts_tool_results_separately() {
    let tmp = fixture();
    let out = stdout(&tmp, &["inspect", ID_A]);
    assert!(
        out.contains("messages: 7 (2 user, 3 assistant, 2 tool results)"),
        "{out}"
    );
}

#[test]
fn long_transcripts_are_indexed_and_viewed_to_the_end() {
    let tmp = TempDir::new().unwrap();
    let filler = "lorem ipsum dolor sit amet ".repeat(40_000); // ~1 MB each
    let mut records: Vec<(&str, Value)> = Vec::new();
    for _ in 0..5 {
        records.push(("user", json!("continue")));
        records.push(("assistant", json!(filler.clone())));
    }
    records.push(("user", json!("finally mention the kestrelmark release")));
    write_claude(tmp.path(), ID_A, records);
    let found = hits(&search(&tmp, "kestrelmark"));
    assert_eq!(found, vec![(ID_A.to_owned(), "user".to_owned(), Some(10))]);
    let out = stdout(&tmp, &["view", ID_A, "--plain", "--tail", "1"]);
    assert!(out.contains("kestrelmark"), "{out}");
}

fn write_codex(root: &Path, records: &[Value]) {
    let dir = root.join(".codex/sessions/2026/09/01");
    fs::create_dir_all(&dir).unwrap();
    let mut lines = vec![json!({
        "timestamp": "2026-09-01T10:00:00Z", "type": "session_meta",
        "payload": {"id": CODEX_ID, "cwd": "/Users/test/proj", "timestamp": "2026-09-01T10:00:00Z"}
    })];
    lines.extend_from_slice(records);
    let text: Vec<String> = lines.iter().map(Value::to_string).collect();
    fs::write(
        dir.join(format!("rollout-2026-09-01T10-00-00-{CODEX_ID}.jsonl")),
        text.join("\n"),
    )
    .unwrap();
}

fn codex_item(ts: u32, payload: Value) -> Value {
    json!({"timestamp": format!("2026-09-01T10:00:{ts:02}Z"), "type": "response_item", "payload": payload})
}

fn codex_fixture() -> TempDir {
    let tmp = TempDir::new().unwrap();
    write_codex(
        tmp.path(),
        &[
            codex_item(
                1,
                json!({"type": "message", "role": "user",
                "content": [{"type": "input_text", "text": "check the cormorant service"}]}),
            ),
            codex_item(
                2,
                json!({"type": "function_call", "name": "exec_command", "call_id": "c1",
                "arguments": "{\"cmd\":\"curl -s localhost:8080/health\"}"}),
            ),
            codex_item(
                3,
                json!({"type": "function_call_output", "call_id": "c1",
                "output": "cormorantline degraded: 3 of 5 replicas ready"}),
            ),
            codex_item(
                4,
                json!({"type": "custom_tool_call", "name": "exec", "call_id": "c2",
                "input": "tools.exec_command({cmd:`chat-history search 'cormorant' --json`})"}),
            ),
            codex_item(
                5,
                json!({"type": "custom_tool_call_output", "call_id": "c2",
                "output": [{"type": "input_text", "text": "grebewing results from another session"}]}),
            ),
            codex_item(
                6,
                json!({"type": "message", "role": "assistant", "phase": "final_answer",
                "content": [{"type": "output_text", "text": "Two replicas are not ready."}]}),
            ),
        ],
    );
    tmp
}

#[test]
fn codex_tool_output_is_indexed_as_tool() {
    let tmp = codex_fixture();
    let found = hits(&search(&tmp, "cormorantline"));
    assert_eq!(
        found,
        vec![(CODEX_ID.to_owned(), "tool".to_owned(), Some(1))]
    );
    let out = stdout(&tmp, &["view", CODEX_ID, "--plain"]);
    assert_eq!(
        out,
        "You: check the cormorant service\n\n\
         Tool: cormorantline degraded: 3 of 5 replicas ready\n\n\
         Tool: grebewing results from another session\n\n\
         Claude: Two replicas are not ready.\n\n"
    );
}

#[test]
fn codex_chat_history_output_is_not_indexed() {
    let tmp = codex_fixture();
    let out = search(&tmp, "grebewing");
    assert!(out["results"].as_array().unwrap().is_empty(), "{out}");
}

#[test]
fn codex_tools_still_attach_to_the_answer() {
    let tmp = codex_fixture();
    let out = stdout(&tmp, &["inspect", CODEX_ID]);
    assert!(out.contains("exec_command"), "{out}");
    assert!(out.contains("exec"), "{out}");
}

#[test]
fn the_calling_codex_thread_is_left_out_of_search() {
    let tmp = codex_fixture();
    let mut inside = cmd(&tmp);
    inside.env("CODEX_THREAD_ID", CODEX_ID);
    let out = search_with(inside, "cormorant", &[]);
    assert!(out["results"].as_array().unwrap().is_empty(), "{out}");
}

#[test]
fn the_calling_cursor_conversation_is_left_out_of_search() {
    let tmp = TempDir::new().unwrap();
    const CURSOR_ID: &str = "0f0f0f0f-1111-2222-3333-444444444444";
    let path = tmp.path().join(format!(
        ".cursor/projects/test-workspace/agent-transcripts/{CURSOR_ID}/{CURSOR_ID}.jsonl"
    ));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let lines = [
        json!({"role": "user", "message": {"content": [{"type": "text", "text": "<user_query>find the osprey notes</user_query>"}]}}),
        json!({"role": "assistant", "message": {"content": [{"type": "text", "text": "Searching for osprey."}]}}),
    ];
    let text: Vec<String> = lines.iter().map(Value::to_string).collect();
    fs::write(&path, text.join("\n")).unwrap();
    assert_eq!(
        session_ids(&search(&tmp, "osprey")),
        vec![CURSOR_ID.to_owned()]
    );
    let mut inside = cmd(&tmp);
    inside.env("CURSOR_CONVERSATION_ID", CURSOR_ID);
    let out = search_with(inside, "osprey", &[]);
    assert!(out["results"].as_array().unwrap().is_empty(), "{out}");
}

#[test]
fn a_tool_result_without_text_keeps_its_position() {
    let tmp = TempDir::new().unwrap();
    write_claude(
        tmp.path(),
        ID_A,
        vec![
            ("user", json!("take a screenshot")),
            ("assistant", tool_use("tu1", "screencapture shot.png")),
            (
                "user",
                json!([{"type": "tool_result", "tool_use_id": "tu1", "content": [{"type": "image", "source": {}}]}]),
            ),
            (
                "assistant",
                json!("The screenshot shows the albatross dialog."),
            ),
        ],
    );
    let out = stdout(&tmp, &["view", ID_A, "--plain", "-n"]);
    assert!(out.contains("[#2] Tool: (no text)"), "{out}");
    assert!(out.contains("[#3] Claude: The screenshot"), "{out}");
    let found = hits(&search(&tmp, "albatross"));
    assert_eq!(
        found,
        vec![(ID_A.to_owned(), "assistant".to_owned(), Some(3))]
    );
}
