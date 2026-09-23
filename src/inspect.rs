use crate::session::*;
use regex::Regex;
use std::collections::{BTreeSet, HashSet};
use std::sync::LazyLock;

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
    pub tool_results: usize,
    pub tools_used: Vec<String>,
    pub files_modified: Vec<String>,
    /// The session's first request.
    pub asked: String,
    /// The latest substantive result: the last turn-ending reply with an
    /// informative sentence, so a closing "All set" does not hide it.
    pub outcome: String,
    /// How each turn ended, most recent last.
    pub accomplishments: Vec<String>,
    pub decisions: Vec<String>,
    pub errors: Vec<String>,
    pub source: String,
    pub also_ide: bool,
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
        .unwrap_or_else(|| text.floor_char_boundary(text.len().min(idx + 150)));
    let sentence = text[start..end].trim();
    if sentence.len() > 200 {
        let trunc = text.floor_char_boundary(start + 197).min(end);
        Some(format!("{}...", text[start..trunc].trim()))
    } else {
        Some(sentence.to_string())
    }
}

static MARKDOWN_LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\]]*)\]\([^)]*\)").unwrap());
static LIST_MARKER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:[-*+•]|\d+[.)])\s+").unwrap());

/// The readable part of an assistant message: the text before its first
/// tool call, without code blocks, tables, headings, quote or list markers,
/// or markdown markup. A blank line separates paragraphs; a removed line, a
/// list item or a line ending in ':' starts a new one.
fn prose(text: &str) -> String {
    let text = text.split("[Tool: ").next().unwrap_or_default();
    let mut lines: Vec<String> = Vec::new();
    let mut in_code = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("```") {
            in_code = !in_code;
            lines.push(String::new());
            continue;
        }
        if in_code || line.starts_with('|') || line.starts_with('#') {
            lines.push(String::new());
            continue;
        }
        let quoted = line.trim_start_matches('>').trim_start();
        if LIST_MARKER_RE.is_match(quoted) || lines.last().is_some_and(|l| l.ends_with(':')) {
            lines.push(String::new());
        }
        let line = LIST_MARKER_RE.replace(quoted, "");
        let line = MARKDOWN_LINK_RE.replace_all(&line, "$1");
        lines.push(line.replace("**", "").replace('`', ""));
    }
    lines.join("\n")
}

/// Whether a message is one the client writes into the transcript (an API
/// error, a usage or login notice) rather than a reply.
fn is_client_notice(text: &str) -> bool {
    let lower = text.trim_start().to_lowercase();
    CLIENT_NOTICES.iter().any(|n| lower.starts_with(n))
}

const CLIENT_NOTICES: &[&str] = &[
    "api error",
    "you're out of usage credits",
    "login expired",
    "[request interrupted",
];

/// Openings of sentences that announce content instead of stating it.
const LEAD_INS: &[&str] = &["here is", "here are", "here's", "below is", "below are"];

/// The first sentence of a message that says something: four words or
/// more, and not a lead-in ("Here are the findings.", or ending in ':'). A
/// sentence ends at . ! or ? before whitespace, so file names, versions and
/// URLs stay whole, and not after an initial.
fn headline(text: &str) -> Option<String> {
    if is_client_notice(text) {
        return None;
    }
    let informative = |s: &str| {
        let lower = s.to_lowercase();
        s.split_whitespace().count() >= 4
            && !s.ends_with(':')
            && !LEAD_INS.iter().any(|l| lower.starts_with(l))
    };
    for paragraph in prose(text).split("\n\n") {
        // Lines within a paragraph are one text: a sentence may wrap.
        let line = paragraph.trim().replace('\n', " ");
        let line = line.as_str();
        let mut start = 0;
        let mut chars = line.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            let next = chars.peek().map(|&(_, n)| n);
            // "D. E. Shaw": a lone capital before the dot is an initial.
            let initial = c == '.'
                && line[..i]
                    .chars()
                    .next_back()
                    .is_some_and(char::is_uppercase)
                && line[..i]
                    .chars()
                    .nth_back(1)
                    .is_none_or(|p| !p.is_alphanumeric());
            let ends =
                matches!(c, '.' | '!' | '?') && !initial && next.is_none_or(char::is_whitespace);
            if ends || next.is_none() {
                let end = i + c.len_utf8();
                let sentence = line[start..end].trim();
                start = end;
                if informative(sentence) {
                    return Some(clip(sentence, 200));
                }
            }
        }
    }
    None
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max - 1).collect();
    format!("{}…", cut.trim_end())
}

