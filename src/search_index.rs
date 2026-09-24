//! Replaceable lexical retrieval over a disposable, incrementally refreshed index.
//! Source transcripts remain authoritative. See docs/search-architecture.md.
use crate::parser::{
    cap_excerpt, clean_first_prompt, is_noise, snippet_around_match, strip_terminal_controls,
};
use crate::search::{
    SearchMatch, SearchResult, direct_session_search, parse_timeframe_duration, scored_search,
};
use crate::session::{Message, Session, parse_any_timestamp, parse_session_recovering_timestamps};
use chrono::{DateTime, Utc};
use rayon::prelude::*;
use rusqlite::{Connection, ErrorCode, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::BuildHasher;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub type SearchError = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, SearchError>;
pub const INDEX_FILENAME: &str = "search-v3.db";
/// Earlier generations have another schema: v1 wrote full-text rows through
/// an insert trigger, v2 had no tool-output column. An older binary sharing the
/// directory would recreate its own schema, so they are not migrated in place:
/// a new file is used and the old one emptied once idle.
const PREVIOUS_INDEX_FILENAMES: [&str; 2] = ["search-v1.db", "search-v2.db"];
const PREVIOUS_INDEX_GRACE: Duration = Duration::from_secs(7 * 86_400);
/// An emptied database is a few pages; anything at or below this is done.
const RECLAIMED_SIZE: u64 = 64 * 1024;
const RECLAIM_TIMEOUT: Duration = Duration::from_millis(250);
// Bump for parser, timestamp, chunking or tokenization changes that require
// re-extracting unchanged sources without changing the relational schema.
const EXTRACTION_VERSION: u32 = 3;
const MAX_ANALYZER_TOKENS: usize = 128;
const MAX_PHRASE_TOKENS: usize = 32;
const MAX_PASSAGE_CHARS: usize = 1600;
const PASSAGE_OVERLAP: usize = 200;
// Each batch is one transaction and one FTS5 flush: larger batches mean fewer
// flushes and merges. Batches are bounded by session count and by source
// bytes, so a run of large transcripts cannot pile up parsed text in memory or
// hold the write lock for long.
const PARSE_BATCH_SIZE: usize = 64;
const PARSE_BATCH_BYTES: u64 = 32 << 20;
// A Cursor IDE conversation lives in a database shared by every chat, and a
// CLI store is read for metadata only, so their file sizes say nothing about
// the text parsed; they weigh at most this much in a batch.
const SHARED_DATABASE_WEIGHT: u64 = 4 << 20;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const WRITER_STALL: Duration = Duration::from_secs(10);
const CACHE_MISMATCH_ATTEMPTS: usize = 3;
const SNIPPET_CHARS: usize = 400;
/// BM25 weight of tool output (command output, file reads) relative to the
/// conversation text at 1.0: long logs repeat a term many times and would
/// otherwise outrank the message where it was discussed.
const TOOL_OUTPUT_WEIGHT: f64 = 0.3;

pub(crate) fn grouped_message_oversample(limit: usize) -> usize {
    limit.saturating_mul(6)
}

pub struct SearchRequest<'a> {
    /// Sessions already selected by source, project, branch, date and sidechains.
    pub sessions: &'a [Session],
    pub query: &'a str,
    pub scope: &'a str,
    pub limit: usize,
    pub timeframe: Option<&'a str>,
    pub group_by_session: bool,
}

/// Engines return the same result contract; CLI rendering knows no index details.
pub trait SearchBackend {
    fn search(&mut self, request: &SearchRequest<'_>) -> Result<Vec<SearchResult>>;
}

pub struct LegacyBackend;

impl SearchBackend for LegacyBackend {
    fn search(&mut self, request: &SearchRequest<'_>) -> Result<Vec<SearchResult>> {
        if request.limit == 0 {
            return Ok(Vec::new());
        }
        let results = scored_search(
            request.sessions,
            request.query,
            request.scope,
            if request.group_by_session {
                grouped_message_oversample(request.limit)
            } else {
                request.limit
            },
            request.timeframe,
        );
        Ok(if request.group_by_session {
            crate::search::group_results(results, request.limit)
        } else {
            results
        })
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SyncStats {
    pub updated: usize,
    pub unchanged: usize,
    pub removed: usize,
}

pub struct Bm25Backend {
    conn: Connection,
}

fn session_key(session: &Session) -> String {
    // Include source and path: providers can reuse IDs, as can separate profiles.
    serde_json::to_string(&(&session.source, &session.id, &session.file)).unwrap()
}

fn indexed_session_keys_match(conn: &Connection, corpus: &[Session]) -> Result<bool> {
    let expected: HashSet<String> = corpus.iter().map(session_key).collect();
    let mut stmt = conn.prepare("SELECT key FROM sessions")?;
    let indexed = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    Ok(expected.is_subset(&indexed))
}

fn source_gone(key: &str) -> bool {
    let Ok((source, id, file)) = serde_json::from_str::<(String, String, String)>(key) else {
        return true;
    };
    match fs::symlink_metadata(&file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => false,
        Ok(_) if source == "cursor-ide" => crate::cursor_ide::open_ro(Path::new(&file))
            .and_then(|c| {
                c.query_row(
                    "SELECT count(*) FROM cursorDiskKV WHERE key = ?1",
                    [format!("composerData:{id}")],
                    |r| r.get::<_, i64>(0),
                )
                .ok()
            })
            .is_some_and(|n| n == 0),
        Ok(_) => false,
    }
}

fn is_corrupt(error: &SearchError) -> bool {
    error
        .downcast_ref::<rusqlite::Error>()
        .and_then(rusqlite::Error::sqlite_error_code)
        .is_some_and(|code| matches!(code, ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt))
}

/// Another process may already have reset and refilled the cache, so only a
/// database that still fails its checks is wiped. Damage is not confined to
/// `sessions`: the passage b-tree and the FTS segments are probed as well.
fn cache_is_healthy(conn: &Connection) -> bool {
    let ok = |sql: &str| conn.query_row(sql, [], |r| r.get::<_, String>(0));
    conn.query_row("SELECT count(*) FROM sessions", [], |r| r.get::<_, i64>(0))
        .is_ok()
        && ok("PRAGMA quick_check(1)").is_ok_and(|v| v == "ok")
        && conn
            .execute(
                "INSERT INTO passages_fts(passages_fts) VALUES ('integrity-check')",
                [],
            )
            .is_ok()
}

fn reset_in_place(dir: &Path) -> Result<()> {
    let conn = Connection::open(dir.join(INDEX_FILENAME))?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    if cache_is_healthy(&conn) {
        return Ok(());
    }
    reset_database(&conn)
}

/// Empties a database under SQLite's own locking, so connections other
/// processes hold stay valid and see an empty schema on their next statement.
fn reset_database(conn: &Connection) -> Result<()> {
    conn.set_db_config(
        rusqlite::config::DbConfig::SQLITE_DBCONFIG_RESET_DATABASE,
        true,
    )?;
    let vacuum = conn.execute_batch("VACUUM");
    conn.set_db_config(
        rusqlite::config::DbConfig::SQLITE_DBCONFIG_RESET_DATABASE,
        false,
    )?;
    vacuum?;
    Ok(())
}

/// Best-effort space reclaim of a previous index generation. Nothing proves a
/// file is unopened, so it is never unlinked: it is emptied in place through
/// SQLite, which an older binary still holding it handles as a fresh cache.
/// Only a file idle for a week is touched, so an older binary in daily use is
/// left alone; a concurrent writer makes the reset wait briefly, then give up
/// until a later run.
fn reclaim_previous_generations(dir: &Path) {
    for stale in PREVIOUS_INDEX_FILENAMES {
        let path = dir.join(stale);
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        let in_use = ["-wal", "-shm", "-journal"]
            .iter()
            .any(|suffix| dir.join(format!("{stale}{suffix}")).exists());
        let idle = meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age >= PREVIOUS_INDEX_GRACE);
        if in_use || !idle || meta.len() <= RECLAIMED_SIZE {
            continue;
        }
        let Ok(conn) = Connection::open(&path) else {
            continue;
        };
        if conn.busy_timeout(RECLAIM_TIMEOUT).is_err() {
            continue;
        }
        let _ = reset_database(&conn);
    }
}

/// Index-sync progress on stderr. A terminal gets one line rewritten in
/// place and cleared when the search ends. Anything else, such as an agent
/// capturing output, gets at most one line, and only when the sync is slow
/// enough to notice.
struct Progress {
    terminal: bool,
    last: Instant,
    shown: bool,
}

impl Progress {
    const INTERVAL: Duration = Duration::from_millis(750);

