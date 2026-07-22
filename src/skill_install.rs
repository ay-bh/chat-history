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

/// Target skill directories for Cursor, Claude Code, and Codex.
pub fn skill_targets() -> Vec<(PathBuf, &'static str)> {
    let mut targets = Vec::new();
    let home = user_home();
    if let Some(ref home) = home {
        targets.push((home.join(".cursor/skills/chat-history"), "Cursor"));
        targets.push((home.join(".claude/skills/chat-history"), "Claude Code"));
    }
    // Codex: CODEX_HOME, or home-derived ~/.codex when home is known.
    if std::env::var_os("CODEX_HOME").is_some_and(|v| !v.is_empty()) || home.is_some() {
        targets.push((session::codex_home().join("skills/chat-history"), "Codex"));
    }
    targets
}

fn write_managed_skill(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let skill = skill_path(dir);
    let sidecar = sidecar_path(dir);
    if std::fs::write(&skill, SKILL_CONTENT).is_err() {
        return false;
    }
    std::fs::write(&sidecar, content_hash(SKILL_CONTENT)).is_ok()
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

fn ensure_one(dir: &Path, mode: Mode) -> EnsureAction {
    let skill = skill_path(dir);
    let sidecar = sidecar_path(dir);
    let embedded_hash = content_hash(SKILL_CONTENT);

    if mode == Mode::Force {
        return if write_managed_skill(dir) {
            EnsureAction::Wrote
        } else {
            EnsureAction::Failed
        };
    }

    match std::fs::read_to_string(&skill) {
        Err(_) => {
            // Missing (or unreadable) → install.
            if write_managed_skill(dir) {
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
                        } else if write_managed_skill(dir) {
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
                        if write_managed_skill(dir) {
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

/// Quietly install or refresh managed skills. Never prints; never overwrites
/// user-edited or ambiguous legacy skills. Safe to call on every CLI invocation.
pub fn ensure_skills() {
    for (dir, _) in skill_targets() {
        let _ = ensure_one(&dir, Mode::Quiet);
    }
}

/// Explicit install used by `chat-history install-skill`.
///
/// Without `force`, refreshes managed copies and adopts sidecar-less legacy
/// installs, but leaves verified user edits alone (prints a `--force` hint).
/// With `force`, overwrites even user-edited skills.
pub fn install_skill(force: bool) {
    let targets = skill_targets();
    if targets.is_empty() {
        eprintln!("Could not determine home directory (set HOME or USERPROFILE)");
        std::process::exit(1);
    }

    let mode = if force { Mode::Force } else { Mode::Explicit };
    let mut any_ok = false;
    for (dir, name) in &targets {
        match ensure_one(dir, mode) {
            EnsureAction::Wrote => {
                println!("  installed → {}", skill_path(dir).display());
                any_ok = true;
            }
            EnsureAction::Refreshed => {
                println!("  refreshed → {}", skill_path(dir).display());
                any_ok = true;
            }
            EnsureAction::AlreadyCurrent => {
                println!("  already current → {}", skill_path(dir).display());
                any_ok = true;
            }
            EnsureAction::LeftUserEdited => {
                println!(
                    "  left alone → {} (user-edited; use --force to overwrite)",
                    skill_path(dir).display()
                );
                any_ok = true;
            }
            EnsureAction::LeftLegacy => {
                // Only reachable in Quiet mode; keep arm for exhaustiveness.
                println!(
                    "  left alone → {} (not managed; use --force to overwrite)",
                    skill_path(dir).display()
                );
                any_ok = true;
            }
            EnsureAction::Failed => {
                eprintln!("  skip {name}: could not write to {}", dir.display());
            }
        }
    }

    if any_ok {
        println!("\nDone. The skill is active immediately — no restart needed.");
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
            assert!(matches!(ensure_one(&dir, Mode::Quiet), EnsureAction::Wrote));
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
                ensure_one(&dir, Mode::Quiet),
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
                ensure_one(&dir, Mode::Quiet),
                EnsureAction::LeftUserEdited
            ));
            assert!(matches!(
                ensure_one(&dir, Mode::Explicit),
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
                ensure_one(&dir, Mode::Quiet),
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
                ensure_one(&dir, Mode::Explicit),
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
            assert!(matches!(ensure_one(&dir, Mode::Force), EnsureAction::Wrote));
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
                ensure_one(&dir, Mode::Quiet),
                EnsureAction::AlreadyCurrent
            ));
            assert_eq!(
                fs::read_to_string(sidecar_path(&dir)).unwrap(),
                content_hash(SKILL_CONTENT)
            );
        });
    }
}
