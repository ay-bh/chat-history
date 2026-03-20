use crate::parser::{clean_prompt, extract_text, is_clear_metadata, is_warmup_message};
use chrono::{DateTime, NaiveDate, Utc};
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
    #[allow(dead_code)]
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
    dirs_next().unwrap_or_else(|| PathBuf::from("."))
}

fn dirs_next() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
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

fn read_cwd_from_jsonl(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
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
        if let Some(cwd) = entry.get("cwd").and_then(Value::as_str) {
            if !cwd.is_empty() {
                return Some(cwd.to_string());
            }
        }
    }
    None
}

pub fn encode_path_for_claude(path: &Path) -> String {
    path.to_string_lossy().replace('/', "-")
}

pub fn copy_session_to_dir(session: &Session, target_dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(target_dir)?;

    let src = Path::new(&session.file);
    if let Some(filename) = src.file_name() {
        fs::copy(src, target_dir.join(filename))?;
    }

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
    Some(dt.format("%Y-%m-%dT%H:%M:%S").to_string())
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
    if let Some(Ok(line)) = reader.lines().next() {
        if let Ok(entry) = serde_json::from_str::<Value>(&line) {
            let content = entry
                .get("message")
                .and_then(|m| m.get("content"))
                .cloned()
                .unwrap_or(Value::String(String::new()));
            let text = extract_text(&content);
            return text.chars().take(300).collect();
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
            && !["<user_query>", "</user_query>", "user:", "assistant:", "<attached_files>"]
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
            sessions.push(Session {
                source: "claude".into(),
                id: entry.session_id,
                summary: entry.summary,
                first_prompt: entry.first_prompt.chars().take(300).collect(),
                created: entry.created.clone(),
                modified: entry.modified,
                date: entry.created.get(..10).unwrap_or("").to_string(),
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
                    let sid = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
                    if indexed_ids.contains(&sid) || sid.starts_with("agent-") {
                        continue;
                    }
                    let iso = mtime_iso(&path).unwrap_or_default();
                    let date = mtime_date(&path).unwrap_or_default();
                    let project = read_cwd_from_jsonl(&path)
                        .unwrap_or_else(|| {
                            dir.file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .replace('-', "/")
                        });
                    let first = claude_first_prompt(&path);
                    sessions.push(Session {
                        source: "claude".into(),
                        id: sid,
                        summary: String::new(),
                        first_prompt: first,
                        created: iso.clone(),
                        modified: iso,
                        date,
                        messages: 0,
                        branch: String::new(),
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

pub fn load_all_sessions() -> Vec<Session> {
    let mut all = load_claude_sessions();
    all.extend(load_cursor_sessions());
    all
}

pub fn parse_claude_jsonl(filepath: &str, extract_meta: bool) -> (Vec<Message>, Option<SessionMeta>) {
    let file = match fs::File::open(filepath) {
        Ok(f) => f,
        Err(_) => return (Vec::new(), if extract_meta { Some(SessionMeta { summary: None, custom_title: None, model: None, total_tokens: 0 }) } else { None }),
    };
    let reader = BufReader::new(file);
    let mut messages = Vec::new();
    let mut meta = SessionMeta {
        summary: None,
        custom_title: None,
        model: None,
        total_tokens: 0,
    };
    let mut skip_next_assistant = false;
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
            meta.summary = entry.get("summary").and_then(Value::as_str).map(String::from);
            continue;
        }
        if etype == "custom_title" && extract_meta && meta.custom_title.is_none() {
            let title = entry.get("custom_title").and_then(Value::as_str).unwrap_or("").trim().to_string();
            if !title.is_empty() {
                meta.custom_title = Some(title);
            }
            continue;
        }

        if etype != "user" && etype != "assistant" {
            continue;
        }

        let msg_obj = entry.get("message").cloned().unwrap_or(Value::Object(Default::default()));
        let content_raw = msg_obj.get("content").cloned().unwrap_or(Value::String(String::new()));
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
            if extract_meta && meta.model.is_none() {
                if let Some(m) = msg_obj.get("model").and_then(Value::as_str) {
                    meta.model = Some(m.to_string());
                }
            }
            if extract_meta {
                if let Some(usage) = msg_obj.get("usage") {
                    let tok = usage.get("input_tokens").and_then(Value::as_u64).unwrap_or(0)
                        + usage.get("output_tokens").and_then(Value::as_u64).unwrap_or(0)
                        + usage.get("cache_creation_input_tokens").and_then(Value::as_u64).unwrap_or(0)
                        + usage.get("cache_read_input_tokens").and_then(Value::as_u64).unwrap_or(0);
                    meta.total_tokens += tok;
                }
            }
            if skip_next_assistant {
                skip_next_assistant = false;
                continue;
            }
        }

        let ctx = crate::parser::extract_context(&content_raw);

        if total_chars < max_chars {
            let role = msg_obj.get("role").and_then(Value::as_str).unwrap_or(etype).to_string();
            let uuid = entry.get("uuid").and_then(Value::as_str).unwrap_or("").to_string();
            let timestamp = entry.get("timestamp").and_then(Value::as_str).unwrap_or("").to_string();
            let session_id = entry.get("sessionId").and_then(Value::as_str).unwrap_or("").to_string();
            let cwd = entry.get("cwd").and_then(Value::as_str).unwrap_or("").to_string();
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
        let role = entry.get("role").and_then(Value::as_str).unwrap_or("").to_string();
        let msg_obj = entry.get("message").cloned().unwrap_or(Value::Object(Default::default()));
        let content_raw = msg_obj.get("content").cloned().unwrap_or(Value::String(String::new()));
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
                        uuid: String::new(), timestamp: String::new(), role, content: text,
                        session_id: String::new(), project_path: String::new(),
                        tool_uses: Vec::new(), files_referenced: Vec::new(), error_patterns: Vec::new(),
                        relevance_score: 0.0, final_score: 0.0,
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
                        uuid: String::new(), timestamp: String::new(), role, content: text,
                        session_id: String::new(), project_path: String::new(),
                        tool_uses: Vec::new(), files_referenced: Vec::new(), error_patterns: Vec::new(),
                        relevance_score: 0.0, final_score: 0.0,
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
                uuid: String::new(), timestamp: String::new(), role, content: text,
                session_id: String::new(), project_path: String::new(),
                tool_uses: Vec::new(), files_referenced: Vec::new(), error_patterns: Vec::new(),
                relevance_score: 0.0, final_score: 0.0,
            });
        }
    }
    messages
}

pub fn parse_session(session: &Session, extract_meta: bool) -> (Vec<Message>, Option<SessionMeta>) {
    if session.source == "claude" {
        return parse_claude_jsonl(&session.file, extract_meta);
    }
    let messages = if session.file.ends_with(".jsonl") {
        parse_cursor_jsonl(&session.file)
    } else {
        parse_cursor_txt(&session.file)
    };
    if extract_meta {
        (messages, Some(SessionMeta {
            summary: if session.summary.is_empty() { None } else { Some(session.summary.clone()) },
            custom_title: None, model: None, total_tokens: 0,
        }))
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
    let mut out: Vec<Session> = sessions
        .iter()
        .filter(|s| {
            if s.date.is_empty() {
                return false;
            }
            let d = match NaiveDate::parse_from_str(&s.date, "%Y-%m-%d") {
                Ok(d) => d,
                Err(_) => return false,
            };
            if let Some(fd) = from_date {
                if d < fd { return false; }
            }
            if let Some(td) = to_date {
                if d > td { return false; }
            }
            if let Some(src) = source {
                if s.source != src { return false; }
            }
            if let Some(proj) = project {
                if !s.project.to_lowercase().contains(&proj.to_lowercase()) { return false; }
            }
            if let Some(br) = branch {
                if !s.branch.to_lowercase().contains(&br.to_lowercase()) { return false; }
            }
            if let Some(kw) = keyword {
                let kw_lower = kw.to_lowercase();
                let haystack = format!("{} {} {} {}", s.summary, s.first_prompt, s.branch, s.project).to_lowercase();
                if !haystack.contains(&kw_lower) { return false; }
            }
            true
        })
        .cloned()
        .collect();
    out.sort_by(|a, b| {
        let ma = if a.modified.is_empty() { &a.created } else { &a.modified };
        let mb = if b.modified.is_empty() { &b.created } else { &b.modified };
        mb.cmp(ma)
    });
    out
}

pub fn find_session<'a>(sessions: &'a [Session], sid: &str) -> Option<&'a Session> {
    sessions.iter().find(|s| s.id == sid)
        .or_else(|| sessions.iter().find(|s| s.id.starts_with(sid)))
}
