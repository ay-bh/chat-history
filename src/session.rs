use crate::parser::{
    clean_first_prompt, clean_prompt, extract_text, is_clear_metadata, is_warmup_message,
};
use chrono::{DateTime, FixedOffset, NaiveDate, Utc};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Clone, Debug)]
pub struct Session {
    pub source: String,
    pub id: String,
    pub summary: String,
    pub first_prompt: String,
    pub created: String,
    pub modified: String,
    pub date: String,
    pub messages: u64,
    pub branch: String,
    pub project: String,
    pub file: String,
    pub is_sidechain: bool,
}

#[derive(Clone, Debug)]
pub struct Message {
    #[allow(dead_code)]
    pub uuid: String,
    pub timestamp: String,
    pub role: String,
    pub content: String,
    pub session_id: String,
    pub project_path: String,
    pub tool_uses: Vec<String>,
    pub files_referenced: Vec<String>,
    pub error_patterns: Vec<String>,
    pub relevance_score: f64,
    pub final_score: f64,
}

impl Message {
    pub fn content_lower(&self) -> String {
        self.content.to_lowercase()
    }
}

#[derive(Default)]
pub struct SessionMeta {
    pub summary: Option<String>,
    pub custom_title: Option<String>,
    pub model: Option<String>,
    pub total_tokens: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IndexFile {
    #[serde(default)]
    entries: Vec<IndexEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IndexEntry {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    first_prompt: String,
    #[serde(default)]
    created: String,
    #[serde(default)]
    modified: String,
    #[serde(default)]
    message_count: u64,
    #[serde(default)]
    git_branch: String,
    #[serde(default)]
    project_path: String,
    #[serde(default)]
    full_path: String,
    #[serde(default)]
    is_sidechain: bool,
}

fn home_dir() -> PathBuf {
    user_home().unwrap_or_else(|| PathBuf::from("."))
}

/// Resolve the user home directory (macOS/Linux `HOME`, Windows `USERPROFILE`).
pub fn user_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

pub fn claude_projects_dir() -> PathBuf {
    if let Ok(config_dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        return PathBuf::from(config_dir).join("projects");
    }
    home_dir().join(".claude").join("projects")
}

fn cursor_projects_dir() -> PathBuf {
    home_dir().join(".cursor").join("projects")
}

pub fn codex_home() -> PathBuf {
    // A set-but-empty CODEX_HOME conventionally means unset; honoring it
    // would resolve everything relative to the current directory.
    if let Some(dir) = std::env::var_os("CODEX_HOME")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    home_dir().join(".codex")
}

fn codex_sessions_dir() -> PathBuf {
    codex_home().join("sessions")
}

// Reads cwd and gitBranch from the first records of a Claude transcript.
fn read_cwd_branch_from_jsonl(path: &Path) -> (Option<String>, Option<String>) {
    let Ok(file) = fs::File::open(path) else {
        return (None, None);
    };
    let reader = BufReader::new(file);
    let mut cwd = None;
    let mut branch = None;
    for line in reader.lines().take(10) {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let entry: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if cwd.is_none()
            && let Some(c) = entry.get("cwd").and_then(Value::as_str)
            && !c.is_empty()
        {
            cwd = Some(c.to_string());
        }
        if branch.is_none()
            && let Some(b) = entry.get("gitBranch").and_then(Value::as_str)
            && !b.is_empty()
        {
            branch = Some(b.to_string());
        }
        if cwd.is_some() && branch.is_some() {
            break;
        }
    }
    (cwd, branch)
}

// Current Claude Code appends ai-title records as the session title evolves;
// the last one is the current title. Read from the tail to avoid scanning
// whole transcripts during listing.
fn claude_ai_title(path: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    const TAIL: u64 = 64 * 1024;
    let start = len.saturating_sub(TAIL);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    // The seek may land mid-character; decode lossily and skip the partial line.
    let buf = String::from_utf8_lossy(&bytes);
    let mut ai_title = None;
    let mut custom_title_cleared = false;
    for line in buf.lines().rev() {
        if !line.contains("\"ai-title\"") && !line.contains("\"custom-title\"") {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        match entry.get("type").and_then(Value::as_str) {
            Some("custom-title") => {
                if custom_title_cleared {
                    continue;
                }
                let title = entry
                    .get("customTitle")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                if !title.is_empty() {
                    return Some(title.to_string());
                }
                custom_title_cleared = true;
            }
            Some("ai-title") if ai_title.is_none() => {
                let title = entry
                    .get("aiTitle")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                if !title.is_empty() {
                    ai_title = Some(title.to_string());
                }
            }
            _ => {}
        }
    }
    ai_title
}

pub fn encode_path_for_claude(path: &Path) -> String {
    // Claude Code encodes project dirs by replacing every character outside
    // ASCII [a-zA-Z0-9] with '-' (claude-code#19972), not just separators.
    path.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn normalize_for_project_match(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .to_lowercase()
}

pub fn copy_session_to_dir(session: &Session, target_dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(target_dir)?;

    let src = Path::new(&session.file);
    let filename = src.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("session file path '{}' has no filename", session.file),
        )
    })?;
    fs::copy(src, target_dir.join(filename))?;

    if let Some(parent) = src.parent() {
        let companion = parent.join(&session.id);
        if companion.is_dir() {
            copy_dir_recursive(&companion, &target_dir.join(&session.id))?;
        }
    }
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let dest_path = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else {
            fs::copy(entry.path(), &dest_path)?;
        }
    }
    Ok(())
}

fn mtime_iso(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let dur = mtime.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    let dt = DateTime::<Utc>::from_timestamp(dur.as_secs() as i64, 0)?;
    Some(dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

pub fn parse_any_timestamp(s: &str) -> Option<DateTime<FixedOffset>> {
    let s = s.replace('Z', "+00:00");
    DateTime::parse_from_rfc3339(&s)
        .or_else(|_| DateTime::parse_from_str(&s, "%Y-%m-%dT%H:%M:%S%.f%:z"))
        .ok()
        .or_else(|| s.parse::<DateTime<Utc>>().ok().map(|t| t.fixed_offset()))
}

fn mtime_date(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let dur = mtime.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    let dt = DateTime::<Utc>::from_timestamp(dur.as_secs() as i64, 0)?;
    Some(dt.format("%Y-%m-%d").to_string())
}

fn claude_first_prompt(path: &Path) -> String {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return String::new(),
    };
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let trimmed = line.trim().to_string();
        if trimmed.is_empty() {
            continue;
        }
        let entry: Value = match serde_json::from_str(&trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if entry.get("type").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let content = entry
            .get("message")
            .and_then(|m| m.get("content"))
            .cloned()
            .unwrap_or(Value::String(String::new()));
        let text = extract_text(&content);
        let cleaned = clean_prompt(&text);
        if !cleaned.is_empty() && !is_warmup_message(&cleaned) && !is_clear_metadata(&cleaned) {
            return cleaned.chars().take(300).collect();
        }
    }
    String::new()
}

fn cursor_first_prompt_jsonl(path: &Path) -> String {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return String::new(),
    };
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let Ok(line) = line else { continue };
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        // Role lives at the top level in Cursor agent transcripts; entries
        // without one are kept as candidates for older formats.
        if let Some(role) = entry.get("role").and_then(Value::as_str)
            && role != "user"
        {
            continue;
        }
        let content = entry
            .get("message")
            .and_then(|m| m.get("content"))
            .cloned()
            .unwrap_or(Value::String(String::new()));
        let text = extract_text(&content);
        // Clean before truncating: the first user message often opens with
        // kilobytes of preamble tags, and a raw prefix would cut off before
        // the actual query.
        let cleaned = clean_first_prompt(&text);
        if !cleaned.is_empty() && !is_warmup_message(&cleaned) && !is_clear_metadata(&cleaned) {
            return cleaned.chars().take(300).collect();
        }
    }
    String::new()
}

fn cursor_first_prompt_txt(path: &Path) -> String {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    for (i, line) in content.lines().enumerate() {
        if i == 0 {
            continue;
        }
        if i > 30 {
            break;
        }
        let s = line.trim();
        if !s.is_empty()
            && ![
                "<user_query>",
                "</user_query>",
                "user:",
                "assistant:",
                "<attached_files>",
            ]
            .contains(&s)
        {
            return s.chars().take(300).collect();
        }
    }
    String::new()
}

