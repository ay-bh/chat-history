//! Disposable metadata cache. Discovery and source merging stay live: only
//! extraction from unchanged files is memoized, never parsed message bodies.
use rusqlite::{Connection, params};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

// Bump when cached extraction semantics or serialized types change.
const VERSION: &str = "1";
const RACY_WINDOW: Duration = Duration::from_secs(2);

fn timestamp(time: SystemTime) -> i128 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    }
}

#[derive(Serialize, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: i128,
    #[cfg(unix)]
    changed: i128,
    #[cfg(unix)]
    identity: (u64, u64, u32),
}

fn stamp(path: &Path) -> std::io::Result<Stamp> {
    // Track the target: these are the bytes the parser reads. Using only
    // symlink_metadata would miss edits through an unchanged symlink.
    let meta = fs::metadata(path)?;
    ACTIVE.with(|slot| {
        if let Some(cache) = slot.borrow_mut().as_mut() {
            cache.metadata.insert(path.to_path_buf(), meta.clone());
        }
    });
    Ok(Stamp {
        len: meta.len(),
        modified: timestamp(meta.modified()?),
        #[cfg(unix)]
        changed: {
            use std::os::unix::fs::MetadataExt;
            i128::from(meta.ctime()) * 1_000_000_000 + i128::from(meta.ctime_nsec())
        },
        // Detect replacements, same-size edits with restored mtime, and chmod.
        #[cfg(unix)]
        identity: {
            use std::os::unix::fs::MetadataExt;
            (meta.dev(), meta.ino(), meta.mode())
        },
    })
}

impl Stamp {
    fn stable_at(&self, now: SystemTime) -> bool {
        let cutoff = timestamp(now) - RACY_WINDOW.as_nanos() as i128;
        let stable = self.modified < cutoff;
        #[cfg(unix)]
        let stable = stable && self.changed < cutoff;
        stable
    }
}

fn fingerprint_at(path: &Path, sqlite: bool, now: SystemTime) -> Option<String> {
    let mut stamps = vec![Some(stamp(path).ok()?)];
    if sqlite {
        // SQLite can commit entirely to the WAL without touching the main DB.
        // Do not watch -shm: readers themselves change that file.
        for suffix in ["-wal", "-journal"] {
            let mut name = path.as_os_str().to_os_string();
            name.push(suffix);
            stamps.push(match stamp(Path::new(&name)) {
                Ok(s) => Some(s),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(_) => return None,
            });
        }
    }
    // Do not save a stat-only fingerprint that could match another write in
    // the same coarse filesystem clock tick, even after this invocation ends.
    if !stamps.iter().flatten().all(|s| s.stable_at(now)) {
        return None;
    }
    serde_json::to_string(&stamps).ok()
}

/// Listing dates reuse the metadata already read for cache validation.
pub(crate) fn metadata(path: &Path) -> std::io::Result<fs::Metadata> {
    if let Some(meta) = ACTIVE.with(|slot| slot.borrow().as_ref()?.metadata.get(path).cloned()) {
        return Ok(meta);
    }
    let meta = fs::metadata(path)?;
    ACTIVE.with(|slot| {
        if let Some(cache) = slot.borrow_mut().as_mut() {
            cache.metadata.insert(path.to_path_buf(), meta.clone());
        }
    });
    Ok(meta)
}

struct Entry {
    source: String,
    fingerprint: String,
    value: String,
    dirty: bool,
}

struct Catalog {
    conn: Connection,
    entries: HashMap<String, Entry>,
    seen: HashSet<String>,
    sources: Vec<String>,
    metadata: HashMap<PathBuf, fs::Metadata>,
    observed_at: SystemTime,
}

