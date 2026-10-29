use std::path::{Path, PathBuf};

use crate::session;

/// Bundled agent skill (Claude Code / Cursor / Codex).
pub const SKILL_CONTENT: &str = include_str!("../SKILL.md");

/// Sidecar next to SKILL.md marking a copy we installed and may refresh.
pub const MANAGED_SIDECAR: &str = ".chat-history-managed";

/// Resolve the user home directory (macOS/Linux `HOME`, Windows `USERPROFILE`).
pub fn user_home() -> Option<PathBuf> {
    session::user_home()
}

/// Stable content fingerprint for managed-skill detection (FNV-1a 64-bit).
pub fn content_hash(content: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in content.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn sidecar_path(dir: &Path) -> PathBuf {
    dir.join(MANAGED_SIDECAR)
}

fn skill_path(dir: &Path) -> PathBuf {
    dir.join("SKILL.md")
}

/// The skill's name when the user has not chosen one: `/history` in Claude
/// Code and Cursor, `$history` in Codex.
pub const DEFAULT_NAME: &str = "history";
/// The name used up to 0.8. A stub under it keeps `/chat-history` working,
/// says it is deprecated, and runs the skill under its current name.
pub const LEGACY_NAME: &str = "chat-history";
/// One line under `~/.chat-history` holding a name chosen with `--name`.
const NAME_FILE: &str = "skill-name";

/// Claude Code's built-in commands (code.claude.com/docs/en/commands, October
/// 2026). A skill with one of these names silently replaces the command.
const BUILT_IN_COMMANDS: &[&str] = &[
    "add-dir",
    "adddir",
    "advisor",
    "agents",
    "allowed-tools",
    "android",
    "app",
    "artifacts",
    "autocompact",
    "autofix-pr",
    "background",
    "bashes",
    "batch",
    "bg",
    "branch",
    "btw",
    "bug",
    "cd",
    "checkpoint",
    "checkup",
    "chrome",
    "clear",
    "code-review",
    "color",
    "compact",
    "config",
    "context",
    "continue",
    "copy",
    "cost",
    "debug",
    "design",
    "desktop",
    "diff",
    "doctor",
    "effort",
    "exit",
    "export",
    "extra-usage",
    "fast",
    "feedback",
    "focus",
    "fork",
    "goal",
    "heapdump",
    "help",
    "hooks",
    "ide",
    "import",
    "init",
    "insights",
    "ios",
    "keybindings",
    "list-agents",
    "login",
    "logout",
    "loop",
    "mcp",
    "memory",
    "mobile",
    "model",
    "new",
    "output-style",
    "passes",
    "peers",
    "permissions",
    "plan",
    "plugin",
    "powerup",
    "pr-comments",
    "privacy-settings",
    "proactive",
    "quit",
    "radio",
    "rc",
    "recap",
    "release-notes",
    "reload-plugins",
    "reload-skills",
    "remote-control",
    "remote-env",
    "rename",
    "reset",
    "resume",
    "review",
    "rewind",
    "routines",
    "run",
    "sandbox",
    "schedule",
    "scroll-speed",
    "security-review",
    "sessions",
    "settings",
    "share",
    "simplify",
    "skills",
    "slides",
    "stats",
    "status",
    "statusline",
    "stickers",
    "stop",
    "subtask",
    "tasks",
    "teleport",
    "terminal-setup",
    "theme",
    "tp",
    "tui",
    "ultraplan",
    "ultrareview",
    "undo",
    "upgrade",
    "usage",
    "verify",
    "vim",
    "voice",
    "workflows",
];

/// Agent Skills names (agentskills.io): 1-64 lowercase letters, digits and
/// hyphens, no leading, trailing or doubled hyphen. Also refuses names Claude
/// Code reserves and names of its built-in commands.
pub fn validate_name(name: &str) -> Result<(), String> {
    let shaped = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--");
    if !shaped {
        return Err(format!(
            "'{name}' is not a valid skill name: use 1-64 lowercase letters, digits and single hyphens"
        ));
    }
    if name == "synced" || name.contains("anthropic") || name.contains("claude") {
        return Err(format!("'{name}' is reserved by Claude Code"));
    }
    if BUILT_IN_COMMANDS.contains(&name) {
        return Err(format!(
            "'{name}' is a built-in Claude Code command; a skill with that name would replace it"
        ));
    }
    Ok(())
}

fn name_file() -> Option<PathBuf> {
    Some(user_home()?.join(".chat-history").join(NAME_FILE))
}

/// The installed skill's name: the one chosen with `--name`, else `history`.
pub fn chosen_name() -> String {
    name_file()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .map(|s| s.trim().to_owned())
        .filter(|n| validate_name(n).is_ok() || n == LEGACY_NAME)
        .unwrap_or_else(|| DEFAULT_NAME.to_owned())
}

fn record_name(name: &str) -> std::io::Result<()> {
    let file = name_file().ok_or_else(|| std::io::Error::other("no home directory"))?;
    std::fs::create_dir_all(file.parent().unwrap())?;
    std::fs::write(file, format!("{name}\n"))
}

/// The bundled skill with its frontmatter `name` set to `name`.
fn skill_content(name: &str) -> String {
    let mut out = String::with_capacity(SKILL_CONTENT.len());
    let mut renamed = false;
    for line in SKILL_CONTENT.split_inclusive('\n') {
        if !renamed && line.starts_with("name:") {
            out.push_str(&format!("name: {name}\n"));
            renamed = true;
        } else {
            out.push_str(line);
        }
    }
    out
}

/// The deprecated `chat-history` skill: only runs when typed. It does what
/// was asked through the skill under its current name, then offers to remove
/// the old name; once the user deletes the stub folders it is not put back.
fn stub_content(name: &str, skill: &Path, stub_dirs: &[PathBuf]) -> String {
    let dirs = stub_dirs
        .iter()
        .map(|d| d.display().to_string())
        .collect::<Vec<_>>()
        .join(" and ");
    format!(
        "---\n\
         name: {LEGACY_NAME}\n\
         description: Deprecated name of the {name} skill (search past Claude Code, Cursor and Codex conversations). Use /{name} instead.\n\
         disable-model-invocation: true\n\
         ---\n\
         \n\
         # /{LEGACY_NAME} is deprecated\n\
         \n\
         This skill was renamed to {name}. /{name} (in Codex, ${name}) is the same skill and loads it directly.\n\
         \n\
         1. Do what the user asked by following the {name} skill: read {} and use it as your instructions.\n\
         2. When that is done, tell the user in one short line that /{LEGACY_NAME} is deprecated and /{name} (in Codex, ${name}) is the same skill under a shorter name, and ask whether to remove the old name.\n\
         3. Only if they agree, delete {dirs}. These folders hold only files chat-history installed, and they are not reinstalled afterwards.\n",
        skill.display()
    )
}

/// Keeps Codex from loading the stub on its own; `$chat-history` still works.
const STUB_OPENAI_YAML: &str = "policy:\n  allow_implicit_invocation: false\n";

/// One managed copy: its directory, its SKILL.md, and an optional extra file
/// (relative path and content) written beside it.
struct Copy {
    dir: PathBuf,
    content: String,
    extra: Option<(&'static str, &'static str)>,
    label: &'static str,
}

/// Where the skill goes: Claude Code reads only `~/.claude/skills`; Codex and
/// Cursor both read `~/.agents/skills`, the cross-tool location.
fn roots() -> Vec<(PathBuf, &'static str)> {
    let Some(home) = user_home() else {
        return Vec::new();
    };
    vec![
        (home.join(".claude/skills"), "Claude Code"),
        (home.join(".agents/skills"), "Codex and Cursor"),
    ]
}

fn copies(name: &str) -> Vec<Copy> {
    // The stub is for people who used /chat-history: it is created only where
    // an old copy is being replaced (Codex and Cursor read ~/.agents/skills,
    // so their old copies count for it) and afterwards only refreshed, so a
    // stub the user deleted stays deleted. New users never get one.
    let had_old = old_locations().iter().any(|d| skill_path(d).exists());
    let stub_roots: Vec<(PathBuf, &'static str)> = if name == LEGACY_NAME {
        Vec::new()
    } else {
        roots()
            .into_iter()
            .filter(|(root, label)| {
                skill_path(&root.join(LEGACY_NAME)).exists() || (*label != "Claude Code" && had_old)
            })
            .collect()
    };
    let stub_dirs: Vec<PathBuf> = stub_roots
        .iter()
        .map(|(r, _)| r.join(LEGACY_NAME))
        .collect();
    let mut out = Vec::new();
    for (root, label) in roots() {
        let skill_dir = root.join(name);
        if stub_roots.iter().any(|(r, _)| *r == root) {
            out.push(Copy {
                dir: root.join(LEGACY_NAME),
                content: stub_content(name, &skill_path(&skill_dir), &stub_dirs),
                extra: (label != "Claude Code").then_some(("agents/openai.yaml", STUB_OPENAI_YAML)),
                label,
            });
        }
        out.push(Copy {
            dir: skill_dir,
            content: skill_content(name),
            extra: None,
            label,
        });
    }
    out
}

/// Copies this tool installed before 0.9 at locations it no longer uses: the
/// Cursor-only folder (Cursor reads `~/.agents/skills`) and Codex's deprecated
/// `$CODEX_HOME/skills`.
fn old_locations() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = user_home() {
        out.push(home.join(".cursor/skills").join(LEGACY_NAME));
    }
    if std::env::var_os("CODEX_HOME").is_some_and(|v| !v.is_empty()) || user_home().is_some() {
        out.push(session::codex_home().join("skills").join(LEGACY_NAME));
    }
    out
}

