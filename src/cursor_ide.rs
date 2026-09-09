//! Cursor IDE chats from `state.vscdb` (SQLite).
//! Schema is unofficial and can drift with Cursor releases.

use crate::parser::{clean_first_prompt, extract_text, is_clear_metadata, is_warmup_message};
use crate::session::{
    Message, Session, cursor_entry_timestamp, iso_date, ms_to_iso, parse_any_timestamp, user_home,
};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub fn cursor_user_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CURSOR_USER_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    let home = user_home().unwrap_or_else(|| PathBuf::from("."));
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Cursor/User")
    } else if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or(home)
            .join("Cursor/User")
    } else {
        home.join(".config/Cursor/User")
    }
}

fn global_vscdb() -> PathBuf {
    cursor_user_dir().join("globalStorage/state.vscdb")
}

/// Read-only SQLite handle that waits briefly on a writer's lock instead of
/// failing the whole listing while Cursor (or a hook) is mid-write.
pub(crate) fn open_ro(path: &Path) -> Option<Connection> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    conn.busy_timeout(std::time::Duration::from_secs(2)).ok()?;
    Some(conn)
}

pub fn workspace_path(value: &Value) -> String {
    value
        .pointer("/workspaceIdentifier/uri/fsPath")
        .or_else(|| value.pointer("/workspaceIdentifier/uri/path"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn blob_text(value: rusqlite::types::ValueRef<'_>) -> String {
    match value {
        rusqlite::types::ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
        rusqlite::types::ValueRef::Blob(b) => String::from_utf8_lossy(b).into_owned(),
        _ => String::new(),
    }
}

fn bubble_text(entry: &Value) -> &str {
    entry
        .get("text")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .or_else(|| entry.get("richText").and_then(Value::as_str))
        .unwrap_or("")
}

fn bubble_key_bounds(composer_id: &str) -> (String, String) {
    (
        format!("bubbleId:{composer_id}:"),
        format!("bubbleId:{composer_id};"),
    )
}

fn kv_blob(conn: &Connection, key: &str) -> Option<String> {
    let mut stmt = conn
        .prepare_cached("SELECT value FROM cursorDiskKV WHERE key = ?1")
        .ok()?;
    stmt.query_row([key], |row| Ok(blob_text(row.get_ref(0)?)))
        .ok()
}

fn message_from_bubble(session: &Session, entry: &Value) -> Option<Message> {
    let ty = entry.get("type").and_then(Value::as_i64).unwrap_or(0);
    let role = match ty {
        1 => "user",
        2 => "assistant",
        _ => return None,
    };
    let text = bubble_text(entry);
    let cleaned = if role == "user" {
        crate::parser::clean_prompt(text)
    } else {
        extract_text(&Value::String(text.to_string()))
    };
    if cleaned.is_empty() {
        return None;
    }
    if role == "user" && (is_warmup_message(&cleaned) || is_clear_metadata(&cleaned)) {
        return None;
    }
    let ts = cursor_entry_timestamp(entry);
    Some(Message {
        uuid: entry
            .get("bubbleId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        timestamp: ts,
        role: role.into(),
        content: cleaned,
        session_id: session.id.clone(),
        project_path: session.project.clone(),
        tool_uses: Vec::new(),
        files_referenced: Vec::new(),
        error_patterns: Vec::new(),
        relevance_score: 0.0,
        final_score: 0.0,
    })
}

fn sort_messages_stable(messages: &mut [Message]) {
    messages.sort_by(|a, b| {
        match (
            parse_any_timestamp(&a.timestamp),
            parse_any_timestamp(&b.timestamp),
        ) {
            (Some(a), Some(b)) => a.cmp(&b),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
    });
}

fn load_bubbles_range(conn: &Connection, composer_id: &str) -> Vec<Value> {
    let (lower, upper) = bubble_key_bounds(composer_id);
    let mut stmt =
        match conn.prepare_cached("SELECT value FROM cursorDiskKV WHERE key >= ?1 AND key < ?2") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
    let rows = stmt.query_map(rusqlite::params![lower, upper], |row| {
        Ok(blob_text(row.get_ref(0)?))
    });
    let Ok(rows) = rows else {
        return Vec::new();
    };
    rows.flatten()
        .filter_map(|val| serde_json::from_str::<Value>(&val).ok())
        .collect()
}

fn composer_header_order(conn: &Connection, composer_id: &str) -> Option<Vec<Value>> {
    // Avoid deserializing the large context/tool payload just to read the
    // active conversation order. The bundled SQLite supports JSON extraction.
    let mut stmt = conn
        .prepare_cached(
            "SELECT json_extract(value, '$.fullConversationHeadersOnly')
         FROM cursorDiskKV WHERE key = ?1",
        )
        .ok()?;
    let raw: String = stmt
        .query_row([format!("composerData:{composer_id}")], |row| row.get(0))
        .ok()?;
    let headers: Vec<Value> = serde_json::from_str(&raw).ok()?;
    if headers.is_empty() {
        return None;
    }
    Some(headers)
}

/// Bubbles for a composer and whether they came in the conversation's own
/// header order (else sorted by time, untimestamped last).
fn load_bubble_entries(conn: &Connection, composer_id: &str) -> (Vec<Value>, bool) {
    if let Some(headers) = composer_header_order(conn, composer_id) {
        let mut entries = Vec::new();
        for h in headers {
            let Some(bid) = h.get("bubbleId").and_then(Value::as_str) else {
                continue;
            };
            let key = format!("bubbleId:{composer_id}:{bid}");
            if let Some(raw) = kv_blob(conn, &key)
                && let Ok(mut entry) = serde_json::from_str::<Value>(&raw)
            {
                if entry.get("type").is_none()
                    && let Some(t) = h.get("type")
                    && let Some(obj) = entry.as_object_mut()
                {
                    obj.insert("type".into(), t.clone());
                }
                entries.push(entry);
            }
        }
        if !entries.is_empty() {
            return (entries, true);
        }
    }
    let mut entries = load_bubbles_range(conn, composer_id);
    // Timestamped first, in order; untimestamped keep their scan order.
    entries.sort_by_cached_key(|entry| {
        let ts = parse_any_timestamp(&cursor_entry_timestamp(entry));
        (ts.is_none(), ts)
    });
    (entries, false)
}

fn first_user_text(conn: &Connection, composer_id: &str, headers: Option<&[Value]>) -> String {
    if let Some(headers) = headers {
        for h in headers {
            if h.get("type").and_then(Value::as_i64) != Some(1) {
                continue;
            }
            let preview = h
                .pointer("/grouping/textPreview")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if !preview.is_empty() {
                let cleaned = clean_first_prompt(preview);
                if !cleaned.is_empty()
                    && !is_warmup_message(&cleaned)
                    && !is_clear_metadata(&cleaned)
                {
                    return cleaned.chars().take(300).collect();
                }
            }
            if let Some(bid) = h.get("bubbleId").and_then(Value::as_str)
                && let Some(raw) = kv_blob(conn, &format!("bubbleId:{composer_id}:{bid}"))
                && let Ok(entry) = serde_json::from_str::<Value>(&raw)
            {
                let cleaned = clean_first_prompt(bubble_text(&entry));
                if !cleaned.is_empty()
                    && !is_warmup_message(&cleaned)
                    && !is_clear_metadata(&cleaned)
                {
                    return cleaned.chars().take(300).collect();
                }
            }
        }
    }
    let stub = Session {
        source: "cursor-ide".into(),
        id: composer_id.into(),
        summary: String::new(),
        first_prompt: String::new(),
        created: String::new(),
        modified: String::new(),
        date: String::new(),
        messages: 0,
        branch: String::new(),
        project: String::new(),
        file: String::new(),
        is_sidechain: false,
        also_ide: false,
    };
    load_bubbles_as_messages(conn, &stub)
        .into_iter()
        .find(|m| m.role == "user")
        .map(|m| m.content.chars().take(300).collect())
        .unwrap_or_default()
}

fn load_bubbles_as_messages(conn: &Connection, session: &Session) -> Vec<Message> {
    let mut messages = Vec::new();
    let mut total_chars: usize = 0;
    let (entries, ordered) = load_bubble_entries(conn, &session.id);
    for entry in entries {
        if total_chars > 4 * 1024 * 1024 {
            break;
        }
        let Some(msg) = message_from_bubble(session, &entry) else {
            continue;
        };
        total_chars += msg.content.len();
        messages.push(msg);
    }
    if !ordered {
        sort_messages_stable(&mut messages);
    }
    messages
}

fn bubble_counts(conn: &Connection) -> HashMap<String, u64> {
    let mut counts = HashMap::new();
    let mut stmt = match conn.prepare(
        "SELECT substr(key, 10, 36), count(*) FROM cursorDiskKV \
         WHERE key >= 'bubbleId:' AND key < 'bubbleId;' GROUP BY 1",
    ) {
        Ok(s) => s,
        Err(_) => return counts,
    };
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
    });
    if let Ok(rows) = rows {
        for row in rows.flatten() {
            counts.insert(row.0, row.1);
        }
    }
    counts
}

pub fn load_cursor_ide_sessions() -> Vec<Session> {
    load_cursor_ide_sessions_from(&global_vscdb())
}

fn load_cursor_ide_sessions_from(db: &Path) -> Vec<Session> {
    if !db.exists() {
        return Vec::new();
    }
    let Some(conn) = open_ro(db) else {
        eprintln!(
            "Warning: cannot read Cursor history database {}. Check file permissions and CURSOR_USER_DIR.",
            db.display()
        );
        return Vec::new();
    };
    let mut sessions = Vec::new();
    // Required columns identify the supported storage family; the message
    // table is checked too so header-only rows never masquerade as readable
    // conversations. Optional header columns vary between Cursor versions and
    // are read by name below, defaulting when absent.
    let supported = conn
        .prepare("SELECT composerId, value FROM composerHeaders LIMIT 0")
        .and(conn.prepare("SELECT key, value FROM cursorDiskKV LIMIT 0"));
    let mut stmt = match supported.and(conn.prepare("SELECT * FROM composerHeaders")) {
        Ok(s) => s,
        Err(reason) => {
            eprintln!(
                "Warning: unsupported Cursor history database {}: {reason}. Agent transcript files are still searched; check CURSOR_USER_DIR or update chat-history.",
                db.display()
            );
            return sessions;
        }
    };
    let optional_int = |row: &rusqlite::Row, name: &str| {
        row.get::<_, Option<i64>>(name).ok().flatten().unwrap_or(0)
    };
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>("composerId")?,
            optional_int(row, "createdAt"),
            optional_int(row, "lastUpdatedAt"),
            optional_int(row, "isSubagent") != 0,
            blob_text(row.get_ref("value")?),
        ))
    });
    let Ok(rows) = rows else {
        return sessions;
    };
    let counts = bubble_counts(&conn);
    let file = db.to_string_lossy().to_string();
    for row in rows.flatten() {
        let (id, mut created_ms, mut updated_ms, mut is_sidechain, raw) = row;
        let meta: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
        if created_ms <= 0 {
            created_ms = meta.get("createdAt").and_then(Value::as_i64).unwrap_or(0);
        }
        if updated_ms <= 0 {
            updated_ms = meta
                .get("lastUpdatedAt")
                .and_then(Value::as_i64)
                .unwrap_or(0);
        }
        is_sidechain |= meta
            .get("isSubagent")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let name = meta
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let subtitle = meta
            .get("subtitle")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let mut first: String = if !subtitle.is_empty() {
            subtitle.chars().take(300).collect()
        } else {
            name.chars().take(300).collect()
        };
        // composerData blobs are large (tens of MB across a real DB); read
        // and parse each one once per listing.
        let headers = composer_header_order(&conn, &id);
        let header_len = headers.as_ref().map(|h| h.len() as u64).unwrap_or(0);
        let nmsg = header_len.max(counts.get(&id).copied().unwrap_or(0));
        if first.is_empty() && nmsg > 0 {
            first = first_user_text(&conn, &id, headers.as_deref());
        }
        let ts = if updated_ms > 0 {
            updated_ms
        } else {
            created_ms
        };
        let iso = ms_to_iso(ts);
        let date = iso_date(&iso);
        // No fabricated creation time: an absent createdAt stays unknown
        // rather than becoming the last-update time.
        let created = ms_to_iso(created_ms);
        sessions.push(Session {
            source: "cursor-ide".into(),
            id,
            summary: name,
            first_prompt: first,
            created,
            modified: iso,
            date,
            messages: nmsg,
            branch: String::new(),
            project: workspace_path(&meta),
            file: file.clone(),
            is_sidechain,
            also_ide: false,
        });
    }
    sessions
}

