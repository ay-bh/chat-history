//! Codex history as current Codex versions leave it on disk: finished
//! rollouts compressed to `.jsonl.zst`, desktop-app threads continued in a
//! second file under the same id, many sessions started within a minute of
//! each other, and conversations resumed on a later day.

use assert_cmd::Command;
use predicates::prelude::*;
use std::{fs, path::Path};
use tempfile::TempDir;

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

/// A rollout's lines: session_meta for `id` started at `start`, then one
/// exchange per (time, prompt, answer).
fn rollout(id: &str, start: &str, turns: &[(&str, &str, &str)]) -> String {
    let mut lines = vec![format!(
        r#"{{"timestamp":"{start}","type":"session_meta","payload":{{"id":"{id}","timestamp":"{start}","cwd":"/work/proj","originator":"codex-tui","cli_version":"0.163.0"}}}}"#
    )];
    for (at, prompt, answer) in turns {
        lines.push(format!(
            r#"{{"timestamp":"{at}","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"{prompt}"}}]}}}}"#
        ));
        lines.push(format!(
            r#"{{"timestamp":"{at}","type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"{answer}"}}]}}}}"#
        ));
    }
    lines.join("\n") + "\n"
}

fn day_dir(h: &Path, ymd: &str) -> std::path::PathBuf {
    let d = h.join(".codex/sessions").join(ymd.replace('-', "/"));
    fs::create_dir_all(&d).unwrap();
    d
}

fn write_compressed(path: &Path, text: &str) {
    let bytes = ruzstd::encoding::compress_to_vec(
        text.as_bytes(),
        ruzstd::encoding::CompressionLevel::Fastest,
    );
    fs::write(path, bytes).unwrap();
}

#[test]
fn compressed_rollouts_are_listed_searched_and_viewed() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    let id = "01a15dc5-a1b0-7681-9259-33dd84b6f672";
    let file = day_dir(h, "2026-10-21").join(format!("rollout-2026-10-21T14-53-15-{id}.jsonl.zst"));
    write_compressed(
        &file,
        &rollout(
            id,
            "2026-10-21T09:23:15.000Z",
            &[(
                "2026-10-21T09:23:20.000Z",
                "tune the zebrafish ledger import",
                "The zebrafish ledger import now batches rows.",
            )],
        ),
    );

    cmd(h)
        .args(["--source", "codex"])
        .assert()
        .success()
        .stdout(predicate::str::contains("tune the zebrafish ledger import"));
    cmd(h)
        .args(["search", "zebrafish", "--compact"])
        .assert()
        .success()
        .stdout(predicate::str::contains("01a15dc5"));
    cmd(h)
        .args(["view", id, "--plain"])
        .assert()
        .success()
        .stdout(predicate::str::contains("now batches rows"));
    cmd(h)
        .args(["find", id])
        .assert()
        .success()
        .stdout(predicate::str::contains(".jsonl.zst"));
}