/// How a turn (a request and the assistant messages after it) ended.
enum TurnEnd {
    /// A reply with an informative sentence: its headline.
    Result(String),
    /// Replies, but none informative ("All set!").
    Short,
    /// No reply: nothing, tool calls, or a client notice (kept, clipped).
    NoReply(Option<String>),
}

impl TurnEnd {
    /// The turn's state after one more assistant message. A later message
    /// with nothing informative ("Done.", a bare tool call, an API error)
    /// keeps the result before it.
    fn after(self, content: &str) -> TurnEnd {
        if let Some(h) = headline(content) {
            return TurnEnd::Result(h);
        }
        match self {
            TurnEnd::Result(_) | TurnEnd::Short => self,
            TurnEnd::NoReply(notice) if is_client_notice(content) => {
                let first = content.trim().lines().next().unwrap_or_default();
                TurnEnd::NoReply(notice.or_else(|| Some(clip(first, 80))))
            }
            TurnEnd::NoReply(_) if !prose(content).trim().is_empty() => TurnEnd::Short,
            no_reply => no_reply,
        }
    }
}

/// The latest substantive result. A last turn with only a short reply
/// ("thanks" / "All set!") falls back to the result before it; a last
/// request that got no reply says so instead of showing an older result.
fn outcome(turns: &[TurnEnd]) -> String {
    for t in turns.iter().rev() {
        match t {
            TurnEnd::Result(h) => return h.clone(),
            TurnEnd::Short => continue,
            TurnEnd::NoReply(Some(notice)) => {
                return format!("no reply to the last request ({notice})");
            }
            TurnEnd::NoReply(None) => return "no reply to the last request".to_string(),
        }
    }
    String::new()
}

fn session_duration_minutes(timestamps: &[&str]) -> i64 {
    let parsed: Vec<_> = timestamps
        .iter()
        .filter_map(|t| crate::session::parse_any_timestamp(t))
        .collect();
    if parsed.len() < 2 {
        return 0;
    }
    match (parsed.iter().min(), parsed.iter().max()) {
        (Some(t1), Some(t2)) => (*t2 - *t1).num_minutes(),
        _ => 0,
    }
}

