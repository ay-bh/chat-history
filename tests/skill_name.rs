//! The agent skill is installed as `history` (or a name the user picks) into
//! `~/.claude/skills` for Claude Code and `~/.agents/skills` for Codex and
//! Cursor. The old name `chat-history` stays typeable as a stub that only runs
//! when invoked by name and points to the new one.

use assert_cmd::Command;
use predicates::prelude::*;
use std::{fs, path::Path};
use tempfile::TempDir;

fn cmd(home: &Path) -> Command {
    let mut c = Command::cargo_bin("chat-history").unwrap();
    c.env("HOME", home)
        .env_remove("USERPROFILE")
        .env_remove("CODEX_HOME")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env("CHAT_HISTORY_CACHE_DIR", home.join("cache"))
        .env("NO_COLOR", "1");
    c
}

fn read(path: impl AsRef<Path>) -> String {
    fs::read_to_string(path).unwrap()
}

/// A copy written by 0.8: the skill text plus the sidecar holding its hash.
fn managed_old_copy(dir: &Path, text: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("SKILL.md"), text).unwrap();
    fs::write(
        dir.join(".chat-history-managed"),
        chat_history::skill_install::content_hash(text),
    )
    .unwrap();
}

#[test]
fn a_fresh_install_is_named_history_in_two_places_with_a_name_only_stub() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    cmd(h).arg("install-skill").assert().success();
    for root in [".claude/skills", ".agents/skills"] {
        let skill = read(h.join(root).join("history/SKILL.md"));
        assert!(skill.contains("\nname: history\n"), "{root}: {skill}");
        assert!(!skill.contains("disable-model-invocation"));
        let stub = read(h.join(root).join("chat-history/SKILL.md"));
        assert!(stub.contains("\nname: chat-history\n"), "{stub}");
        assert!(stub.contains("disable-model-invocation: true"), "{stub}");
        assert!(stub.contains("deprecated"), "{stub}");
        assert!(stub.contains("/history"), "{stub}");
    }
    assert!(
        read(h.join(".agents/skills/chat-history/agents/openai.yaml"))
            .contains("allow_implicit_invocation: false")
    );
    assert!(
        !h.join(".cursor/skills").exists(),
        "Cursor reads ~/.agents/skills"
    );
    assert!(
        !h.join(".codex/skills").exists(),
        "a deprecated Codex location"
    );
}

#[test]
fn an_upgrade_retires_old_managed_copies_and_leaves_edited_ones() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    let old = "---\nname: chat-history\ndescription: old\n---\nold skill\n";
    managed_old_copy(&h.join(".claude/skills/chat-history"), old);
    managed_old_copy(&h.join(".codex/skills/chat-history"), old);
    // Edited by the user after we installed it: the sidecar no longer matches.
    managed_old_copy(&h.join(".cursor/skills/chat-history"), old);
    fs::write(
        h.join(".cursor/skills/chat-history/SKILL.md"),
        "my own notes",
    )
    .unwrap();

    // Any ordinary command refreshes the skills quietly.
    cmd(h).arg("--version").assert().success();
    cmd(h).args(["--from", "today"]).assert().success();

    assert!(read(h.join(".claude/skills/history/SKILL.md")).contains("name: history"));
    assert!(
        read(h.join(".claude/skills/chat-history/SKILL.md")).contains("deprecated"),
        "our old copy became the stub"
    );
    assert!(
        !h.join(".codex/skills/chat-history").exists(),
        "our unedited copy at the deprecated Codex location is gone"
    );
    assert_eq!(
        read(h.join(".cursor/skills/chat-history/SKILL.md")),
        "my own notes",
        "an edited copy is never touched"
    );
}

#[test]
fn the_user_can_choose_the_name_and_it_survives_upgrades() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    cmd(h).arg("install-skill").assert().success();
    cmd(h)
        .args(["install-skill", "--name", "past-chats"])
        .assert()
        .success()
        .stdout(predicate::str::contains("/past-chats"))
        .stdout(predicate::str::contains("$past-chats"));
    for root in [".claude/skills", ".agents/skills"] {
        let skill = read(h.join(root).join("past-chats/SKILL.md"));
        assert!(skill.contains("\nname: past-chats\n"), "{skill}");
        assert!(
            !h.join(root).join("history").exists(),
            "the previous name is retired"
        );
        assert!(read(h.join(root).join("chat-history/SKILL.md")).contains("/past-chats"));
    }
    // A later run, as after an upgrade, keeps the chosen name.
    cmd(h).args(["--from", "today"]).assert().success();
    assert!(h.join(".claude/skills/past-chats/SKILL.md").exists());
    assert!(!h.join(".claude/skills/history").exists());
}

#[test]
fn choosing_chat_history_keeps_the_old_name_without_a_stub() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    cmd(h)
        .args(["install-skill", "--name", "chat-history"])
        .assert()
        .success();
    let skill = read(h.join(".claude/skills/chat-history/SKILL.md"));
    assert!(skill.contains("\nname: chat-history\n"));
    assert!(
        !skill.contains("disable-model-invocation"),
        "the real skill, not the stub"
    );
    assert!(!h.join(".claude/skills/history").exists());
}

#[test]
fn names_that_break_the_rules_or_shadow_a_built_in_command_are_refused() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    for bad in [
        "Resume",
        "my_skill",
        "-x",
        "a--b",
        "resume",
        "export",
        "synced",
        "claude-notes",
    ] {
        cmd(h)
            .args(["install-skill", "--name", bad])
            .assert()
            .failure()
            .stderr(predicate::str::contains(bad));
    }
    assert!(!h.join(".claude/skills").exists(), "nothing was written");
}