    fn new() -> Self {
        use std::io::IsTerminal;
        Self::starting_at(std::io::stderr().is_terminal(), Instant::now())
    }

    fn starting_at(terminal: bool, start: Instant) -> Self {
        Self {
            terminal,
            last: start,
            shown: false,
        }
    }

    /// What to print now, if anything.
    fn tick(&mut self, done: usize, total: usize, now: Instant) -> Option<String> {
        if now.duration_since(self.last) < Self::INTERVAL || (self.shown && !self.terminal) {
            return None;
        }
        self.last = now;
        self.shown = true;
        Some(if self.terminal {
            format!("\rUpdating search index: {done}/{total} sessions")
        } else {
            format!("Updating search index ({total} sessions); later searches reuse it.\n")
        })
    }

    /// What clears the terminal line once the search is done.
    fn finish(&self) -> Option<String> {
        (self.terminal && self.shown).then(|| "\r\x1b[2K".to_string())
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        if let Some(clear) = self.finish() {
            eprint!("{clear}");
        }
    }
}

fn sync_corpus(
    backend: &mut Bm25Backend,
    corpus: &[Session],
    rebuild: bool,
    progress: &mut Progress,
) -> Result<SyncStats> {
    backend.sync_with_progress(corpus, rebuild, |done, total| {
        if let Some(line) = progress.tick(done, total, Instant::now()) {
            eprint!("{line}");
        }
    })
}

fn memory_search(
    corpus: &[Session],
    request: &SearchRequest<'_>,
    rebuild: bool,
    progress: &mut Progress,
) -> Result<Vec<SearchResult>> {
    let mut backend = Bm25Backend::open(None)?;
    sync_corpus(&mut backend, corpus, rebuild, progress)?;
    backend.search(request)
}

/// CLI orchestration. Cache failures preserve BM25 semantics using an ephemeral
/// index; malformed user queries remain errors rather than triggering a rebuild.
pub fn search_corpus(
    corpus: &[Session],
    request: &SearchRequest<'_>,
    directory: Option<&Path>,
    rebuild: bool,
) -> Result<Vec<SearchResult>> {
    if request.limit == 0 || request.sessions.is_empty() {
        return Ok(Vec::new());
    }
    if let Some(results) = direct_session_search(request.sessions, request.query, request.timeframe)
    {
        return Ok(results);
    }
    if request.scope == "similar" {
        return LegacyBackend.search(request);
    }
    if match_query(request.query)?.is_none() {
        return Ok(Vec::new());
    }
    let mut progress = Progress::new();
    let Some(dir) = directory else {
        return memory_search(corpus, request, rebuild, &mut progress);
    };
    let mut reset_done = false;
    for attempt in 0..CACHE_MISMATCH_ATTEMPTS {
        let mut backend = match Bm25Backend::open(Some(dir)) {
            Ok(backend) => backend,
            Err(error) if is_corrupt(&error) && !reset_done => {
                reset_done = true;
                eprintln!("Warning: search cache is corrupt ({error}); resetting it in place.");
                if let Err(e) = reset_in_place(dir) {
                    eprintln!("Warning: search cache reset failed ({e}).");
                }
                continue;
            }
            Err(error) => {
                eprintln!(
                    "Warning: search cache unavailable ({error}); searching with an in-memory BM25 index."
                );
                return memory_search(corpus, request, rebuild, &mut progress);
            }
        };
        if let Err(error) = sync_corpus(&mut backend, corpus, rebuild, &mut progress) {
            if error.to_string().starts_with("no such table")
                && attempt + 1 < CACHE_MISMATCH_ATTEMPTS
            {
                drop(backend);
                continue;
            }
            if is_corrupt(&error) && !reset_done {
                reset_done = true;
                drop(backend);
                eprintln!("Warning: search cache is corrupt ({error}); resetting it in place.");
                if let Err(e) = reset_in_place(dir) {
                    eprintln!("Warning: search cache reset failed ({e}).");
                }
                continue;
            }
            eprintln!(
                "Warning: could not save search cache ({error}); searching with an in-memory BM25 index."
            );
            return memory_search(corpus, request, rebuild, &mut progress);
        }
        match backend.search(request) {
            Ok(results) => match indexed_session_keys_match(&backend.conn, corpus) {
                Ok(true) => return Ok(results),
                Ok(false) if attempt + 1 < CACHE_MISMATCH_ATTEMPTS => {}
                Ok(false) => {
                    eprintln!(
                        "Warning: search cache replaced during retrieval; searching with an in-memory BM25 index."
                    );
                    break;
                }
                Err(error) => {
                    eprintln!(
                        "Warning: search cache query failed ({error}); searching with an in-memory BM25 index."
                    );
                    return memory_search(corpus, request, rebuild, &mut progress);
                }
            },
            Err(error) if is_corrupt(&error) && !reset_done => {
                // Damaged postings surface at MATCH time, after a clean sync.
                reset_done = true;
                drop(backend);
                eprintln!("Warning: search cache is corrupt ({error}); resetting it in place.");
                if let Err(e) = reset_in_place(dir) {
                    eprintln!("Warning: search cache reset failed ({e}).");
                }
            }
            Err(error) => {
                eprintln!(
                    "Warning: search cache query failed ({error}); searching with an in-memory BM25 index."
                );
                return memory_search(corpus, request, rebuild, &mut progress);
            }
        }
    }
    memory_search(corpus, request, rebuild, &mut progress)
}

#[derive(Clone, Deserialize, Serialize)]
struct CursorStamp {
    path: PathBuf,
    observed: Option<String>,
    digest: String,
}

#[derive(Clone, Deserialize, Serialize)]
struct Fingerprint {
    version: u32,
    signature: Option<String>,
    cursor: Option<CursorStamp>,
}

impl Fingerprint {
    fn matches(&self, other: &Self) -> bool {
        self.version == other.version
            && self.signature.is_some()
            && self.signature == other.signature
    }
}

fn fingerprint(session: &Session, previous: Option<&Fingerprint>) -> Option<Fingerprint> {
    let path = Path::new(&session.file);
    let sqlite = session.file.ends_with(".db") || session.file.ends_with(".vscdb");
    let ide = session.source == "cursor-ide";
    // SQLite conversation bytes, rather than the entire file's mtime, are the
    // authoritative dependency for IDE rows. Files retain conservative stats.
    let source = if ide {
        None
    } else {
        Some(crate::catalog::fingerprint(path, sqlite)?)
    };
    let cursor = if ide || (session.source == "cursor" && !session.is_cursor_store_only()) {
        let db = if ide {
            path.to_path_buf()
        } else {
            crate::cursor_ide::global_vscdb()
        };
        let (observed, digest) = match fs::metadata(&db) {
            Ok(_) => {
                let observed = crate::catalog::fingerprint(&db, true);
                let cached = previous
                    .and_then(|p| p.cursor.as_ref())
                    .filter(|p| p.path == db && observed.is_some() && p.observed == observed);
                if let Some(cached) = cached {
                    (observed, cached.digest.clone())
                } else {
                    let digest = crate::cursor_ide::conversation_fingerprint(&db, &session.id)?;
                    let after = crate::catalog::fingerprint(&db, true);
                    let observed = if observed == after { observed } else { None };
                    (observed, digest)
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !ide => (None, "missing".into()),
            Err(_) => return None,
        };
        Some(CursorStamp {
            path: db,
            observed,
            digest,
        })
    } else {
        None
    };
    let signature = serde_json::to_string(&(
        session,
        source,
        cursor.as_ref().map(|c| (&c.path, &c.digest)),
    ))
    .ok()?;
    Some(Fingerprint {
        version: EXTRACTION_VERSION,
        signature: Some(signature),
        cursor,
    })
}

impl Bm25Backend {
    /// None builds an ephemeral index with identical ranking semantics.
    pub fn open(directory: Option<&Path>) -> Result<Self> {
        let conn = if let Some(dir) = directory {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(dir)?;
            reclaim_previous_generations(dir);
            let path = dir.join(INDEX_FILENAME);
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
                Err(e) => return Err(e.into()),
            }
            Connection::open(path)?
        } else {
            Connection::open_in_memory()?
        };
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.pragma_update(None, "foreign_keys", true)?;
        if directory.is_some() {
            conn.pragma_update(None, "secure_delete", "ON")?;
            conn.pragma_update(None, "journal_size_limit", 67_108_864)?;
            let started = Instant::now();
            loop {
                let mode = conn
                    .query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
                    .and_then(|m| {
                        if m == "wal" {
                            Ok(m)
                        } else {
                            conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
                        }
                    });
                match mode {
                    Ok(m) if m == "wal" => break,
                    Err(e)
                        if e.sqlite_error_code() == Some(ErrorCode::DatabaseBusy)
                            && started.elapsed() < BUSY_TIMEOUT =>
                    {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Ok(_) if started.elapsed() < BUSY_TIMEOUT => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Ok(m) => return Err(format!("could not enable WAL (journal_mode={m})").into()),
                    Err(e) => return Err(e.into()),
                }
            }
        }
        let core_tables: i64 = conn.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name IN ('sessions', 'messages', 'passages')",
            [],
            |r| r.get(0),
        )?;
        if core_tables != 0 && core_tables != 3 {
            return Err("Search cache has incomplete message tables; remove the disposable search database to recreate it.".into());
        }
        let initialized: bool = conn.query_row(
            "SELECT count(*) = 7 FROM sqlite_schema WHERE name IN (
             'sessions', 'messages', 'messages_session', 'passages', 'passages_message',
             'passages_fts', 'passages_delete')",
            [],
            |r| r.get(0),
        )?;
        if !initialized {
            let tx = begin_immediate(&conn)?;
            let initialized: bool = tx.query_row(
                "SELECT count(*) = 7 FROM sqlite_schema WHERE name IN (
                 'sessions', 'messages', 'messages_session', 'passages', 'passages_message',
                 'passages_fts', 'passages_delete')",
                [],
                |r| r.get(0),
            )?;
            if !initialized {
                tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                key TEXT PRIMARY KEY, fingerprint TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY,
                session_key TEXT NOT NULL REFERENCES sessions(key) ON DELETE CASCADE,
                ordinal INTEGER NOT NULL, payload TEXT NOT NULL,
                timestamp INTEGER, errors INTEGER NOT NULL, tools INTEGER NOT NULL,
                files INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS messages_session ON messages(session_key);
             CREATE TABLE IF NOT EXISTS passages (
                id INTEGER PRIMARY KEY,
                message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
                content TEXT NOT NULL, title TEXT NOT NULL, prompt TEXT NOT NULL,
                project TEXT NOT NULL, branch TEXT NOT NULL, tool TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS passages_message ON passages(message_id);
             CREATE VIRTUAL TABLE IF NOT EXISTS passages_fts USING fts5(
                content, title, prompt, project, branch, tool,
                content='passages', content_rowid='id',
                tokenize='unicode61 remove_diacritics 2', prefix='2 3 4'
             );
             CREATE TRIGGER IF NOT EXISTS passages_delete AFTER DELETE ON passages BEGIN
                INSERT INTO passages_fts(passages_fts, rowid, content, title, prompt, project, branch, tool)
                VALUES ('delete', old.id, old.content, old.title, old.prompt, old.project, old.branch, old.tool);
             END;",
        )?;
                tx.execute(
                    "INSERT INTO passages_fts(passages_fts) VALUES ('rebuild')",
                    [],
                )?;
            }
            tx.commit()?;
        }
        conn.execute_batch("CREATE TEMP TABLE allowed_sessions (key TEXT PRIMARY KEY)")?;
        Ok(Self { conn })
    }