pub fn inspect_session(session: &Session) -> Option<InspectInfo> {
    let (messages, meta_opt) = parse_session_recovering_timestamps(session, true);
    if messages.is_empty() {
        return None;
    }
    let meta = meta_opt.unwrap_or_default();

    let mut tools_used: BTreeSet<String> = BTreeSet::new();
    let mut files_modified: BTreeSet<String> = BTreeSet::new();
    // How each turn ended, one entry per turn.
    let mut turns: Vec<TurnEnd> = Vec::new();
    let mut turn = TurnEnd::NoReply(None);
    let mut in_turn = false;
    let mut decisions = Vec::new();
    let mut errors_seen = Vec::new();
    let mut err_set: HashSet<String> = HashSet::new();
    let mut user_count = 0usize;
    let mut assistant_count = 0usize;
    let mut tool_count = 0usize;
    let mut dec_set: HashSet<String> = HashSet::new();

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
        match msg.role.as_str() {
            "user" => {
                user_count += 1;
                if in_turn {
                    turns.push(std::mem::replace(&mut turn, TurnEnd::NoReply(None)));
                }
                in_turn = true;
            }
            "tool" => tool_count += 1,
            _ => {
                assistant_count += 1;
                in_turn = true;
                turn = turn.after(&msg.content);
            }
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
            let text = prose(&msg.content);
            let cl = text.to_lowercase();
            for sig in &decision_signals {
                if cl.contains(sig) {
                    if let Some(snippet) = extract_sentence_around(&text, sig)
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
    let duration = session_duration_minutes(&timestamps);

    let effective_summary = meta
        .custom_title
        .or(meta.summary)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            if !session.summary.is_empty() {
                session.summary.clone()
            } else {
                // Same fallback the list rows use, so a session titled by its
                // first prompt doesn't inspect as "(no summary)".
                crate::parser::display_title(&session.first_prompt, 120)
            }
        });

    if in_turn {
        turns.push(turn);
    }
    let outcome = outcome(&turns);
    let mut accomplishments: Vec<String> = turns
        .into_iter()
        .filter_map(|t| match t {
            TurnEnd::Result(h) => Some(h),
            _ => None,
        })
        .collect();
    let accomplishments = accomplishments.split_off(accomplishments.len().saturating_sub(10));
    let asked = messages
        .iter()
        .filter(|m| m.role == "user")
        .map(|m| crate::parser::display_title(&m.content, 160))
        .find(|t| !t.is_empty())
        .unwrap_or_default();
    decisions.truncate(5);
    errors_seen.truncate(5);
    let files_vec: Vec<String> = files_modified.into_iter().collect();

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
        tool_results: tool_count,
        tools_used: tools_used.into_iter().collect(),
        files_modified: files_vec,
        asked,
        outcome,
        accomplishments,
        decisions,
        errors: errors_seen,
        source: session.source.clone(),
        also_ide: session.also_ide,
        model: meta.model.unwrap_or_default(),
        total_tokens: meta.total_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headline_keeps_urls_versions_and_file_names_whole() {
        assert_eq!(
            headline("Created [PR #41](https://github.com/acme/web/pull/41) for review.")
                .as_deref(),
            Some("Created PR #41 for review.")
        );
        assert_eq!(
            headline("Bumped to 0.7.1 and edited src/main.rs today. Then more.").as_deref(),
            Some("Bumped to 0.7.1 and edited src/main.rs today.")
        );
    }

    #[test]
    fn headline_skips_client_notices_and_keeps_initials() {
        assert_eq!(
            headline("API Error: 529 Overloaded. Try again later."),
            None
        );
        assert_eq!(headline("You're out of usage credits."), None);
        assert_eq!(
            headline("I put AI agents into production at D. E. Shaw for two years.").as_deref(),
            Some("I put AI agents into production at D. E. Shaw for two years.")
        );
    }

    #[test]
    fn headline_skips_lead_ins_and_non_prose() {
        assert_eq!(headline("Done. All set."), None);
        assert_eq!(
            headline("## Summary\nHere is what changed:\n- **Moved** the cache into `src/cache.rs` so it loads once")
                .as_deref(),
            Some("Moved the cache into src/cache.rs so it loads once")
        );
        assert_eq!(headline("| a | b |\n```\nlet x = 1; done now\n```"), None);
        assert_eq!(
            headline("[Tool: Bash] for id in a b; do chat-history inspect $id; done"),
            None
        );
    }

    #[test]
    fn extract_sentence_multibyte_boundary_no_panic() {
        // No '.' after the keyword and a 3-byte char straddling the idx+150
        // fallback cut point: must not panic on a non-boundary byte index.
        let text = format!("fixed: {}", "日".repeat(100));
        let sentence = extract_sentence_around(&text, "fixed");
        assert!(sentence.is_some());
    }

    #[test]
    fn duration_orders_by_parsed_time_not_string() {
        // 23:00-08:00 is 07:00Z the next day: chronologically LATER than
        // 01:00Z despite sorting earlier as a string.
        let ts = ["2025-01-15T23:00:00-08:00", "2025-01-16T01:00:00Z"];
        assert_eq!(session_duration_minutes(&ts), 360);
    }
}
