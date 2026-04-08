use crate::session::*;
use std::collections::{BTreeSet, HashSet};

pub struct InspectInfo {
    pub session_id: String,
    pub summary: String,
    pub project: String,
    pub branch: String,
    pub date: String,
    pub duration_minutes: i64,
    pub message_count: usize,
    pub user_messages: usize,
    pub assistant_messages: usize,
    pub tools_used: Vec<String>,
    pub files_modified: Vec<String>,
    pub accomplishments: Vec<String>,
    pub decisions: Vec<String>,
    pub errors: Vec<String>,
    pub source: String,
    pub model: String,
    pub total_tokens: u64,
}

fn find_case_insensitive(text: &str, keyword: &str) -> Option<(usize, usize)> {
    let kw_chars: Vec<char> = keyword.chars().collect();
    for (i, _) in text.char_indices() {
        let mut chars = text[i..].chars();
        let mut matched = true;
        let mut end = i;
        for &kc in &kw_chars {
            match chars.next() {
                Some(tc) if tc.to_lowercase().next() == Some(kc) => {
                    end += tc.len_utf8();
                }
                _ => {
                    matched = false;
                    break;
                }
            }
        }
        if matched {
            return Some((i, end));
        }
    }
    None
}

fn extract_sentence_around(text: &str, keyword: &str) -> Option<String> {
    let (idx, kw_end) = find_case_insensitive(text, keyword)?;
    let start = text[..idx].rfind('.').map(|p| p + 1).unwrap_or(0);
    let end = text[kw_end..]
        .find('.')
        .map(|p| kw_end + p + 1)
        .unwrap_or_else(|| text.len().min(idx + 150));
    let sentence = text[start..end].trim();
    if sentence.len() > 200 {
        let trunc = text.floor_char_boundary(start + 197).min(end);
        Some(format!("{}...", &text[start..trunc].trim()))
    } else {
        Some(sentence.to_string())
    }
}

pub fn inspect_session(session: &Session) -> Option<InspectInfo> {
    let (messages, meta_opt) = parse_session(session, true);
    if messages.is_empty() {
        return None;
    }
    let meta = meta_opt.unwrap_or(SessionMeta {
        summary: None,
        custom_title: None,
        model: None,
        total_tokens: 0,
    });

    let mut tools_used: BTreeSet<String> = BTreeSet::new();
    let mut files_modified: BTreeSet<String> = BTreeSet::new();
    let mut accomplishments = Vec::new();
    let mut decisions = Vec::new();
    let mut errors_seen = Vec::new();
    let mut err_set: HashSet<String> = HashSet::new();
    let mut user_count = 0usize;
    let mut assistant_count = 0usize;
    let mut acc_set: HashSet<String> = HashSet::new();
    let mut dec_set: HashSet<String> = HashSet::new();

    let accomplishment_signals = [
        "successfully",
        "completed",
        "fixed",
        "implemented",
        "created",
        "added",
        "updated",
        "resolved",
        "built",
        "configured",
        "here's what we accomplished",
        "done",
        "finished",
    ];
    let decision_signals = [
        "decided to",
        "chose",
        "instead of",
        "opted for",
        "trade-off",
        "rationale",
        "the approach",
    ];

    for msg in &messages {
        if msg.role == "user" {
            user_count += 1;
        } else {
            assistant_count += 1;
        }
        for t in &msg.tool_uses {
            tools_used.insert(t.clone());
        }
        for f in &msg.files_referenced {
            files_modified.insert(f.clone());
        }
        for e in msg.error_patterns.iter().take(3) {
            if err_set.insert(e.clone()) {
                errors_seen.push(e.clone());
            }
        }

        if msg.role == "assistant" && msg.content.len() > 80 {
            let cl = msg.content_lower();
            for sig in &accomplishment_signals {
                if cl.contains(sig) {
                    if let Some(snippet) = extract_sentence_around(&msg.content, sig)
                        && acc_set.insert(snippet.clone())
                    {
                        accomplishments.push(snippet);
                    }
                    break;
                }
            }
            for sig in &decision_signals {
                if cl.contains(sig) {
                    if let Some(snippet) = extract_sentence_around(&msg.content, sig)
                        && dec_set.insert(snippet.clone())
                    {
                        decisions.push(snippet);
                    }
                    break;
                }
            }
        }
    }

    let timestamps: Vec<&str> = messages
        .iter()
        .map(|m| m.timestamp.as_str())
        .filter(|t| !t.is_empty())
        .collect();
    let duration = if timestamps.len() >= 2 {
        let min_ts = timestamps.iter().min().unwrap();
        let max_ts = timestamps.iter().max().unwrap();
        let parse_ts = crate::session::parse_any_timestamp;
        match (parse_ts(min_ts), parse_ts(max_ts)) {
            (Some(t1), Some(t2)) => (t2 - t1).num_minutes(),
            _ => 0,
        }
    } else {
        0
    };

    let effective_summary = meta
        .custom_title
        .or(meta.summary)
        .unwrap_or_else(|| session.summary.clone());

    accomplishments.truncate(10);
    decisions.truncate(5);
    errors_seen.truncate(5);
    let files_vec: Vec<String> = files_modified.into_iter().take(20).collect();

    Some(InspectInfo {
        session_id: session.id.clone(),
        summary: effective_summary,
        project: session.project.clone(),
        branch: session.branch.clone(),
        date: session.date.clone(),
        duration_minutes: duration,
        message_count: messages.len(),
        user_messages: user_count,
        assistant_messages: assistant_count,
        tools_used: tools_used.into_iter().collect(),
        files_modified: files_vec,
        accomplishments,
        decisions,
        errors: errors_seen,
        source: session.source.clone(),
        model: meta.model.unwrap_or_default(),
        total_tokens: meta.total_tokens,
    })
}