    /// Refresh against a complete discovery snapshot, before applying user filters.
    /// Missing sessions are dropped from this disposable cache, never from sources.
    pub fn sync(&mut self, corpus: &[Session], rebuild: bool) -> Result<SyncStats> {
        self.sync_with_progress(corpus, rebuild, |_, _| {})
    }

    pub fn sync_with_progress(
        &mut self,
        corpus: &[Session],
        rebuild: bool,
        mut progress: impl FnMut(usize, usize),
    ) -> Result<SyncStats> {
        let live: HashSet<String> = corpus.iter().map(session_key).collect();
        let existing = read_existing(&self.conn)?;
        let fingerprints: HashMap<_, Fingerprint> = existing
            .iter()
            .filter_map(|(k, v)| serde_json::from_str(v).ok().map(|s| (k.clone(), s)))
            .collect();
        let plan: Vec<(String, Option<Fingerprint>, bool)> = corpus
            .par_iter()
            .map(|session| {
                let key = session_key(session);
                let previous = fingerprints.get(&key);
                let before = fingerprint(session, previous);
                let skip = !rebuild
                    && before
                        .as_ref()
                        .zip(previous)
                        .is_some_and(|(a, b)| a.matches(b));
                (key, before, !skip)
            })
            .collect();
        // A shared Cursor database changes far more often than any one
        // conversation in it. Save the new observation with the unchanged
        // digest; otherwise every later search re-hashes every conversation.
        let restamp: Vec<(&String, String)> = plan
            .iter()
            .filter(|(_, _, parse)| !parse)
            .filter_map(|(key, stamp, _)| {
                let stamp = stamp.as_ref()?;
                stamp.cursor.as_ref()?.observed.as_ref()?;
                let stamp = serde_json::to_string(stamp).ok()?;
                (existing.get(key) != Some(&stamp)).then_some((key, stamp))
            })
            .collect();
        if !restamp.is_empty() {
            // Best effort: losing this write costs time on the next search only.
            let _ = restamp_sessions(&self.conn, &existing, &restamp);
        }
        let missing: Vec<&String> = existing.keys().filter(|k| !live.contains(*k)).collect();
        let gone: Vec<&String> = missing
            .par_iter()
            .copied()
            .filter(|k| source_gone(k))
            .collect();
        let removed: Vec<&String> = if gone.len() > 100 && gone.len() * 2 > existing.len() {
            Vec::new()
        } else {
            gone
        };
        let todo: Vec<usize> = (0..corpus.len()).filter(|&i| plan[i].2).collect();
        let mut stats = SyncStats {
            unchanged: corpus.len() - todo.len(),
            ..SyncStats::default()
        };
        if removed.is_empty() && todo.is_empty() {
            return Ok(stats);
        }
        if !removed.is_empty() {
            let tx = begin_immediate(&self.conn)?;
            for key in &removed {
                stats.removed += tx.execute(
                    "DELETE FROM sessions WHERE key = ?1 AND fingerprint = ?2",
                    params![key, existing[*key]],
                )?;
            }
            tx.commit()?;
            if stats.removed > 0 {
                let tx = begin_immediate(&self.conn)?;
                tx.execute(
                    "INSERT INTO passages_fts(passages_fts) VALUES('optimize')",
                    [],
                )?;
                tx.commit()?;
                let _ = self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)");
            }
        }
        let order = std::collections::hash_map::RandomState::new();
        let mut todo = todo;
        todo.sort_by_cached_key(|&i| order.hash_one(&plan[i].0));
        let mut done = stats.unchanged;
        let source_bytes = |i: usize| {
            let size = fs::metadata(&corpus[i].file).map(|m| m.len()).unwrap_or(0);
            if corpus[i].source == "cursor-ide" || corpus[i].is_cursor_store_only() {
                size.min(SHARED_DATABASE_WEIGHT)
            } else {
                size
            }
        };
        for batch in batches(&todo, source_bytes, PARSE_BATCH_SIZE, PARSE_BATCH_BYTES) {
            let mut need = Vec::with_capacity(batch.len());
            for &i in &batch {
                let current = current_fingerprint(&self.conn, &plan[i].0)?;
                if published_elsewhere(&current, existing.get(&plan[i].0), plan[i].1.as_ref()) {
                    stats.unchanged += 1;
                    done += 1;
                } else {
                    need.push(i);
                }
            }
            let prepared: Vec<_> = need
                .par_iter()
                .map(|&i| {
                    let session = &corpus[i];
                    let before = plan[i].1.clone();
                    let (messages, _) = parse_session_recovering_timestamps(session, false);
                    let after = fingerprint(session, before.as_ref());
                    (i, extract_session(session, before, messages, after))
                })
                .collect();
            let mut writes = Vec::new();
            for (i, (stamp, extracted)) in prepared {
                let key = &plan[i].0;
                let current = current_fingerprint(&self.conn, key)?;
                if !rebuild
                    && current.as_deref() == Some(stamp.as_str())
                    && same_messages(&self.conn, key, &extracted)?
                {
                    stats.unchanged += 1;
                    done += 1;
                    progress(done, corpus.len());
                } else {
                    writes.push((i, stamp, extracted, current));
                }
            }
            if writes.is_empty() {
                progress(done, corpus.len());
                continue;
            }
            let tx = begin_immediate(&self.conn)?;
            for (i, stamp, extracted, seen) in writes {
                let key = &plan[i].0;
                let current = current_fingerprint(&tx, key)?;
                if current != seen
                    && current
                        .as_deref()
                        .and_then(|c| serde_json::from_str::<Fingerprint>(c).ok())
                        .zip(serde_json::from_str::<Fingerprint>(&stamp).ok())
                        .is_some_and(|(c, s)| c.matches(&s))
                {
                    stats.unchanged += 1;
                } else {
                    publish_session(
                        &tx,
                        &corpus[i],
                        key,
                        current.as_deref(),
                        &stamp,
                        extracted,
                        rebuild,
                    )?;
                    stats.updated += 1;
                }
                done += 1;
                progress(done, corpus.len());
            }
            tx.commit()?;
        }
        Ok(stats)
    }
}

