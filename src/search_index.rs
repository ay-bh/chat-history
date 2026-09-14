//! Replaceable lexical retrieval over a disposable, incrementally refreshed index.
//! Source transcripts remain authoritative. See docs/search-architecture.md.
use crate::parser::{clean_first_prompt, is_noise};
use crate::search::{
    SearchMatch, SearchResult, direct_session_search, parse_timeframe_duration, scored_search,
};
use crate::session::{Message, Session, parse_any_timestamp, parse_session_recovering_timestamps};
use chrono::Utc;
use rayon::prelude::*;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub type SearchError = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, SearchError>;
pub const INDEX_FILENAME: &str = "search-v1.db";
// Bump for parser, timestamp, chunking or tokenization changes that require
// re-extracting unchanged sources without changing the relational schema.
const EXTRACTION_VERSION: u32 = 1;
const MAX_QUERY_TERMS: usize = 64;
const MAX_PASSAGE_CHARS: usize = 1600;
const PASSAGE_OVERLAP: usize = 200;
const PARSE_BATCH_SIZE: usize = 8;

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
                usize::MAX
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

pub fn default_index_dir() -> Option<PathBuf> {
    std::env::var_os("CHAT_HISTORY_CACHE_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::session::user_home().map(|p| p.join(".chat-history/cache")))
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
    let mut last_progress = std::time::Instant::now();
    let mut prepare = |dir| -> Result<Bm25Backend> {
        let mut backend = Bm25Backend::open(dir)?;
        // Keep synchronization and retrieval in one snapshot. Another process
        // may use a different provider profile and replace the cached corpus.
        backend.conn.execute_batch("BEGIN")?;
        backend.sync_with_progress(corpus, rebuild, |done, total| {
            if last_progress.elapsed() >= Duration::from_millis(750) {
                eprintln!("Updating search index: {done}/{total} sessions");
                last_progress = std::time::Instant::now();
            }
        })?;
        Ok(backend)
    };
    let mut backend = match prepare(directory) {
        Ok(backend) => backend,
        Err(error) if directory.is_some() => {
            eprintln!(
                "Warning: search cache unavailable ({error}); searching with an in-memory BM25 index."
            );
            prepare(None)?
        }
        Err(error) => return Err(error),
    };
    let results = match backend.search(request) {
        Ok(results) => results,
        Err(error) if directory.is_some() => {
            eprintln!(
                "Warning: search cache query failed ({error}); searching with an in-memory BM25 index."
            );
            drop(backend);
            backend = prepare(None)?;
            backend.search(request)?
        }
        Err(error) => return Err(error),
    };
    if let Err(error) = backend.conn.execute_batch("COMMIT") {
        // Search already read a valid snapshot. A disposable cache write must
        // not turn those results into a failed command (e.g. a full disk).
        eprintln!(
            "Warning: could not save search cache ({error}); returning current search results."
        );
        let _ = backend.conn.execute_batch("ROLLBACK");
    }
    Ok(results)
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
        conn.busy_timeout(Duration::from_secs(1))?;
        conn.pragma_update(None, "foreign_keys", true)?;
        if directory.is_some() {
            let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
            if mode != "wal" {
                conn.pragma_update(None, "journal_mode", "WAL")?;
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
            "SELECT count(*) = 8 FROM sqlite_schema WHERE name IN (
             'sessions', 'messages', 'messages_session', 'passages', 'passages_message',
             'passages_fts', 'passages_insert', 'passages_delete')",
            [],
            |r| r.get(0),
        )?;
        if !initialized {
            let tx = conn.unchecked_transaction()?;
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
                project TEXT NOT NULL, branch TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS passages_message ON passages(message_id);
             CREATE VIRTUAL TABLE IF NOT EXISTS passages_fts USING fts5(
                content, title, prompt, project, branch,
                content='passages', content_rowid='id',
                tokenize='unicode61 remove_diacritics 2', prefix='2 3 4'
             );
             CREATE TRIGGER IF NOT EXISTS passages_insert AFTER INSERT ON passages BEGIN
                INSERT INTO passages_fts(rowid, content, title, prompt, project, branch)
                VALUES (new.id, new.content, new.title, new.prompt, new.project, new.branch);
             END;
             CREATE TRIGGER IF NOT EXISTS passages_delete AFTER DELETE ON passages BEGIN
                INSERT INTO passages_fts(passages_fts, rowid, content, title, prompt, project, branch)
                VALUES ('delete', old.id, old.content, old.title, old.prompt, old.project, old.branch);
             END;",
        )?;
            // Creating external-content FTS/triggers does not index rows that
            // already exist. Repair missing derived assets atomically on open.
            tx.execute(
                "INSERT INTO passages_fts(passages_fts) VALUES ('rebuild')",
                [],
            )?;
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
        let existing: HashMap<String, String> = self
            .conn
            .prepare("SELECT key, fingerprint FROM sessions")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let live: HashSet<String> = corpus.iter().map(session_key).collect();
        let fingerprints: HashMap<_, Fingerprint> = existing
            .iter()
            .filter_map(|(key, value)| {
                serde_json::from_str(value)
                    .ok()
                    .map(|stamp| (key.clone(), stamp))
            })
            .collect();
        let mut stats = SyncStats::default();
        // Parse changed sessions in bounded batches; never hold the whole parsed
        // corpus in memory. Publish all changes in one transaction.
        let tx = self.conn.savepoint()?;
        for key in existing.keys().filter(|key| !live.contains(*key)) {
            tx.execute("DELETE FROM sessions WHERE key = ?1", [key])?;
            stats.removed += 1;
        }
        for (batch_number, batch) in corpus.chunks(PARSE_BATCH_SIZE).enumerate() {
            let prepared: Vec<_> = batch
                .par_iter()
                .map(|session| {
                    let key = session_key(session);
                    let previous = fingerprints.get(&key);
                    let before = fingerprint(session, previous);
                    let parsed = if !rebuild
                        && before
                            .as_ref()
                            .zip(previous)
                            .is_some_and(|(a, b)| a.matches(b))
                    {
                        None
                    } else {
                        let (messages, _) = parse_session_recovering_timestamps(session, false);
                        let after = fingerprint(session, before.as_ref());
                        Some((messages, after))
                    };
                    (session, key, before, parsed)
                })
                .collect();
            for (offset, (session, key, before, parsed)) in prepared.into_iter().enumerate() {
                let i = batch_number * PARSE_BATCH_SIZE + offset;
                let Some((messages, after)) = parsed else {
                    let observed = serde_json::to_string(before.as_ref().unwrap())?;
                    if existing.get(&key) != Some(&observed) {
                        tx.execute(
                            "UPDATE sessions SET fingerprint = ?2 WHERE key = ?1",
                            params![key, observed],
                        )?;
                    }
                    stats.unchanged += 1;
                    progress(i + 1, corpus.len());
                    continue;
                };
                // Racy or unsuccessful reads can serve this query but must be retried.
                let stable = before
                    .as_ref()
                    .zip(after.as_ref())
                    .is_some_and(|(a, b)| a.matches(b))
                    && (!messages.is_empty() || session.is_cursor_store_only());
                // Keep the extraction version even when the source needs retrying.
                let mut stamp = after.unwrap_or(Fingerprint {
                    version: EXTRACTION_VERSION,
                    signature: None,
                    cursor: None,
                });
                if !stable {
                    stamp.signature = None;
                }
                let stamp = serde_json::to_string(&stamp)?;
                let mut extracted = Vec::new();
                let metadata = metadata_message(session);
                if !metadata.content.is_empty() {
                    extracted.push((0, metadata.clone(), true));
                }
                if !session.first_prompt.is_empty() {
                    let mut prompt = metadata.clone();
                    prompt.uuid = "index-prompt".into();
                    prompt.content = session.first_prompt.clone();
                    // A prompt is message text: session activity is not proof of
                    // when it was written. Do not launder unknown times via metadata.
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
                    if is_noise(&message.content_lower()) {
                        continue;
                    }
                    message.session_id = session.id.clone();
                    message.project_path = session.project.clone();
                    extracted.push((ordinal + 2, message, false));
                }
                // Cursor's shared database changes for unrelated chats and settings.
                // Recheck extraction, but do not rewrite unchanged FTS postings.
                // Compare complete payloads, not a lossy hash; policy changes and
                // explicit rebuilds must still regenerate passages.
                let same_version = existing
                    .get(&key)
                    .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
                    .is_some_and(|value| {
                        value
                            .get("version")
                            .or_else(|| value.get(0))
                            .and_then(|v| v.as_u64())
                            == Some(u64::from(EXTRACTION_VERSION))
                    });
                let same_messages = if !rebuild && same_version {
                    let mut stmt = tx.prepare_cached(
                    "SELECT ordinal, payload FROM messages WHERE session_key = ?1 ORDER BY ordinal",
                )?;
                    let mut rows = stmt.query([&key])?;
                    let mut equal = true;
                    for (ordinal, message, _) in &extracted {
                        let Some(row) = rows.next()? else {
                            equal = false;
                            break;
                        };
                        if row.get::<_, usize>(0)? != *ordinal
                            || row.get::<_, String>(1)? != serde_json::to_string(message)?
                        {
                            equal = false;
                            break;
                        }
                    }
                    equal && rows.next()?.is_none()
                } else {
                    false
                };
                if same_messages {
                    tx.execute(
                        "UPDATE sessions SET fingerprint = ?2 WHERE key = ?1",
                        params![key, stamp],
                    )?;
                } else {
                    tx.execute("DELETE FROM sessions WHERE key = ?1", [&key])?;
                    tx.execute("INSERT INTO sessions VALUES (?1, ?2)", params![key, stamp])?;
                    for (ordinal, message, metadata) in extracted {
                        insert_message(&tx, &key, session, &message, ordinal, metadata)?;
                    }
                }
                stats.updated += 1;
                progress(i + 1, corpus.len());
            }
        }
        tx.commit()?;
        Ok(stats)
    }
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
    let mut stmt = conn.prepare_cached(
        "INSERT INTO passages(message_id, content, title, prompt, project, branch)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    if metadata {
        let prompt = message.uuid == "index-prompt";
        stmt.execute(params![
            id,
            "",
            if prompt { "" } else { &session.summary },
            if prompt { &session.first_prompt } else { "" },
            if prompt { "" } else { &session.project },
            if prompt { "" } else { &session.branch }
        ])?;
    } else {
        for content in passages(&message.content) {
            stmt.execute(params![id, content, "", "", "", ""])?;
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

fn match_query(query: &str) -> Result<Option<LexicalQuery>> {
    if crate::scoring::is_uuid(query) {
        return Ok(Some(LexicalQuery {
            broad: format!("\"{}\"", query.trim().to_lowercase()),
            complete: None,
            phrase: None,
        }));
    }
    let terms: BTreeSet<String> = query
        .split_whitespace()
        .filter(|s| s.chars().any(char::is_alphanumeric))
        .map(str::to_lowercase)
        .collect();
    if terms.len() > MAX_QUERY_TERMS {
        return Err(format!(
            "Search query has more than {MAX_QUERY_TERMS} terms; use a shorter query."
        )
        .into());
    }
    if terms.is_empty() {
        return Ok(None);
    }
    let clauses = terms
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
        .collect::<Vec<_>>();
    Ok(Some(LexicalQuery {
        broad: clauses.join(" OR "),
        complete: (clauses.len() > 1).then(|| clauses.join(" AND ")),
        phrase: (clauses.len() > 1).then(|| format!("\"{}\"", query.replace('"', "\"\""))),
    }))
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
        let cutoff = request
            .timeframe
            .map(|tf| (Utc::now() - parse_timeframe_duration(tf)).timestamp_millis());
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
                    -bm25(passages_fts, 1.0, 3.0, 2.0, 0.5, 0.5)
                    * (1.0 + 0.25 * (complete.rowid IS NOT NULL)
                           + 0.15 * (phrase.rowid IS NOT NULL)) AS score, p.id
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
            query.phrase
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
            let sig = (
                message.role.clone(),
                message.content.trim().to_owned(),
                message.tool_uses.clone(),
                message.files_referenced.clone(),
            );
            if !seen_content.insert(sig) {
                continue;
            }
            let score: f64 = row.get(2)?;
            message.relevance_score = score;
            message.final_score = score;
            let passage_id: i64 = row.get(3)?;
            let snippet = excerpt.query_row(params![passage_id, query.broad], |r| r.get(0))?;
            *counts.entry(logical_session.clone()).or_default() += 1;
            if request.group_by_session
                && let Some(&group) = groups.get(&logical_session)
            {
                results[group].additional_matches.push(SearchMatch {
                    message,
                    snippet: Some(snippet),
                });
            } else {
                groups.insert(logical_session, results.len());
                results.push(SearchResult {
                    session: session.clone(),
                    message,
                    snippet: Some(snippet),
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
