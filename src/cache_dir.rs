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
                Some(dir) => resolve_with(Path::new(&dir), true, &[]),
                None => {
                    let home = crate::session::user_home()?.join(".chat-history/cache");
                    resolve_with(&home, false, &fallback_roots())
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
/// candidate directories. When no fallback root is usable the unwritable
/// default is returned anyway, so the backend's open fails with its usual
/// warning rather than silently searching without a cache.
pub fn resolve_with(
    preferred: &Path,
    explicit: bool,
    fallback_roots: &[PathBuf],
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
    for root in fallback_roots {
        if private_dir(root).is_none() {
            continue;
        }
        let dir = root.join("cache");
        let Some(created) = private_dir(&dir) else {
            continue;
        };
        return Some(Resolved {
            dir,
            kind: Kind::Fallback {
                from: preferred.to_path_buf(),
                created,
            },
        });
    }
    Some(Resolved {
        dir: preferred.to_path_buf(),
        kind: Kind::Default,
    })
}

/// User-private roots to try in order. Sandboxes for coding agents keep temp
/// directories writable when the home directory is not. The shared `/tmp`
/// comes first because it outlives a session, while some agents give each
/// session its own `$TMPDIR`; a copy there would be redone every session.
fn fallback_roots() -> Vec<PathBuf> {
    // Undocumented: lets tests keep their fallback out of the real /tmp.
    if let Some(root) = std::env::var_os("CHAT_HISTORY_FALLBACK_ROOT").filter(|v| !v.is_empty()) {
        return vec![PathBuf::from(root)];
    }
    #[cfg(unix)]
    // SAFETY: getuid has no preconditions and cannot fail.
    let owner = unsafe { libc::getuid() }.to_string();
    #[cfg(not(unix))]
    let owner = match std::env::var("USERNAME") {
        Ok(name) => name,
        Err(_) => return Vec::new(),
    };
    let name = format!("chat-history-{owner}");
    let mut roots = Vec::new();
    #[cfg(unix)]
    roots.push(PathBuf::from("/tmp").join(&name));
    let session = std::env::temp_dir().join(&name);
    if !roots.contains(&session) {
        roots.push(session);
    }
    roots
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
    // Named uniquely enough that a probe left by a killed process with a
    // reused PID cannot make a writable directory look unwritable.
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let probe = dir.join(format!(".write-probe-{}-{nonce}", std::process::id()));
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
/// Returns whether this call created it.
fn private_dir(dir: &Path) -> Option<bool> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    let created = match builder.create(dir) {
        Ok(()) => true,
        Err(e) if e.kind() == ErrorKind::AlreadyExists => false,
        Err(_) => return None,
    };
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
    Some(created)
}

/// Copies one cache database into `to` unless a copy is already there. Any
/// SQLite sidecar beside the source means it is in use or was left by an
/// interrupted writer, and the copy is skipped: copying a database and its
/// WAL separately could pair a pre-checkpoint file with post-checkpoint
/// frames, which is inconsistent yet structurally valid. The next unsandboxed
/// run recovers such a WAL, after which seeding proceeds. A copy whose source
/// changed underneath it is discarded. Each process cleans up only its own
/// staging file: another process's may be a copy in progress, and a leftover
/// from a crash is the temp directory's to expire. The copy is only a head
/// start: every session is re-verified on the next sync.
fn seed_file(from: &Path, to: &Path, name: &str) {
    let target = to.join(name);
    let in_use = ["-wal", "-shm", "-journal"]
        .iter()
        .any(|suffix| from.join(format!("{name}{suffix}")).exists());
    if target.exists() || in_use {
        return;
    }
    let staging = to.join(format!(".{name}.seed-{}", std::process::id()));
    if copy_unchanged(&from.join(name), &staging) {
        place(&staging, &target);
    }
    let _ = fs::remove_file(&staging);
}

/// Copies `source` to `staging` privately, succeeding only if the source did
/// not change while it was being read.
fn copy_unchanged(source: &Path, staging: &Path) -> bool {
    let Ok(before) = fs::metadata(source) else {
        return false;
    };
    let copied = fs::copy(source, staging).is_ok()
        && fs::metadata(source).is_ok_and(|after| {
            after.len() == before.len() && after.modified().ok() == before.modified().ok()
        });
    #[cfg(unix)]
    let copied = copied && {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(staging, fs::Permissions::from_mode(0o600)).is_ok()
    };
    copied
}

/// Moves a finished copy into place only if nothing is there yet. A hard
/// link refuses an existing target atomically, so two processes seeding at
/// once cannot replace a copy the other has already opened.
fn place(staging: &Path, target: &Path) -> bool {
    let placed = match fs::hard_link(staging, target) {
        Ok(()) => true,
        Err(e) if e.kind() == ErrorKind::AlreadyExists => false,
        // No hard links on this filesystem: a guarded rename is the best left.
        Err(_) => !target.exists() && fs::rename(staging, target).is_ok(),
    };
    let _ = fs::remove_file(staging);
    placed
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    /// Restores write permission on drop so the temp directory can be removed.
    struct ReadOnly(PathBuf);

    impl Drop for ReadOnly {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
        }
    }

    fn read_only(dir: &Path) -> ReadOnly {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o500)).unwrap();
        ReadOnly(dir.to_path_buf())
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

    fn unwritable_cache(tmp: &TempDir) -> (PathBuf, ReadOnly) {
        let preferred = tmp.path().join("cache");
        fs::create_dir(&preferred).unwrap();
        fs::write(preferred.join("search-v2.db"), "index").unwrap();
        fs::write(preferred.join("catalog-v1.db"), "catalog").unwrap();
        let guard = read_only(&preferred);
        (preferred, guard)
    }

    #[test]
    fn private_dir_reports_whether_it_created_the_directory() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("private");
        assert_eq!(private_dir(&dir), Some(true));
        assert_eq!(private_dir(&dir), Some(false));
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn writable_default_directory_is_used_as_is() {
        let tmp = TempDir::new().unwrap();
        let preferred = tmp.path().join("home/.chat-history/cache");
        let root = tmp.path().join("tmp");
        let resolved = resolve_with(&preferred, false, std::slice::from_ref(&root)).unwrap();
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
        let _guard = read_only(&preferred);
        let root = tmp.path().join("tmp");
        let resolved = resolve_with(&preferred, true, std::slice::from_ref(&root)).unwrap();
        assert_eq!(resolved.dir, preferred);
        assert!(matches!(resolved.kind, Kind::Explicit));
        assert!(!root.exists());
    }

    #[test]
    fn unwritable_default_falls_back_to_a_private_directory_without_copying() {
        let tmp = TempDir::new().unwrap();
        let (preferred, _guard) = unwritable_cache(&tmp);
        let root = tmp.path().join("tmp");
        let resolved = resolve_with(&preferred, false, std::slice::from_ref(&root)).unwrap();
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
        let again = resolve_with(&preferred, false, std::slice::from_ref(&root)).unwrap();
        assert!(matches!(again.kind, Kind::Fallback { created: false, .. }));
    }

    #[test]
    fn each_database_is_seeded_only_when_asked_for() {
        let tmp = TempDir::new().unwrap();
        let (preferred, _guard) = unwritable_cache(&tmp);
        let root = tmp.path().join("tmp");
        let resolved = resolve_with(&preferred, false, std::slice::from_ref(&root)).unwrap();
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
    fn a_database_with_a_write_ahead_log_is_not_seeded() {
        // Copying the database and WAL separately can pair a pre-checkpoint
        // file with post-checkpoint frames: inconsistent, yet structurally
        // valid. A WAL therefore means "in use", and the next unsandboxed run
        // recovers a WAL an interrupted writer left behind.
        let tmp = TempDir::new().unwrap();
        let preferred = tmp.path().join("cache");
        fs::create_dir(&preferred).unwrap();
        fs::write(preferred.join("search-v2.db"), "index").unwrap();
        fs::write(preferred.join("search-v2.db-wal"), "frames").unwrap();
        let to = tmp.path().join("to");
        fs::create_dir(&to).unwrap();
        seed_file(&preferred, &to, "search-v2.db");
        assert!(fs::read_dir(&to).unwrap().next().is_none());
    }

    #[test]
    fn a_database_with_any_other_sidecar_is_not_seeded_either() {
        let tmp = TempDir::new().unwrap();
        let preferred = tmp.path().join("cache");
        fs::create_dir(&preferred).unwrap();
        let to = tmp.path().join("to");
        fs::create_dir(&to).unwrap();
        for sidecar in ["-shm", "-journal"] {
            fs::write(preferred.join("search-v2.db"), "busy").unwrap();
            fs::write(preferred.join(format!("search-v2.db{sidecar}")), "x").unwrap();
            seed_file(&preferred, &to, "search-v2.db");
            assert!(!to.join("search-v2.db").exists(), "{sidecar} means in use");
            fs::remove_file(preferred.join(format!("search-v2.db{sidecar}"))).unwrap();
        }
    }

    #[test]
    fn a_probe_file_left_by_a_killed_process_does_not_make_a_directory_unwritable() {
        let tmp = TempDir::new().unwrap();
        let preferred = tmp.path().join("cache");
        fs::create_dir(&preferred).unwrap();
        fs::write(
            preferred.join(format!(".write-probe-{}", std::process::id())),
            "leftover",
        )
        .unwrap();
        let root = tmp.path().join("tmp");
        let resolved = resolve_with(&preferred, false, std::slice::from_ref(&root)).unwrap();
        assert!(matches!(resolved.kind, Kind::Default));
    }

    #[test]
    fn seeding_never_replaces_an_existing_copy() {
        let tmp = TempDir::new().unwrap();
        let preferred = tmp.path().join("cache");
        fs::create_dir(&preferred).unwrap();
        fs::write(preferred.join("catalog-v1.db"), "new catalog").unwrap();
        let _guard = read_only(&preferred);
        let to = tmp.path().join("to");
        fs::create_dir(&to).unwrap();
        fs::write(to.join("catalog-v1.db"), "old catalog").unwrap();
        seed_file(&preferred, &to, "catalog-v1.db");
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
        let (preferred, _guard) = unwritable_cache(&tmp);
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
    fn a_fallback_root_that_is_a_symlink_is_refused_and_the_default_kept() {
        let tmp = TempDir::new().unwrap();
        let preferred = tmp.path().join("cache");
        fs::create_dir(&preferred).unwrap();
        let _guard = read_only(&preferred);
        let target = tmp.path().join("elsewhere");
        fs::create_dir(&target).unwrap();
        let root = tmp.path().join("tmp");
        std::os::unix::fs::symlink(&target, &root).unwrap();
        // The unwritable default is returned so its open fails with today's
        // warning instead of a silent in-memory search.
        let resolved = resolve_with(&preferred, false, &[root]).unwrap();
        assert_eq!(resolved.dir, preferred);
        assert!(matches!(resolved.kind, Kind::Default));
        assert!(fs::read_dir(&target).unwrap().next().is_none());
    }

    #[test]
    fn the_first_usable_fallback_root_wins() {
        let tmp = TempDir::new().unwrap();
        let (preferred, _guard) = unwritable_cache(&tmp);
        let unusable = tmp.path().join("unusable");
        fs::create_dir(&unusable).unwrap();
        let _unusable = read_only(&unusable);
        let first = unusable.join("chat-history-1");
        fs::create_dir(tmp.path().join("tmp")).unwrap();
        let second = tmp.path().join("tmp/chat-history-1");
        let resolved = resolve_with(&preferred, false, &[first.clone(), second.clone()]).unwrap();
        assert_eq!(resolved.dir, second.join("cache"));
        assert!(!first.exists());
    }

    #[test]
    fn a_cache_child_that_is_a_symlink_is_refused_even_inside_a_valid_root() {
        let tmp = TempDir::new().unwrap();
        let preferred = tmp.path().join("cache");
        fs::create_dir(&preferred).unwrap();
        let _guard = read_only(&preferred);
        let root = tmp.path().join("tmp");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let target = tmp.path().join("elsewhere");
        fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, root.join("cache")).unwrap();
        let resolved = resolve_with(&preferred, false, &[root]).unwrap();
        assert_eq!(resolved.dir, preferred);
        assert!(matches!(resolved.kind, Kind::Default));
    }

    #[test]
    fn no_fallback_root_keeps_the_default_so_its_failure_is_reported() {
        let tmp = TempDir::new().unwrap();
        let preferred = tmp.path().join("cache");
        fs::create_dir(&preferred).unwrap();
        let _guard = read_only(&preferred);
        let resolved = resolve_with(&preferred, false, &[]).unwrap();
        assert_eq!(resolved.dir, preferred);
        assert!(matches!(resolved.kind, Kind::Default));
    }
}