pub fn load_claude_sessions() -> Vec<Session> {
    let base = claude_projects_dir();
    if !base.exists() {
        return Vec::new();
    }
    let mut sessions = Vec::new();
    let mut indexed_ids = HashSet::new();

    for idx_path in glob::glob(&format!("{}/**/sessions-index.json", base.display()))
        .into_iter()
        .flatten()
        .flatten()
    {
        let data = match fs::read_to_string(&idx_path) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let index: IndexFile = match serde_json::from_str(&data) {
            Ok(i) => i,
            Err(_) => continue,
        };
        for entry in index.entries {
            indexed_ids.insert(entry.session_id.clone());
            let date = entry
                .created
                .get(..10)
                .filter(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").is_ok())
                .map(str::to_string)
                .unwrap_or_else(|| mtime_date(Path::new(&entry.full_path)).unwrap_or_default());
            sessions.push(Session {
                source: "claude".into(),
                id: entry.session_id,
                summary: entry.summary,
                first_prompt: entry.first_prompt.chars().take(300).collect(),
                created: entry.created.clone(),
                modified: entry.modified,
                date,
                messages: entry.message_count,
                branch: entry.git_branch,
                project: entry.project_path,
                file: entry.full_path,
                is_sidechain: entry.is_sidechain,
            });
        }
    }

    if let Ok(entries) = fs::read_dir(&base) {
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            if let Ok(files) = fs::read_dir(&dir) {
                for f in files.flatten() {
                    let path = f.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                        continue;
                    }
                    let sid = path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    if indexed_ids.contains(&sid) || sid.starts_with("agent-") {
                        continue;
                    }
                    let iso = mtime_iso(&path).unwrap_or_default();
                    let date = mtime_date(&path).unwrap_or_default();
                    let (cwd, branch) = read_cwd_branch_from_jsonl(&path);
                    let project = cwd.unwrap_or_else(|| {
                        dir.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .replace('-', "/")
                    });
                    let summary = claude_ai_title(&path).unwrap_or_default();
                    let first = claude_first_prompt(&path);
                    sessions.push(Session {
                        source: "claude".into(),
                        id: sid,
                        summary,
                        first_prompt: first,
                        created: iso.clone(),
                        modified: iso,
                        date,
                        messages: 0,
                        branch: branch.unwrap_or_default(),
                        project,
                        file: path.to_string_lossy().to_string(),
                        is_sidechain: false,
                    });
                }
            }
        }
    }
    sessions
}

pub fn load_cursor_sessions() -> Vec<Session> {
    let base = cursor_projects_dir();
    if !base.exists() {
        return Vec::new();
    }
    let mut sessions = Vec::new();

    if let Ok(project_dirs) = fs::read_dir(&base) {
        for pd in project_dirs.flatten() {
            let transcripts = pd.path().join("agent-transcripts");
            if !transcripts.is_dir() {
                continue;
            }
            let mut txt_ids: HashSet<String> = HashSet::new();
            let mut dir_entries: Vec<(String, PathBuf)> = Vec::new();
            if let Ok(entries) = fs::read_dir(&transcripts) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("txt") {
                        let sid = path
                            .file_stem()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        txt_ids.insert(sid.clone());
                        let jsonl_alt = transcripts.join(&sid).join(format!("{sid}.jsonl"));
                        let iso = mtime_iso(&path).unwrap_or_default();
                        let date = mtime_date(&path).unwrap_or_default();
                        let first = cursor_first_prompt_txt(&path);
                        let file = if jsonl_alt.exists() {
                            jsonl_alt.to_string_lossy().to_string()
                        } else {
                            path.to_string_lossy().to_string()
                        };
                        sessions.push(Session {
                            source: "cursor".into(),
                            id: sid,
                            summary: String::new(),
                            first_prompt: first,
                            created: iso.clone(),
                            modified: iso,
                            date,
                            messages: 0,
                            branch: String::new(),
                            project: pd.file_name().to_string_lossy().to_string(),
                            file,
                            is_sidechain: false,
                        });
                    } else if path.is_dir() {
                        let dirname = path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        dir_entries.push((dirname, path));
                    }
                }
            }
            for (dirname, path) in dir_entries {
                let subagents = path.join("subagents");
                if let Ok(entries) = fs::read_dir(&subagents) {
                    for entry in entries.flatten() {
                        let subagent_path = entry.path();
                        if subagent_path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                            continue;
                        }
                        let sid = subagent_path
                            .file_stem()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        let iso = mtime_iso(&subagent_path).unwrap_or_default();
                        let date = mtime_date(&subagent_path).unwrap_or_default();
                        let first = cursor_first_prompt_jsonl(&subagent_path);
                        sessions.push(Session {
                            source: "cursor".into(),
                            id: sid,
                            summary: String::new(),
                            first_prompt: first,
                            created: iso.clone(),
                            modified: iso,
                            date,
                            messages: 0,
                            branch: String::new(),
                            project: pd.file_name().to_string_lossy().to_string(),
                            file: subagent_path.to_string_lossy().to_string(),
                            is_sidechain: true,
                        });
                    }
                }
                if txt_ids.contains(&dirname) {
                    continue;
                }
                let jf = path.join(format!("{dirname}.jsonl"));
                if !jf.exists() {
                    continue;
                }
                let iso = mtime_iso(&jf).unwrap_or_default();
                let date = mtime_date(&jf).unwrap_or_default();
                let first = cursor_first_prompt_jsonl(&jf);
                sessions.push(Session {
                    source: "cursor".into(),
                    id: dirname,
                    summary: String::new(),
                    first_prompt: first,
                    created: iso.clone(),
                    modified: iso,
                    date,
                    messages: 0,
                    branch: String::new(),
                    project: pd.file_name().to_string_lossy().to_string(),
                    file: jf.to_string_lossy().to_string(),
                    is_sidechain: false,
                });
            }
        }
    }
    sessions
}

struct CodexMeta {
    id: String,
    created: String,
    cwd: String,
    branch: String,
    is_subagent: bool,
}

// Codex rollout lines are {"timestamp", "type", "payload": {...}}; older CLI
// versions wrote the payload fields at the top level without a wrapper.
fn codex_payload(entry: &Value) -> &Value {
    entry.get("payload").unwrap_or(entry)
}

fn read_codex_meta(path: &Path) -> Option<CodexMeta> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    let line = reader.lines().next()?.ok()?;
    let entry: Value = serde_json::from_str(line.trim()).ok()?;
    let payload = codex_payload(&entry);
    let id = payload.get("id").and_then(Value::as_str)?.to_string();
    let created = payload
        .get("timestamp")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let branch = payload
        .get("git")
        .and_then(|g| g.get("branch"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let is_subagent = payload
        .get("source")
        .and_then(|s| s.get("subagent"))
        .is_some();
    Some(CodexMeta {
        id,
        created,
        cwd,
        branch,
        is_subagent,
    })
}

// Codex injects system wrappers (environment, permissions, goal context,
// aborted-turn markers, subagent notifications) as user-role messages.
fn is_codex_noise(text: &str) -> bool {
    let t = text.trim_start();
    [
        "<environment_context>",
        "<permissions",
        "<user_instructions>",
        "<turn_aborted",
        "<goal_context",
        "<subagent_notification",
    ]
    .iter()
    .any(|tag| t.starts_with(tag))
}

fn codex_first_prompt(path: &Path) -> String {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return String::new(),
    };
    let reader = BufReader::new(file);
    for line in reader.lines().take(100) {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let entry: Value = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let payload = codex_payload(&entry);
        let text = match payload.get("type").and_then(Value::as_str) {
            Some("message") if payload.get("role").and_then(Value::as_str) == Some("user") => {
                let content = payload
                    .get("content")
                    .cloned()
                    .unwrap_or(Value::String(String::new()));
                extract_text(&content)
            }
            // event_msg user_message records carry the literal text the user
            // typed, without Codex's XML wrappers (e.g. <user_action>).
            Some("user_message") => payload
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            _ => continue,
        };
        let trimmed = text.trim();
        if !trimmed.is_empty() && !is_codex_noise(trimmed) && !trimmed.starts_with("<user_action") {
            return trimmed.chars().take(300).collect();
        }
    }
    String::new()
}