#[test]
fn a_rollout_being_compressed_is_listed_once() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    let id = "01a15dc5-a1b0-7681-9259-33dd84b6f672";
    let text = rollout(
        id,
        "2026-10-21T09:23:15.000Z",
        &[(
            "2026-10-21T09:23:20.000Z",
            "only once please",
            "Listed once.",
        )],
    );
    let dir = day_dir(h, "2026-10-21");
    let base = format!("rollout-2026-10-21T14-53-15-{id}.jsonl");
    fs::write(dir.join(&base), &text).unwrap();
    write_compressed(&dir.join(format!("{base}.zst")), &text);
    cmd(h)
        .args(["--source", "codex"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 sessions"));
}

#[test]
fn a_continuation_page_of_a_codex_thread_opens_under_its_own_id() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    let thread = "01a181ea-eb4d-7b32-b28d-a34666d8653d";
    let page = "01a181ee-9fd1-7b62-a9bc-2140a4c8b116";
    let dir = day_dir(h, "2026-10-28");
    fs::write(
        dir.join(format!("rollout-2026-10-28T15-20-18-{thread}.jsonl")),
        rollout(
            thread,
            "2026-10-28T09:50:18.000Z",
            &[(
                "2026-10-28T09:50:20.000Z",
                "first page question",
                "First page answer.",
            )],
        ),
    )
    .unwrap();
    // The desktop app's paginated history: same thread id in the header.
    fs::write(
        dir.join(format!("rollout-2026-10-28T15-24-21-{thread}_{page}.jsonl")),
        rollout(
            thread,
            "2026-10-28T09:54:21.000Z",
            &[(
                "2026-10-28T09:54:24.000Z",
                "second page question",
                "Second page answer.",
            )],
        ),
    )
    .unwrap();

    cmd(h)
        .args(["--source", "codex"])
        .assert()
        .success()
        .stdout(predicate::str::contains("2 sessions"))
        .stdout(predicate::str::contains("01a181ea"))
        .stdout(predicate::str::contains("01a181ee"));
    cmd(h)
        .args(["view", thread, "--plain"])
        .assert()
        .success()
        .stdout(predicate::str::contains("First page answer."))
        .stdout(predicate::str::contains("Second page").not());
    cmd(h)
        .args(["view", page, "--plain"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Second page answer."));
    // A search hit on the second page opens.
    let out = cmd(h)
        .args(["search", "second page question", "--json", "--limit", "1"])
        .output()
        .unwrap();
    let hits: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let id = hits["results"][0]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(id, page);
    cmd(h).args(["inspect", &id, "--brief"]).assert().success();

    // Searching from inside that thread leaves out all of its pages.
    cmd(h)
        .env("CODEX_THREAD_ID", thread)
        .args(["search", "second page question", "--compact"])
        .assert()
        .success()
        .stdout(predicate::str::contains("01a181e").not());
}

/// Codex ids are time-ordered (UUIDv7): sessions started within about a
/// minute share their first eight characters.
#[test]
fn displayed_ids_are_long_enough_to_reopen_their_session() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    let dir = day_dir(h, "2026-10-30");
    for (id, word) in [
        ("01a18bda-48db-73b1-91f0-fb1383b4a17d", "aardvark"),
        ("01a18bda-e676-73a3-95be-6e386f8a68f1", "bandicoot"),
    ] {
        fs::write(
            dir.join(format!("rollout-2026-10-30T13-40-00-{id}.jsonl")),
            rollout(
                id,
                "2026-10-30T08:10:00.000Z",
                &[(
                    "2026-10-30T08:10:05.000Z",
                    &format!("ask about the {word}"),
                    "Answered.",
                )],
            ),
        )
        .unwrap();
    }
    let list = cmd(h).args(["--source", "codex"]).output().unwrap();
    let list = String::from_utf8(list.stdout).unwrap();
    assert!(
        list.contains("01a18bda-4") && list.contains("01a18bda-e"),
        "{list}"
    );
    let compact = cmd(h)
        .args(["search", "aardvark", "--compact"])
        .output()
        .unwrap();
    let shown = String::from_utf8(compact.stdout).unwrap();
    let short = shown.split_whitespace().next().unwrap().to_owned();
    assert_eq!(short, "01a18bda-4", "{shown}");
    cmd(h)
        .args(["view", &short, "--plain"])
        .assert()
        .success()
        .stdout(predicate::str::contains("aardvark"));
    cmd(h)
        .args(["inspect", &short, "--brief"])
        .assert()
        .success()
        .stdout(predicate::str::contains("01a18bda-4"));
}

#[test]
fn a_conversation_resumed_on_a_later_day_lists_on_the_day_it_started_and_the_day_it_was_last_active()
 {
    let home = TempDir::new().unwrap();
    let h = home.path();
    let id = "01a17cd4-0000-7000-8000-000000000001";
    let file = day_dir(h, "2026-10-28").join(format!("rollout-2026-10-28T10-00-00-{id}.jsonl"));
    fs::write(
        &file,
        rollout(
            id,
            "2026-10-28T10:00:00.000Z",
            &[
                (
                    "2026-10-28T10:00:05.000Z",
                    "start the walrus report",
                    "Started.",
                ),
                (
                    "2026-10-30T09:00:00.000Z",
                    "finish the walrus report",
                    "Finished.",
                ),
            ],
        ),
    )
    .unwrap();
    // The file was last written when the conversation was resumed.
    let resumed = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_793_350_800); // 2026-10-30T09:00:00Z
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(resumed)
        .unwrap();

    for (from, to) in [
        ("2026-10-28", "2026-10-28"),
        ("2026-10-30", "2026-10-30"),
        ("2026-10-27", "2026-10-29"),
        ("2026-10-29", "2026-10-31"),
    ] {
        cmd(h)
            .args(["--source", "codex", "--from", from, "--to", to])
            .assert()
            .success()
            .stdout(predicate::str::contains("walrus report"));
    }
    // Nothing happened on the 29th.
    for (from, to) in [
        ("2026-10-29", "2026-10-29"),
        ("2026-10-26", "2026-10-27"),
        ("2026-10-31", "2026-11-01"),
    ] {
        cmd(h)
            .args(["--source", "codex", "--from", from, "--to", to])
            .assert()
            .success()
            .stdout(predicate::str::contains("walrus report").not());
    }
}

