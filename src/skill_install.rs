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
/// Present under `~/.chat-history` once the upgrade from `chat-history` has
/// put its stubs in place (or had none to make), or once the user removed the
/// old name. From then on stubs are only refreshed where they exist.
const MIGRATED_FILE: &str = "skill-migrated";
/// Names the skill was installed under before a `--name` change, one per
/// line, whose unedited copies are still to be removed (a folder may have
/// been blocked when the name changed).
const OLD_NAMES_FILE: &str = "skill-old-names";
/// Text every SKILL.md this tool has shipped contains; a copy without a
/// sidecar is adopted by `install-skill` only if it has it.
const OWN_SKILL_MARK: &str = "cargo install chat-history";

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

fn state_file(file: &str) -> Option<PathBuf> {
    Some(user_home()?.join(".chat-history").join(file))
}

fn write_state(file: &str, contents: &str) -> std::io::Result<()> {
    let path = state_file(file).ok_or_else(|| std::io::Error::other("no home directory"))?;
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(path, contents)
}

fn migrated() -> bool {
    state_file(MIGRATED_FILE).is_some_and(|f| f.exists())
}

/// The installed skill's name: the one chosen with `--name`, else `history`.
pub fn chosen_name() -> String {
    state_file(NAME_FILE)
        .and_then(|f| std::fs::read_to_string(f).ok())
        .map(|s| s.trim().to_owned())
        .filter(|n| validate_name(n).is_ok())
        .unwrap_or_else(|| DEFAULT_NAME.to_owned())
}