/// Remove a copy only if this tool wrote it and nobody edited it since: its
/// SKILL.md still has the hash recorded in the sidecar. Removes only the
/// files this tool writes, then the directories if they are left empty.
fn retire(dir: &Path) -> Retired {
    let (Ok(on_disk), Ok(managed)) = (
        std::fs::read_to_string(skill_path(dir)),
        std::fs::read_to_string(sidecar_path(dir)),
    ) else {
        return if skill_path(dir).exists() {
            Retired::LeftAlone
        } else {
            Retired::Absent
        };
    };
    if content_hash(&on_disk) != managed.trim() {
        return Retired::LeftAlone;
    }
    let _ = std::fs::remove_file(skill_path(dir));
    let _ = std::fs::remove_file(sidecar_path(dir));
    let yaml = dir.join("agents/openai.yaml");
    if std::fs::read_to_string(&yaml).is_ok_and(|y| y == STUB_OPENAI_YAML) {
        let _ = std::fs::remove_file(&yaml);
        let _ = std::fs::remove_dir(dir.join("agents"));
    }
    let _ = std::fs::remove_dir(dir);
    Retired::Removed
}

#[derive(Debug, PartialEq, Eq)]
enum Retired {
    Removed,
    LeftAlone,
    Absent,
}

fn write_managed_skill(dir: &Path, content: &str, extra: Option<(&str, &str)>) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    if let Some((rel, body)) = extra {
        let path = dir.join(rel);
        if path
            .parent()
            .is_some_and(|p| std::fs::create_dir_all(p).is_err())
            || std::fs::write(&path, body).is_err()
        {
            return false;
        }
    }
    std::fs::write(skill_path(dir), content).is_ok()
        && std::fs::write(sidecar_path(dir), content_hash(content)).is_ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Silent auto-path: never overwrite user edits or ambiguous legacy copies.
    Quiet,
    /// `install-skill`: refresh managed + adopt sidecar-less legacy; keep user edits.
    Explicit,
    /// `install-skill --force`: overwrite everything.
    Force,
}

