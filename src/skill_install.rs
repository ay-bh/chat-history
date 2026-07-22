use std::path::{Path, PathBuf};

use crate::session;

/// Bundled agent skill (Claude Code / Cursor / Codex).
pub const SKILL_CONTENT: &str = include_str!("../SKILL.md");

/// Sidecar next to SKILL.md marking a copy we installed and may refresh.
pub const MANAGED_SIDECAR: &str = ".chat-history-managed";

/// Resolve the user home directory (macOS/Linux `HOME`, Windows `USERPROFILE`).
pub fn user_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
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
    if let Some(home) = user_home() {
        targets.push((home.join(".cursor/skills/chat-history"), "Cursor"));
        targets.push((home.join(".claude/skills/chat-history"), "Claude Code"));
    }
    // Codex: CODEX_HOME, or home-derived ~/.codex when home is known.
    if std::env::var_os("CODEX_HOME").is_some() || user_home().is_some() {
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

enum EnsureAction {
    Wrote,
    Refreshed,
    Skipped,
    Failed,
}

fn ensure_one(dir: &Path, force: bool) -> EnsureAction {
    let skill = skill_path(dir);
    let sidecar = sidecar_path(dir);
    let embedded_hash = content_hash(SKILL_CONTENT);

    if force {
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
                            EnsureAction::Skipped
                        } else if write_managed_skill(dir) {
                            EnsureAction::Refreshed
                        } else {
                            EnsureAction::Failed
                        }
                    } else {
                        // Sidecar present but content changed → user-edited.
                        EnsureAction::Skipped
                    }
                }
                Err(_) => {
                    // Legacy install (no sidecar). Adopt only if it already
                    // matches the current embedded skill; never overwrite edits
                    // or older defaults we can't prove are ours.
                    if on_disk_hash == embedded_hash {
                        let _ = std::fs::write(&sidecar, embedded_hash);
                        EnsureAction::Skipped
                    } else {
                        EnsureAction::Skipped
                    }
                }
            }
        }
    }
}

/// Quietly install or refresh managed skills. Never prints; never overwrites
/// user-edited skills. Safe to call on every CLI invocation.
pub fn ensure_skills() {
    for (dir, _) in skill_targets() {
        let _ = ensure_one(&dir, false);
    }
}

/// Explicit install used by `chat-history install-skill`.
///
/// Without `force`, same rules as [`ensure_skills`] but reports what happened.
/// With `force`, overwrites even user-edited skills.
pub fn install_skill(force: bool) {
    let targets = skill_targets();
    if targets.is_empty() {
        eprintln!("Could not determine home directory (set HOME or USERPROFILE)");
        std::process::exit(1);
    }

    let mut any_ok = false;
    for (dir, name) in &targets {
        match ensure_one(dir, force) {
            EnsureAction::Wrote => {
                println!("  installed → {}", skill_path(dir).display());
                any_ok = true;
            }
            EnsureAction::Refreshed => {
                println!("  refreshed → {}", skill_path(dir).display());
                any_ok = true;
            }
            EnsureAction::Skipped => {
                if skill_path(dir).exists() {
                    println!("  unchanged → {}", skill_path(dir).display());
                    any_ok = true;
                } else {
                    eprintln!("  skip {name}: could not write to {}", dir.display());
                }
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

    fn with_home<F: FnOnce(&Path)>(f: F) {
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
        with_home(|home| {
            let dir = home.join(".claude/skills/chat-history");
            assert!(matches!(ensure_one(&dir, false), EnsureAction::Wrote));
            assert_eq!(fs::read_to_string(skill_path(&dir)).unwrap(), SKILL_CONTENT);
            assert_eq!(
                fs::read_to_string(sidecar_path(&dir)).unwrap(),
                content_hash(SKILL_CONTENT)
            );
        });
    }

    #[test]
    fn refreshes_managed_stale_copy() {
        with_home(|home| {
            let dir = home.join(".claude/skills/chat-history");
            fs::create_dir_all(&dir).unwrap();
            fs::write(skill_path(&dir), "old default").unwrap();
            fs::write(sidecar_path(&dir), content_hash("old default")).unwrap();
            assert!(matches!(ensure_one(&dir, false), EnsureAction::Refreshed));
            assert_eq!(fs::read_to_string(skill_path(&dir)).unwrap(), SKILL_CONTENT);
        });
    }

    #[test]
    fn preserves_user_edits() {
        with_home(|home| {
            let dir = home.join(".claude/skills/chat-history");
            fs::create_dir_all(&dir).unwrap();
            fs::write(skill_path(&dir), "my custom skill").unwrap();
            fs::write(sidecar_path(&dir), content_hash("old default")).unwrap();
            assert!(matches!(ensure_one(&dir, false), EnsureAction::Skipped));
            assert_eq!(
                fs::read_to_string(skill_path(&dir)).unwrap(),
                "my custom skill"
            );
        });
    }

    #[test]
    fn preserves_legacy_without_sidecar() {
        with_home(|home| {
            let dir = home.join(".claude/skills/chat-history");
            fs::create_dir_all(&dir).unwrap();
            fs::write(skill_path(&dir), "legacy custom").unwrap();
            assert!(matches!(ensure_one(&dir, false), EnsureAction::Skipped));
            assert_eq!(
                fs::read_to_string(skill_path(&dir)).unwrap(),
                "legacy custom"
            );
            assert!(!sidecar_path(&dir).exists());
        });
    }

    #[test]
    fn force_overwrites_user_edits() {
        with_home(|home| {
            let dir = home.join(".claude/skills/chat-history");
            fs::create_dir_all(&dir).unwrap();
            fs::write(skill_path(&dir), "my custom skill").unwrap();
            assert!(matches!(ensure_one(&dir, true), EnsureAction::Wrote));
            assert_eq!(fs::read_to_string(skill_path(&dir)).unwrap(), SKILL_CONTENT);
        });
    }

    #[test]
    fn adopts_legacy_matching_embedded() {
        with_home(|home| {
            let dir = home.join(".claude/skills/chat-history");
            fs::create_dir_all(&dir).unwrap();
            fs::write(skill_path(&dir), SKILL_CONTENT).unwrap();
            assert!(matches!(ensure_one(&dir, false), EnsureAction::Skipped));
            assert_eq!(
                fs::read_to_string(sidecar_path(&dir)).unwrap(),
                content_hash(SKILL_CONTENT)
            );
        });
    }
}
