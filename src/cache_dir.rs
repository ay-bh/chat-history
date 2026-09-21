//! Chooses where the disposable caches live. The default directory is under
//! the user's home; when a sandbox makes it unwritable, a user-private
//! directory under the OS temp dir is used instead, seeded from the home copy
//! so a sandboxed search does not rebuild an index that already exists.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

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

/// The cache directory for one database, seeded from the home copy when a
/// fallback is in use. Each backend asks for its own file, so a listing that
/// needs only the metadata catalog never copies the search index.
pub fn prepare(name: &str) -> Option<PathBuf> {
    let resolved = resolve()?;
    if let Kind::Fallback { from, .. } = &resolved.kind {
        seed_file(from, &resolved.dir, name);
    }
    Some(resolved.dir.clone())
}

/// The process-wide cache directory: the environment override, else the
/// default under the home directory, else a private temp directory. Prints
/// one note the first time the temp directory is created; reuse is quiet.
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
    let created = fs::symlink_metadata(&dir).is_err();
    private_dir(&dir)?;
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

/// Copies one cache database into `to` unless a copy is already there. A
/// database with a write-ahead log beside it may be mid-update and is
/// skipped; a copy whose source changed underneath it is discarded. Each
/// process cleans up only its own staging file: another process's file may
/// be a copy in progress, and a leftover from a crash is the temp
/// directory's to expire. The copy is only a head start: every session is
/// re-verified on the next sync.
fn seed_file(from: &Path, to: &Path, name: &str) {
    let source = from.join(name);
    let target = to.join(name);
    if target.exists() || from.join(format!("{name}-wal")).exists() {
        return;
    }
    let Ok(before) = fs::metadata(&source) else {
        return;
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
    if copied {
        place(&staging, &target);
    } else {
        let _ = fs::remove_file(&staging);
    }
}

/// Moves a finished copy into place only if nothing is there yet. A hard
/// link refuses an existing target atomically, so two processes seeding at
/// once cannot replace a copy the other has already opened.
fn place(staging: &Path, target: &Path) -> bool {
    let placed = fs::hard_link(staging, target).is_ok();
    let _ = fs::remove_file(staging);
    placed
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn read_only(dir: &Path) {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o500)).unwrap();
    }

    fn age(path: &Path, secs: u64) {
        let when = std::time::SystemTime::now() - std::time::Duration::from_secs(secs);
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    fn unwritable_cache(tmp: &TempDir) -> PathBuf {
        let preferred = tmp.path().join("cache");
        fs::create_dir(&preferred).unwrap();
        fs::write(preferred.join("search-v2.db"), "index").unwrap();
        fs::write(preferred.join("catalog-v1.db"), "catalog").unwrap();
        read_only(&preferred);
        preferred
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
    fn unwritable_default_falls_back_to_a_private_directory_without_copying() {
        let tmp = TempDir::new().unwrap();
        let preferred = unwritable_cache(&tmp);
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
        for dir in [&root, &root.join("cache")] {
            assert_eq!(
                fs::metadata(dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        assert_eq!(
            fs::read_dir(root.join("cache")).unwrap().count(),
            0,
            "seeding is per database, on demand"
        );
        let again = resolve_with(&preferred, false, Some(&root)).unwrap();
        assert!(matches!(again.kind, Kind::Fallback { created: false, .. }));
    }

    #[test]
    fn each_database_is_seeded_only_when_asked_for() {
        let tmp = TempDir::new().unwrap();
        let preferred = unwritable_cache(&tmp);
        let root = tmp.path().join("tmp");
        let resolved = resolve_with(&preferred, false, Some(&root)).unwrap();
        seed_file(&preferred, &resolved.dir, "catalog-v1.db");
        assert_eq!(
            fs::read(root.join("cache/catalog-v1.db")).unwrap(),
            b"catalog"
        );
        assert!(!root.join("cache/search-v2.db").exists());
        seed_file(&preferred, &resolved.dir, "search-v2.db");
        assert_eq!(fs::read(root.join("cache/search-v2.db")).unwrap(), b"index");
        let mode = fs::metadata(root.join("cache/search-v2.db"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn seeding_skips_a_database_with_a_live_write_ahead_log_or_an_existing_copy() {
        let tmp = TempDir::new().unwrap();
        let preferred = tmp.path().join("cache");
        fs::create_dir(&preferred).unwrap();
        fs::write(preferred.join("search-v2.db"), "busy").unwrap();
        fs::write(preferred.join("search-v2.db-wal"), "frames").unwrap();
        fs::write(preferred.join("catalog-v1.db"), "new catalog").unwrap();
        read_only(&preferred);
        let to = tmp.path().join("to");
        fs::create_dir(&to).unwrap();
        fs::write(to.join("catalog-v1.db"), "old catalog").unwrap();
        seed_file(&preferred, &to, "search-v2.db");
        seed_file(&preferred, &to, "catalog-v1.db");
        assert!(!to.join("search-v2.db").exists());
        assert_eq!(fs::read(to.join("catalog-v1.db")).unwrap(), b"old catalog");
    }

    #[test]
    fn a_copy_placed_first_by_another_process_is_kept() {
        let tmp = TempDir::new().unwrap();
        let staging = tmp.path().join(".search-v2.db.seed-1");
        let target = tmp.path().join("search-v2.db");
        fs::write(&staging, "mine").unwrap();
        fs::write(&target, "theirs").unwrap();
        assert!(!place(&staging, &target));
        assert_eq!(fs::read(&target).unwrap(), b"theirs");
        assert!(!staging.exists(), "staging copy is discarded");
        fs::write(&staging, "mine").unwrap();
        fs::remove_file(&target).unwrap();
        assert!(place(&staging, &target));
        assert_eq!(fs::read(&target).unwrap(), b"mine");
        assert!(!staging.exists());
    }

    #[test]
    fn other_processes_staging_files_are_never_touched() {
        // A copy keeps its source's timestamp on some platforms, so age says
        // nothing about whether another process is still copying.
        let tmp = TempDir::new().unwrap();
        let preferred = unwritable_cache(&tmp);
        age(&preferred.join("search-v2.db"), 8 * 86_400);
        let to = tmp.path().join("to");
        fs::create_dir(&to).unwrap();
        let theirs = to.join(".search-v2.db.seed-99999");
        fs::write(&theirs, "partial").unwrap();
        age(&theirs, 8 * 86_400);
        seed_file(&preferred, &to, "search-v2.db");
        assert!(theirs.exists());
        assert_eq!(fs::read(to.join("search-v2.db")).unwrap(), b"index");
        assert_eq!(
            fs::read_dir(&to).unwrap().count(),
            2,
            "own staging file removed, theirs kept"
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
    fn a_cache_child_that_is_a_symlink_is_refused_even_inside_a_valid_root() {
        let tmp = TempDir::new().unwrap();
        let preferred = tmp.path().join("cache");
        fs::create_dir(&preferred).unwrap();
        read_only(&preferred);
        let root = tmp.path().join("tmp");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let target = tmp.path().join("elsewhere");
        fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, root.join("cache")).unwrap();
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