#[derive(Debug, PartialEq, Eq)]
enum EnsureAction {
    Wrote,
    Refreshed,
    AlreadyCurrent,
    /// Sidecar present but content hash no longer matches — treat as user edit.
    LeftUserEdited,
    /// Sidecar-less copy that differs from embedded (quiet path only).
    LeftLegacy,
    Failed,
}

fn ensure_one(copy: &Copy, mode: Mode) -> EnsureAction {
    let dir = copy.dir.as_path();
    let skill = skill_path(dir);
    let sidecar = sidecar_path(dir);
    let embedded_hash = content_hash(&copy.content);
    let write = || write_managed_skill(dir, &copy.content, copy.extra);

    if mode == Mode::Force {
        return if write() {
            EnsureAction::Wrote
        } else {
            EnsureAction::Failed
        };
    }

    match std::fs::read_to_string(&skill) {
        Err(_) => {
            // Missing (or unreadable) → install.
            if write() {
                EnsureAction::Wrote
            } else {
                EnsureAction::Failed
            }
        }
        Ok(on_disk) => {
            let on_disk_hash = content_hash(&on_disk);
            match std::fs::read_to_string(&sidecar) {
                Ok(managed) => {
                    let managed = managed.trim();
                    if managed == on_disk_hash {
                        // Still our copy. Refresh if embedded skill changed.
                        if on_disk_hash == embedded_hash {
                            EnsureAction::AlreadyCurrent
                        } else if write() {
                            EnsureAction::Refreshed
                        } else {
                            EnsureAction::Failed
                        }
                    } else {
                        // Sidecar present but content changed → user-edited.
                        EnsureAction::LeftUserEdited
                    }
                }
                Err(_) => {
                    // Legacy install (no sidecar).
                    if on_disk_hash == embedded_hash {
                        let _ = std::fs::write(&sidecar, &embedded_hash);
                        EnsureAction::AlreadyCurrent
                    } else if mode == Mode::Explicit {
                        // Explicit install-skill: adopt pre-sidecar installs so
                        // upgrades that re-run install-skill get the new skill
                        // (matches 0.2.x "re-run install-skill after upgrades").
                        if write() {
                            EnsureAction::Refreshed
                        } else {
                            EnsureAction::Failed
                        }
                    } else {
                        // Quiet path: don't guess — leave alone.
                        EnsureAction::LeftLegacy
                    }
                }
            }
        }
    }
}