/// Splits work into batches bounded by item count and by total size. An item
/// larger than the byte bound forms a batch of its own.
fn batches<'a>(
    items: &'a [usize],
    size: impl Fn(usize) -> u64 + 'a,
    max_items: usize,
    max_bytes: u64,
) -> impl Iterator<Item = Vec<usize>> + 'a {
    let mut next = 0;
    std::iter::from_fn(move || {
        let first = *items.get(next)?;
        let mut batch = vec![first];
        let mut bytes = size(first);
        next += 1;
        while next < items.len() && batch.len() < max_items {
            let item = items[next];
            let more = size(item);
            if bytes + more > max_bytes {
                break;
            }
            batch.push(item);
            bytes += more;
            next += 1;
        }
        Some(batch)
    })
}

fn restamp_sessions(
    conn: &Connection,
    existing: &HashMap<String, String>,
    restamp: &[(&String, String)],
) -> Result<()> {
    let tx = begin_immediate(conn)?;
    {
        let mut update = tx.prepare_cached(
            "UPDATE sessions SET fingerprint = ?2 WHERE key = ?1 AND fingerprint = ?3",
        )?;
        for (key, stamp) in restamp {
            update.execute(params![key, stamp, existing[*key]])?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn read_existing(conn: &Connection) -> Result<HashMap<String, String>> {
    Ok(conn
        .prepare("SELECT key, fingerprint FROM sessions")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

/// BEGIN IMMEDIATE, waiting as long as other writers keep committing. SQLite's
/// busy handler is not fair: under sustained contention one connection can lose
/// every retry within busy_timeout although the system is making progress.
fn begin_immediate(conn: &Connection) -> Result<Transaction<'_>> {
    let mut last_version: Option<i64> = None;
    let mut last_progress = Instant::now();
    loop {
        match Transaction::new_unchecked(conn, TransactionBehavior::Immediate) {
            Ok(tx) => return Ok(tx),
            Err(e) if e.sqlite_error_code() == Some(ErrorCode::DatabaseBusy) => {
                let version: i64 = conn.query_row("PRAGMA data_version", [], |r| r.get(0))?;
                if last_version != Some(version) {
                    last_version = Some(version);
                    last_progress = Instant::now();
                } else if last_progress.elapsed() > WRITER_STALL {
                    return Err(e.into());
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
}

fn current_fingerprint(conn: &Connection, key: &str) -> Result<Option<String>> {
    match conn
        .prepare_cached("SELECT fingerprint FROM sessions WHERE key = ?1")?
        .query_row([key], |r| r.get(0))
    {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn published_elsewhere(
    current: &Option<String>,
    planned: Option<&String>,
    want: Option<&Fingerprint>,
) -> bool {
    current.as_ref() != planned
        && current
            .as_deref()
            .and_then(|c| serde_json::from_str::<Fingerprint>(c).ok())
            .zip(want)
            .is_some_and(|(c, w)| c.matches(w))
}

type Extracted = Vec<(usize, Message, bool)>;

/// Parsers report a failed read as an empty message list. An empty parse of a
/// plain transcript is kept only when the file reads cleanly from start to end;
/// SQLite-backed sources cannot be checked this way and are retried.
fn empty_parse_is_durable(session: &Session) -> bool {
    let sqlite = session.file.ends_with(".db") || session.file.ends_with(".vscdb");
    !sqlite
        && session.source != "cursor-ide"
        && fs::File::open(&session.file)
            .and_then(|mut file| std::io::copy(&mut file, &mut std::io::sink()))
            .is_ok()
}

fn extract_session(
    session: &Session,
    before: Option<Fingerprint>,
    messages: Vec<Message>,
    after: Option<Fingerprint>,
) -> (String, Extracted) {
    let stable = before
        .as_ref()
        .zip(after.as_ref())
        .is_some_and(|(a, b)| a.matches(b))
        && (!messages.is_empty()
            || session.is_cursor_store_only()
            || empty_parse_is_durable(session));
    let mut stamp = after.unwrap_or(Fingerprint {
        version: EXTRACTION_VERSION,
        signature: None,
        cursor: None,
    });
    if !stable {
        stamp.signature = None;
    }
    let stamp = serde_json::to_string(&stamp).unwrap();
    let mut extracted = Vec::new();
    let metadata = metadata_message(session);
    if !metadata.content.is_empty() {
        extracted.push((0, metadata.clone(), true));
    }
    if !session.first_prompt.is_empty() {
        let mut prompt = metadata.clone();
        prompt.uuid = "index-prompt".into();
        prompt.content = session.first_prompt.clone();
        let preview = clean_first_prompt(&session.first_prompt);
        prompt.timestamp = messages
            .iter()
            .find(|m| {
                m.role == "user"
                    && !preview.is_empty()
                    && clean_first_prompt(&m.content).starts_with(&preview)
            })
            .map(|m| m.timestamp.clone())
            .unwrap_or_default();
        extracted.push((1, prompt, true));
    }
    for (ordinal, mut message) in messages.into_iter().enumerate() {
        if message.history_output || is_noise(&message.content_lower()) {
            continue;
        }
        message.session_id = session.id.clone();
        message.project_path = session.project.clone();
        extracted.push((ordinal + 2, message, false));
    }
    (stamp, extracted)
}

fn same_messages(conn: &Connection, key: &str, extracted: &Extracted) -> Result<bool> {
    let mut stmt = conn.prepare_cached(
        "SELECT ordinal, payload FROM messages WHERE session_key = ?1 ORDER BY ordinal",
    )?;
    let mut rows = stmt.query([key])?;
    for (ordinal, message, _) in extracted {
        let Some(row) = rows.next()? else {
            return Ok(false);
        };
        if row.get::<_, usize>(0)? != *ordinal
            || row.get::<_, String>(1)? != serde_json::to_string(message)?
        {
            return Ok(false);
        }
    }
    Ok(rows.next()?.is_none())
}

fn publish_session(
    tx: &Connection,
    session: &Session,
    key: &str,
    current: Option<&str>,
    stamp: &str,
    extracted: Extracted,
    rebuild: bool,
) -> Result<()> {
    let same_version = current
        .and_then(|value| serde_json::from_str::<Fingerprint>(value).ok())
        .is_some_and(|value| value.version == EXTRACTION_VERSION);
    if !rebuild && same_version && same_messages(tx, key, &extracted)? {
        if current != Some(stamp) {
            tx.execute(
                "UPDATE sessions SET fingerprint = ?2 WHERE key = ?1",
                params![key, stamp],
            )?;
        }
        return Ok(());
    }
    // The cascading delete is a multi-write statement and takes a savepoint,
    // which makes FTS5 flush; a session absent from the index has nothing to
    // remove.
    if current.is_some() {
        tx.execute("DELETE FROM sessions WHERE key = ?1", [key])?;
    }
    tx.execute("INSERT INTO sessions VALUES (?1, ?2)", params![key, stamp])?;
    for (ordinal, message, metadata) in extracted {
        insert_message(tx, key, session, &message, ordinal, metadata)?;
    }
    Ok(())
}

fn metadata_message(session: &Session) -> Message {
    let mut fields = Vec::new();
    for value in [&session.summary, &session.project, &session.branch] {
        if !value.is_empty() && !fields.contains(&value) {
            fields.push(value);
        }
    }
    Message {
        uuid: "index-title".into(),
        timestamp: if session.modified.is_empty() {
            &session.created
        } else {
            &session.modified
        }
        .clone(),
        role: "user".into(),
        content: fields
            .into_iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(" · "),
        session_id: session.id.clone(),
        project_path: session.project.clone(),
        tool_uses: Vec::new(),
        files_referenced: Vec::new(),
        error_patterns: Vec::new(),
        relevance_score: 0.0,
        final_score: 0.0,
        history_output: false,
    }
}

/// Prefer 1,600-character passages with up to 200 characters of overlap, but
/// never invent a token by cutting a whitespace-delimited span. A single span
/// longer than the budget stays intact (e.g. a minified identifier or CJK run).
fn passages(content: &str) -> Vec<&str> {
    let mut boundaries = vec![(0, 0)];
    let mut chars = 0;
    for (byte, ch) in content.char_indices() {
        chars += 1;
        if ch.is_whitespace() {
            boundaries.push((byte + ch.len_utf8(), chars));
        }
    }
    if boundaries.last().unwrap().0 != content.len() {
        boundaries.push((content.len(), chars));
    }
    let mut result = Vec::new();
    let mut start = 0;
    while start + 1 < boundaries.len() {
        let target = boundaries[start].1 + MAX_PASSAGE_CHARS;
        let end = boundaries
            .partition_point(|&(_, chars)| chars <= target)
            .saturating_sub(1)
            .max(start + 1);
        result.push(&content[boundaries[start].0..boundaries[end].0]);
        if end == boundaries.len() - 1 {
            break;
        }
        let overlap = boundaries[end].1.saturating_sub(PASSAGE_OVERLAP);
        start = boundaries
            .partition_point(|&(_, chars)| chars < overlap)
            .max(start + 1)
            .min(end);
    }
    result
}

fn insert_message(
    conn: &Connection,
    key: &str,
    session: &Session,
    message: &Message,
    ordinal: usize,
    metadata: bool,
) -> Result<()> {
    conn.execute(
        "INSERT INTO messages(session_key, ordinal, payload, timestamp, errors, tools, files)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            key,
            ordinal,
            serde_json::to_string(message)?,
            parse_any_timestamp(&message.timestamp).map(|t| t.timestamp_millis()),
            !metadata
                && (!message.error_patterns.is_empty()
                    || message.content_lower().contains("error")),
            !message.tool_uses.is_empty(),
            !message.files_referenced.is_empty()
        ],
    )?;
    let id = conn.last_insert_rowid();
    // Written directly rather than through an AFTER INSERT trigger: a trigger
    // makes each passage insert a multi-write statement with its own savepoint,
    // and FTS5 flushes its pending index to disk on every savepoint.
    let mut row = conn.prepare_cached(
        "INSERT INTO passages(message_id, content, title, prompt, project, branch, tool)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    let mut fts = conn.prepare_cached(
        "INSERT INTO passages_fts(rowid, content, title, prompt, project, branch, tool)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    let mut insert = |[content, title, prompt, project, branch, tool]: [&str; 6]| {
        row.execute(params![id, content, title, prompt, project, branch, tool])?;
        let passage = conn.last_insert_rowid();
        fts.execute(params![
            passage, content, title, prompt, project, branch, tool
        ])?;
        Ok::<(), rusqlite::Error>(())
    };
    if metadata {
        let prompt = message.uuid == "index-prompt";
        if prompt {
            insert(["", "", &session.first_prompt, "", "", ""])?;
        } else {
            insert([
                "",
                &session.summary,
                "",
                &session.project,
                &session.branch,
                "",
            ])?;
        }
    } else if message.role == "tool" {
        for passage in passages(&message.content) {
            insert(["", "", "", "", "", passage])?;
        }
    } else {
        for passage in passages(&message.content) {
            insert([passage, "", "", "", "", ""])?;
        }
    }
    Ok(())
}

/// Plain user text, never an executable FTS expression. Let the same Unicode61
/// analyzer handle both sides. Quoted chunks preserve adjacent identifier/path
/// components instead of broadening `src/foo.rs` into `src OR foo OR rs`.
struct LexicalQuery {
    broad: String,
    complete: Option<String>,
    phrase: Option<String>,
}

fn analyzer_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for c in text.chars() {
        if is_analyzer_token_char(c) {
            current.extend(c.to_lowercase());
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn is_analyzer_token_char(c: char) -> bool {
    c.is_alphanumeric()
        || c == '_'
        || matches!(
            c,
            '\u{0300}'..='\u{036F}'
                | '\u{1AB0}'..='\u{1AFF}'
                | '\u{20D0}'..='\u{20FF}'
                | '\u{FE20}'..='\u{FE2F}'
        )
}

fn match_query(query: &str) -> Result<Option<LexicalQuery>> {
    let query = strip_terminal_controls(query);
    if crate::scoring::is_uuid(&query) {
        return Ok(Some(LexicalQuery {
            broad: format!("\"{}\"", query.trim().to_lowercase()),
            complete: None,
            phrase: None,
        }));
    }
    let mut terms = Vec::new();
    let mut unique_analyzer = HashSet::new();
    let mut seen_terms = HashSet::new();
    let mut analyzer_count = 0usize;
    for chunk in query.split_whitespace() {
        let toks = analyzer_tokens(chunk);
        if toks.is_empty() {
            continue;
        }
        analyzer_count = analyzer_count.saturating_add(toks.len());
        if toks.len() <= MAX_PHRASE_TOKENS {
            // A pasted log repeats the same words thousands of times. Each
            // distinct chunk is one clause, however often it occurs.
            let term = chunk.to_lowercase();
            if terms.len() < MAX_ANALYZER_TOKENS && seen_terms.insert(term.clone()) {
                terms.push(term);
                unique_analyzer.extend(toks);
            }
        } else {
            for tok in toks {
                if unique_analyzer.insert(tok.clone()) && terms.len() < MAX_ANALYZER_TOKENS {
                    terms.push(tok);
                }
            }
        }
        if unique_analyzer.len() >= MAX_ANALYZER_TOKENS || terms.len() >= MAX_ANALYZER_TOKENS {
            break;
        }
    }
    if terms.is_empty() {
        return Ok(None);
    }
    let clauses: Vec<String> = terms
        .iter()
        .map(|term| {
            let literal = format!("\"{}\"", term.replace('"', "\"\""));
            if term.chars().filter(|c| c.is_alphanumeric()).count() >= 2 {
                // Exact words contribute their own IDF in addition to the
                // broader prefix. Otherwise WAL can lose to wall/Waltham.
                format!("({literal} OR {literal}*)")
            } else {
                literal
            }
        })
        .collect();
    Ok(Some(LexicalQuery {
        broad: clauses.join(" OR "),
        complete: (clauses.len() > 1).then(|| clauses.join(" AND ")),
        phrase: (clauses.len() > 1 && analyzer_count <= MAX_PHRASE_TOKENS)
            .then(|| format!("\"{}\"", query.replace('"', "\"\""))),
    }))
}

/// Comparable text of an accepted hit and, for a title or prompt row, its uuid.
type Echo = (String, Option<String>);

fn is_synthetic(message: &Message) -> bool {
    message.uuid == "index-title" || message.uuid == "index-prompt"
}

/// Comparable form of a title, prompt preview or user message. Titles and
/// previews are truncated, so callers compare by prefix.
fn echo_text(text: &str) -> String {
    clean_first_prompt(text)
        .trim_end_matches(['…', '.', ' '])
        .to_lowercase()
}

impl SearchBackend for Bm25Backend {
    fn search(&mut self, request: &SearchRequest<'_>) -> Result<Vec<SearchResult>> {
        if request.limit == 0 || request.sessions.is_empty() {
            return Ok(Vec::new());
        }
        if let Some(results) =
            direct_session_search(request.sessions, request.query, request.timeframe)
        {
            return Ok(results);
        }
        // Similarity remains the existing user-message similarity operation.
        if request.scope == "similar" {
            return LegacyBackend.search(request);
        }
        let Some(query) = match_query(request.query)? else {
            return Ok(Vec::new());
        };
        let cutoff = match request.timeframe {
            Some(tf) => Some(
                parse_timeframe_duration(tf)
                    .map(|dur| {
                        Utc::now()
                            .checked_sub_signed(dur)
                            .unwrap_or(DateTime::<Utc>::MIN_UTC)
                            .timestamp_millis()
                    })
                    .map_err(|e| -> SearchError { e.into() })?,
            ),
            None => None,
        };
        let selected: HashMap<String, &Session> = request
            .sessions
            .iter()
            .map(|s| (session_key(s), s))
            .collect();
        let tx = self.conn.savepoint()?;
        tx.execute("DELETE FROM allowed_sessions", [])?;
        {
            let mut insert = tx.prepare("INSERT INTO allowed_sessions VALUES (?1)")?;
            for key in selected.keys() {
                insert.execute([key])?;
            }
        }
        // Apply filters before consuming ranked candidates. No fixed oversampling
        // limit: duplicates or a busy session must not starve later sessions.
        let mut stmt = tx.prepare(
            "WITH complete AS MATERIALIZED (
                SELECT rowid FROM passages_fts WHERE ?4 IS NOT NULL AND passages_fts MATCH ?4
             ), phrase AS MATERIALIZED (
                SELECT rowid FROM passages_fts WHERE ?5 IS NOT NULL AND passages_fts MATCH ?5
             )
             SELECT m.id, m.session_key,
                    -bm25(passages_fts, 1.0, 3.0, 2.0, 0.5, 0.5, ?6)
                    * (1.0 + 0.25 * (complete.rowid IS NOT NULL)
                           + 0.15 * (phrase.rowid IS NOT NULL)) AS score, p.id,
                    m.ordinal
             FROM passages_fts
             JOIN passages p ON p.id = passages_fts.rowid
             JOIN messages m ON m.id = p.message_id
             JOIN allowed_sessions a ON a.key = m.session_key
             LEFT JOIN complete ON complete.rowid = p.id
             LEFT JOIN phrase ON phrase.rowid = p.id
             WHERE passages_fts MATCH ?1
               AND (?2 IS NULL OR m.timestamp >= ?2)
               AND (?3 != 'errors' OR m.errors = 1)
               AND (?3 != 'tools' OR m.tools = 1)
               AND (?3 != 'files' OR m.files = 1)
             ORDER BY score DESC,
                      m.timestamp DESC, m.session_key, m.ordinal, p.id",
        )?;
        let mut rows = stmt.query(params![
            query.broad,
            cutoff,
            request.scope,
            query.complete,
            query.phrase,
            TOOL_OUTPUT_WEIGHT
        ])?;
        // Keep full message JSON out of SQLite's candidate sorter. A long
        // message can have thousands of matching passages but is hydrated once.
        let mut hydrate = tx.prepare_cached("SELECT payload FROM messages WHERE id = ?1")?;
        // Compute excerpts only for accepted hits, keeping text out of the
        // candidate sorter. FTS uses the same analyzer and winning passage.
        let mut excerpt = tx.prepare_cached(
            "SELECT snippet(passages_fts, -1, '', '', ' … ', 40)
             FROM passages_fts WHERE rowid = ?1 AND passages_fts MATCH ?2",
        )?;
        let mut seen_messages = HashSet::new();
        let mut seen_content = HashSet::new();
        let mut counts: HashMap<(String, String), usize> = HashMap::new();
        let mut groups: HashMap<(String, String), usize> = HashMap::new();
        let mut echoes: HashMap<(String, String), Vec<Echo>> = HashMap::new();
        let mut results: Vec<SearchResult> = Vec::new();
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            if !seen_messages.insert(id) {
                continue;
            }
            let key: String = row.get(1)?;
            let session = selected[&key];
            let logical_session = (session.source.clone(), session.id.to_lowercase());
            if request.group_by_session
                && results.len() == request.limit
                && !groups.contains_key(&logical_session)
            {
                continue;
            }
            if counts.get(&logical_session).copied().unwrap_or(0) >= 3 {
                continue;
            }
            let payload: String = hydrate.query_row([id], |r| r.get(0))?;
            let mut message: Message = serde_json::from_str(&payload)?;
            // Preserve code, digits, quotes, case and the full message. The
            // legacy fuzzy signature conflates E100/E200 and shared preambles.
            // Dedup is per conversation so the same error text in another
            // session remains a separate hit.
            let sig = (
                logical_session.clone(),
                message.role.clone(),
                message.content.trim().to_owned(),
                message.tool_uses.clone(),
                message.files_referenced.clone(),
            );
            if !seen_content.insert(sig) {
                continue;
            }
            // The title and prompt rows usually repeat the first user message.
            // One sentence must not fill a conversation's three slots: a
            // synthetic row yields to, or is replaced by, the real message.
            let synthetic = is_synthetic(&message);
            let echo = match message.uuid.as_str() {
                "index-title" => echo_text(&session.summary),
                _ if synthetic || message.role == "user" => echo_text(&message.content),
                _ => String::new(),
            };
            let shown = echoes.entry(logical_session.clone()).or_default();
            let twin = (!echo.is_empty())
                .then(|| {
                    shown.iter().position(|(other, other_synthetic): &Echo| {
                        (synthetic || other_synthetic.is_some())
                            && (other.starts_with(&echo) || echo.starts_with(other.as_str()))
                    })
                })
                .flatten();
            if synthetic && twin.is_some() {
                continue;
            }
            let score: f64 = row.get(2)?;
            message.relevance_score = score;
            message.final_score = score;
            let passage_id: i64 = row.get(3)?;
            // Stored ordinals 0 and 1 are the title and prompt rows; real
            // messages are stored at their transcript position + 2.
            let ordinal = (!synthetic)
                .then(|| row.get::<_, i64>(4))
                .transpose()?
                .and_then(|stored| usize::try_from(stored).ok()?.checked_sub(2));
            let snippet: String =
                excerpt.query_row(params![passage_id, query.broad], |r| r.get(0))?;
            let snippet = if snippet.trim().is_empty() {
                let q = if request.query.len() > 4096 {
                    &request.query[..request.query.floor_char_boundary(4096)]
                } else {
                    request.query
                };
                snippet_around_match(&message.content, q, 200)
            } else {
                snippet
            };
            let snippet = cap_excerpt(&snippet, SNIPPET_CHARS);
            if let Some(twin) = twin {
                // Keep the synthetic row's rank; show the message it echoed.
                // The title and prompt rows are distinct: replace the twin.
                let twin_uuid = std::mem::replace(&mut shown[twin], (echo, None)).1;
                let slot = results
                    .iter_mut()
                    .filter(|r| {
                        r.session.source == session.source
                            && r.session.id.eq_ignore_ascii_case(&session.id)
                    })
                    .flat_map(|r| {
                        std::iter::once((&mut r.message, &mut r.snippet, &mut r.ordinal)).chain(
                            r.additional_matches
                                .iter_mut()
                                .map(|m| (&mut m.message, &mut m.snippet, &mut m.ordinal)),
                        )
                    })
                    .find(|(m, _, _)| Some(&m.uuid) == twin_uuid.as_ref());
                if let Some((slot_message, slot_snippet, slot_ordinal)) = slot {
                    message.relevance_score = slot_message.relevance_score;
                    message.final_score = slot_message.final_score;
                    *slot_message = message;
                    *slot_snippet = Some(snippet);
                    *slot_ordinal = ordinal;
                }
                continue;
            }
            if !echo.is_empty() {
                shown.push((echo, synthetic.then(|| message.uuid.clone())));
            }
            *counts.entry(logical_session.clone()).or_default() += 1;
            if request.group_by_session
                && let Some(&group) = groups.get(&logical_session)
            {
                results[group].additional_matches.push(SearchMatch {
                    message,
                    snippet: Some(snippet),
                    ordinal,
                });
            } else {
                groups.insert(logical_session, results.len());
                results.push(SearchResult {
                    session: session.clone(),
                    message,
                    snippet: Some(snippet),
                    ordinal,
                    additional_matches: Vec::new(),
                });
            }
            if results.len() == request.limit
                && (!request.group_by_session || counts.values().all(|count| *count >= 3))
            {
                break;
            }
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::Progress;
    use std::time::{Duration, Instant};

    #[test]
    fn sync_batches_are_bounded_by_session_count_and_source_bytes() {
        let sizes = [10u64, 30, 5, 1, 1, 1, 50];
        let batches: Vec<Vec<usize>> =
            super::batches(&[0, 1, 2, 3, 4, 5, 6], |i| sizes[i], 3, 32).collect();
        // 10 fits; 30 would exceed 32 bytes; 5 joins nothing after 30; then a
        // count-bounded run of three; an oversized session is its own batch.
        assert_eq!(
            batches,
            vec![vec![0], vec![1], vec![2, 3, 4], vec![5], vec![6]]
        );
    }

    use super::{grouped_message_oversample, match_query};

    #[test]
    fn progress_off_a_terminal_is_one_line_and_only_when_slow() {
        let start = Instant::now();
        let mut p = Progress::starting_at(false, start);
        assert_eq!(
            p.tick(10, 100, start + Duration::from_millis(200)),
            None,
            "fast syncs say nothing"
        );
        let first = p.tick(40, 100, start + Duration::from_millis(800));
        assert_eq!(
            first.as_deref(),
            Some("Updating search index (100 sessions); later searches reuse it.\n")
        );
        assert_eq!(p.tick(70, 100, start + Duration::from_secs(3)), None);
        assert_eq!(p.tick(99, 100, start + Duration::from_secs(9)), None);
        assert_eq!(p.finish(), None);
    }

    #[test]
    fn progress_on_a_terminal_rewrites_one_line_and_clears_it() {
        let start = Instant::now();
        let mut p = Progress::starting_at(true, start);
        assert_eq!(p.finish(), None, "nothing shown, nothing to clear");
        assert_eq!(p.tick(1, 100, start + Duration::from_millis(100)), None);
        assert_eq!(
            p.tick(40, 100, start + Duration::from_millis(800))
                .as_deref(),
            Some("\rUpdating search index: 40/100 sessions")
        );
        assert_eq!(p.tick(41, 100, start + Duration::from_millis(900)), None);
        assert_eq!(
            p.tick(90, 100, start + Duration::from_millis(1600))
                .as_deref(),
            Some("\rUpdating search index: 90/100 sessions")
        );
        assert_eq!(p.finish().as_deref(), Some("\r\x1b[2K"));
    }

    #[test]
    fn grouped_oversample_stays_bounded() {
        assert_eq!(grouped_message_oversample(2), 12);
        assert!(grouped_message_oversample(15) < usize::MAX / 2);
    }

    #[test]
    fn match_query_caps_analyzer_tokens_on_dotted_repeats() {
        let parsed = match_query(&"sqlite.rust.".repeat(5000))
            .unwrap()
            .expect("repeated dotted text still has analyzer tokens");
        assert!(parsed.phrase.is_none(), "{}", parsed.broad);
        assert!(parsed.broad.len() < 200, "{}", parsed.broad);
        assert!(parsed.broad.contains("sqlite"));
        assert!(parsed.broad.contains("rust"));
    }

    #[test]
    fn match_query_deduplicates_repeated_words() {
        let parsed = match_query(&"error fix ".repeat(3000))
            .unwrap()
            .expect("repeated words still form a query");
        assert_eq!(parsed.broad.matches(" OR (").count(), 1, "{}", parsed.broad);
        assert!(parsed.phrase.is_none());
        // Distinct chunks that share analyzer tokens are still bounded.
        let noisy: String = (0..5000)
            .map(|i| format!("{} ", "-".repeat(i % 700 + 1) + "x"))
            .collect();
        let parsed = match_query(&noisy).unwrap().unwrap();
        assert!(parsed.broad.matches(" OR ").count() <= 2 * super::MAX_ANALYZER_TOKENS);
    }

    #[test]
    fn match_query_accepts_more_than_sixty_four_terms() {
        let query = (0..100)
            .map(|i| format!("term{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let parsed = match_query(&query)
            .unwrap()
            .expect("long term lists are capped, not rejected");
        assert!(parsed.phrase.is_none());
        assert!(parsed.broad.contains("term0"));
        assert!(parsed.broad.contains("term99"));
    }
}
