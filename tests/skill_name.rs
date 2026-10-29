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

/// What 0.8 installed: the same managed copy in three places.
fn old_layout(h: &Path) {
    let old = "---\nname: chat-history\ndescription: old\n---\nold skill\n";
    for root in [".claude/skills", ".cursor/skills", ".codex/skills"] {
        managed_old_copy(&h.join(root).join("chat-history"), old);
    }
}

#[test]
fn a_fresh_install_is_named_history_in_two_places_without_the_old_name() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    cmd(h).arg("install-skill").assert().success();
    for root in [".claude/skills", ".agents/skills"] {
        let skill = read(h.join(root).join("history/SKILL.md"));
        assert!(skill.contains("\nname: history\n"), "{root}: {skill}");
        assert!(!skill.contains("disable-model-invocation"));
        assert!(
            !h.join(root).join("chat-history").exists(),
            "a new user never had /chat-history, so no stub"
        );
    }
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
    for root in [".claude/skills", ".agents/skills"] {
        let stub = read(h.join(root).join("chat-history/SKILL.md"));
        assert!(stub.contains("\nname: chat-history\n"), "{stub}");
        assert!(stub.contains("disable-model-invocation: true"), "{stub}");
        assert!(stub.contains("deprecated"), "{stub}");
        assert!(stub.contains("/history"), "{stub}");
        assert!(
            stub.contains("install-skill --remove-old-name"),
            "the stub offers to remove the old name: {stub}"
        );
    }
    assert!(
        read(h.join(".agents/skills/chat-history/agents/openai.yaml"))
            .contains("allow_implicit_invocation: false")
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
    old_layout(h);
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

#[test]
fn a_deleted_stub_stays_deleted() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    old_layout(h);
    cmd(h).args(["--from", "today"]).assert().success();
    assert!(
        h.join(".claude/skills/chat-history/SKILL.md").exists(),
        "stub installed"
    );
    // What the stub offers to do when the user agrees.
    fs::remove_dir_all(h.join(".claude/skills/chat-history")).unwrap();
    fs::remove_dir_all(h.join(".agents/skills/chat-history")).unwrap();
    cmd(h).args(["--from", "today"]).assert().success();
    cmd(h).arg("install-skill").assert().success();
    assert!(
        !h.join(".claude/skills/chat-history").exists(),
        "not put back"
    );
    assert!(
        !h.join(".agents/skills/chat-history").exists(),
        "not put back"
    );
    assert!(h.join(".claude/skills/history/SKILL.md").exists());
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(unix)]
#[test]
fn a_rename_that_cannot_be_installed_keeps_the_working_skill_and_its_name() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    cmd(h).arg("install-skill").assert().success();
    let roots = [h.join(".claude/skills"), h.join(".agents/skills")];
    for root in &roots {
        set_mode(root, 0o500);
    }
    let out = cmd(h)
        .args(["install-skill", "--name", "past-chats"])
        .output()
        .unwrap();
    for root in &roots {
        set_mode(root, 0o700);
    }
    assert!(!out.status.success());
    for root in &roots {
        assert!(
            root.join("history/SKILL.md").exists(),
            "{}: the working skill is kept",
            root.display()
        );
    }
    cmd(h).args(["--from", "today"]).assert().success();
    assert!(
        !h.join(".claude/skills/past-chats").exists(),
        "the failed name was not remembered"
    );
}

#[cfg(unix)]
#[test]
fn a_rename_retires_the_previous_name_only_where_the_new_one_was_installed() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    cmd(h).arg("install-skill").assert().success();
    set_mode(&h.join(".agents/skills"), 0o500);
    let out = cmd(h)
        .args(["install-skill", "--name", "past-chats"])
        .output()
        .unwrap();
    set_mode(&h.join(".agents/skills"), 0o700);
    assert!(out.status.success());
    assert!(h.join(".claude/skills/past-chats/SKILL.md").exists());
    assert!(!h.join(".claude/skills/history").exists());
    assert!(
        h.join(".agents/skills/history/SKILL.md").exists(),
        "Codex and Cursor keep a working skill"
    );
}

#[cfg(unix)]
#[test]
fn old_copies_keep_working_until_their_replacement_is_installed() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    old_layout(h);
    let before = read(h.join(".claude/skills/chat-history/SKILL.md"));
    fs::create_dir_all(h.join(".agents")).unwrap();
    set_mode(&h.join(".claude/skills"), 0o500);
    set_mode(&h.join(".agents"), 0o500);
    let out = cmd(h).args(["--from", "today"]).output().unwrap();
    set_mode(&h.join(".claude/skills"), 0o700);
    set_mode(&h.join(".agents"), 0o700);
    assert!(out.status.success());
    assert_eq!(
        read(h.join(".claude/skills/chat-history/SKILL.md")),
        before,
        "not replaced by a stub pointing at a skill that is not there"
    );
    assert!(h.join(".cursor/skills/chat-history/SKILL.md").exists());
    assert!(h.join(".codex/skills/chat-history/SKILL.md").exists());

    cmd(h).args(["--from", "today"]).assert().success();
    assert!(read(h.join(".claude/skills/chat-history/SKILL.md")).contains("deprecated"));
    assert!(h.join(".agents/skills/history/SKILL.md").exists());
    assert!(!h.join(".cursor/skills/chat-history").exists());
    assert!(!h.join(".codex/skills/chat-history").exists());
}