/// Quietly install or refresh managed skills and retire our copies at the
/// locations used before 0.9. Never prints; never overwrites user-edited or
/// ambiguous legacy skills. Safe to call on every CLI invocation.
pub fn ensure_skills() {
    for copy in copies(&chosen_name()) {
        let _ = ensure_one(&copy, Mode::Quiet);
    }
    for dir in old_locations() {
        let _ = retire(&dir);
    }
}

/// Explicit install used by `chat-history install-skill [--name NAME]`.
///
/// Without `force`, refreshes managed copies and adopts sidecar-less legacy
/// installs, but leaves verified user edits alone (prints a `--force` hint).
/// With `force`, overwrites even user-edited skills. A new `name` is recorded
/// so later automatic refreshes keep it; copies under the previous name are
/// retired when nobody edited them.
pub fn install_skill(force: bool, name: Option<&str>) {
    if roots().is_empty() {
        eprintln!("Could not determine home directory (set HOME or USERPROFILE)");
        std::process::exit(1);
    }
    let previous = chosen_name();
    let name = match name {
        Some(n) => {
            if let Err(e) =
                validate_name(n).or_else(|e| if n == LEGACY_NAME { Ok(()) } else { Err(e) })
            {
                eprintln!("{e}");
                std::process::exit(2);
            }
            if let Err(e) = record_name(n) {
                eprintln!("Could not save the skill name ({e}).");
                std::process::exit(1);
            }
            n.to_owned()
        }
        None => previous.clone(),
    };

    let mode = if force { Mode::Force } else { Mode::Explicit };
    let mut any_ok = false;
    for copy in copies(&name) {
        let shown = skill_path(&copy.dir);
        match ensure_one(&copy, mode) {
            EnsureAction::Wrote => {
                println!("  installed → {}", shown.display());
                any_ok = true;
            }
            EnsureAction::Refreshed => {
                println!("  refreshed → {}", shown.display());
                any_ok = true;
            }
            EnsureAction::AlreadyCurrent => {
                println!("  already current → {}", shown.display());
                any_ok = true;
            }
            EnsureAction::LeftUserEdited => {
                println!(
                    "  left alone → {} (user-edited; use --force to overwrite)",
                    shown.display()
                );
                any_ok = true;
            }
            EnsureAction::LeftLegacy => {
                println!(
                    "  left alone → {} (not managed; use --force to overwrite)",
                    shown.display()
                );
                any_ok = true;
            }
            EnsureAction::Failed => {
                eprintln!(
                    "  skip {}: could not write to {}",
                    copy.label,
                    copy.dir.display()
                );
            }
        }
    }
    let mut retired = old_locations();
    if previous != name && previous != LEGACY_NAME {
        retired.extend(roots().into_iter().map(|(root, _)| root.join(&previous)));
    }
    for dir in retired {
        match retire(&dir) {
            Retired::Removed => println!("  removed old copy → {}", dir.display()),
            Retired::LeftAlone => println!(
                "  left alone → {} (edited or not ours; remove it if you no longer want it)",
                skill_path(&dir).display()
            ),
            Retired::Absent => {}
        }
    }

    if any_ok {
        println!(
            "\nDone. Use /{name} in Claude Code and Cursor, ${name} in Codex. The skill is active immediately — no restart needed."
        );
    } else {
        eprintln!("\nNo skills were installed.");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn copy(dir: &Path) -> Copy {
        Copy {
            dir: dir.to_path_buf(),
            content: SKILL_CONTENT.to_owned(),
            extra: None,
            label: "test",
        }
    }

    fn with_tmp<F: FnOnce(&Path)>(f: F) {
        let tmp = TempDir::new().unwrap();
        f(tmp.path());
    }

    #[test]
    fn content_hash_stable() {
        assert_eq!(content_hash("abc"), content_hash("abc"));
        assert_ne!(content_hash("abc"), content_hash("abd"));
    }

    #[test]
    fn writes_when_missing() {
        with_tmp(|home| {
            let dir = home.join(".claude/skills/chat-history");
            assert!(matches!(
                ensure_one(&copy(&dir), Mode::Quiet),
                EnsureAction::Wrote
            ));
            assert_eq!(fs::read_to_string(skill_path(&dir)).unwrap(), SKILL_CONTENT);
            assert_eq!(
                fs::read_to_string(sidecar_path(&dir)).unwrap(),
                content_hash(SKILL_CONTENT)
            );
        });
    }

    #[test]
    fn refreshes_managed_stale_copy() {
        with_tmp(|home| {
            let dir = home.join(".claude/skills/chat-history");
            fs::create_dir_all(&dir).unwrap();
            fs::write(skill_path(&dir), "old default").unwrap();
            fs::write(sidecar_path(&dir), content_hash("old default")).unwrap();
            assert!(matches!(
                ensure_one(&copy(&dir), Mode::Quiet),
                EnsureAction::Refreshed
            ));
            assert_eq!(fs::read_to_string(skill_path(&dir)).unwrap(), SKILL_CONTENT);
        });
    }

    #[test]
    fn preserves_user_edits() {
        with_tmp(|home| {
            let dir = home.join(".claude/skills/chat-history");
            fs::create_dir_all(&dir).unwrap();
            fs::write(skill_path(&dir), "my custom skill").unwrap();
            fs::write(sidecar_path(&dir), content_hash("old default")).unwrap();
            assert!(matches!(
                ensure_one(&copy(&dir), Mode::Quiet),
                EnsureAction::LeftUserEdited
            ));
            assert!(matches!(
                ensure_one(&copy(&dir), Mode::Explicit),
                EnsureAction::LeftUserEdited
            ));
            assert_eq!(
                fs::read_to_string(skill_path(&dir)).unwrap(),
                "my custom skill"
            );
        });
    }

    #[test]
    fn quiet_preserves_legacy_without_sidecar() {
        with_tmp(|home| {
            let dir = home.join(".claude/skills/chat-history");
            fs::create_dir_all(&dir).unwrap();
            fs::write(skill_path(&dir), "legacy custom").unwrap();
            assert!(matches!(
                ensure_one(&copy(&dir), Mode::Quiet),
                EnsureAction::LeftLegacy
            ));
            assert_eq!(
                fs::read_to_string(skill_path(&dir)).unwrap(),
                "legacy custom"
            );
            assert!(!sidecar_path(&dir).exists());
        });
    }

    #[test]
    fn explicit_adopts_legacy_without_sidecar() {
        with_tmp(|home| {
            let dir = home.join(".claude/skills/chat-history");
            fs::create_dir_all(&dir).unwrap();
            fs::write(skill_path(&dir), "legacy 0.2.x skill").unwrap();
            assert!(matches!(
                ensure_one(&copy(&dir), Mode::Explicit),
                EnsureAction::Refreshed
            ));
            assert_eq!(fs::read_to_string(skill_path(&dir)).unwrap(), SKILL_CONTENT);
            assert_eq!(
                fs::read_to_string(sidecar_path(&dir)).unwrap(),
                content_hash(SKILL_CONTENT)
            );
        });
    }

    #[test]
    fn force_overwrites_user_edits() {
        with_tmp(|home| {
            let dir = home.join(".claude/skills/chat-history");
            fs::create_dir_all(&dir).unwrap();
            fs::write(skill_path(&dir), "my custom skill").unwrap();
            assert!(matches!(
                ensure_one(&copy(&dir), Mode::Force),
                EnsureAction::Wrote
            ));
            assert_eq!(fs::read_to_string(skill_path(&dir)).unwrap(), SKILL_CONTENT);
        });
    }

    #[test]
    fn adopts_legacy_matching_embedded() {
        with_tmp(|home| {
            let dir = home.join(".claude/skills/chat-history");
            fs::create_dir_all(&dir).unwrap();
            fs::write(skill_path(&dir), SKILL_CONTENT).unwrap();
            assert!(matches!(
                ensure_one(&copy(&dir), Mode::Quiet),
                EnsureAction::AlreadyCurrent
            ));
            assert_eq!(
                fs::read_to_string(sidecar_path(&dir)).unwrap(),
                content_hash(SKILL_CONTENT)
            );
        });
    }
}