pub fn load_codex_sessions() -> Vec<Session> {
    let base = codex_sessions_dir();
    if !base.exists() {
        return Vec::new();
    }
    let mut sessions = Vec::new();
    for path in glob::glob(&format!("{}/**/rollout-*.jsonl", base.display()))
        .into_iter()
        .flatten()
        .flatten()
    {
        let Some(meta) = read_codex_meta(&path) else {
            continue;
        };
        if meta.is_subagent {
            continue;
        }
        let modified = mtime_iso(&path).unwrap_or_default();
        let date = meta
            .created
            .get(..10)
            .map(str::to_string)
            .unwrap_or_else(|| mtime_date(&path).unwrap_or_default());
        let first = codex_first_prompt(&path);
        sessions.push(Session {
            source: "codex".into(),
            id: meta.id,
            summary: String::new(),
            first_prompt: first,
            created: meta.created,
            modified,
            date,
            messages: 0,
            branch: meta.branch,
            project: meta.cwd,
            file: path.to_string_lossy().to_string(),
            is_sidechain: false,
        });
    }
    sessions
}

pub fn load_all_sessions() -> Vec<Session> {
    let mut all = load_claude_sessions();
    all.extend(load_cursor_sessions());
    all.extend(load_codex_sessions());
    all
}

pub fn parse_claude_jsonl(
    filepath: &str,
    extract_meta: bool,
) -> (Vec<Message>, Option<SessionMeta>) {
    let file = match fs::File::open(filepath) {
        Ok(f) => f,
        Err(_) => {
            return (
                Vec::new(),
                if extract_meta {
                    Some(SessionMeta::default())
                } else {
                    None
                },
            );
        }
    };
    let reader = BufReader::new(file);
    let mut messages = Vec::new();
    let mut meta = SessionMeta::default();
    let mut skip_next_assistant = false;
    // One API response is stored as one JSONL record per content block, each
    // repeating the same message id and usage — count usage once per id.
    let mut counted_usage_ids: HashSet<String> = HashSet::new();
    let mut user_texts = Vec::new();
    let mut total_chars: usize = 0;
    let max_chars: usize = 4 * 1024 * 1024;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let trimmed = line.trim().to_string();
        if trimmed.is_empty() {
            continue;
        }
        let entry: Value = match serde_json::from_str(&trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let etype = entry.get("type").and_then(Value::as_str).unwrap_or("");

        if etype == "summary" && extract_meta && meta.summary.is_none() {
            meta.summary = entry
                .get("summary")
                .and_then(Value::as_str)
                .map(String::from);
            continue;
        }
        if etype == "ai-title" && extract_meta {
            if let Some(title) = entry.get("aiTitle").and_then(Value::as_str) {
                let title = title.trim();
                if !title.is_empty() {
                    meta.summary = Some(title.to_string());
                }
            }
            continue;
        }
        if (etype == "custom-title" || etype == "custom_title") && extract_meta {
            let title = entry
                .get("customTitle")
                .or_else(|| entry.get("custom_title"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            meta.custom_title = if title.is_empty() { None } else { Some(title) };
            continue;
        }

        if etype != "user" && etype != "assistant" {
            continue;
        }

        let msg_obj = entry
            .get("message")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));
        let content_raw = msg_obj
            .get("content")
            .cloned()
            .unwrap_or(Value::String(String::new()));
        let text = extract_text(&content_raw);

        if etype == "user" {
            user_texts.push(text.clone());
            if is_warmup_message(&text) {
                skip_next_assistant = true;
                continue;
            }
            if is_clear_metadata(&text) {
                continue;
            }
        }

        if etype == "assistant" {
            if extract_meta
                && meta.model.is_none()
                && let Some(m) = msg_obj.get("model").and_then(Value::as_str)
            {
                meta.model = Some(m.to_string());
            }
            let msg_id = msg_obj.get("id").and_then(Value::as_str).unwrap_or("");
            if extract_meta
                && let Some(usage) = msg_obj.get("usage")
                && (msg_id.is_empty() || counted_usage_ids.insert(msg_id.to_string()))
            {
                let tok = usage
                    .get("input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    + usage
                        .get("output_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0)
                    + usage
                        .get("cache_creation_input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0)
                    + usage
                        .get("cache_read_input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                meta.total_tokens += tok;
            }
            if skip_next_assistant {
                skip_next_assistant = false;
                continue;
            }
        }

        let ctx = crate::parser::extract_context(&content_raw);

        if total_chars < max_chars {
            let role = msg_obj
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or(etype)
                .to_string();
            let uuid = entry
                .get("uuid")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let timestamp = entry
                .get("timestamp")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let session_id = entry
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let cwd = entry
                .get("cwd")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            total_chars += text.len();
            messages.push(Message {
                uuid,
                timestamp,
                role,
                content: text,
                session_id,
                project_path: cwd,
                tool_uses: ctx.tools,
                files_referenced: ctx.files,
                error_patterns: ctx.errors,
                relevance_score: 0.0,
                final_score: 0.0,
            });
        }
    }

    if extract_meta {
        if crate::parser::is_clear_only_conversation(&user_texts) {
            return (Vec::new(), Some(meta));
        }
        return (messages, Some(meta));
    }
    (messages, None)
}

// Commentary-phase assistant messages are dropped only for turns that reach a
// final_answer; interrupted turns have no other assistant text, so their
// commentary is kept. A turn ends at the next user record (real or injected).
fn codex_commentary_to_skip(entries: &[Value]) -> HashSet<usize> {
    let mut skip = HashSet::new();
    let mut open_commentary: Vec<usize> = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        let payload = codex_payload(entry);
        if payload.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        match payload.get("role").and_then(Value::as_str) {
            Some("user") => open_commentary.clear(),
            Some("assistant") => match payload.get("phase").and_then(Value::as_str) {
                Some("commentary") => open_commentary.push(i),
                Some("final_answer") => skip.extend(open_commentary.drain(..)),
                _ => {}
            },
            _ => {}
        }
    }
    skip
}

pub fn parse_codex_jsonl(filepath: &str) -> Vec<Message> {
    // Whole-file read + lossy decode: an unreadable path fails once up front
    // (io::Lines can yield Err forever), and an invalid UTF-8 line only
    // corrupts itself instead of truncating the rest of the transcript.
    let Ok(bytes) = fs::read(filepath) else {
        return Vec::new();
    };
    let entries: Vec<Value> = String::from_utf8_lossy(&bytes)
        .lines()
        .filter_map(|l| serde_json::from_str(l.trim()).ok())
        .collect();
    let skip_commentary = codex_commentary_to_skip(&entries);

    let mut messages: Vec<Message> = Vec::new();
    let mut session_id = String::new();
    let mut project_path = String::new();
    // Tool calls made before the assistant says anything; attached to the
    // next assistant message so they aren't lost.
    let mut pending_tools: Vec<String> = Vec::new();
    // Set while a skipped commentary message is the turn's latest assistant
    // output: tool calls must wait for the turn's final answer instead of
    // attaching to the previous turn's message.
    let mut route_tools_forward = false;
    let mut total_chars: usize = 0;
    let max_chars: usize = 4 * 1024 * 1024;

    for (i, entry) in entries.iter().enumerate() {
        // session_meta carries id/cwd; legacy files put them flat on line 1.
        let etype = entry.get("type").and_then(Value::as_str);
        if etype == Some("session_meta") || (etype.is_none() && entry.get("id").is_some()) {
            let p = codex_payload(entry);
            session_id = p
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            project_path = p
                .get("cwd")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            continue;
        }
        let timestamp = entry
            .get("timestamp")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let payload = codex_payload(entry);
        match payload.get("type").and_then(Value::as_str) {
            Some("message") => {
                let role = payload.get("role").and_then(Value::as_str).unwrap_or("");
                if role != "user" && role != "assistant" {
                    continue;
                }
                if skip_commentary.contains(&i) {
                    route_tools_forward = true;
                    continue;
                }
                let content_raw = payload
                    .get("content")
                    .cloned()
                    .unwrap_or(Value::String(String::new()));
                let text = extract_text(&content_raw);
                if text.trim().is_empty() || (role == "user" && is_codex_noise(&text)) {
                    continue;
                }
                let ctx = crate::parser::extract_context(&content_raw);
                if total_chars < max_chars {
                    total_chars += text.len();
                    let mut tool_uses = ctx.tools;
                    if role == "assistant" {
                        tool_uses.append(&mut pending_tools);
                        route_tools_forward = false;
                    }
                    messages.push(Message {
                        uuid: String::new(),
                        timestamp,
                        role: role.to_string(),
                        content: text,
                        session_id: session_id.clone(),
                        project_path: project_path.clone(),
                        tool_uses,
                        files_referenced: ctx.files,
                        error_patterns: ctx.errors,
                        relevance_score: 0.0,
                        final_score: 0.0,
                    });
                }
            }
            // Tool calls are separate records in Codex rollouts; attach the
            // tool name to the assistant message that initiated it, or hold
            // it for the next assistant message if none exists yet.
            Some("function_call") => {
                if let Some(name) = payload.get("name").and_then(Value::as_str) {
                    match messages.last_mut() {
                        Some(last) if last.role == "assistant" && !route_tools_forward => {
                            last.tool_uses.push(name.to_string());
                        }
                        _ => pending_tools.push(name.to_string()),
                    }
                }
            }
            _ => {}
        }
    }
    messages
}

pub fn parse_cursor_jsonl(filepath: &str) -> Vec<Message> {
    let file = match fs::File::open(filepath) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let reader = BufReader::new(file);
    let mut messages = Vec::new();
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let trimmed = line.trim().to_string();
        if trimmed.is_empty() {
            continue;
        }
        let entry: Value = match serde_json::from_str(&trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let role = entry
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let msg_obj = entry
            .get("message")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));
        let content_raw = msg_obj
            .get("content")
            .cloned()
            .unwrap_or(Value::String(String::new()));
        let text = extract_text(&content_raw);
        let ctx = crate::parser::extract_context(&content_raw);
        if !role.is_empty() && !text.trim().is_empty() {
            messages.push(Message {
                uuid: String::new(),
                timestamp: String::new(),
                role,
                content: text,
                session_id: String::new(),
                project_path: String::new(),
                tool_uses: ctx.tools,
                files_referenced: ctx.files,
                error_patterns: ctx.errors,
                relevance_score: 0.0,
                final_score: 0.0,
            });
        }
    }
    messages
}

pub fn parse_cursor_txt(filepath: &str) -> Vec<Message> {
    let content = match fs::read_to_string(filepath) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut messages = Vec::new();
    let mut current_role: Option<String> = None;
    let mut current_lines = Vec::new();

    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("user:") {
            if let Some(role) = current_role.take() {
                let text = current_lines.join("\n").trim().to_string();
                if !text.is_empty() {
                    messages.push(Message {
                        uuid: String::new(),
                        timestamp: String::new(),
                        role,
                        content: text,
                        session_id: String::new(),
                        project_path: String::new(),
                        tool_uses: Vec::new(),
                        files_referenced: Vec::new(),
                        error_patterns: Vec::new(),
                        relevance_score: 0.0,
                        final_score: 0.0,
                    });
                }
            }
            current_role = Some("user".into());
            current_lines = vec![rest.trim().to_string()];
        } else if let Some(rest) = line.strip_prefix("assistant:") {
            if let Some(role) = current_role.take() {
                let text = current_lines.join("\n").trim().to_string();
                if !text.is_empty() {
                    messages.push(Message {
                        uuid: String::new(),
                        timestamp: String::new(),
                        role,
                        content: text,
                        session_id: String::new(),
                        project_path: String::new(),
                        tool_uses: Vec::new(),
                        files_referenced: Vec::new(),
                        error_patterns: Vec::new(),
                        relevance_score: 0.0,
                        final_score: 0.0,
                    });
                }
            }
            current_role = Some("assistant".into());
            current_lines = vec![rest.trim().to_string()];
        } else {
            current_lines.push(line.to_string());
        }
    }
    if let Some(role) = current_role {
        let text = current_lines.join("\n").trim().to_string();
        if !text.is_empty() {
            messages.push(Message {
                uuid: String::new(),
                timestamp: String::new(),
                role,
                content: text,
                session_id: String::new(),
                project_path: String::new(),
                tool_uses: Vec::new(),
                files_referenced: Vec::new(),
                error_patterns: Vec::new(),
                relevance_score: 0.0,
                final_score: 0.0,
            });
        }
    }
    messages
}

