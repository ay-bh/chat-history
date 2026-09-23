//! Search hits point at a message (`ordinal`, `timestamp`) and `view` reads a
//! bounded slice of a transcript around it, instead of agents piping whole
//! transcripts through grep/sed/head.
use assert_cmd::Command;
use serde_json::{Value, json};
use std::fs;
use tempfile::TempDir;

const ID: &str = "aaaaaaaa-1111-2222-3333-bbbbbbbbbbbb";

const TEXTS: [(&str, &str); 8] = [
    ("user", "please look at the zebracorn parser module"),
    (
        "assistant",
        "I read the parser module and found the entry point.",
    ),
    ("user", "now check the build pipeline"),
    ("assistant", "The build pipeline passes on main."),
    ("user", "what about the quokkafield timeouts in production"),
    ("assistant", "LONG"),
    ("user", "ok ship it"),
    (
        "assistant",
        "Shipped the zebracorn fix and closed the ticket.",
    ),
];

fn long_answer() -> String {
    format!(
        "Quokkafield timeouts come from the retry loop. {}",
        "The backoff doubles on every attempt without a cap. ".repeat(8)
    )
}

fn fixture() -> TempDir {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join(".claude/projects/-Users-test-proj");
    fs::create_dir_all(&dir).unwrap();
    let lines: Vec<String> = TEXTS
        .iter()
        .enumerate()
        .map(|(i, (role, text))| {
            let text = if *text == "LONG" {
                long_answer()
            } else {
                (*text).to_owned()
            };
            json!({
                "type": role,
                "cwd": "/Users/test/proj",
                "message": {"role": role, "content": text},
                "timestamp": format!("2026-09-01T10:0{i}:00Z"),
                "uuid": format!("u{i}"),
                "sessionId": ID,
            })
            .to_string()
        })
        .collect();
    fs::write(dir.join(format!("{ID}.jsonl")), lines.join("\n")).unwrap();
    tmp
}