#[test]
fn a_compressed_rollout_counts_only_on_the_day_it_started() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    let id = "01a00000-0000-7000-8000-000000000009";
    let file = day_dir(h, "2026-09-01").join(format!("rollout-2026-09-01T10-00-00-{id}.jsonl.zst"));
    write_compressed(
        &file,
        &rollout(
            id,
            "2026-09-01T10:00:00.000Z",
            &[("2026-09-01T10:00:05.000Z", "the september narwhal", "Done.")],
        ),
    );
    // Compressed weeks later: the file time says nothing about activity.
    let compressed = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_793_350_800); // 2026-10-30
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(compressed)
        .unwrap();
    cmd(h)
        .args([
            "--source",
            "codex",
            "--from",
            "2026-10-30",
            "--to",
            "2026-10-30",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("narwhal").not());
    cmd(h)
        .args([
            "--source",
            "codex",
            "--from",
            "2026-09-01",
            "--to",
            "2026-09-01",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("narwhal"));
}

#[test]
fn archived_codex_threads_are_still_history() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    let id = "01a158b9-0c85-7ce0-94eb-879fa19bacbc";
    let dir = h.join(".codex/archived_sessions");
    fs::create_dir_all(&dir).unwrap();
    write_compressed(
        &dir.join(format!("rollout-2026-10-20T15-21-26-{id}.jsonl.zst")),
        &rollout(
            id,
            "2026-10-20T09:51:26.000Z",
            &[(
                "2026-10-20T09:51:30.000Z",
                "the archived okapi plan",
                "Archived.",
            )],
        ),
    );
    cmd(h)
        .args(["--source", "codex"])
        .assert()
        .success()
        .stdout(predicate::str::contains("the archived okapi plan"));
    cmd(h)
        .args(["view", id, "--plain"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Archived."));
}

/// Codex names its threads (the names its sidebar shows) in its state
/// database.
#[test]
fn codex_thread_titles_are_shown_and_searched() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    let id = "01a17e42-0000-7000-8000-000000000042";
    fs::write(
        day_dir(h, "2026-10-27").join(format!("rollout-2026-10-27T10-00-00-{id}.jsonl")),
        rollout(
            id,
            "2026-10-27T04:30:00.000Z",
            &[("2026-10-27T04:30:05.000Z", "please look at this", "Looked.")],
        ),
    )
    .unwrap();
    let db = rusqlite::Connection::open(h.join(".codex/state_5.sqlite")).unwrap();
    db.execute_batch(&format!(
        "CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT, title TEXT, archived INTEGER);
         INSERT INTO threads VALUES ('{id}', '', 'Quokka pipeline review', 0);"
    ))
    .unwrap();
    drop(db);
    cmd(h)
        .args(["--source", "codex"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Quokka pipeline review"));
    cmd(h)
        .args(["search", "quokka", "--compact"])
        .assert()
        .success()
        .stdout(predicate::str::contains("01a17e42"));
}
