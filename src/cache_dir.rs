//! Chooses where the disposable caches live. The default directory is under
//! the user's home; when a sandbox makes it unwritable, a user-private
//! directory under the OS temp dir is used instead, seeded from the home copy
//! so a sandboxed search does not rebuild an index that already exists.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Cache databases worth carrying into a fallback directory. Older
/// generations are left behind; the owning module removes them on open.
fn seedable() -> [String; 2] {
    [
        crate::search_index::INDEX_FILENAME.to_owned(),
        crate::catalog::filename(),
    ]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// `CHAT_HISTORY_CACHE_DIR` or `--cache-dir`: used as given, never replaced.
    Explicit,
    /// The writable default under the user's home.
    Default,
    /// A private temp directory standing in for an unwritable default.
    Fallback { from: PathBuf, created: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub dir: PathBuf,
    pub kind: Kind,
}

/// The process-wide cache directory: the environment override, else the
/// default under the home directory, else a private temp copy. Prints one
/// note the first time the temp copy is created; reuse is quiet.
pub fn resolve() -> Option<&'static Resolved> {
    static RESOLVED: OnceLock<Option<Resolved>> = OnceLock::new();
    RESOLVED
        .get_or_init(|| {
            let resolved = match std::env::var_os("CHAT_HISTORY_CACHE_DIR").filter(|v| !v.is_empty()) {
                Some(dir) => resolve_with(Path::new(&dir), true, None),
                None => {
                    let home = crate::session::user_home()?.join(".chat-history/cache");
                    resolve_with(&home, false, fallback_root().as_deref())
                }
            };
            if let Some(Resolved {
                dir,
                kind: Kind::Fallback {
                    from,
                    created: true,
                },
            }) = &resolved
            {
                let parent = from.parent().unwrap_or(from);
                eprintln!(
                    "Note: {} is not writable here (sandboxed?); using {} instead. Allow writes to {} to reuse the shared cache.",
                    from.display(),
                    dir.display(),
                    parent.display()
                );
            }
            resolved
        })
        .as_ref()
}

/// Selection without process state, for callers and tests that supply the
/// candidate directories. `None` means no usable persistent directory.
pub fn resolve_with(
    preferred: &Path,
    explicit: bool,
    fallback_root: Option<&Path>,
) -> Option<Resolved> {
    if explicit {
        return Some(Resolved {
            dir: preferred.to_path_buf(),
            kind: Kind::Explicit,
        });
    }
    if writable(preferred) {
        return Some(Resolved {
            dir: preferred.to_path_buf(),
            kind: Kind::Default,
        });
    }
    let root = fallback_root?;
    private_dir(root)?;
    let dir = root.join("cache");
    let created = !dir.is_dir();
    if created {
        private_dir(&dir)?;
    }
    seed(preferred, &dir);
    Some(Resolved {
        dir,
        kind: Kind::Fallback {
            from: preferred.to_path_buf(),
            created,
        },
    })
}

/// A user-private root under the OS temp directory. Sandboxes for coding
/// agents keep the temp directory writable when the home directory is not.
fn fallback_root() -> Option<PathBuf> {
    #[cfg(unix)]
    // SAFETY: getuid has no preconditions and cannot fail.
    let owner = unsafe { libc::getuid() }.to_string();
    #[cfg(not(unix))]
    let owner = std::env::var("USERNAME").ok()?;
    Some(std::env::temp_dir().join(format!("chat-history-{owner}")))
}

/// Whether this process can create files in `dir`, creating it if needed.
/// Sandboxes deny at the system call, not in permission bits, so the probe
/// performs the same operation SQLite needs for its WAL sidecars.
fn writable(dir: &Path) -> bool {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    if builder.create(dir).is_err() {
        return false;
    }
    let probe = dir.join(format!(".write-probe-{}", std::process::id()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&probe) {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Creates `dir` privately or accepts an existing one only if it is a real
/// directory owned by this user. Temp directories are shared, so a symlink
/// or foreign directory at the expected name is refused rather than used.
fn private_dir(dir: &Path) -> Option<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
        Err(_) => return None,
    }
    let meta = fs::symlink_metadata(dir).ok()?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        // SAFETY: getuid has no preconditions and cannot fail.
        if meta.uid() != unsafe { libc::getuid() } {
            return None;
        }
        if meta.mode() & 0o077 != 0 {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).ok()?;
        }
    }
    Some(())
}