fn cmd(tmp: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("chat-history").unwrap();
    cmd.env("HOME", tmp.path())
        .env("CLAUDE_CONFIG_DIR", tmp.path().join(".claude"))
        .env("CURSOR_USER_DIR", tmp.path().join("no-cursor"))
        .env("CHAT_HISTORY_CACHE_DIR", tmp.path().join("cache"))
        .env("NO_COLOR", "1")
        .env_remove("CODEX_HOME")
        .env_remove("CLAUDE_CODE_SESSION_ID");
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

fn search(tmp: &TempDir, args: &[&str]) -> Value {
    let mut all = vec!["search"];
    all.extend_from_slice(args);
    all.push("--json");
    serde_json::from_str(&stdout(tmp, &all)).unwrap()
}

#[test]
fn search_hits_carry_the_message_ordinal_and_timestamp() {
    let tmp = fixture();
    let out = search(&tmp, &["quokkafield"]);
    let hit = &out["results"][0];
    assert_eq!(hit["session_id"], ID);
    let ordinal = hit["ordinal"]
        .as_u64()
        .expect("ordinal on a transcript hit");
    assert!(ordinal == 4 || ordinal == 5, "{hit}");
    assert_eq!(
        hit["timestamp"],
        format!("2026-09-01T10:0{ordinal}:00Z"),
        "{hit}"
    );
    for extra in hit["additional_matches"].as_array().unwrap() {
        let o = extra["ordinal"].as_u64().expect("ordinal on extra matches");
        assert_eq!(extra["timestamp"], format!("2026-09-01T10:0{o}:00Z"));
    }
}

#[test]
fn legacy_engine_hits_carry_the_same_ordinal() {
    let tmp = fixture();
    let out = search(&tmp, &["quokkafield", "--engine", "legacy", "--deep"]);
    let ordinals: Vec<u64> = out["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["ordinal"].as_u64())
        .collect();
    assert!(ordinals.contains(&4) || ordinals.contains(&5), "{out}");
}

#[test]
fn view_around_a_hit_ordinal_shows_that_message() {
    let tmp = fixture();
    let hit = &search(&tmp, &["quokkafield"])["results"][0];
    let ordinal = hit["ordinal"].as_u64().unwrap().to_string();
    let out = stdout(
        &tmp,
        &[
            "view",
            ID,
            "--plain",
            "--around",
            &ordinal,
            "--context",
            "0",
        ],
    );
    assert!(out.to_lowercase().contains("quokkafield"), "{out}");
    assert!(out.contains(&format!("[#{ordinal}]")), "{out}");
    assert!(!out.contains("zebracorn"), "only the hit message: {out}");
}

#[test]
fn view_around_includes_context_messages_on_each_side() {
    let tmp = fixture();
    let out = stdout(
        &tmp,
        &["view", ID, "--plain", "--around", "3", "--context", "1"],
    );
    for n in [2, 3, 4] {
        assert!(out.contains(&format!("[#{n}]")), "missing #{n}: {out}");
    }
    for n in [1, 5] {
        assert!(!out.contains(&format!("[#{n}]")), "unexpected #{n}: {out}");
    }
    assert!(out.contains("build pipeline"));
}

#[test]
fn view_around_clamps_at_the_transcript_edges() {
    let tmp = fixture();
    let out = stdout(
        &tmp,
        &["view", ID, "--plain", "--around", "0", "--context", "2"],
    );
    for n in [0, 1, 2] {
        assert!(out.contains(&format!("[#{n}]")), "{out}");
    }
    assert!(!out.contains("[#3]"), "{out}");
}

#[test]
fn view_around_past_the_end_fails_with_the_valid_range() {
    let tmp = fixture();
    cmd(&tmp)
        .args(["view", ID, "--plain", "--around", "99"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("0..=7"));
}

#[test]
fn view_grep_shows_matching_messages_case_insensitively_with_gaps_marked() {
    let tmp = fixture();
    let out = stdout(
        &tmp,
        &[
            "view",
            ID,
            "--plain",
            "--grep",
            "ZEBRACORN",
            "--context",
            "0",
        ],
    );
    assert!(out.contains("[#0]") && out.contains("[#7]"), "{out}");
    for n in 1..=6 {
        assert!(!out.contains(&format!("[#{n}]")), "unexpected #{n}: {out}");
    }
    assert!(
        out.contains("…"),
        "a gap between #0 and #7 is marked: {out}"
    );
}

#[test]
fn view_grep_accepts_a_regex_and_merges_overlapping_context() {
    let tmp = fixture();
    let out = stdout(
        &tmp,
        &[
            "view",
            ID,
            "--plain",
            "--grep",
            "build|quokka",
            "--context",
            "1",
        ],
    );
    // matches #2, #3, #4, #5 -> context 1..=6, one contiguous block
    for n in 1..=6 {
        assert!(out.contains(&format!("[#{n}]")), "missing #{n}: {out}");
    }
    assert!(!out.contains("[#0]") && !out.contains("[#7]"), "{out}");
    assert!(!out.contains('…'), "contiguous, no gap marker: {out}");
}

#[test]
fn view_grep_with_no_match_says_so_on_stderr() {
    let tmp = fixture();
    let out = cmd(&tmp)
        .args(["view", ID, "--plain", "--grep", "nothing-matches-this"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).trim().is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("No messages match"));
}

#[test]
fn view_rejects_an_invalid_grep_pattern() {
    let tmp = fixture();
    cmd(&tmp)
        .args(["view", ID, "--plain", "--grep", "("])
        .assert()
        .code(2);
}

#[test]
fn view_head_and_tail_limit_the_message_count() {
    let tmp = fixture();
    let tail = stdout(&tmp, &["view", ID, "--plain", "--tail", "2"]);
    assert!(tail.contains("[#6]") && tail.contains("[#7]"), "{tail}");
    assert!(!tail.contains("[#5]"), "{tail}");
    let head = stdout(&tmp, &["view", ID, "--plain", "--head", "2"]);
    assert!(head.contains("[#0]") && head.contains("[#1]"), "{head}");
    assert!(!head.contains("[#2]"), "{head}");
}

#[test]
fn view_tail_applies_after_grep() {
    let tmp = fixture();
    let out = stdout(
        &tmp,
        &[
            "view",
            ID,
            "--plain",
            "--grep",
            "zebracorn",
            "--context",
            "0",
            "--tail",
            "1",
        ],
    );
    assert!(out.contains("[#7]") && !out.contains("[#0]"), "{out}");
}

#[test]
fn view_max_chars_truncates_each_message_and_says_how_much_was_cut() {
    let tmp = fixture();
    let out = stdout(
        &tmp,
        &[
            "view",
            ID,
            "--plain",
            "--around",
            "5",
            "--context",
            "0",
            "--max-chars",
            "40",
        ],
    );
    let cut = long_answer().chars().count() - 40;
    assert!(out.contains(&format!("[+{cut} chars]")), "{out}");
    assert!(!out.contains("without a cap. The backoff"), "{out}");
}

#[test]
fn plain_view_without_new_flags_is_unchanged() {
    let tmp = fixture();
    let out = stdout(&tmp, &["view", ID, "--plain"]);
    assert!(!out.contains("[#"), "no numbering unless asked: {out}");
    assert!(out.starts_with("You: please look at the zebracorn parser module\n\n"));
}

#[test]
fn number_flag_numbers_a_full_view() {
    let tmp = fixture();
    let out = stdout(&tmp, &["view", ID, "--plain", "-n"]);
    for n in 0..8 {
        assert!(out.contains(&format!("[#{n}]")), "{out}");
    }
}

#[test]
fn rich_view_honours_the_same_selection() {
    let tmp = fixture();
    let out = stdout(&tmp, &["view", ID, "--around", "4", "--context", "0"]);
    assert!(out.contains("#4"), "{out}");
    assert!(out.contains("quokkafield"), "{out}");
    // The header may repeat the first prompt as the title; bodies are bounded.
    assert!(!out.contains("build pipeline"), "{out}");
}

#[test]
fn view_grep_shows_only_matching_messages_by_default() {
    let tmp = fixture();
    let out = stdout(&tmp, &["view", ID, "--plain", "--grep", "quokkafield"]);
    assert!(out.contains("[#4]") && out.contains("[#5]"), "{out}");
    assert!(!out.contains("[#3]") && !out.contains("[#6]"), "{out}");
}

#[test]
fn view_around_keeps_two_messages_of_context_by_default() {
    let tmp = fixture();
    let out = stdout(&tmp, &["view", ID, "--plain", "--around", "4"]);
    for n in 2..=6 {
        assert!(out.contains(&format!("[#{n}]")), "missing #{n}: {out}");
    }
}

#[test]
fn view_grep_max_chars_centres_the_excerpt_on_the_match() {
    let tmp = fixture();
    // "cap" first appears ~95 chars into message #5, past a 40-char head.
    let out = stdout(
        &tmp,
        &[
            "view",
            ID,
            "--plain",
            "--grep",
            "without a cap",
            "--max-chars",
            "40",
        ],
    );
    assert!(out.contains("[#5]"), "{out}");
    assert!(
        out.contains("without a cap"),
        "the match must survive the cut: {out}"
    );
    assert!(out.contains("chars] "), "a leading cut is marked: {out}");
    assert!(out.contains("[+"), "a trailing cut is marked: {out}");
}

#[test]
fn view_grep_anchors_match_at_any_line_start_like_grep() {
    // Agents pipe `view --plain | grep '^## '` to find headings inside long
    // answers; `^` must match every line start, not only the message start.
    let tmp = fixture();
    let dir = tmp.path().join(".claude/projects/-Users-test-proj");
    let id = "cccccccc-1111-2222-3333-dddddddddddd";
    let lines = [
        json!({"type": "user", "cwd": "/Users/test/proj", "message": {"role": "user", "content": "compare the options"},
               "timestamp": "2026-09-02T10:00:00Z", "uuid": "a0", "sessionId": id}),
        json!({"type": "assistant", "message": {"role": "assistant", "content": "Here is the comparison.\n## Option A\ncheap\n## Option B\nfast"},
               "timestamp": "2026-09-02T10:01:00Z", "uuid": "a1", "sessionId": id}),
    ];
    fs::write(
        dir.join(format!("{id}.jsonl")),
        lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let out = stdout(&tmp, &["view", id, "--plain", "--grep", "^## option"]);
    assert!(out.contains("[#1]") && out.contains("## Option B"), "{out}");
}

#[test]
fn a_huge_context_is_clamped_instead_of_overflowing() {
    let tmp = fixture();
    let out = stdout(
        &tmp,
        &[
            "view",
            ID,
            "--plain",
            "--around",
            "1",
            "-C",
            &usize::MAX.to_string(),
        ],
    );
    for n in 0..8 {
        assert!(out.contains(&format!("[#{n}]")), "{out}");
    }
}

#[test]
fn a_huge_grep_context_is_clamped_instead_of_overflowing() {
    let tmp = fixture();
    let out = stdout(
        &tmp,
        &[
            "view",
            ID,
            "--plain",
            "--grep",
            "zebracorn",
            "-C",
            &usize::MAX.to_string(),
        ],
    );
    for n in 0..8 {
        assert!(out.contains(&format!("[#{n}]")), "{out}");
    }
}

#[test]
fn grep_head_counts_matches_so_a_match_is_never_cut_off() {
    let tmp = fixture();
    // zebracorn matches #0 and #7; with context 2 the first window is 0..=2.
    let out = stdout(
        &tmp,
        &[
            "view",
            ID,
            "--plain",
            "--grep",
            "zebracorn",
            "-C",
            "2",
            "--head",
            "1",
        ],
    );
    assert!(out.contains("[#0]"), "{out}");
    assert!(!out.contains("[#7]"), "only the first match: {out}");
    // quokkafield matches #4 and #5; the last match keeps its leading context.
    let out = stdout(
        &tmp,
        &[
            "view",
            ID,
            "--plain",
            "--grep",
            "quokkafield",
            "-C",
            "1",
            "--tail",
            "1",
        ],
    );
    assert!(
        out.contains("[#5]") && out.contains("[#4]") && out.contains("[#6]"),
        "{out}"
    );
    assert!(!out.contains("[#3]"), "{out}");
}

#[test]
fn head_or_tail_of_zero_is_a_usage_error_not_a_false_no_match() {
    let tmp = fixture();
    for flag in ["--head", "--tail"] {
        cmd(&tmp)
            .args(["view", ID, "--plain", "--grep", "zebracorn", flag, "0"])
            .assert()
            .code(2);
    }
}

#[test]
fn a_message_without_text_is_shown_as_a_stub_in_bounded_views() {
    let tmp = fixture();
    let dir = tmp.path().join(".claude/projects/-Users-test-proj");
    let id = "eeeeeeee-1111-2222-3333-ffffffffffff";
    let lines = [
        json!({"type": "user", "cwd": "/Users/test/proj", "message": {"role": "user", "content": "think about it"},
               "timestamp": "2026-09-03T10:00:00Z", "uuid": "e0", "sessionId": id}),
        json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "thinking", "thinking": "hmm"}]},
               "timestamp": "2026-09-03T10:01:00Z", "uuid": "e1", "sessionId": id}),
        json!({"type": "assistant", "message": {"role": "assistant", "content": "Here is the answer."},
               "timestamp": "2026-09-03T10:02:00Z", "uuid": "e2", "sessionId": id}),
    ];
    fs::write(
        dir.join(format!("{id}.jsonl")),
        lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let out = stdout(&tmp, &["view", id, "--plain", "--around", "1", "-C", "0"]);
    assert!(out.contains("[#1] Claude: (no text)"), "{out}");
    let out = stdout(&tmp, &["view", id, "--plain", "--around", "1"]);
    for n in 0..3 {
        assert!(out.contains(&format!("[#{n}]")), "no silent gap: {out}");
    }
    // A plain full view is unchanged: empty messages stay skipped.
    let out = stdout(&tmp, &["view", id, "--plain"]);
    assert!(!out.contains("(no text)"), "{out}");
}