impl Catalog {
    fn open(dir: &Path, sources: &[&str]) -> Option<Self> {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(dir).ok()?;
        let path = dir.join(format!("catalog-v{VERSION}.db"));
        // Cached titles and prompt previews have the same sensitivity as the
        // source metadata. Create the DB privately before SQLite opens it.
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return None,
        }
        let conn = Connection::open(&path).ok()?;
        conn.busy_timeout(Duration::from_secs(1)).ok()?;
        // Once configured, opening a warm cache must remain read-only even
        // while another process holds its WAL write transaction.
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .ok()?;
        if mode != "wal" {
            conn.pragma_update(None, "journal_mode", "WAL").ok()?;
        }
        let initialized: bool = conn.query_row(
            "SELECT count(*) = 2 FROM sqlite_schema WHERE name IN ('metadata', 'metadata_source')",
            [], |r| r.get(0)
        ).ok()?;
        if !initialized {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS metadata (
                key TEXT PRIMARY KEY, source TEXT NOT NULL,
                fingerprint TEXT NOT NULL, value TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS metadata_source ON metadata(source)",
            )
            .ok()?;
        }
        let mut entries = HashMap::new();
        {
            let mut stmt = conn
                .prepare("SELECT key, source, fingerprint, value FROM metadata WHERE source = ?1")
                .ok()?;
            for source in sources {
                let rows = stmt
                    .query_map([source], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            Entry {
                                source: row.get(1)?,
                                fingerprint: row.get(2)?,
                                value: row.get(3)?,
                                dirty: false,
                            },
                        ))
                    })
                    .ok()?;
                for row in rows {
                    let (key, entry) = row.ok()?;
                    entries.insert(key, entry);
                }
            }
        }
        Some(Self {
            conn,
            entries,
            seen: HashSet::new(),
            sources: sources.iter().map(|s| (*s).to_owned()).collect(),
            metadata: HashMap::new(),
            observed_at: SystemTime::now(),
        })
    }

    fn flush(mut self) -> rusqlite::Result<()> {
        // An unvisited key is not proof of deletion (a profile switch,
        // unmounted root or failed walk may have skipped it). Require a
        // complete, successful listing of its parent that omits the name.
        let mut parents: HashMap<PathBuf, Option<HashSet<OsString>>> = HashMap::new();
        let stale: Vec<_> = self
            .entries
            .keys()
            .filter(|k| !self.seen.contains(*k))
            .filter(|key| {
                let Ok((_, _, path)) = serde_json::from_str::<(String, String, PathBuf)>(key)
                else {
                    return false;
                };
                let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
                    return false;
                };
                parents
                    .entry(parent.to_path_buf())
                    .or_insert_with(|| {
                        fs::read_dir(parent)
                            .ok()?
                            .map(|e| e.map(|e| e.file_name()))
                            .collect::<std::io::Result<HashSet<_>>>()
                            .ok()
                    })
                    .as_ref()
                    .is_some_and(|names| !names.contains(name))
            })
            .collect();
        if stale.is_empty() && !self.entries.values().any(|e| e.dirty) {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        {
            let mut delete = tx.prepare_cached("DELETE FROM metadata WHERE key = ?1")?;
            for key in stale {
                delete.execute([key])?;
            }
            let mut insert = tx.prepare_cached(
                "INSERT INTO metadata VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(key) DO UPDATE SET fingerprint=excluded.fingerprint, value=excluded.value"
            )?;
            for (key, entry) in &self.entries {
                if entry.dirty && self.seen.contains(key) {
                    insert.execute(params![key, entry.source, entry.fingerprint, entry.value])?;
                }
            }
        }
        tx.commit()
    }
}

thread_local! {
    // Scoped to one load_sessions call; inspect/deep parsing never populates
    // this cache, and separate library calls revalidate source files.
    static ACTIVE: RefCell<Option<Catalog>> = const { RefCell::new(None) };
}

struct Scope;
impl Drop for Scope {
    fn drop(&mut self) {
        if let Some(cache) = ACTIVE.with(|slot| slot.borrow_mut().take()) {
            let _ = cache.flush();
        }
    }
}