pub fn parse_session(session: &Session, extract_meta: bool) -> (Vec<Message>, Option<SessionMeta>) {
    if session.source == "claude" {
        return parse_claude_jsonl(&session.file, extract_meta);
    }
    if session.source == "codex" {
        let messages = parse_codex_jsonl(&session.file);
        if extract_meta {
            return (messages, Some(SessionMeta::default()));
        }
        return (messages, None);
    }
    let messages = if session.file.ends_with(".jsonl") {
        parse_cursor_jsonl(&session.file)
    } else {
        parse_cursor_txt(&session.file)
    };
    if extract_meta {
        (
            messages,
            Some(SessionMeta {
                summary: if session.summary.is_empty() {
                    None
                } else {
                    Some(session.summary.clone())
                },
                custom_title: None,
                model: None,
                total_tokens: 0,
            }),
        )
    } else {
        (messages, None)
    }
}

pub fn filter_sessions(
    sessions: &[Session],
    from_date: Option<NaiveDate>,
    to_date: Option<NaiveDate>,
    keyword: Option<&str>,
    source: Option<&str>,
    project: Option<&str>,
    branch: Option<&str>,
) -> Vec<Session> {
    let project_norm = project.map(normalize_for_project_match);
    let mut out: Vec<Session> = sessions
        .iter()
        .filter(|s| {
            // Only require a parseable date when a date filter is in play;
            // a session with a missing date must still list otherwise.
            if from_date.is_some() || to_date.is_some() {
                let d = match NaiveDate::parse_from_str(&s.date, "%Y-%m-%d") {
                    Ok(d) => d,
                    Err(_) => return false,
                };
                if let Some(fd) = from_date
                    && d < fd
                {
                    return false;
                }
                if let Some(td) = to_date
                    && d > td
                {
                    return false;
                }
            }
            if let Some(src) = source
                && s.source != src
            {
                return false;
            }
            if let Some(proj_norm) = project_norm.as_ref()
                && !normalize_for_project_match(&s.project).contains(proj_norm)
            {
                return false;
            }
            if let Some(br) = branch
                && !s.branch.to_lowercase().contains(&br.to_lowercase())
            {
                return false;
            }
            if let Some(kw) = keyword {
                let kw_lower = kw.to_lowercase();
                let haystack = format!(
                    "{} {} {} {}",
                    s.summary, s.first_prompt, s.branch, s.project
                )
                .to_lowercase();
                if !haystack.contains(&kw_lower) {
                    return false;
                }
            }
            true
        })
        .cloned()
        .collect();
    out.sort_by_key(|s| std::cmp::Reverse(recency_key(s)));
    out
}

/// Recency ordering for "newest first" sorts and `--last`: parsed timestamp
/// when possible (raw string comparison misorders mixed UTC offsets), raw
/// string as a tiebreak/fallback for unparseable values.
pub fn recency_key(s: &Session) -> (Option<DateTime<FixedOffset>>, String) {
    let ts = if s.modified.is_empty() {
        &s.created
    } else {
        &s.modified
    };
    (parse_any_timestamp(ts), ts.clone())
}

pub enum SessionLookup<'a> {
    Found(&'a Session),
    /// Multiple sessions share the prefix — never silently pick one.
    Ambiguous(Vec<&'a Session>),
    NotFound,
}

pub fn lookup_session<'a>(sessions: &'a [Session], sid: &str) -> SessionLookup<'a> {
    if let Some(s) = sessions.iter().find(|s| s.id.eq_ignore_ascii_case(sid)) {
        return SessionLookup::Found(s);
    }
    let matches: Vec<&Session> = sessions
        .iter()
        .filter(|s| {
            s.id.get(..sid.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(sid))
        })
        .collect();
    match matches.len() {
        0 => SessionLookup::NotFound,
        1 => SessionLookup::Found(matches[0]),
        _ => SessionLookup::Ambiguous(matches),
    }
}