/// Enrich only unambiguous matches. Do not reorder, replace, or drop transcript
/// content, and never turn a session timestamp into a message timestamp.
pub fn enrich_transcript_timestamps(session: &Session, messages: &mut [Message]) {
    if messages.is_empty() || messages.iter().all(|m| !m.timestamp.is_empty()) {
        return;
    }
    let db = global_vscdb();
    if !db.is_file() {
        return;
    }
    // Deep search enriches many sessions from a rayon pool: keep one
    // read-only connection per worker thread instead of reopening the
    // (multi-GB) database for every transcript. The bubbles come from the
    // same loader the IDE reader uses, so every bubble the sidebar shows is
    // a candidate here too, including ones whose `type` only the
    // conversation headers carry.
    ENRICH_SOURCE.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.as_ref().is_none_or(|(path, _)| *path != db) {
            *slot = open_ro(&db).map(|conn| (db.clone(), conn));
        }
        if let Some((_, conn)) = slot.as_ref() {
            merge_message_timestamps(messages, &load_bubbles_as_messages(conn, session));
        }
    });
}

thread_local! {
    static ENRICH_SOURCE: RefCell<Option<(PathBuf, Connection)>> = const { RefCell::new(None) };
}

fn merge_message_timestamps(messages: &mut [Message], bubbles: &[Message]) {
    let content_key = |m: &Message| {
        (
            m.role.clone(),
            if m.role == "user" {
                clean_first_prompt(&m.content)
            } else {
                m.content.trim().to_owned()
            },
        )
    };
    let mut by_id: HashMap<(&str, &str), Vec<&Message>> = HashMap::new();
    let mut by_text: HashMap<(String, String), Vec<&Message>> = HashMap::new();
    let mut occurrences = HashMap::new();
    for bubble in bubbles {
        if !bubble.uuid.is_empty() {
            by_id
                .entry((&bubble.role, &bubble.uuid))
                .or_default()
                .push(bubble);
        }
        by_text.entry(content_key(bubble)).or_default().push(bubble);
    }
    for message in messages.iter() {
        *occurrences.entry(content_key(message)).or_insert(0usize) += 1;
    }
    for message in messages {
        if !message.timestamp.is_empty() {
            continue;
        }
        let key = content_key(message);
        let by_unique_text = || {
            (occurrences.get(&key) == Some(&1) && !key.1.is_empty())
                .then(|| by_text.get(&key))
                .flatten()
        };
        // A transcript id the IDE never assigned must not block the text
        // match; a real bubble id still wins when it resolves.
        let matched = if message.uuid.is_empty() {
            by_unique_text()
        } else {
            by_id
                .get(&(message.role.as_str(), message.uuid.as_str()))
                .or_else(by_unique_text)
        };
        if let Some(matches) = matched
            && matches.len() == 1
        {
            message.timestamp = matches[0].timestamp.clone();
        }
    }
}