pub(crate) fn with_catalog<T>(sources: &[&str], load: impl FnOnce() -> T) -> T {
    if std::env::var_os("CHAT_HISTORY_NO_CACHE").is_some() {
        return load();
    }
    let dir = std::env::var_os("CHAT_HISTORY_CACHE_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::session::user_home().map(|p| p.join(".chat-history/cache")));
    with_directory(dir.as_deref(), sources, load)
}

fn with_directory<T>(dir: Option<&Path>, sources: &[&str], load: impl FnOnce() -> T) -> T {
    if ACTIVE.with(|slot| slot.borrow().is_some()) {
        return load();
    }
    ACTIVE.with(|slot| *slot.borrow_mut() = dir.and_then(|d| Catalog::open(d, sources)));
    let _scope = Scope;
    load()
}

/// Cache only successful, stable reads. Missing/unreadable inputs and source
/// changes during extraction are retried on the next invocation.
pub(crate) fn read<T: Serialize + DeserializeOwned>(
    source: &str,
    kind: &str,
    path: &Path,
    sqlite: bool,
    load: impl FnOnce() -> Option<T>,
) -> Option<T> {
    if !ACTIVE.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|c| c.sources.iter().any(|s| s == source))
    }) {
        return load();
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else if let Ok(cwd) = std::env::current_dir() {
        cwd.join(path)
    } else {
        return load();
    };
    let Some(path_key) = absolute.to_str() else {
        return load();
    };
    let key = serde_json::to_string(&(source, kind, path_key)).ok()?;
    let observed_at = ACTIVE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let cache = slot.as_mut().unwrap();
        // Preserve an existing row even when stat or extraction fails.
        cache.seen.insert(key.clone());
        cache.observed_at
    });
    let Some(before) = fingerprint_at(path, sqlite, observed_at) else {
        return load();
    };
    let hit = ACTIVE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let cache = slot.as_mut()?;
        let entry = cache.entries.get(&key)?;
        if entry.fingerprint != before {
            return None;
        }
        serde_json::from_str::<T>(&entry.value).ok()
    });
    if hit.is_some() {
        return hit;
    }
    let result = load();
    let unchanged = fingerprint_at(path, sqlite, observed_at).as_ref() == Some(&before);
    ACTIVE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(cache) = slot.as_mut() else { return };
        if let Some(value) = result.as_ref()
            && unchanged
            && let Ok(value) = serde_json::to_string(value)
        {
            cache.seen.insert(key.clone());
            cache.entries.insert(
                key,
                Entry {
                    source: source.into(),
                    fingerprint: before,
                    value,
                    dirty: true,
                },
            );
        }
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use tempfile::TempDir;

    // Advance the observation clock, not filesystem ctime, for deterministic
    // cache-hit tests. Racy-stat tests below exercise the real boundary.
    fn with_directory<T>(dir: Option<&Path>, sources: &[&str], load: impl FnOnce() -> T) -> T {
        super::with_directory(dir, sources, || {
            ACTIVE.with(|slot| {
                if let Some(c) = slot.borrow_mut().as_mut() {
                    c.observed_at = SystemTime::now() + Duration::from_secs(3);
                }
            });
            load()
        })
    }

    fn fingerprint(path: &Path, sqlite: bool) -> Option<String> {
        fingerprint_at(path, sqlite, SystemTime::now() + Duration::from_secs(3))
    }

    fn cached_text(dir: &Path, path: &Path, calls: &Cell<usize>) -> String {
        with_directory(Some(dir), &["claude"], || {
            read("claude", "text", path, false, || {
                calls.set(calls.get() + 1);
                fs::read_to_string(path).ok()
            })
            .unwrap()
        })
    }

    #[test]
    fn persists_hits_and_refreshes_edits() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("cache");
        let path = tmp.path().join("source");
        fs::write(&path, "first").unwrap();
        let calls = Cell::new(0);
        assert_eq!(cached_text(&dir, &path, &calls), "first");
        assert_eq!(cached_text(&dir, &path, &calls), "first");
        assert_eq!(calls.get(), 1);
        fs::write(&path, "other").unwrap();
        assert_eq!(cached_text(&dir, &path, &calls), "other");
        assert_eq!(calls.get(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn unix_ctime_catches_same_size_edits_with_restored_mtime() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("cache");
        let path = tmp.path().join("source");
        fs::write(&path, "first").unwrap();
        let calls = Cell::new(0);
        assert_eq!(cached_text(&dir, &path, &calls), "first");
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        fs::write(&path, "other").unwrap();
        fs::File::open(&path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        assert_eq!(cached_text(&dir, &path, &calls), "other");
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn changed_files_do_not_reextract_unchanged_neighbors_and_deleted_rows_are_pruned() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("cache");
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        fs::write(&a, "a").unwrap();
        fs::write(&b, "b").unwrap();
        let calls = Cell::new(0);
        let scan = || {
            with_directory(Some(&dir), &["claude"], || {
                for path in [&a, &b] {
                    if path.exists() {
                        read("claude", "text", path, false, || {
                            calls.set(calls.get() + 1);
                            fs::read_to_string(path).ok()
                        });
                    }
                }
            })
        };
        scan();
        assert_eq!(calls.get(), 2);
        fs::write(&a, "changed").unwrap();
        scan();
        assert_eq!(calls.get(), 3);
        fs::remove_file(&b).unwrap();
        scan();
        let cache = Catalog::open(&dir, &["claude"]).unwrap();
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn filtered_load_does_not_evict_other_sources() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("cache");
        let path = tmp.path().join("source");
        fs::write(&path, "metadata").unwrap();
        with_directory(Some(&dir), &["claude", "codex"], || {
            for source in ["claude", "codex"] {
                read(source, "text", &path, false, || Some(source.to_owned()));
            }
        });
        with_directory(Some(&dir), &["claude"], || {});
        with_directory(Some(&dir), &["codex"], || {
            assert_eq!(
                read::<String>("codex", "text", &path, false, || panic!("cache miss")),
                Some("codex".into())
            );
        });
    }

    #[test]
    fn failures_and_files_changing_during_extraction_are_not_cached() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("cache");
        let path = tmp.path().join("source");
        fs::write(&path, "before").unwrap();
        with_directory(Some(&dir), &["claude"], || {
            assert!(read::<String>("claude", "text", &path, false, || None).is_none());
            assert_eq!(
                read("claude", "text", &path, false, || {
                    fs::write(&path, "after mutation").unwrap();
                    Some("before".to_owned())
                }),
                Some("before".into())
            );
        });
        assert!(Catalog::open(&dir, &["claude"]).unwrap().entries.is_empty());
        assert_eq!(cached_text(&dir, &path, &Cell::new(0)), "after mutation");
    }

    #[test]
    fn sqlite_wal_commits_and_checkpoints_invalidate_without_main_db_mtime_changes() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("cache");
        let db = tmp.path().join("source.db");
        let writer = Connection::open(&db).unwrap();
        writer
            .execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
            CREATE TABLE item(value TEXT); INSERT INTO item VALUES ('before');
            PRAGMA wal_checkpoint(TRUNCATE);",
            )
            .unwrap();
        let main_stamp = fingerprint(&db, false).unwrap();
        let calls = Cell::new(0);
        let load = || {
            with_directory(Some(&dir), &["cursor-ide"], || {
                read("cursor-ide", "snapshot", &db, true, || {
                    calls.set(calls.get() + 1);
                    let reader = Connection::open_with_flags(
                        &db,
                        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                    )
                    .ok()?;
                    reader
                        .query_row("SELECT value FROM item", [], |r| r.get::<_, String>(0))
                        .ok()
                })
                .unwrap()
            })
        };
        assert_eq!(load(), "before");
        assert_eq!(load(), "before");
        assert_eq!(calls.get(), 1);
        writer.execute("UPDATE item SET value='after'", []).unwrap();
        assert_eq!(fingerprint(&db, false).unwrap(), main_stamp);
        assert_eq!(load(), "after");
        assert_eq!(calls.get(), 2);
        writer
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
        assert_eq!(load(), "after");
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn corrupt_or_unavailable_cache_falls_back_and_bad_entries_are_repaired() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("source");
        fs::write(&path, "value").unwrap();
        let calls = Cell::new(0);
        // A file in place of the cache directory must not stop discovery.
        assert_eq!(cached_text(&path, &path, &calls), "value");
        let dir = tmp.path().join("cache");
        fs::create_dir(&dir).unwrap();
        let db = dir.join(format!("catalog-v{VERSION}.db"));
        fs::write(&db, "not sqlite").unwrap();
        assert_eq!(cached_text(&dir, &path, &calls), "value");
        fs::remove_file(&db).unwrap();
        assert_eq!(cached_text(&dir, &path, &calls), "value");
        Connection::open(&db)
            .unwrap()
            .execute("UPDATE metadata SET value='invalid json'", [])
            .unwrap();
        assert_eq!(cached_text(&dir, &path, &calls), "value");
        assert_eq!(cached_text(&dir, &path, &calls), "value");
        assert_eq!(calls.get(), 4);
    }

    #[test]
    fn wal_reader_hits_cache_while_another_connection_is_writing() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("cache");
        let path = tmp.path().join("source");
        fs::write(&path, "value").unwrap();
        let calls = Cell::new(0);
        cached_text(&dir, &path, &calls);
        let locker = Catalog::open(&dir, &["claude"]).unwrap();
        locker.conn.execute_batch("BEGIN EXCLUSIVE").unwrap();
        assert_eq!(cached_text(&dir, &path, &calls), "value");
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn empty_scans_and_unavailable_roots_preserve_existing_entries() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("cache");
        let root = tmp.path().join("projects");
        fs::create_dir(&root).unwrap();
        let path = root.join("source");
        fs::write(&path, "value").unwrap();
        let calls = Cell::new(0);
        cached_text(&dir, &path, &calls);
        // Selecting an empty/different profile must not evict this root.
        with_directory(Some(&dir), &["claude"], || {});
        assert_eq!(cached_text(&dir, &path, &calls), "value");
        assert_eq!(calls.get(), 1);
        let away = tmp.path().join("unmounted");
        fs::rename(&root, &away).unwrap();
        with_directory(Some(&dir), &["claude"], || {});
        with_directory(Some(&dir), &["claude"], || {
            assert!(read::<String>("claude", "text", &path, false, || None).is_none());
        });
        assert_eq!(Catalog::open(&dir, &["claude"]).unwrap().entries.len(), 1);
        fs::rename(&away, &root).unwrap();
        assert_eq!(cached_text(&dir, &path, &calls), "value");
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn failed_reextraction_preserves_the_previous_row() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("cache");
        let path = tmp.path().join("source");
        fs::write(&path, "before").unwrap();
        cached_text(&dir, &path, &Cell::new(0));
        fs::write(&path, "changed").unwrap();
        with_directory(Some(&dir), &["claude"], || {
            assert!(read::<String>("claude", "text", &path, false, || None).is_none());
        });
        let cache = Catalog::open(&dir, &["claude"]).unwrap();
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.entries.values().next().unwrap().value, "\"before\"");
        assert_eq!(cached_text(&dir, &path, &Cell::new(0)), "changed");
    }

    #[cfg(unix)]
    #[test]
    fn permission_failure_does_not_evict_a_warm_row() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("cache");
        let parent = tmp.path().join("projects");
        fs::create_dir(&parent).unwrap();
        let path = parent.join("source");
        fs::write(&path, "value").unwrap();
        let calls = Cell::new(0);
        cached_text(&dir, &path, &calls);
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o000)).unwrap();
        let failed = with_directory(Some(&dir), &["claude"], || {
            read("claude", "text", &path, false, || {
                fs::read_to_string(&path).ok()
            })
        });
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
        // Privileged test runners may still read the file; either way, do
        // not lose its row. On normal Unix runners this exercises EACCES.
        assert!(failed.is_none() || failed.as_deref() == Some("value"));
        assert_eq!(Catalog::open(&dir, &["claude"]).unwrap().entries.len(), 1);
        assert_eq!(cached_text(&dir, &path, &calls), "value");
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn young_and_future_timestamps_are_not_trusted_or_persisted() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("cache");
        let path = tmp.path().join("source");
        fs::write(&path, "first").unwrap();
        let mut s = stamp(&path).unwrap();
        let tick = UNIX_EPOCH + Duration::from_secs(100);
        s.modified = timestamp(tick);
        #[cfg(unix)]
        {
            s.changed = timestamp(tick);
        }
        // Same stat on either side of a same-size write in a one-second tick
        // must never be eligible for reuse or persistence.
        assert!(!s.stable_at(tick));
        assert!(!s.stable_at(tick + RACY_WINDOW));
        assert!(!s.stable_at(tick - Duration::from_secs(1)));
        assert!(s.stable_at(tick + RACY_WINDOW + Duration::from_nanos(1)));
        super::with_directory(Some(&dir), &["claude"], || {
            assert_eq!(
                read("claude", "text", &path, false, || Some("first".to_owned())),
                Some("first".into())
            );
            fs::write(&path, "other").unwrap();
            assert_eq!(
                read("claude", "text", &path, false, || Some("other".to_owned())),
                Some("other".into())
            );
        });
        assert!(Catalog::open(&dir, &["claude"]).unwrap().entries.is_empty());
    }

    #[test]
    fn pre_epoch_mtimes_are_cacheable() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("source");
        fs::write(&path, "same prefix FIRST").unwrap();
        let old = UNIX_EPOCH - Duration::from_secs(60);
        fs::File::open(&path).unwrap().set_modified(old).unwrap();
        let serialized = fingerprint(&path, false).unwrap();
        assert!(serialized.contains("-60000000000"));
        let calls = Cell::new(0);
        let dir = tmp.path().join("cache");
        cached_text(&dir, &path, &calls);
        cached_text(&dir, &path, &calls);
        assert_eq!(calls.get(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_targets_are_revalidated_without_replacing_the_link() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("target");
        let link = tmp.path().join("link");
        fs::write(&target, "first").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let calls = Cell::new(0);
        let dir = tmp.path().join("cache");
        cached_text(&dir, &link, &calls);
        fs::write(&target, "other").unwrap();
        assert_eq!(cached_text(&dir, &link, &calls), "other");
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn concurrent_processes_hit_while_writer_holds_lock() {
        const CHILD: &str = "CHAT_HISTORY_CATALOG_READER_TEST";
        if let Some(root) = std::env::var_os(CHILD) {
            let root = PathBuf::from(root);
            let calls = Cell::new(0);
            assert_eq!(
                cached_text(&root.join("cache"), &root.join("source"), &calls),
                "value"
            );
            assert_eq!(
                calls.get(),
                0,
                "a concurrent reader re-extracted instead of hitting WAL"
            );
            return;
        }
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("cache");
        let path = tmp.path().join("source");
        fs::write(&path, "value").unwrap();
        cached_text(&dir, &path, &Cell::new(0));
        let writer = Catalog::open(&dir, &["claude"]).unwrap();
        writer
            .conn
            .execute_batch("BEGIN IMMEDIATE; UPDATE metadata SET value=value")
            .unwrap();
        let children: Vec<_> = (0..3)
            .map(|_| {
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "catalog::tests::concurrent_processes_hit_while_writer_holds_lock",
                    ])
                    .env(CHILD, tmp.path())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap()
            })
            .collect();
        for child in children {
            let out = child.wait_with_output().unwrap();
            assert!(
                out.status.success(),
                "{} {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        writer.conn.execute_batch("COMMIT").unwrap();
    }

    #[test]
    fn contended_flush_waits_and_persists_its_extraction() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("cache");
        let path = tmp.path().join("source");
        fs::write(&path, "value").unwrap();
        let writer = Catalog::open(&dir, &["claude"]).unwrap();
        writer.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        let (ready, receive) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let task = scope.spawn(|| {
                with_directory(Some(&dir), &["claude"], || {
                    read("claude", "text", &path, false, || {
                        ready.send(()).unwrap();
                        Some("value".to_owned())
                    })
                })
            });
            receive.recv_timeout(Duration::from_secs(2)).unwrap();
            std::thread::sleep(Duration::from_millis(100));
            writer.conn.execute_batch("COMMIT").unwrap();
            assert_eq!(task.join().unwrap(), Some("value".into()));
        });
        let calls = Cell::new(0);
        assert_eq!(cached_text(&dir, &path, &calls), "value");
        assert_eq!(calls.get(), 0, "contended flush discarded its write");
    }

    #[cfg(unix)]
    #[test]
    fn cache_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("cache");
        let cache = Catalog::open(&dir, &["claude"]).unwrap();
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(dir.join(format!("catalog-v{VERSION}.db")))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        drop(cache);
    }
}