/// Exact id or *unique* prefix. Ambiguous prefixes resolve to None; callers
/// that can report candidates should use [`lookup_session`] instead.
pub fn find_session<'a>(sessions: &'a [Session], sid: &str) -> Option<&'a Session> {
    match lookup_session(sessions, sid) {
        SessionLookup::Found(s) => Some(s),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_session(
        id: &str,
        date: &str,
        source: &str,
        project: &str,
        branch: &str,
        summary: &str,
    ) -> Session {
        Session {
            source: source.into(),
            id: id.into(),
            summary: summary.into(),
            first_prompt: String::new(),
            created: format!("{date}T00:00:00"),
            modified: String::new(),
            date: date.into(),
            messages: 0,
            branch: branch.into(),
            project: project.into(),
            file: String::new(),
            is_sidechain: false,
        }
    }

    #[test]
    fn encode_path_basic() {
        let path = std::path::Path::new("/Users/ayush/Documents/project");
        assert_eq!(
            encode_path_for_claude(path),
            "-Users-ayush-Documents-project"
        );
    }

    #[test]
    fn encode_path_root() {
        assert_eq!(encode_path_for_claude(std::path::Path::new("/")), "-");
    }

    #[test]
    fn encode_path_replaces_all_non_alphanumerics() {
        // Claude Code replaces every non-alphanumeric character with '-',
        // not just '/'; dots and underscores must match its encoding or
        // resume copies the session where Claude Code never looks.
        let path = std::path::Path::new("/Users/x/my_app.v2");
        assert_eq!(encode_path_for_claude(path), "-Users-x-my-app-v2");
    }

    #[test]
    fn encode_path_replaces_non_ascii() {
        // Claude Code keeps only ASCII alphanumerics (claude-code#19972):
        // CJK/accented characters become '-' too.
        let path = std::path::Path::new("/Users/x/café");
        assert_eq!(encode_path_for_claude(path), "-Users-x-caf-");
        let cjk = std::path::Path::new("/Users/x/研究");
        assert_eq!(encode_path_for_claude(cjk), "-Users-x---");
    }

    #[test]
    fn lookup_session_reports_ambiguous_prefix() {
        let sessions = vec![
            make_session("abc-123", "2025-01-01", "claude", "/proj", "main", "one"),
            make_session("abc-456", "2025-01-02", "cursor", "/proj", "main", "two"),
        ];
        match lookup_session(&sessions, "abc") {
            SessionLookup::Ambiguous(candidates) => assert_eq!(candidates.len(), 2),
            _ => panic!("expected ambiguous lookup"),
        }
        // find_session must not silently pick one of them.
        assert!(find_session(&sessions, "abc").is_none());
        // A unique longer prefix still resolves.
        assert!(matches!(
            lookup_session(&sessions, "abc-1"),
            SessionLookup::Found(s) if s.id == "abc-123"
        ));
    }

    #[test]
    fn find_session_exact_match() {
        let sessions = vec![make_session(
            "abc-123",
            "2025-01-01",
            "claude",
            "/proj",
            "main",
            "test",
        )];
        assert!(find_session(&sessions, "abc-123").is_some());
    }

    #[test]
    fn find_session_prefix_match() {
        let sessions = vec![make_session(
            "abc-123-def-456",
            "2025-01-01",
            "claude",
            "/proj",
            "main",
            "test",
        )];
        let found = find_session(&sessions, "abc-123");
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, "abc-123-def-456");
    }

    #[test]
    fn find_session_not_found() {
        let sessions = vec![make_session(
            "abc-123",
            "2025-01-01",
            "claude",
            "/proj",
            "main",
            "test",
        )];
        assert!(find_session(&sessions, "xyz-999").is_none());
    }

    #[test]
    fn filter_by_source() {
        let sessions = vec![
            make_session("1", "2025-01-01", "claude", "/proj", "", "s1"),
            make_session("2", "2025-01-01", "cursor", "/proj", "", "s2"),
        ];
        let filtered = filter_sessions(&sessions, None, None, None, Some("claude"), None, None);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].source, "claude");
    }

    #[test]
    fn filter_by_project() {
        let sessions = vec![
            make_session("1", "2025-01-01", "claude", "chat-history", "", "s1"),
            make_session("2", "2025-01-01", "claude", "other-project", "", "s2"),
        ];
        let filtered = filter_sessions(&sessions, None, None, None, None, Some("chat"), None);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].project, "chat-history");
    }

    #[test]
    fn filter_by_project_underscore_dash_mismatch() {
        let sessions = vec![make_session(
            "1",
            "2025-01-01",
            "cursor",
            "proj-one-two-123",
            "",
            "",
        )];
        let filtered = filter_sessions(
            &sessions,
            None,
            None,
            None,
            None,
            Some("proj_one_two_123"),
            None,
        );
        assert_eq!(
            filtered.len(),
            1,
            "underscore in filter should match dashes in project path"
        );
    }

    #[test]
    fn filter_by_branch() {
        let sessions = vec![
            make_session("1", "2025-01-01", "claude", "/proj", "main", "s1"),
            make_session("2", "2025-01-01", "claude", "/proj", "feature-x", "s2"),
        ];
        let filtered = filter_sessions(&sessions, None, None, None, None, None, Some("feature"));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].branch, "feature-x");
    }

    #[test]
    fn filter_by_keyword() {
        let sessions = vec![
            make_session("1", "2025-01-01", "claude", "/proj", "", "implement auth"),
            make_session("2", "2025-01-01", "claude", "/proj", "", "fix docker build"),
        ];
        let filtered = filter_sessions(&sessions, None, None, Some("docker"), None, None, None);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].summary, "fix docker build");
    }

    #[test]
    fn filter_by_date_range() {
        let sessions = vec![
            make_session("1", "2025-01-01", "claude", "/proj", "", "old"),
            make_session("2", "2025-06-15", "claude", "/proj", "", "new"),
        ];
        let from = NaiveDate::from_ymd_opt(2025, 6, 1).unwrap();
        let filtered = filter_sessions(&sessions, Some(from), None, None, None, None, None);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].summary, "new");
    }

    #[test]
    fn filter_by_to_date() {
        let sessions = vec![
            make_session("1", "2025-01-01", "claude", "/proj", "", "old"),
            make_session("2", "2025-06-15", "claude", "/proj", "", "new"),
        ];
        let to = NaiveDate::from_ymd_opt(2025, 3, 1).unwrap();
        let filtered = filter_sessions(&sessions, None, Some(to), None, None, None, None);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].summary, "old");
    }

    #[test]
    fn find_session_is_case_insensitive() {
        let sessions = vec![make_session(
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            "2025-01-15",
            "claude",
            "/p",
            "",
            "s",
        )];
        assert!(find_session(&sessions, "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE").is_some());
        assert!(find_session(&sessions, "AAAAAAAA").is_some(), "prefix");
        assert!(find_session(&sessions, "zzzz").is_none());
    }

    #[test]
    fn filter_sorts_by_parsed_time_not_string() {
        // 16:00+05:30 is 10:30Z — chronologically EARLIER than 12:00Z but
        // lexicographically greater as a string.
        let mut offset = make_session("1", "2026-07-15", "claude", "/p", "", "offset");
        offset.modified = "2026-07-15T16:00:00+05:30".into();
        let mut zulu = make_session("2", "2026-07-15", "claude", "/p", "", "zulu");
        zulu.modified = "2026-07-15T12:00:00Z".into();
        let out = filter_sessions(&[offset, zulu], None, None, None, None, None, None);
        assert_eq!(out[0].summary, "zulu");
    }

    #[test]
    fn filter_keeps_dateless_sessions_when_no_date_filter() {
        // A missing/unparseable date should only matter when the user is
        // actually filtering by date — otherwise the session must still list.
        let sessions = vec![make_session("1", "", "claude", "/proj", "", "no date")];
        let filtered = filter_sessions(&sessions, None, None, None, None, None, None);
        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn filter_excludes_dateless_sessions_from_date_range() {
        let sessions = vec![make_session("1", "", "claude", "/proj", "", "no date")];
        let from = NaiveDate::from_ymd_opt(2025, 1, 1).unwrap();
        let filtered = filter_sessions(&sessions, Some(from), None, None, None, None, None);
        assert!(filtered.is_empty());
    }

    #[test]
    fn filter_sorted_by_recency() {
        let sessions = vec![
            make_session("1", "2025-01-01", "claude", "/proj", "", "old"),
            make_session("2", "2025-06-15", "claude", "/proj", "", "new"),
            make_session("3", "2025-03-10", "claude", "/proj", "", "mid"),
        ];
        let filtered = filter_sessions(&sessions, None, None, None, None, None, None);
        assert_eq!(filtered[0].summary, "new");
        assert_eq!(filtered[1].summary, "mid");
        assert_eq!(filtered[2].summary, "old");
    }

    #[test]
    fn filter_keyword_case_insensitive() {
        let sessions = vec![make_session(
            "1",
            "2025-01-01",
            "claude",
            "/proj",
            "",
            "Docker Build",
        )];
        let filtered = filter_sessions(&sessions, None, None, Some("docker"), None, None, None);
        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn filter_combined() {
        let sessions = vec![
            make_session(
                "1",
                "2025-06-15",
                "claude",
                "chat-history",
                "main",
                "fix auth",
            ),
            make_session(
                "2",
                "2025-06-15",
                "cursor",
                "chat-history",
                "main",
                "fix docker",
            ),
            make_session(
                "3",
                "2025-06-15",
                "claude",
                "other-proj",
                "main",
                "fix auth",
            ),
        ];
        let filtered = filter_sessions(
            &sessions,
            None,
            None,
            None,
            Some("claude"),
            Some("chat"),
            None,
        );
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, "1");
    }

    #[test]
    fn parse_claude_jsonl_with_fixture() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let data = r#"{"type":"user","message":{"role":"user","content":"hello world"},"timestamp":"2025-01-01T00:00:00Z","uuid":"u1"}
{"type":"assistant","message":{"role":"assistant","content":"Hi there! How can I help you today?"},"timestamp":"2025-01-01T00:01:00Z","uuid":"u2"}"#;
        std::fs::write(tmp.path(), data).unwrap();
        let (messages, _) = parse_claude_jsonl(tmp.path().to_str().unwrap(), false);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, "hello world");
        assert_eq!(messages[1].role, "assistant");
    }

    #[test]
    fn parse_claude_jsonl_skips_warmup() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let data = r#"{"type":"user","message":{"role":"user","content":"warmup"},"timestamp":"2025-01-01T00:00:00Z","uuid":"u1"}
{"type":"assistant","message":{"role":"assistant","content":"warmed up"},"timestamp":"2025-01-01T00:01:00Z","uuid":"u2"}
{"type":"user","message":{"role":"user","content":"real question here"},"timestamp":"2025-01-01T00:02:00Z","uuid":"u3"}"#;
        std::fs::write(tmp.path(), data).unwrap();
        let (messages, _) = parse_claude_jsonl(tmp.path().to_str().unwrap(), false);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "real question here");
    }

    #[test]
    fn parse_claude_jsonl_extracts_meta() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let data = r#"{"type":"summary","summary":"Test session summary"}
{"type":"user","message":{"role":"user","content":"test prompt"},"timestamp":"2025-01-01T00:00:00Z","uuid":"u1"}
{"type":"assistant","message":{"role":"assistant","content":"response","model":"claude-3-opus","usage":{"input_tokens":100,"output_tokens":50}},"timestamp":"2025-01-01T00:01:00Z","uuid":"u2"}"#;
        std::fs::write(tmp.path(), data).unwrap();
        let (messages, meta) = parse_claude_jsonl(tmp.path().to_str().unwrap(), true);
        assert_eq!(messages.len(), 2);
        let meta = meta.unwrap();
        assert_eq!(meta.summary.as_deref(), Some("Test session summary"));
        assert_eq!(meta.model.as_deref(), Some("claude-3-opus"));
        assert_eq!(meta.total_tokens, 150);
    }

    #[test]
    fn parse_claude_jsonl_nonexistent_file() {
        let (messages, _) = parse_claude_jsonl("/nonexistent/path.jsonl", false);
        assert!(messages.is_empty());
    }

    #[test]
    fn parse_cursor_txt_basic() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let data = "title line\nuser: hello world\nassistant: hi there\nuser: another question\nassistant: another answer";
        std::fs::write(tmp.path(), data).unwrap();
        let messages = parse_cursor_txt(tmp.path().to_str().unwrap());
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, "user");
        assert!(messages[0].content.contains("hello world"));
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[2].role, "user");
        assert_eq!(messages[3].role, "assistant");
    }

    const CODEX_FIXTURE: &str = r#"{"timestamp":"2026-06-10T10:00:00.000Z","type":"session_meta","payload":{"id":"019e0000-aaaa-bbbb-cccc-000000000001","timestamp":"2026-06-10T10:00:00.000Z","cwd":"/Users/test/myproj","originator":"codex-tui","cli_version":"0.136.0","git":{"commit_hash":"abc123","branch":"feature-codex"}}}
{"timestamp":"2026-06-10T10:00:01.000Z","type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<permissions instructions>\nsandbox stuff"}]}}
{"timestamp":"2026-06-10T10:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\n<cwd>/Users/test/myproj</cwd>\n</environment_context>"}]}}
{"timestamp":"2026-06-10T10:00:02.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"add connection pooling to the database layer"}]}}
{"timestamp":"2026-06-10T10:00:05.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"I'll add connection pooling using a bounded pool."}],"phase":"final_answer"}}
{"timestamp":"2026-06-10T10:00:06.000Z","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":[\"ls\"]}","call_id":"call_1"}}
{"timestamp":"2026-06-10T10:00:07.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_1","output":"src tests"}}
{"timestamp":"2026-06-10T10:00:09.000Z","type":"event_msg","payload":{"type":"token_count","info":{}}}"#;

    #[test]
    fn parse_codex_jsonl_with_fixture() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), CODEX_FIXTURE).unwrap();
        let messages = parse_codex_jsonl(tmp.path().to_str().unwrap());
        assert_eq!(
            messages.len(),
            2,
            "developer and environment_context messages should be filtered"
        );
        assert_eq!(messages[0].role, "user");
        assert_eq!(
            messages[0].content,
            "add connection pooling to the database layer"
        );
        assert_eq!(
            messages[0].session_id,
            "019e0000-aaaa-bbbb-cccc-000000000001"
        );
        assert_eq!(messages[0].project_path, "/Users/test/myproj");
        assert_eq!(messages[0].timestamp, "2026-06-10T10:00:02.000Z");
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(
            messages[1].tool_uses,
            vec!["shell".to_string()],
            "function_call tool name should attach to preceding assistant message"
        );
    }

    #[test]
    fn parse_codex_jsonl_legacy_flat_records() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let data = r#"{"id":"legacy-id","timestamp":"2025-08-01T10:00:00.000Z","instructions":null}
{"type":"message","role":"user","content":[{"type":"input_text","text":"legacy format question"}]}
{"type":"message","role":"assistant","content":[{"type":"output_text","text":"legacy format answer"}]}"#;
        std::fs::write(tmp.path(), data).unwrap();
        let messages = parse_codex_jsonl(tmp.path().to_str().unwrap());
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "legacy format question");
        assert_eq!(messages[1].content, "legacy format answer");
        assert_eq!(
            messages[0].session_id, "legacy-id",
            "session id should be read from the legacy flat meta line"
        );
    }

    #[test]
    fn parse_codex_jsonl_attaches_leading_tool_calls_to_next_assistant() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let data = r#"{"timestamp":"2026-06-10T10:00:00.000Z","type":"session_meta","payload":{"id":"s1","timestamp":"2026-06-10T10:00:00.000Z","cwd":"/p"}}
{"timestamp":"2026-06-10T10:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"run the tests"}]}}
{"timestamp":"2026-06-10T10:00:02.000Z","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{}","call_id":"c1"}}
{"timestamp":"2026-06-10T10:00:03.000Z","type":"response_item","payload":{"type":"function_call","name":"read_file","arguments":"{}","call_id":"c2"}}
{"timestamp":"2026-06-10T10:00:04.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"All tests pass."}]}}"#;
        std::fs::write(tmp.path(), data).unwrap();
        let messages = parse_codex_jsonl(tmp.path().to_str().unwrap());
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[1].tool_uses,
            vec!["shell".to_string(), "read_file".to_string()],
            "tool calls made before any assistant text should attach to the next assistant message"
        );
    }

    #[test]
    fn is_codex_noise_covers_injected_wrappers() {
        for noise in [
            "<environment_context>\n<cwd>/p</cwd>",
            "<permissions instructions>\nsandbox",
            "<user_instructions>do x</user_instructions>",
            "<turn_aborted>user interrupted</turn_aborted>",
            "<goal_context>...</goal_context>",
            "<subagent_notification>done</subagent_notification>",
        ] {
            assert!(is_codex_noise(noise), "should filter: {noise}");
        }
        assert!(!is_codex_noise("fix the <div> rendering bug"));
        assert!(!is_codex_noise("plain user prompt"));
    }

    #[test]
    fn parse_codex_jsonl_nonexistent_file() {
        assert!(parse_codex_jsonl("/nonexistent/rollout.jsonl").is_empty());
    }

    #[test]
    fn cursor_first_prompt_recovers_query_after_long_preamble() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let filler = "x".repeat(500);
        let data = format!(
            concat!(
                r#"{{"role":"user","message":{{"content":[{{"type":"text","text":"<manually_attached_skills>\n{}\n</manually_attached_skills>\n<timestamp>Tuesday, Jul 21, 2026</timestamp>\n<user_query>\nreview security compliance\n</user_query>"}}]}}}}"#,
                "\n",
                r#"{{"role":"assistant","message":{{"content":[{{"type":"text","text":"On it."}}]}}}}"#
            ),
            filler
        );
        std::fs::write(tmp.path(), data).unwrap();
        assert_eq!(
            cursor_first_prompt_jsonl(tmp.path()),
            "review security compliance"
        );
    }

    #[test]
    fn cursor_first_prompt_skips_assistant_lines() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let data = concat!(
            r#"{"role":"assistant","message":{"content":[{"type":"text","text":"Thinking about the plan."}]}}"#,
            "\n",
            r#"{"role":"user","message":{"content":[{"type":"text","text":"fix the login bug"}]}}"#
        );
        std::fs::write(tmp.path(), data).unwrap();
        assert_eq!(cursor_first_prompt_jsonl(tmp.path()), "fix the login bug");
    }

    #[test]
    fn codex_first_prompt_skips_noise() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), CODEX_FIXTURE).unwrap();
        assert_eq!(
            codex_first_prompt(tmp.path()),
            "add connection pooling to the database layer"
        );
    }

    #[test]
    fn codex_first_prompt_prefers_typed_user_message_over_user_action_wrapper() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let data = r#"{"timestamp":"2026-06-10T10:00:00.000Z","type":"session_meta","payload":{"id":"s1","timestamp":"2026-06-10T10:00:00.000Z","cwd":"/p"}}
{"timestamp":"2026-06-10T10:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<user_action>\n<context>User initiated a review task.</context>\n</user_action>"}]}}
{"timestamp":"2026-06-10T10:00:02.000Z","type":"event_msg","payload":{"type":"user_message","message":"https://github.com/org/repo/pull/31","images":null}}"#;
        std::fs::write(tmp.path(), data).unwrap();
        assert_eq!(
            codex_first_prompt(tmp.path()),
            "https://github.com/org/repo/pull/31"
        );
    }

    #[test]
    fn read_codex_meta_basic() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), CODEX_FIXTURE).unwrap();
        let meta = read_codex_meta(tmp.path()).unwrap();
        assert_eq!(meta.id, "019e0000-aaaa-bbbb-cccc-000000000001");
        assert_eq!(meta.created, "2026-06-10T10:00:00.000Z");
        assert_eq!(meta.cwd, "/Users/test/myproj");
        assert_eq!(meta.branch, "feature-codex");
        assert!(!meta.is_subagent);
    }

    #[test]
    fn read_codex_meta_detects_subagent() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let data = r#"{"timestamp":"2026-06-10T10:00:00.000Z","type":"session_meta","payload":{"id":"sub-1","timestamp":"2026-06-10T10:00:00.000Z","cwd":"/p","source":{"subagent":{"parent_thread_id":"parent-1","depth":1}}}}"#;
        std::fs::write(tmp.path(), data).unwrap();
        assert!(read_codex_meta(tmp.path()).unwrap().is_subagent);
    }

    #[test]
    fn copy_session_to_dir_creates_target() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src_file = tmp.path().join("test.jsonl");
        std::fs::write(&src_file, "test data").unwrap();

        let session = Session {
            source: "claude".into(),
            id: "test-id".into(),
            summary: String::new(),
            first_prompt: String::new(),
            created: String::new(),
            modified: String::new(),
            date: String::new(),
            messages: 0,
            branch: String::new(),
            project: String::new(),
            file: src_file.to_string_lossy().to_string(),
            is_sidechain: false,
        };

        let target = tmp.path().join("target-dir");
        copy_session_to_dir(&session, &target).unwrap();
        assert!(target.join("test.jsonl").exists());
        assert_eq!(
            std::fs::read_to_string(target.join("test.jsonl")).unwrap(),
            "test data"
        );
    }

    #[test]
    fn read_cwd_branch_from_jsonl_extracts_cwd_and_branch() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let data = r#"{"type":"user","cwd":"/Users/test/project","gitBranch":"feature-x","message":{"content":"hello"}}"#;
        std::fs::write(tmp.path(), data).unwrap();
        assert_eq!(
            read_cwd_branch_from_jsonl(tmp.path()),
            (
                Some("/Users/test/project".to_string()),
                Some("feature-x".to_string())
            )
        );
    }

    #[test]
    fn read_cwd_branch_from_jsonl_no_fields() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let data = r#"{"type":"user","message":{"content":"hello"}}"#;
        std::fs::write(tmp.path(), data).unwrap();
        assert_eq!(read_cwd_branch_from_jsonl(tmp.path()), (None, None));
    }

    #[test]
    fn claude_ai_title_reads_last_title_from_tail() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut lines =
            vec![r#"{"type":"ai-title","aiTitle":"First title","sessionId":"abc"}"#.to_string()];
        lines.extend(std::iter::repeat_n(
            r#"{"type":"assistant","message":{"role":"assistant","content":"x"}}"#.to_string(),
            50,
        ));
        lines.push(r#"{"type":"ai-title","aiTitle":"Final title","sessionId":"abc"}"#.to_string());
        std::fs::write(tmp.path(), lines.join("\n")).unwrap();
        assert_eq!(claude_ai_title(tmp.path()), Some("Final title".to_string()));
    }

    #[test]
    fn claude_ai_title_custom_title_beats_later_ai_title() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let data = [
            r#"{"type":"ai-title","aiTitle":"Auto title","sessionId":"abc"}"#,
            r#"{"type":"custom-title","customTitle":"Manual title","sessionId":"abc"}"#,
            r#"{"type":"ai-title","aiTitle":"Newer auto title","sessionId":"abc"}"#,
        ]
        .join("\n");
        std::fs::write(tmp.path(), data).unwrap();
        assert_eq!(
            claude_ai_title(tmp.path()),
            Some("Manual title".to_string())
        );
    }

    // The tail window is a deliberate tradeoff: titles more than 64KB before
    // EOF are not found during listing. These two tests pin both sides of it.
    #[test]
    fn claude_ai_title_finds_title_within_64k_tail_of_large_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let filler = format!(
            r#"{{"type":"assistant","message":{{"role":"assistant","content":"{}"}}}}"#,
            "x".repeat(200)
        );
        let mut lines: Vec<String> = std::iter::repeat_n(filler, 400).collect();
        lines.push(r#"{"type":"ai-title","aiTitle":"Tail title","sessionId":"abc"}"#.to_string());
        let data = lines.join("\n");
        assert!(
            data.len() > 64 * 1024,
            "fixture must exceed the tail window"
        );
        std::fs::write(tmp.path(), data).unwrap();
        assert_eq!(claude_ai_title(tmp.path()), Some("Tail title".to_string()));
    }

    #[test]
    fn claude_ai_title_misses_title_older_than_64k_tail() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let filler = format!(
            r#"{{"type":"assistant","message":{{"role":"assistant","content":"{}"}}}}"#,
            "x".repeat(200)
        );
        let mut lines =
            vec![r#"{"type":"ai-title","aiTitle":"Head title","sessionId":"abc"}"#.to_string()];
        lines.extend(std::iter::repeat_n(filler, 400));
        let data = lines.join("\n");
        assert!(
            data.len() > 64 * 1024,
            "fixture must exceed the tail window"
        );
        std::fs::write(tmp.path(), data).unwrap();
        assert_eq!(claude_ai_title(tmp.path()), None);
    }

    #[test]
    fn claude_ai_title_empty_custom_title_falls_back_to_ai_title() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let data = [
            r#"{"type":"ai-title","aiTitle":"Auto title","sessionId":"abc"}"#,
            r#"{"type":"custom-title","customTitle":"Manual title","sessionId":"abc"}"#,
            r#"{"type":"custom-title","customTitle":"","sessionId":"abc"}"#,
        ]
        .join("\n");
        std::fs::write(tmp.path(), data).unwrap();
        assert_eq!(claude_ai_title(tmp.path()), Some("Auto title".to_string()));
    }

    #[test]
    fn parse_claude_jsonl_ai_title_updates_summary() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let data = concat!(
            r#"{"type":"ai-title","aiTitle":"Old title"}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":"hi"}}"#,
            "\n",
            r#"{"type":"ai-title","aiTitle":"Current title"}"#,
        );
        std::fs::write(tmp.path(), data).unwrap();
        let (_, meta) = parse_claude_jsonl(tmp.path().to_str().unwrap(), true);
        assert_eq!(meta.unwrap().summary.as_deref(), Some("Current title"));
    }

    #[test]
    fn parse_claude_jsonl_custom_title_overrides_ai_title() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let data = concat!(
            r#"{"type":"ai-title","aiTitle":"Auto title"}"#,
            "\n",
            r#"{"type":"custom-title","customTitle":"Manual title"}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":"hi"}}"#,
        );
        std::fs::write(tmp.path(), data).unwrap();
        let (_, meta) = parse_claude_jsonl(tmp.path().to_str().unwrap(), true);
        let meta = meta.unwrap();
        assert_eq!(meta.summary.as_deref(), Some("Auto title"));
        assert_eq!(meta.custom_title.as_deref(), Some("Manual title"));
    }

    #[test]
    fn parse_claude_jsonl_empty_custom_title_clears_manual_title() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let data = concat!(
            r#"{"type":"ai-title","aiTitle":"Auto title"}"#,
            "\n",
            r#"{"type":"custom-title","customTitle":"Manual title"}"#,
            "\n",
            r#"{"type":"custom-title","customTitle":""}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":"hi"}}"#,
        );
        std::fs::write(tmp.path(), data).unwrap();
        let (_, meta) = parse_claude_jsonl(tmp.path().to_str().unwrap(), true);
        let meta = meta.unwrap();
        assert_eq!(meta.summary.as_deref(), Some("Auto title"));
        assert_eq!(meta.custom_title, None);
    }

    #[test]
    fn parse_codex_jsonl_skips_commentary_assistant_messages() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let data = r#"{"timestamp":"2026-06-10T10:00:00.000Z","type":"session_meta","payload":{"id":"s1","timestamp":"2026-06-10T10:00:00.000Z","cwd":"/p"}}
{"timestamp":"2026-06-10T10:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"fix the parser"}]}}
{"timestamp":"2026-06-10T10:00:02.000Z","type":"response_item","payload":{"type":"message","role":"assistant","phase":"commentary","content":[{"type":"output_text","text":"I will inspect the files first."}]}}
{"timestamp":"2026-06-10T10:00:03.000Z","type":"response_item","payload":{"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"The parser is fixed."}]}}"#;
        std::fs::write(tmp.path(), data).unwrap();
        let messages = parse_codex_jsonl(tmp.path().to_str().unwrap());
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].content, "The parser is fixed.");
        assert!(
            messages
                .iter()
                .all(|m| !m.content.contains("inspect the files")),
            "commentary assistant messages should not be included"
        );
    }

    #[test]
    fn parse_codex_jsonl_continues_past_invalid_utf8_lines() {
        use std::io::Write;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut f = std::fs::File::create(tmp.path()).unwrap();
        writeln!(
            f,
            r#"{{"timestamp":"2026-06-10T10:00:01.000Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"first prompt"}}]}}}}"#
        )
        .unwrap();
        f.write_all(b"\xff\xfe invalid utf8 line \xff\n").unwrap();
        writeln!(
            f,
            r#"{{"timestamp":"2026-06-10T10:00:03.000Z","type":"response_item","payload":{{"type":"message","role":"assistant","phase":"final_answer","content":[{{"type":"output_text","text":"answer after bad line"}}]}}}}"#
        )
        .unwrap();
        drop(f);
        let messages = parse_codex_jsonl(tmp.path().to_str().unwrap());
        assert_eq!(
            messages.len(),
            2,
            "lines after an invalid UTF-8 line must still be parsed"
        );
        assert_eq!(messages[1].content, "answer after bad line");
    }

    #[test]
    fn parse_codex_jsonl_keeps_commentary_for_turns_without_final_answer() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let data = r#"{"timestamp":"2026-06-10T10:00:00.000Z","type":"session_meta","payload":{"id":"s1","timestamp":"2026-06-10T10:00:00.000Z","cwd":"/p"}}
{"timestamp":"2026-06-10T10:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"refactor the auth module"}]}}
{"timestamp":"2026-06-10T10:00:02.000Z","type":"response_item","payload":{"type":"message","role":"assistant","phase":"commentary","content":[{"type":"output_text","text":"I found a blocker in the token refresh path."}]}}
{"timestamp":"2026-06-10T10:00:03.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<turn_aborted>user interrupted</turn_aborted>"}]}}
{"timestamp":"2026-06-10T10:00:04.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"just fix the login bug instead"}]}}
{"timestamp":"2026-06-10T10:00:05.000Z","type":"response_item","payload":{"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"Login bug fixed."}]}}"#;
        std::fs::write(tmp.path(), data).unwrap();
        let messages = parse_codex_jsonl(tmp.path().to_str().unwrap());
        let contents: Vec<&str> = messages.iter().map(|m| m.content.as_str()).collect();
        assert!(
            contents.contains(&"I found a blocker in the token refresh path."),
            "commentary from an aborted turn (no final_answer) should be kept, got: {contents:?}"
        );
        assert!(contents.contains(&"Login bug fixed."));
    }

    #[test]
    fn parse_codex_jsonl_attaches_tools_to_turn_final_answer_after_skipped_commentary() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let data = r#"{"timestamp":"2026-06-10T10:00:00.000Z","type":"session_meta","payload":{"id":"s1","timestamp":"2026-06-10T10:00:00.000Z","cwd":"/p"}}
{"timestamp":"2026-06-10T10:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"do task A"}]}}
{"timestamp":"2026-06-10T10:00:02.000Z","type":"response_item","payload":{"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"Task A is done."}]}}
{"timestamp":"2026-06-10T10:00:03.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\n<cwd>/p</cwd>"}]}}
{"timestamp":"2026-06-10T10:00:04.000Z","type":"response_item","payload":{"type":"message","role":"assistant","phase":"commentary","content":[{"type":"output_text","text":"Checking the repo layout first."}]}}
{"timestamp":"2026-06-10T10:00:05.000Z","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":[\"ls\"]}","call_id":"call_1"}}
{"timestamp":"2026-06-10T10:00:06.000Z","type":"response_item","payload":{"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"Task B is done."}]}}"#;
        std::fs::write(tmp.path(), data).unwrap();
        let messages = parse_codex_jsonl(tmp.path().to_str().unwrap());
        let task_a = messages
            .iter()
            .find(|m| m.content == "Task A is done.")
            .expect("turn 1 final answer present");
        let task_b = messages
            .iter()
            .find(|m| m.content == "Task B is done.")
            .expect("turn 2 final answer present");
        assert!(
            task_a.tool_uses.is_empty(),
            "turn 2's tool call must not attach to turn 1's answer, got {:?}",
            task_a.tool_uses
        );
        assert_eq!(
            task_b.tool_uses,
            vec!["shell".to_string()],
            "tool call after skipped commentary should attach to the turn's final answer"
        );
    }

    #[test]
    #[ignore = "manual: requires local Claude transcript"]
    fn load_claude_sessions_populates_ai_title_summary() {
        let sessions = load_claude_sessions();
        let Some(s) = sessions.iter().find(|s| s.id.starts_with("5e081f75")) else {
            return; // transcript only exists on the machine that recorded it
        };
        assert!(
            !s.summary.is_empty(),
            "expected ai-title summary, got empty (branch={:?})",
            s.branch
        );
        assert!(!s.branch.is_empty(), "expected gitBranch from JSONL");
    }

    #[test]
    #[ignore = "manual: requires local Claude transcript"]
    fn claude_ai_title_real_session() {
        // Resolve HOME at runtime: env! would make compilation fail on any
        // host without HOME set (e.g. Windows), even though this test is
        // ignored — ignored tests are still compiled.
        let Ok(home) = std::env::var("HOME") else {
            return;
        };
        let p = std::path::PathBuf::from(home).join(
            ".claude/projects/-Users-ayushbhardwaj-Documents-GitHub-chat-history/5e081f75-04b9-4461-a805-8bfb9f8c75fc.jsonl",
        );
        let p = p.as_path();
        if !p.exists() {
            return;
        }
        assert_eq!(
            claude_ai_title(p).as_deref(),
            Some("Add Codex chat-history integrations with graceful fallbacks")
        );
    }

    #[test]
    fn parse_any_timestamp_rfc3339() {
        assert!(parse_any_timestamp("2025-01-15T10:30:00+00:00").is_some());
    }

    #[test]
    fn parse_any_timestamp_z_suffix() {
        assert!(parse_any_timestamp("2025-01-15T10:30:00Z").is_some());
    }

    #[test]
    fn parse_any_timestamp_invalid() {
        assert!(parse_any_timestamp("not-a-date").is_none());
        assert!(parse_any_timestamp("").is_none());
    }

    #[test]
    fn mtime_iso_appends_z() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let ts = mtime_iso(tmp.path()).unwrap();
        assert!(
            ts.ends_with('Z'),
            "mtime_iso should produce Z-suffixed timestamps, got: {ts}"
        );
        assert!(
            parse_any_timestamp(&ts).is_some(),
            "mtime_iso output should be parseable"
        );
    }
}