/// Copies current cache databases that are not already present in `to`.
/// A database with a write-ahead log beside it may be mid-update and is
/// skipped; a copy whose source changed underneath it is discarded. The
/// copies are only a head start: every session is re-verified on sync.
fn seed(from: &Path, to: &Path) {
    for name in seedable() {
        let source = from.join(&name);
        let target = to.join(&name);
        if target.exists() || from.join(format!("{name}-wal")).exists() {
            continue;
        }
        let Ok(before) = fs::metadata(&source) else {
            continue;
        };
        let staging = to.join(format!(".{name}.seed-{}", std::process::id()));
        let copied = fs::copy(&source, &staging).is_ok()
            && fs::metadata(&source).is_ok_and(|after| {
                after.len() == before.len() && after.modified().ok() == before.modified().ok()
            });
        #[cfg(unix)]
        let copied = copied && {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&staging, fs::Permissions::from_mode(0o600)).is_ok()
        };
        if !copied || fs::rename(&staging, &target).is_err() {
            let _ = fs::remove_file(&staging);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn read_only(dir: &std::path::Path) {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o500)).unwrap();
    }

    #[test]
    fn writable_default_directory_is_used_as_is() {
        let tmp = TempDir::new().unwrap();
        let preferred = tmp.path().join("home/.chat-history/cache");
        let root = tmp.path().join("tmp");
        let resolved = resolve_with(&preferred, false, Some(&root)).unwrap();
        assert_eq!(resolved.dir, preferred);
        assert!(matches!(resolved.kind, Kind::Default));
        assert!(preferred.is_dir());
        assert!(
            !root.exists(),
            "no fallback directory is created when unneeded"
        );
    }

    #[test]
    fn explicit_directory_never_falls_back() {
        let tmp = TempDir::new().unwrap();
        let preferred = tmp.path().join("explicit");
        fs::create_dir(&preferred).unwrap();
        read_only(&preferred);
        let root = tmp.path().join("tmp");
        let resolved = resolve_with(&preferred, true, Some(&root)).unwrap();
        assert_eq!(resolved.dir, preferred);
        assert!(matches!(resolved.kind, Kind::Explicit));
        assert!(!root.exists());
    }

    #[test]
    fn unwritable_default_falls_back_to_a_private_seeded_copy() {
        let tmp = TempDir::new().unwrap();
        let preferred = tmp.path().join("cache");
        fs::create_dir(&preferred).unwrap();
        fs::write(preferred.join("search-v2.db"), "index").unwrap();
        fs::write(preferred.join("catalog-v1.db"), "catalog").unwrap();
        read_only(&preferred);
        let root = tmp.path().join("tmp");
        let resolved = resolve_with(&preferred, false, Some(&root)).unwrap();
        assert_eq!(resolved.dir, root.join("cache"));
        match &resolved.kind {
            Kind::Fallback { from, created } => {
                assert_eq!(from, &preferred);
                assert!(created);
            }
            other => panic!("expected fallback, got {other:?}"),
        }
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(fs::read(root.join("cache/search-v2.db")).unwrap(), b"index");
        assert_eq!(
            fs::read(root.join("cache/catalog-v1.db")).unwrap(),
            b"catalog"
        );
        let again = resolve_with(&preferred, false, Some(&root)).unwrap();
        assert!(matches!(again.kind, Kind::Fallback { created: false, .. }));
    }

    #[test]
    fn seeding_skips_databases_with_an_open_write_ahead_log_or_existing_copies() {
        let tmp = TempDir::new().unwrap();
        let preferred = tmp.path().join("cache");
        fs::create_dir(&preferred).unwrap();
        fs::write(preferred.join("search-v2.db"), "busy").unwrap();
        fs::write(preferred.join("search-v2.db-wal"), "frames").unwrap();
        fs::write(preferred.join("catalog-v1.db"), "new catalog").unwrap();
        read_only(&preferred);
        let root = tmp.path().join("tmp");
        fs::create_dir_all(root.join("cache")).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(root.join("cache/catalog-v1.db"), "old catalog").unwrap();
        let resolved = resolve_with(&preferred, false, Some(&root)).unwrap();
        assert!(matches!(
            resolved.kind,
            Kind::Fallback { created: false, .. }
        ));
        assert!(!root.join("cache/search-v2.db").exists());
        assert_eq!(
            fs::read(root.join("cache/catalog-v1.db")).unwrap(),
            b"old catalog"
        );
    }

    #[test]
    fn a_fallback_root_that_is_a_symlink_is_refused() {
        let tmp = TempDir::new().unwrap();
        let preferred = tmp.path().join("cache");
        fs::create_dir(&preferred).unwrap();
        read_only(&preferred);
        let target = tmp.path().join("elsewhere");
        fs::create_dir(&target).unwrap();
        let root = tmp.path().join("tmp");
        std::os::unix::fs::symlink(&target, &root).unwrap();
        assert!(resolve_with(&preferred, false, Some(&root)).is_none());
    }

    #[test]
    fn no_fallback_root_means_no_directory() {
        let tmp = TempDir::new().unwrap();
        let preferred = tmp.path().join("cache");
        fs::create_dir(&preferred).unwrap();
        read_only(&preferred);
        assert!(resolve_with(&preferred, false, None).is_none());
    }
}