fn old_names() -> Vec<String> {
    state_file(OLD_NAMES_FILE)
        .and_then(|f| std::fs::read_to_string(f).ok())
        .map(|s| s.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

fn set_old_names(names: &[String]) {
    if names.is_empty() {
        if let Some(file) = state_file(OLD_NAMES_FILE) {
            let _ = std::fs::remove_file(file);
        }
    } else {
        let _ = write_state(OLD_NAMES_FILE, &(names.join("\n") + "\n"));
    }
}

/// The bundled skill with its frontmatter `name` set to `name`.
fn skill_content(name: &str) -> String {
    let mut out = String::with_capacity(SKILL_CONTENT.len());
    let mut renamed = false;
    for line in SKILL_CONTENT.split_inclusive('\n') {
        if !renamed && line.starts_with("name:") {
            // Quoted so names like `123` or `true` stay strings in YAML.
            out.push_str(&format!("name: \"{name}\"\n"));
            renamed = true;
        } else {
            out.push_str(line);
        }
    }
    out
}

/// The stub's heading; `--remove-old-name` recognises a stub by it.
const STUB_HEADING: &str = "# /chat-history is deprecated";

/// The deprecated `chat-history` skill: only runs when typed. It does what
/// was asked through the skill under its current name, then offers to remove
/// the old name with `install-skill --remove-old-name`, which deletes only
/// the files this tool wrote; once removed it is not put back.
fn stub_content(name: &str, skill: &Path) -> String {
    format!(
        "---\n\
         name: {LEGACY_NAME}\n\
         description: Deprecated name of the {name} skill (search past Claude Code, Cursor and Codex conversations). Use /{name} (in Codex, ${name}) instead.\n\
         disable-model-invocation: true\n\
         ---\n\
         \n\
         {STUB_HEADING}\n\
         \n\
         This skill was renamed to {name}. /{name} (in Codex, ${name}) is the same skill and loads it directly.\n\
         \n\
         1. Do what the user asked by following the {name} skill: read {} and use it as your instructions.\n\
         2. When that is done, tell the user in one short line that /{LEGACY_NAME} is deprecated and /{name} (in Codex, ${name}) is the same skill under a shorter name, and ask whether to remove the old name.\n\
         3. Only if they agree, run `chat-history install-skill --remove-old-name`. It removes only the files chat-history installed for the old name, and the old name is not put back afterwards.\n",
        skill.display()
    )
}

/// Beside the stub in the shared root: keeps Codex from loading it on its
/// own; `$chat-history` still works.
const STUB_POLICY_FILE: &str = "agents/openai.yaml";
const STUB_OPENAI_YAML: &str = "policy:\n  allow_implicit_invocation: false\n";

/// Where the skill goes: Claude Code reads only `~/.claude/skills`; Codex and
/// Cursor both read `~/.agents/skills`, the cross-tool location.
struct Root {
    dir: PathBuf,
    label: &'static str,
    /// The cross-tool root: its stub carries the Codex policy, and it
    /// replaces the old Cursor and Codex locations.
    shared: bool,
}

fn roots() -> Vec<Root> {
    let Some(home) = user_home() else {
        return Vec::new();
    };
    vec![
        Root {
            dir: home.join(".claude/skills"),
            label: "Claude Code",
            shared: false,
        },
        Root {
            dir: home.join(".agents/skills"),
            label: "Codex and Cursor",
            shared: true,
        },
    ]
}

fn require_home() {
    if user_home().is_none() {
        eprintln!("Could not determine home directory (set HOME or USERPROFILE)");
        std::process::exit(1);
    }
}

/// One managed copy: its directory and SKILL.md, and whether the stub's
/// Codex policy goes beside it (otherwise one left there is removed).
struct Target {
    dir: PathBuf,
    content: String,
    policy: bool,
}

/// What happened in one root: the skill, and the stub where one was due and
/// the skill was in place for it to point at.
struct RootResult {
    root: Root,
    skill: (Target, EnsureAction),
    stub: Option<(Target, EnsureAction)>,
}

impl RootResult {
    fn placed(&self) -> bool {
        in_place(&self.skill.1)
    }

    /// The skill and its stub, if one is due, are in place: only then can
    /// what they replace go, since the stub points at the skill.
    /// A stub the user edited may still point at the previous name, so it
    /// counts only once it names this skill.
    fn ready(&self) -> bool {
        let skill = skill_path(&self.skill.0.dir).display().to_string();
        self.placed()
            && self.stub.as_ref().is_none_or(|(stub, a)| {
                in_place(a)
                    && std::fs::read_to_string(skill_path(&stub.dir))
                        .is_ok_and(|s| s.contains(&skill))
            })
    }
}

/// A skill is usable where it was written or kept as the user edited it.
fn in_place(action: &EnsureAction) -> bool {
    matches!(
        action,
        EnsureAction::Wrote
            | EnsureAction::Refreshed
            | EnsureAction::AlreadyCurrent
            | EnsureAction::LeftUserEdited
    )
}

/// Install the skill under `name` in each root, then the stub where due. The
/// stub is for people who used /chat-history: until the upgrade is recorded
/// as done it is made where an old copy is being replaced (Codex and Cursor
/// read ~/.agents/skills, so their old copies count for it); afterwards it is
/// only refreshed where it exists, so a stub the user removed stays removed.
/// New users never get one. A stub replaces an old copy only once the skill
/// it points to is in place in the same root, so a failed write never leaves
/// a stub pointing at nothing.
fn install(name: &str, mode: Mode) -> Vec<RootResult> {
    let had_old = !migrated() && old_locations().iter().any(|d| skill_path(d).exists());
    roots()
        .into_iter()
        .map(|root| {
            let skill = Target {
                dir: root.dir.join(name),
                content: skill_content(name),
                policy: false,
            };
            let skill_action = ensure_one(&skill, mode);
            let stub_dir = root.dir.join(LEGACY_NAME);
            let stub_due =
                name != LEGACY_NAME && (skill_path(&stub_dir).exists() || (root.shared && had_old));
            let stub = (stub_due && in_place(&skill_action)).then(|| {
                let stub = Target {
                    content: stub_content(name, &skill_path(&skill.dir)),
                    dir: stub_dir,
                    policy: root.shared,
                };
                let action = ensure_one(&stub, mode);
                (stub, action)
            });
            RootResult {
                root,
                skill: (skill, skill_action),
                stub,
            }
        })
        .collect()
}

/// After an install: retire copies under earlier names where the current one
/// and its stub are in place; retire the old locations once the shared copy
/// and stub that replace them are in place; and record the upgrade as done
/// once the skill and every due stub are in place everywhere. Returns what
/// was retired, for `install-skill` to report.
fn finish_upgrade(results: &[RootResult], name: &str) -> Vec<(PathBuf, Retired)> {
    let mut retired = Vec::new();

    let before = old_names();
    let mut pending = before.clone();
    pending.retain(|old| old != name && old != LEGACY_NAME);
    for old in &pending {
        for result in results.iter().filter(|r| r.ready()) {
            let dir = result.root.dir.join(old);
            let outcome = retire(&dir);
            retired.push((dir, outcome));
        }
    }
    // Kept while a copy nobody edited remains, e.g. in a folder that was
    // blocked; edited copies are the user's.
    pending.retain(|old| results.iter().any(|r| removable(&r.root.dir.join(old))));
    if pending != before {
        set_old_names(&pending);
    }

    // $chat-history must keep working from ~/.agents/skills before the old
    // Codex and Cursor copies go.
    if results.iter().any(|r| r.root.shared && r.ready()) {
        for dir in old_locations() {
            let outcome = retire(&dir);
            if outcome == Retired::Removed {
                // 0.8 made these folders just for the skill; kept if not empty.
                let _ = dir.parent().map(std::fs::remove_dir);
            }
            retired.push((dir, outcome));
        }
    }
    if results.iter().all(RootResult::ready) && !migrated() {
        let _ = write_state(MIGRATED_FILE, "");
    }
    retired
}

/// Print one retirement; `left_alone` says why a copy was kept. Returns true
/// if the copy could not be deleted.
fn report_retired(dir: &Path, retired: &Retired, left_alone: &str) -> bool {
    match retired {
        Retired::Removed => println!("  removed → {}", skill_path(dir).display()),
        Retired::LeftAlone => println!(
            "  left alone → {} ({left_alone})",
            skill_path(dir).display()
        ),
        Retired::Absent => {}
        Retired::Failed(e) => {
            eprintln!("  could not remove {} ({e})", skill_path(dir).display());
            return true;
        }
    }
    false
}

/// Copies this tool installed before 0.9 at locations it no longer uses: the
/// Cursor-only folder (Cursor reads `~/.agents/skills`) and Codex's deprecated
/// `$CODEX_HOME/skills`.
fn old_locations() -> Vec<PathBuf> {
    let Some(home) = user_home() else {
        return Vec::new();
    };
    vec![
        home.join(".cursor/skills").join(LEGACY_NAME),
        session::codex_home().join("skills").join(LEGACY_NAME),
    ]
}

/// Remove a copy only if this tool wrote it and nobody edited it since: its
/// SKILL.md still has the hash recorded in the sidecar. Removes only the
/// files this tool writes, then the directories if they are left empty.
fn retire(dir: &Path) -> Retired {
    if !removable(dir) {
        return if skill_path(dir).exists() {
            Retired::LeftAlone
        } else {
            Retired::Absent
        };
    }
    let stub = is_stub(dir);
    // SKILL.md is what makes it a skill; the rest is cleanup.
    if let Err(e) = std::fs::remove_file(skill_path(dir)) {
        return Retired::Failed(e.to_string());
    }
    let _ = std::fs::remove_file(sidecar_path(dir));
    if stub {
        let _ = remove_stub_policy(dir);
    }
    let _ = std::fs::remove_dir(dir);
    Retired::Removed
}

/// The folder holds the deprecated stub (recognised by its heading).
fn is_stub(dir: &Path) -> bool {
    std::fs::read_to_string(skill_path(dir)).is_ok_and(|s| s.contains(STUB_HEADING))
}

/// This tool wrote the copy's SKILL.md and nobody edited it since: it still
/// has the hash recorded in the sidecar.
fn removable(dir: &Path) -> bool {
    match (
        std::fs::read_to_string(skill_path(dir)),
        std::fs::read_to_string(sidecar_path(dir)),
    ) {
        (Ok(on_disk), Ok(managed)) => content_hash(&on_disk) == managed.trim(),
        _ => false,
    }
}

/// Remove the stub's Codex policy if it is still the one this tool wrote.
/// Callers check the folder holds a stub: beside a real skill, even the same
/// text is the user's. False only if the policy is there and could not be
/// deleted.
fn remove_stub_policy(dir: &Path) -> bool {
    if !has_stub_policy(dir) {
        return true;
    }
    if std::fs::remove_file(dir.join(STUB_POLICY_FILE)).is_err() {
        return false;
    }
    let _ = std::fs::remove_dir(dir.join("agents"));
    true
}

/// Only the stub's folder ever had this tool's policy; elsewhere such a file
/// is the user's.
fn has_stub_policy(dir: &Path) -> bool {
    dir.file_name() == Some(LEGACY_NAME.as_ref())
        && std::fs::read_to_string(dir.join(STUB_POLICY_FILE)).is_ok_and(|y| y == STUB_OPENAI_YAML)
}

#[derive(Debug, PartialEq, Eq)]
enum Retired {
    Removed,
    LeftAlone,
    Absent,
    /// Ours and unedited, but SKILL.md could not be deleted (the error).
    Failed(String),
}

fn write_managed_skill(dir: &Path, content: &str, policy: bool) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    if policy {
        let path = dir.join(STUB_POLICY_FILE);
        // A policy file the user customised is theirs to keep.
        if !path.exists()
            && (std::fs::create_dir_all(path.parent().unwrap()).is_err()
                || std::fs::write(&path, STUB_OPENAI_YAML).is_err())
        {
            return false;
        }
    } else if is_stub(dir) && !remove_stub_policy(dir) {
        // The real skill replacing a stub must not keep its no-implicit-use
        // policy. Failing here leaves the stub, so the next run retries.
        return false;
    }
    std::fs::write(skill_path(dir), content).is_ok()
        && std::fs::write(sidecar_path(dir), content_hash(content)).is_ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Silent auto-path: never overwrite user edits or ambiguous legacy copies.
    Quiet,
    /// `install-skill`: refresh managed copies and adopt sidecar-less copies
    /// of this skill; keep user edits and other skills.
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
    /// Sidecar-less copy that differs from embedded and is not adopted (the
    /// quiet path, or not recognisably this skill).
    LeftLegacy,
    Failed,
}

fn ensure_one(target: &Target, mode: Mode) -> EnsureAction {
    let dir = target.dir.as_path();
    let embedded_hash = content_hash(&target.content);
    let write = |done| {
        if write_managed_skill(dir, &target.content, target.policy) {
            done
        } else {
            EnsureAction::Failed
        }
    };

    if mode == Mode::Force {
        return write(EnsureAction::Wrote);
    }
    // Missing (or unreadable) → install.
    let Ok(on_disk) = std::fs::read_to_string(skill_path(dir)) else {
        return write(EnsureAction::Wrote);
    };
    let on_disk_hash = content_hash(&on_disk);
    match std::fs::read_to_string(sidecar_path(dir)) {
        // Sidecar present but content changed → user-edited.
        Ok(managed) if managed.trim() != on_disk_hash => EnsureAction::LeftUserEdited,
        // Still our copy. Refresh if embedded skill changed.
        Ok(_) if on_disk_hash == embedded_hash => EnsureAction::AlreadyCurrent,
        Ok(_) => write(EnsureAction::Refreshed),
        // Legacy install (no sidecar).
        Err(_) if on_disk_hash == embedded_hash => {
            let _ = std::fs::write(sidecar_path(dir), &embedded_hash);
            EnsureAction::AlreadyCurrent
        }
        // Explicit install-skill: adopt pre-sidecar installs so upgrades that
        // re-run install-skill get the new skill (matches 0.2.x "re-run
        // install-skill after upgrades"). Only recognisably ours: another
        // skill may use the name.
        Err(_) if mode == Mode::Explicit && on_disk.contains(OWN_SKILL_MARK) => {
            write(EnsureAction::Refreshed)
        }
        // Quiet path, or not recognisably ours: don't guess — leave alone.
        Err(_) => EnsureAction::LeftLegacy,
    }
}

/// Quietly install or refresh managed skills and retire our copies at the
/// locations used before 0.9 once their replacement is in place. Never
/// prints; never overwrites user-edited or ambiguous legacy skills. Safe to
/// call on every CLI invocation.
pub fn ensure_skills() {
    let name = chosen_name();
    let results = install(&name, Mode::Quiet);
    finish_upgrade(&results, &name);
}

fn report_install(target: &Target, action: &EnsureAction, label: &str) {
    let shown = skill_path(&target.dir);
    match action {
        EnsureAction::Wrote => println!("  installed → {}", shown.display()),
        EnsureAction::Refreshed => println!("  refreshed → {}", shown.display()),
        EnsureAction::AlreadyCurrent => println!("  already current → {}", shown.display()),
        EnsureAction::LeftUserEdited => println!(
            "  left alone → {} (user-edited; use --force to overwrite)",
            shown.display()
        ),
        EnsureAction::LeftLegacy => println!(
            "  left alone → {} (not managed; use --force to overwrite)",
            shown.display()
        ),
        EnsureAction::Failed => eprintln!(
            "  skip {label}: could not write to {}",
            target.dir.display()
        ),
    }
}

/// Explicit install used by `chat-history install-skill [--name NAME]`.
///
/// Without `force`, refreshes managed copies and adopts sidecar-less copies
/// of this skill, but leaves verified user edits and other skills alone
/// (prints a `--force` hint). With `force`, overwrites even those. A new
/// `name` is recorded only once the skill is installed under it, so later
/// automatic refreshes keep it; copies under the previous name are retired
/// where the new one replaced them, when nobody edited them.
pub fn install_skill(force: bool, name: Option<&str>) {
    require_home();
    let previous = chosen_name();
    let name = match name {
        Some(n) => {
            if let Err(e) = validate_name(n) {
                eprintln!("{e}");
                std::process::exit(2);
            }
            n.to_owned()
        }
        None => previous.clone(),
    };

    let mode = if force { Mode::Force } else { Mode::Explicit };
    let results = install(&name, mode);
    for result in &results {
        report_install(&result.skill.0, &result.skill.1, result.root.label);
        if let Some((stub, action)) = &result.stub {
            report_install(stub, action, result.root.label);
        }
    }
    if !results.iter().any(RootResult::placed) {
        eprintln!("\nNo skills were installed; nothing else was changed.");
        std::process::exit(1);
    }
    if name != previous {
        if let Err(e) = write_state(NAME_FILE, &format!("{name}\n")) {
            eprintln!("Could not save the skill name ({e}); the skill under {previous} was kept.");
            std::process::exit(1);
        }
        // finish_upgrade retires it where the new name is in place, now or on
        // a later run once a blocked folder is writable.
        let mut pending = old_names();
        pending.retain(|old| *old != name && *old != previous);
        pending.push(previous.clone());
        set_old_names(&pending);
    }
    for (dir, retired) in finish_upgrade(&results, &name) {
        report_retired(
            &dir,
            &retired,
            "edited or not ours; remove it if you no longer want it",
        );
    }
    let taken: Vec<&str> = results
        .iter()
        .filter(|r| r.skill.1 == EnsureAction::LeftLegacy)
        .map(|r| r.root.label)
        .collect();
    if taken.is_empty() {
        println!(
            "\nDone. Use /{name} in Claude Code and Cursor, ${name} in Codex. The skill is active immediately — no restart needed."
        );
    } else {
        println!(
            "\nIn {}, another skill already uses the name {name}, so /{name} there is that skill. Pick another name with chat-history install-skill --name <name>, or replace it with --force.",
            taken.join(" and ")
        );
    }
}

/// `chat-history install-skill --remove-old-name`, which the stub offers:
/// where the skill is installed under its current name, removes the
/// `chat-history` stub's files (only those this tool wrote and nobody edited,
/// leaving anything else in the folder), and records that the old name is
/// gone so it is not put back.
pub fn remove_old_name() {
    require_home();
    // The upgrade may not have run yet (install-skill skips it): do it first,
    // so the stub exists to remove and is not created afterwards.
    ensure_skills();
    let name = chosen_name();
    if name == LEGACY_NAME {
        eprintln!(
            "The skill is installed as {LEGACY_NAME}; to rename it, run chat-history install-skill --name {DEFAULT_NAME}."
        );
        std::process::exit(2);
    }
    let mut failed = false;
    for root in roots() {
        let dir = root.dir.join(LEGACY_NAME);
        let skill = skill_path(&dir);
        if !skill.exists() {
            continue;
        }
        // Only a stub goes, and only where this tool's skill replaces it: a
        // full 0.8 copy may be the only working one, e.g. when another skill
        // already holds the new name.
        let current = root.dir.join(&name);
        if !(skill_path(&current).exists() && sidecar_path(&current).exists()) {
            println!(
                "  left alone → {} (the skill is not installed as {name} here, so this is still the working copy)",
                skill.display()
            );
            continue;
        }
        if !is_stub(&dir) {
            println!(
                "  left alone → {} (not the deprecated stub; delete it yourself if you no longer want it)",
                skill.display()
            );
            continue;
        }
        failed |= report_retired(
            &dir,
            &retire(&dir),
            "edited; delete it yourself if you no longer want it",
        );
    }
    if write_state(MIGRATED_FILE, "").is_err() {
        eprintln!(
            "Note: could not write ~/.chat-history/{MIGRATED_FILE}; a later run may put the stub back."
        );
    }
    if failed {
        eprintln!(
            "\nThe old name is still installed where it could not be removed; run this again with write access to that folder (an agent sandbox may block it)."
        );
        std::process::exit(1);
    }
    println!("\nDone. Use /{name} in Claude Code and Cursor, ${name} in Codex.");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn target(dir: &Path) -> Target {
        Target {
            dir: dir.to_path_buf(),
            content: SKILL_CONTENT.to_owned(),
            policy: false,
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
                ensure_one(&target(&dir), Mode::Quiet),
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
                ensure_one(&target(&dir), Mode::Quiet),
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
                ensure_one(&target(&dir), Mode::Quiet),
                EnsureAction::LeftUserEdited
            ));
            assert!(matches!(
                ensure_one(&target(&dir), Mode::Explicit),
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
                ensure_one(&target(&dir), Mode::Quiet),
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
            fs::write(
                skill_path(&dir),
                "legacy 0.2.x skill: cargo install chat-history",
            )
            .unwrap();
            assert!(matches!(
                ensure_one(&target(&dir), Mode::Explicit),
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
                ensure_one(&target(&dir), Mode::Force),
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
                ensure_one(&target(&dir), Mode::Quiet),
                EnsureAction::AlreadyCurrent
            ));
            assert_eq!(
                fs::read_to_string(sidecar_path(&dir)).unwrap(),
                content_hash(SKILL_CONTENT)
            );
        });
    }
}