pub fn parse_cursor_ide(session: &Session) -> Vec<Message> {
    let db = Path::new(&session.file);
    let Some(conn) = open_ro(db) else {
        return Vec::new();
    };
    load_bubbles_as_messages(&conn, session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn empty_schema(conn: &Connection) {
        conn.execute_batch(
            r#"
            CREATE TABLE composerHeaders (
                composerId TEXT PRIMARY KEY,
                workspaceId TEXT,
                createdAt INTEGER,
                lastUpdatedAt INTEGER,
                isArchived INTEGER,
                isSubagent INTEGER,
                recency INTEGER,
                checkpointAt INTEGER,
                value TEXT
            );
            CREATE TABLE cursorDiskKV (key TEXT UNIQUE, value BLOB);
            "#,
        )
        .unwrap();
    }

    fn fixture_db() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::TempDir::new().unwrap();
        let user = tmp.path().join("User");
        let dir = user.join("globalStorage");
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("state.vscdb");
        let conn = Connection::open(&db).unwrap();
        empty_schema(&conn);
        let header = serde_json::json!({
            "name": "Explain the error handler",
            "unifiedMode": "agent",
            "subtitle": "Read src/main.rs",
            "workspaceIdentifier": {"uri": {"fsPath": "/home/alice/src/myapp", "path": "/home/alice/src/myapp"}}
        });
        conn.execute(
            "INSERT INTO composerHeaders (composerId, createdAt, lastUpdatedAt, isSubagent, value)
             VALUES (?1, 1782941109570, 1782941109570, 0, ?2)",
            rusqlite::params!["aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee", header.to_string()],
        )
        .unwrap();
        let bubble = serde_json::json!({
            "type": 1,
            "text": "How does the error handler work?",
            "bubbleId": "b1",
            "createdAt": "2026-07-01T21:25:09.594Z"
        });
        conn.execute(
            "INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
            rusqlite::params![
                "bubbleId:aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee:b1",
                bubble.to_string()
            ],
        )
        .unwrap();
        drop(conn);
        (tmp, db)
    }

    #[test]
    fn loads_ide_chat_from_vscdb() {
        let (_tmp, db) = fixture_db();
        let sessions = load_cursor_ide_sessions_from(&db);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].source, "cursor-ide");
        assert_eq!(sessions[0].summary, "Explain the error handler");
        assert!(
            sessions[0].first_prompt.contains("error handler")
                || sessions[0].first_prompt.contains("main.rs")
        );
        assert_eq!(sessions[0].project, "/home/alice/src/myapp");
        assert!(sessions[0].messages >= 1);
        let msgs = parse_cursor_ide(&sessions[0]);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "user");
    }

    #[test]
    fn range_query_uses_index() {
        let (_tmp, db) = fixture_db();
        let conn = Connection::open(&db).unwrap();
        let mut stmt = conn
            .prepare(
                "EXPLAIN QUERY PLAN SELECT value FROM cursorDiskKV WHERE key >= ?1 AND key < ?2",
            )
            .unwrap();
        let plan: String = stmt
            .query_map(
                [
                    "bubbleId:aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee:",
                    "bubbleId:aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee;",
                ],
                |row| row.get::<_, String>(3),
            )
            .unwrap()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        let upper = plan.to_ascii_uppercase();
        assert!(
            upper.contains("SEARCH") && upper.contains("INDEX"),
            "expected index search, got {plan}"
        );
        assert!(
            !upper.contains("SCAN CURSORDISKKV"),
            "full table scan: {plan}"
        );
    }

    #[test]
    fn sorts_bubbles_by_created_at_not_insert_order() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = tmp.path().join("state.vscdb");
        let conn = Connection::open(&db).unwrap();
        empty_schema(&conn);
        let id = "bbbb2222-cccc-dddd-eeee-ffffffffffff";
        conn.execute(
            "INSERT INTO composerHeaders (composerId, createdAt, lastUpdatedAt, isSubagent, value)
             VALUES (?1, 1, 1, 0, '{}')",
            [id],
        )
        .unwrap();
        let assistant = serde_json::json!({
            "type": 2, "text": "later assistant", "bubbleId": "a",
            "createdAt": "2026-08-01T10:00:05Z"
        });
        let user = serde_json::json!({
            "type": 1, "text": "earlier user prompt here", "bubbleId": "u",
            "createdAt": "2026-08-01T10:00:00Z"
        });
        conn.execute(
            "INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
            rusqlite::params![format!("bubbleId:{id}:a"), assistant.to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
            rusqlite::params![format!("bubbleId:{id}:u"), user.to_string()],
        )
        .unwrap();
        drop(conn);
        let session = Session {
            source: "cursor-ide".into(),
            id: id.into(),
            summary: String::new(),
            first_prompt: String::new(),
            created: String::new(),
            modified: String::new(),
            date: String::new(),
            messages: 0,
            branch: String::new(),
            project: String::new(),
            file: db.to_string_lossy().into(),
            is_sidechain: false,
            also_ide: false,
        };
        let msgs = parse_cursor_ide(&session);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[1].role, "assistant");
    }

    #[test]
    fn prefers_composer_data_header_order() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = tmp.path().join("state.vscdb");
        let conn = Connection::open(&db).unwrap();
        empty_schema(&conn);
        let id = "cccc3333-dddd-eeee-ffff-111111111111";
        conn.execute(
            "INSERT INTO composerHeaders (composerId, createdAt, lastUpdatedAt, isSubagent, value)
             VALUES (?1, 1, 1, 0, '{}')",
            [id],
        )
        .unwrap();
        let u1 = serde_json::json!({"type": 1, "text": "first user message body", "bubbleId": "u1", "createdAt": "2026-08-01T10:01:00Z"});
        let a1 = serde_json::json!({"type": 2, "text": "assistant reply body", "bubbleId": "a1", "createdAt": "2026-08-01T10:00:05Z"});
        let u2 = serde_json::json!({"type": 1, "text": "second user message body", "bubbleId": "u2", "createdAt": "2026-08-01T09:00:00Z"});
        for (k, v) in [("u1", &u1), ("a1", &a1), ("u2", &u2)] {
            conn.execute(
                "INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
                rusqlite::params![format!("bubbleId:{id}:{k}"), v.to_string()],
            )
            .unwrap();
        }
        let data = serde_json::json!({
            "fullConversationHeadersOnly": [
                {"bubbleId": "u1", "type": 1, "createdAt": "2026-08-01T10:00:00Z"},
                {"bubbleId": "a1", "type": 2, "createdAt": "2026-08-01T10:00:05Z"},
                {"bubbleId": "u2", "type": 1, "createdAt": "2026-08-01T10:01:00Z"}
            ]
        });
        conn.execute(
            "INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
            rusqlite::params![format!("composerData:{id}"), data.to_string()],
        )
        .unwrap();
        drop(conn);
        let session = Session {
            source: "cursor-ide".into(),
            id: id.into(),
            summary: String::new(),
            first_prompt: String::new(),
            created: String::new(),
            modified: String::new(),
            date: String::new(),
            messages: 0,
            branch: String::new(),
            project: String::new(),
            file: db.to_string_lossy().into(),
            is_sidechain: false,
            also_ide: false,
        };
        let msgs = parse_cursor_ide(&session);
        assert_eq!(msgs.len(), 3);
        assert!(msgs[0].content.contains("first user"));
        assert!(msgs[1].content.contains("assistant"));
        assert!(msgs[2].content.contains("second user"));
    }

    #[test]
    fn rich_text_only_bubble_is_indexed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = tmp.path().join("state.vscdb");
        let conn = Connection::open(&db).unwrap();
        empty_schema(&conn);
        let id = "dddd4444-eeee-ffff-aaaa-222222222222";
        conn.execute(
            "INSERT INTO composerHeaders (composerId, createdAt, lastUpdatedAt, isSubagent, value)
             VALUES (?1, 1, 1, 0, '{\"name\":\"\"}')",
            [id],
        )
        .unwrap();
        let bubble = serde_json::json!({
            "type": 1, "text": "", "richText": "please explain the cache layer design",
            "bubbleId": "r1", "createdAt": "2026-08-01T10:00:00Z"
        });
        conn.execute(
            "INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
            rusqlite::params![format!("bubbleId:{id}:r1"), bubble.to_string()],
        )
        .unwrap();
        drop(conn);
        let sessions = load_cursor_ide_sessions_from(&db);
        assert_eq!(sessions.len(), 1);
        assert!(sessions[0].first_prompt.contains("cache layer"));
        let msgs = parse_cursor_ide(&sessions[0]);
        assert_eq!(msgs[0].content, "please explain the cache layer design");
    }

    #[test]
    fn opens_db_when_path_contains_hash() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("cursor#profile").join("globalStorage");
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("state.vscdb");
        let conn = Connection::open(&db).unwrap();
        empty_schema(&conn);
        conn.execute(
            "INSERT INTO composerHeaders (composerId, createdAt, lastUpdatedAt, isSubagent, value)
             VALUES ('eeee5555-ffff-aaaa-bbbb-333333333333', 1, 1, 0, '{\"name\":\"hash path\"}')",
            [],
        )
        .unwrap();
        drop(conn);
        let sessions = load_cursor_ide_sessions_from(&db);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].summary, "hash path");
    }

    #[test]
    fn cursor_ide_preserves_creation_and_activity_times() {
        let (_tmp, db) = fixture_db();
        let conn = Connection::open(&db).unwrap();
        conn.execute(
            "UPDATE composerHeaders SET createdAt = 1788220800000, lastUpdatedAt = 1788307200000",
            [],
        )
        .unwrap();
        let sessions = load_cursor_ide_sessions_from(&db);
        assert_eq!(sessions[0].created, "2026-09-01T00:00:00Z");
        assert_eq!(sessions[0].modified, "2026-09-02T00:00:00Z");
        assert_eq!(sessions[0].date, "2026-09-02");
        assert_eq!(ms_to_iso(0), "");
    }

    #[test]
    fn cursor_ide_tolerates_missing_optional_header_columns() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = tmp.path().join("state.vscdb");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE composerHeaders (composerId TEXT PRIMARY KEY, value TEXT);
            CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value BLOB);
            INSERT INTO composerHeaders VALUES ('id', '{\"name\":\"older header\",\"createdAt\":1788220800000,\"isSubagent\":true}');
            INSERT INTO composerHeaders VALUES ('id2', '{\"name\":\"no times\",\"lastUpdatedAt\":1788307200000}');").unwrap();
        let mut sessions = load_cursor_ide_sessions_from(&db);
        sessions.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].summary, "older header");
        assert_eq!(sessions[0].created, "2026-09-01T00:00:00Z");
        assert!(sessions[0].is_sidechain);
        // No createdAt anywhere: creation stays unknown, activity is known.
        assert_eq!(sessions[1].created, "");
        assert_eq!(sessions[1].date, "2026-09-02");
    }

    #[test]
    fn enrichment_recovers_bubble_type_from_conversation_headers() {
        let (_tmp, db) = fixture_db();
        let conn = Connection::open(&db).unwrap();
        let id = "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee";
        // Some bubbles carry no `type`; the IDE reader takes it from the
        // conversation headers, and enrichment must do the same.
        conn.execute(
            "INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
            rusqlite::params![
                format!("bubbleId:{id}:b2"),
                serde_json::json!({"text":"Second question","bubbleId":"b2","createdAt":"2026-07-01T21:30:00.000Z"}).to_string()
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
            rusqlite::params![
                format!("composerData:{id}"),
                serde_json::json!({"fullConversationHeadersOnly":[{"bubbleId":"b1","type":1},{"bubbleId":"b2","type":1}]}).to_string()
            ],
        )
        .unwrap();
        let sessions = load_cursor_ide_sessions_from(&db);
        let mut messages = parse_cursor_ide(&sessions[0]);
        assert_eq!(messages.len(), 2, "IDE reader sees both bubbles");
        for m in &mut messages {
            m.timestamp.clear();
            m.uuid.clear();
        }
        merge_message_timestamps(
            &mut messages,
            &load_bubbles_as_messages(&conn, &sessions[0]),
        );
        assert_eq!(messages[0].timestamp, "2026-07-01T21:25:09.594Z");
        assert_eq!(messages[1].timestamp, "2026-07-01T21:30:00.000Z");
    }

    #[test]
    fn unique_text_fallback_applies_when_uuid_is_not_a_bubble_id() {
        let (_tmp, db) = fixture_db();
        let sessions = load_cursor_ide_sessions_from(&db);
        let original = parse_cursor_ide(&sessions[0]).remove(0);
        let mut message = original.clone();
        message.uuid = "msg_01X-not-a-bubble-id".into();
        message.timestamp.clear();
        let mut messages = vec![message];
        merge_message_timestamps(&mut messages, std::slice::from_ref(&original));
        assert_eq!(messages[0].timestamp, original.timestamp);
    }

    #[test]
    fn cursor_ide_enrichment_preserves_content_and_rejects_ambiguous_matches() {
        let (_tmp, db) = fixture_db();
        let sessions = load_cursor_ide_sessions_from(&db);
        let original = parse_cursor_ide(&sessions[0]).remove(0);
        let mut unknown = original.clone();
        unknown.uuid.clear();
        unknown.timestamp.clear();
        let mut messages = vec![unknown.clone()];
        merge_message_timestamps(&mut messages, std::slice::from_ref(&original));
        assert_eq!(messages[0].timestamp, original.timestamp);
        assert_eq!(messages[0].content, unknown.content);
        assert!(messages[0].uuid.is_empty());

        let mut repeated = vec![unknown.clone(), unknown.clone()];
        merge_message_timestamps(&mut repeated, std::slice::from_ref(&original));
        assert!(repeated.iter().all(|m| m.timestamp.is_empty()));
        let mut repeated_bubbles = vec![unknown.clone()];
        merge_message_timestamps(&mut repeated_bubbles, &[original.clone(), original.clone()]);
        assert!(repeated_bubbles[0].timestamp.is_empty());

        let mut native = original.clone();
        native.timestamp = "2026-09-07T10:00:00Z".into();
        let mut with_native = vec![native.clone()];
        merge_message_timestamps(&mut with_native, std::slice::from_ref(&original));
        assert_eq!(with_native[0].timestamp, native.timestamp);

        unknown.uuid = original.uuid.clone();
        unknown.content = "same ID, richer transcript text with tools".into();
        let mut by_id = vec![unknown];
        merge_message_timestamps(&mut by_id, std::slice::from_ref(&original));
        assert_eq!(by_id[0].timestamp, original.timestamp);
        assert_eq!(
            by_id[0].content,
            "same ID, richer transcript text with tools"
        );
    }
}