#[test]
fn an_unrelated_skill_with_the_same_name_is_not_overwritten_without_force() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    let theirs = "---\nname: history\ndescription: shell history helper\n---\nsomeone else's\n";
    let dir = h.join(".claude/skills/history");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("SKILL.md"), theirs).unwrap();
    cmd(h)
        .arg("install-skill")
        .assert()
        .success()
        .stdout(predicate::str::contains("not managed; use --force"));
    assert_eq!(read(dir.join("SKILL.md")), theirs);
    assert!(h.join(".agents/skills/history/SKILL.md").exists());
    cmd(h).args(["install-skill", "--force"]).assert().success();
    assert!(read(dir.join("SKILL.md")).contains("cargo install chat-history"));
}

#[test]
fn a_pre_sidecar_copy_of_ours_is_still_adopted() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    let dir = h.join(".claude/skills/history");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        "---\nname: chat-history\n---\nIf the command is not found: `cargo install chat-history`.\n",
    )
    .unwrap();
    cmd(h).arg("install-skill").assert().success();
    assert!(read(dir.join("SKILL.md")).contains("\nname: history\n"));
}

#[test]
fn the_stub_removes_the_old_name_with_a_command_that_keeps_other_files() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    old_layout(h);
    cmd(h).args(["--from", "today"]).assert().success();
    let stub = read(h.join(".claude/skills/chat-history/SKILL.md"));
    assert!(
        stub.contains("chat-history install-skill --remove-old-name"),
        "{stub}"
    );
    assert!(!stub.contains("delete"), "no folder deletion: {stub}");
    fs::write(h.join(".claude/skills/chat-history/notes.md"), "mine").unwrap();

    cmd(h)
        .args(["install-skill", "--remove-old-name"])
        .assert()
        .success();
    assert!(!h.join(".claude/skills/chat-history/SKILL.md").exists());
    assert_eq!(read(h.join(".claude/skills/chat-history/notes.md")), "mine");
    assert!(!h.join(".agents/skills/chat-history").exists());
    for root in [".claude/skills", ".agents/skills"] {
        assert!(h.join(root).join("history/SKILL.md").exists());
    }
    cmd(h).args(["--from", "today"]).assert().success();
    cmd(h).arg("install-skill").assert().success();
    assert!(!h.join(".claude/skills/chat-history/SKILL.md").exists());
    assert!(!h.join(".agents/skills/chat-history").exists());
}

#[test]
fn going_back_to_chat_history_lets_codex_pick_the_skill_again() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    old_layout(h);
    cmd(h).args(["--from", "today"]).assert().success();
    assert!(
        h.join(".agents/skills/chat-history/agents/openai.yaml")
            .exists()
    );
    cmd(h)
        .args(["install-skill", "--name", "chat-history"])
        .assert()
        .success();
    let skill = read(h.join(".agents/skills/chat-history/SKILL.md"));
    assert!(!skill.contains("disable-model-invocation"), "{skill}");
    assert!(
        !h.join(".agents/skills/chat-history/agents").exists(),
        "the stub's no-implicit-use policy is gone"
    );
}

#[test]
fn deleted_stubs_stay_deleted_while_an_edited_old_copy_remains() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    old_layout(h);
    fs::write(h.join(".cursor/skills/chat-history/SKILL.md"), "my edits").unwrap();
    cmd(h).args(["--from", "today"]).assert().success();
    assert!(h.join(".agents/skills/chat-history/SKILL.md").exists());
    fs::remove_dir_all(h.join(".claude/skills/chat-history")).unwrap();
    fs::remove_dir_all(h.join(".agents/skills/chat-history")).unwrap();
    cmd(h).args(["--from", "today"]).assert().success();
    cmd(h).arg("install-skill").assert().success();
    assert!(!h.join(".claude/skills/chat-history").exists());
    assert!(!h.join(".agents/skills/chat-history").exists());
    assert_eq!(
        read(h.join(".cursor/skills/chat-history/SKILL.md")),
        "my edits"
    );
}

#[cfg(unix)]
#[test]
fn removing_the_old_name_reports_a_stub_it_could_not_delete() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    old_layout(h);
    cmd(h).args(["--from", "today"]).assert().success();
    // As in Codex's sandbox, which keeps ~/.agents read-only.
    let shared = h.join(".agents/skills/chat-history");
    set_mode(&shared, 0o500);
    let out = cmd(h)
        .args(["install-skill", "--remove-old-name"])
        .output()
        .unwrap();
    set_mode(&shared, 0o700);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(stderr.contains("could not remove"), "{stderr}");
    assert!(stderr.contains(shared.to_str().unwrap()), "{stderr}");
    assert!(!h.join(".claude/skills/chat-history/SKILL.md").exists());
    assert!(shared.join("SKILL.md").exists());
}
