use crate::inspect::InspectInfo;
use crate::parser::{clean_prompt, display_title, snippet_around_match, strip_terminal_controls};
use crate::search::{IndexResult, SearchResult};
use crate::session::{self, Message, Session};
use std::collections::BTreeMap;
use std::sync::LazyLock;

static USE_COLOR: LazyLock<bool> = LazyLock::new(|| {
    std::io::IsTerminal::is_terminal(&std::io::stdout())
        && std::env::var("NO_COLOR").is_err()
        && std::env::var("TERM").map(|t| t != "dumb").unwrap_or(true)
});

macro_rules! c {
    ($name:expr) => {
        if *USE_COLOR {
            match $name {
                "reset" => "\x1b[0m",
                "bold" => "\x1b[1m",
                "dim" => "\x1b[2m",
                "cyan" => "\x1b[36m",
                "green" => "\x1b[32m",
                "yellow" => "\x1b[33m",
                "magenta" => "\x1b[35m",
                "blue" => "\x1b[34m",
                "red" => "\x1b[31m",
                "bg_blue" => "\x1b[44m",
                "bg_magenta" => "\x1b[45m",
                "bg_cyan" => "\x1b[46m",
                "bg_yellow" => "\x1b[43m",
                _ => "",
            }
        } else {
            ""
        }
    };
}

fn src_tag(source: &str, also_ide: bool) -> String {
    // IDE Agent chats write SQLite *and* an agent-transcripts jsonl with the
    // same composer id. Prefer the IDE label — that's the UI the user used.
    let label = match source {
        "claude" => "claude",
        "codex" => "codex",
        "cursor-ide" => "cursor-ide",
        "cursor" if also_ide => "cursor-ide",
        _ => "cursor-agent",
    };
    let color = match source {
        "claude" => "bg_cyan",
        "codex" => "bg_magenta",
        "cursor-ide" => "bg_yellow",
        "cursor" if also_ide => "bg_yellow",
        _ => "bg_blue",
    };
    format!("{}{} {:<12} {}", c!(color), c!("bold"), label, c!("reset"))
}

fn labeled(label: &str, value: &str) -> String {
    if value.is_empty() {
        String::new()
    } else {
        format!(" {}{}:{} {}", c!("dim"), label, c!("reset"), value)
    }
}

/// Replace the user home directory with `~` for display (`/home/you/x` or
/// `/Users/you/x` → `~/x`). Uses `$HOME` (Linux/macOS) or `%USERPROFILE%`.
pub fn abbreviate_home(path: &str) -> String {
    let home = session::user_home();
    abbreviate_home_with(path, home.as_deref().and_then(|p| p.to_str()))
}

fn abbreviate_home_with(path: &str, home: Option<&str>) -> String {
    let Some(home) = home.filter(|h| !h.is_empty()) else {
        return path.to_string();
    };
    let home = home.trim_end_matches(['/', '\\']);
    if path == home || path.trim_end_matches(['/', '\\']) == home {
        return "~".into();
    }
    for sep in ['/', '\\'] {
        let prefix = format!("{home}{sep}");
        if let Some(rest) = path.strip_prefix(&prefix) {
            return format!("~/{}", rest.replace('\\', "/"));
        }
    }
    path.to_string()
}

fn tty(s: &str) -> String {
    strip_terminal_controls(s)
}

fn dir_label(project: &str) -> String {
    labeled("DIR", &tty(&abbreviate_home(project)))
}

fn copies_label(
    counts: &std::collections::HashMap<(String, String), usize>,
    s: &Session,
) -> String {
    let n = counts
        .get(&(s.source.clone(), s.id.to_ascii_lowercase()))
        .copied()
        .unwrap_or(0);
    if n > 1 {
        labeled("COPIES", &n.to_string())
    } else {
        String::new()
    }
}

fn copy_counts(sessions: &[Session]) -> std::collections::HashMap<(String, String), usize> {
    let mut m = std::collections::HashMap::new();
    for s in sessions {
        if s.source != "cursor" {
            continue;
        }
        *m.entry((s.source.clone(), s.id.to_ascii_lowercase()))
            .or_insert(0) += 1;
    }
    m
}

fn print_title_line(title: &str, session: &Session) {
    let availability = if session.is_cursor_store_only() {
        " [metadata only]"
    } else {
        ""
    };
    println!(
        "        {}{}{}{}",
        c!("bold"),
        title,
        c!("reset"),
        availability
    );
}

/// Dim 8-char session-id chip. Every row shows it: the prefix resolves via
/// inspect/view/export/find for all sources, and via resume for everything
/// except `cursor-ide` rows (those print a sidebar hint instead).
fn id_chip(session: &Session) -> String {
    let short: String = session.id.chars().take(8).collect();
    format!("{}{:8}{}", c!("dim"), short, c!("reset"))
}

/// Best one-line title for a session: summary, else cleaned first prompt.
fn title_of(summary: &str, first_prompt: &str, max: usize) -> String {
    let t = display_title(summary, max);
    if !t.is_empty() {
        return tty(&t);
    }
    let t = display_title(first_prompt, max);
    if !t.is_empty() {
        return tty(&t);
    }
    "(untitled)".into()
}

pub fn print_list(sessions: &[Session], verbose: bool) {
    if sessions.is_empty() {
        println!("{}No sessions found.{}", c!("dim"), c!("reset"));
        return;
    }
    println!(
        "\n{}{} sessions{}\n",
        c!("bold"),
        sessions.len(),
        c!("reset")
    );
    let counts = copy_counts(sessions);
    for (i, s) in sessions.iter().enumerate() {
        let tag = src_tag(&s.source, s.also_ide);
        let title = title_of(&s.summary, &s.first_prompt, 100);
        let branch = labeled("BRANCH", &tty(&s.branch));
        let sidechain = if s.is_sidechain {
            format!(" {}[subagent]{}", c!("dim"), c!("reset"))
        } else {
            String::new()
        };
        let msgs = if s.messages > 0 {
            format!(" {}[{} msgs]{}", c!("dim"), s.messages, c!("reset"))
        } else {
            String::new()
        };
        println!(
            "  {}{:3}.{} {} {}{}{}  {}{}{}{}{}{}",
            c!("dim"),
            i + 1,
            c!("reset"),
            tag,
            c!("cyan"),
            s.date,
            c!("reset"),
            id_chip(s),
            dir_label(&s.project),
            copies_label(&counts, s),
            sidechain,
            branch,
            msgs
        );
        print_title_line(&title, s);
        if verbose {
            println!("       {}id: {}{}", c!("dim"), s.id, c!("reset"));
            println!(
                "       {}file: {}{}",
                c!("dim"),
                abbreviate_home(&s.file),
                c!("reset")
            );
        }
    }
    println!();
}

pub fn print_summarized(sessions: &[Session]) {
    if sessions.is_empty() {
        println!("{}No sessions found.{}", c!("dim"), c!("reset"));
        return;
    }
    let counts = copy_counts(sessions);
    let mut by_day: BTreeMap<&str, Vec<&Session>> = BTreeMap::new();
    for s in sessions {
        by_day.entry(&s.date).or_default().push(s);
    }
    println!(
        "\n{}{} sessions across {} days{}\n",
        c!("bold"),
        sessions.len(),
        by_day.len(),
        c!("reset")
    );
    for (day, ds) in by_day.iter().rev() {
        println!(
            "  {}{}{}{}  {}({} sessions){}",
            c!("cyan"),
            c!("bold"),
            day,
            c!("reset"),
            c!("dim"),
            ds.len(),
            c!("reset")
        );
        for s in ds {
            let title = title_of(&s.summary, &s.first_prompt, 100);
            println!(
                "    {} {}{}{}{}",
                src_tag(&s.source, s.also_ide),
                id_chip(s),
                dir_label(&s.project),
                copies_label(&counts, s),
                labeled("BRANCH", &tty(&s.branch))
            );
            print_title_line(&title, s);
        }
        println!();
    }
}

pub fn print_index_results(results: &[IndexResult], query: &str) {
    if results.is_empty() {
        println!("{}No results for \"{}\".{}", c!("dim"), query, c!("reset"));
        return;
    }
    println!(
        "\n{}{} results for \"{}\"{}  {}(index search — use --deep for full transcript search){}\n",
        c!("bold"),
        results.len(),
        query,
        c!("reset"),
        c!("dim"),
        c!("reset")
    );
    for (i, r) in results.iter().enumerate() {
        let tag = src_tag(&r.session.source, r.session.also_ide);
        let score = format!("{}★ {:.1}{}", c!("yellow"), r.score, c!("reset"));
        let title = title_of(&r.session.summary, &r.display, 100);
        println!(
            "  {}{:3}.{} {} {}{}{} {} {}{}{}",
            c!("dim"),
            i + 1,
            c!("reset"),
            tag,
            c!("cyan"),
            r.session.date,
            c!("reset"),
            id_chip(&r.session),
            score,
            dir_label(&r.session.project),
            labeled("INDEX_FIELD", &r.matched_field)
        );
        print_title_line(&title, &r.session);
    }
    println!();
}

pub fn print_search_results(results: &[SearchResult], query: &str) {
    if results.is_empty() {
        println!("{}No results for \"{}\".{}", c!("dim"), query, c!("reset"));
        return;
    }
    println!(
        "\n{}{} results for \"{}\"{}\n",
        c!("bold"),
        results.len(),
        query,
        c!("reset")
    );
    for (i, r) in results.iter().enumerate() {
        let tag = src_tag(&r.session.source, r.session.also_ide);
        let score = format!(
            "{}★ {}{}",
            c!("yellow"),
            if r.message.final_score.abs() < 0.05 {
                format!("{:.2e}", r.message.final_score)
            } else {
                format!("{:.2}", r.message.final_score)
            },
            c!("reset")
        );
        let role_str = role_chip(&r.message.role);
        let title = title_of(&r.session.summary, &r.session.first_prompt, 100);
        println!(
            "  {}{:3}.{} {} {}{}{} {} {}{}",
            c!("dim"),
            i + 1,
            c!("reset"),
            tag,
            c!("cyan"),
            r.session.date,
            c!("reset"),
            id_chip(&r.session),
            score,
            dir_label(&r.session.project)
        );
        print_title_line(&title, &r.session);
        let snippet = usable_search_snippet(r.snippet.as_deref(), &r.message.content, query, 200);
        println!(
            "        {}: {}{}",
            role_str,
            format_search_preview(&snippet, query),
            ordinal_tag(r.ordinal)
        );
        print_hit_details(&r.message, "       ");
        for other in &r.additional_matches {
            let role = role_chip(&other.message.role);
            let snippet =
                usable_search_snippet(other.snippet.as_deref(), &other.message.content, query, 200);
            println!(
                "          also {role}: {}{}",
                format_search_preview(&snippet, query),
                ordinal_tag(other.ordinal)
            );
            print_hit_details(&other.message, "            ");
        }
        println!();
    }
}

/// Colour and display name of a message role.
fn role_style(role: &str) -> (&'static str, &'static str) {
    match role {
        "user" => ("green", "You"),
        "tool" => ("yellow", "Tool"),
        _ => ("blue", "Assistant"),
    }
}

fn role_chip(role: &str) -> String {
    let (color, name) = role_style(role);
    format!("{}{name}{}", c!(color), c!("reset"))
}

/// `  #12` after a hit's excerpt: the message position `view --around` takes.
fn ordinal_tag(ordinal: Option<usize>) -> String {
    ordinal
        .map(|o| format!("  {}#{o}{}", c!("dim"), c!("reset")))
        .unwrap_or_default()
}

fn usable_search_snippet(
    snippet: Option<&str>,
    content: &str,
    query: &str,
    context_chars: usize,
) -> String {
    snippet
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| snippet_around_match(content, query, context_chars))
}

fn compact_search_preview(snippet: &str) -> String {
    snippet.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn query_term_keys(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .filter_map(|s| {
            let key: String = s
                .chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
                .to_lowercase();
            (!key.is_empty()).then_some(key)
        })
        .collect()
}

/// Bold query tokens in a compacted preview. `color` is explicit so tests do
/// not depend on whether the test runner's stdout is a TTY.
pub fn emphasize_search_preview(preview: &str, query: &str, color: bool) -> String {
    if !color || preview.is_empty() {
        return preview.to_string();
    }
    let terms = query_term_keys(query);
    if terms.is_empty() {
        return preview.to_string();
    }
    preview
        .split(' ')
        .map(|tok| {
            let key: String = tok
                .chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
                .to_lowercase();
            if !key.is_empty() && terms.iter().any(|term| term == &key) {
                format!("\x1b[1m{tok}\x1b[0m")
            } else {
                tok.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn format_search_preview(snippet: &str, query: &str) -> String {
    // Search previews stay on one logical line; inspect/view retain the
    // original formatting, as does structured search output.
    emphasize_search_preview(&compact_search_preview(&tty(snippet)), query, *USE_COLOR)
}

fn print_hit_details(message: &Message, indent: &str) {
    if !message.tool_uses.is_empty() {
        let tools: String = message
            .tool_uses
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        println!("{indent}{}tools: {}{}", c!("dim"), tty(&tools), c!("reset"));
    }
    if !message.files_referenced.is_empty() {
        let files: String = message
            .files_referenced
            .iter()
            .take(3)
            .map(|f| abbreviate_home(f))
            .collect::<Vec<_>>()
            .join(", ");
        println!("{indent}{}files: {}{}", c!("dim"), tty(&files), c!("reset"));
    }
}

pub fn print_search_results_json(results: &[SearchResult], query: &str) {
    let items: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            serde_json::json!({
                "session_id": r.session.id,
                "source": r.session.source,
                "also_ide": r.session.also_ide,
                "metadata_only": r.session.is_cursor_store_only(),
                "date": r.session.date,
                "summary": r.session.summary,
                "project": r.session.project,
                "score": r.message.final_score,
                "role": r.message.role,
                "ordinal": r.ordinal,
                "timestamp": r.message.timestamp,
                "snippet": usable_search_snippet(r.snippet.as_deref(), &r.message.content, query, 300),
                "tools": r.message.tool_uses,
                "files": r.message.files_referenced,
                "additional_matches": r.additional_matches.iter().map(|hit| serde_json::json!({
                    "score": hit.message.final_score,
                    "role": hit.message.role,
                    "ordinal": hit.ordinal,
                    "timestamp": hit.message.timestamp,
                    "snippet": usable_search_snippet(hit.snippet.as_deref(), &hit.message.content, query, 300),
                    "tools": hit.message.tool_uses,
                    "files": hit.message.files_referenced,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let out = serde_json::json!({ "query": query, "count": items.len(), "results": items });
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
}

pub fn print_index_results_json(results: &[IndexResult], query: &str) {
    let items: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            serde_json::json!({
                "session_id": r.session.id,
                "source": r.session.source,
                "also_ide": r.session.also_ide,
                "metadata_only": r.session.is_cursor_store_only(),
                "date": r.session.date,
                "summary": r.session.summary,
                "project": r.session.project,
                "score": (r.score * 10.0).round() / 10.0,
                "matched_field": r.matched_field,
                "snippet": clean_prompt(&r.display).chars().take(200).collect::<String>(),
            })
        })
        .collect();
    let out = serde_json::json!({ "query": query, "count": items.len(), "results": items, "search_type": "index" });
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
}

pub fn print_inspect(info: &InspectInfo) {
    let tag = src_tag(&info.source, info.also_ide);
    let cleaned = tty(&display_title(&info.summary, 120));
    let summary = if cleaned.is_empty() {
        "(no summary)"
    } else {
        &cleaned
    };
    println!("\n{}", "─".repeat(80));
    println!("  {}  {}{}{}", tag, c!("bold"), summary, c!("reset"));
    let cwd = if info.project.is_empty() {
        "-".to_string()
    } else {
        tty(&abbreviate_home(&info.project))
    };
    println!(
        "  {}id: {}{}",
        c!("dim"),
        tty(&info.session_id),
        c!("reset")
    );
    println!(
        "  {}date: {}  cwd: {}  branch: {}{}",
        c!("dim"),
        tty(&info.date),
        cwd,
        if info.branch.is_empty() {
            "-".to_string()
        } else {
            tty(&info.branch)
        },
        c!("reset")
    );
    let model_str = if info.model.is_empty() {
        String::new()
    } else {
        format!("  model: {}", info.model)
    };
    let token_str = if info.total_tokens == 0 {
        String::new()
    } else {
        format!("  tokens: {}", info.total_tokens)
    };
    let tool_str = if info.tool_results == 0 {
        String::new()
    } else {
        format!(", {} tool results", info.tool_results)
    };
    println!(
        "  {}duration: {}min  messages: {} ({} user, {} assistant{tool_str}){}{}{}",
        c!("dim"),
        info.duration_minutes,
        info.message_count,
        info.user_messages,
        info.assistant_messages,
        model_str,
        token_str,
        c!("reset")
    );
    println!("{}\n", "─".repeat(80));

    if !info.tools_used.is_empty() {
        println!("  {}{}Tools Used:{}", c!("cyan"), c!("bold"), c!("reset"));
        for t in &info.tools_used {
            println!("    • {}", tty(t));
        }
        println!();
    }
    if !info.files_modified.is_empty() {
        println!(
            "  {}{}Files Touched:{}",
            c!("green"),
            c!("bold"),
            c!("reset")
        );
        for f in &info.files_modified {
            println!("    • {}", tty(&abbreviate_home(f)));
        }
        println!();
    }
    if !info.accomplishments.is_empty() {
        println!(
            "  {}{}Accomplishments:{}",
            c!("yellow"),
            c!("bold"),
            c!("reset")
        );
        for a in &info.accomplishments {
            println!("    ✓ {}", tty(a));
        }
        println!();
    }
    if !info.decisions.is_empty() {
        println!(
            "  {}{}Key Decisions:{}",
            c!("magenta"),
            c!("bold"),
            c!("reset")
        );
        for d in &info.decisions {
            println!("    → {}", tty(d));
        }
        println!();
    }
    if !info.errors.is_empty() {
        println!(
            "  {}{}Errors Encountered:{}",
            c!("red"),
            c!("bold"),
            c!("reset")
        );
        for e in &info.errors {
            let truncated: String = e.chars().take(100).collect();
            println!("    ✗ {}", tty(&truncated));
        }
        println!();
    }
}

pub fn print_transcript(
    messages: &[Message],
    session: &Session,
    show_tools: bool,
    opts: &ViewOptions,
) {
    let tag = src_tag(&session.source, session.also_ide);
    let cleaned = title_of(&session.summary, &session.first_prompt, 120);
    let summary = if cleaned == "(untitled)" {
        "(no summary)"
    } else {
        &cleaned
    };
    println!("\n{}", "─".repeat(80));
    println!("  {}  {}{}{}", tag, c!("bold"), summary, c!("reset"));
    let cwd = if session.project.is_empty() {
        "-".to_string()
    } else {
        abbreviate_home(&session.project)
    };
    println!(
        "  {}id: {}  date: {}  branch: {}  cwd: {}{}",
        c!("dim"),
        session.id,
        session.date,
        if session.branch.is_empty() {
            "-"
        } else {
            &session.branch
        },
        cwd,
        c!("reset")
    );
    println!("{}\n", "─".repeat(80));
    if messages.is_empty() {
        println!(
            "  {}(no messages — transcript may be expired){}",
            c!("dim"),
            c!("reset")
        );
        return;
    }
    for slot in opts.slots(messages) {
        let Slot::Message(ordinal) = slot else {
            println!("  {}…{}\n", c!("dim"), c!("reset"));
            continue;
        };
        let msg = &messages[ordinal];
        let number = if opts.numbered() {
            format!("  {}#{ordinal}{}", c!("dim"), c!("reset"))
        } else {
            String::new()
        };
        let (color, name) = role_style(&msg.role);
        println!("{}{}▌ {name}{}{number}", c!(color), c!("bold"), c!("reset"));
        if show_tools && !msg.tool_uses.is_empty() {
            println!(
                "  {}tools: {}{}",
                c!("dim"),
                tty(&msg.tool_uses.join(", ")),
                c!("reset")
            );
        }
        let text = tty(&opts.clip(&view_text(msg)));
        for line in text.lines() {
            println!("  {line}");
        }
        println!();
    }
}

pub fn print_plain(messages: &[Message], opts: &ViewOptions) {
    for slot in opts.slots(messages) {
        let Slot::Message(ordinal) = slot else {
            println!("…\n");
            continue;
        };
        let msg = &messages[ordinal];
        let role = match msg.role.as_str() {
            "user" => "You",
            "tool" => "Tool",
            _ => "Claude",
        };
        let text = tty(&opts.clip(&view_text(msg)));
        if opts.numbered() {
            // A numbered view accounts for every ordinal: `…` alone marks
            // skipped messages, so an empty one is shown, not dropped.
            let text = if text.trim().is_empty() {
                empty_stub(msg)
            } else {
                text
            };
            println!("[#{ordinal}] {role}: {text}\n");
        } else if !text.trim().is_empty() {
            println!("{role}: {text}\n");
        }
    }
}

/// Stands in for a message with no text (e.g. only a thinking block).
fn empty_stub(msg: &Message) -> String {
    if msg.tool_uses.is_empty() {
        "(no text)".to_owned()
    } else {
        format!("(no text; tools: {})", msg.tool_uses.join(", "))
    }
}

/// The text `view` shows for a message (and `--grep` matches against).
fn view_text(msg: &Message) -> String {
    if msg.role == "user" {
        clean_prompt(&msg.content)
    } else {
        msg.content.clone()
    }
}

/// Which messages `view` prints. Ordinals are positions in the parsed
/// transcript, the same numbers search hits report, so an agent can read a
/// bounded slice around a hit instead of piping the whole transcript.
#[derive(Default)]
pub struct ViewOptions {
    pub around: Option<usize>,
    /// Messages on each side; defaults to 2 for --around and 0 for --grep.
    pub context: Option<usize>,
    pub grep: Option<regex::Regex>,
    pub head: Option<usize>,
    pub tail: Option<usize>,
    pub number: bool,
    pub max_chars: Option<usize>,
    /// Only messages with these roles (`user`, `assistant`, `tool`); empty
    /// means all. Context and --head/--tail count the selected messages.
    pub roles: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Slot {
    Message(usize),
    /// Messages were skipped between the neighbouring slots.
    Gap,
}

impl ViewOptions {
    fn bounded(&self) -> bool {
        self.around.is_some()
            || self.grep.is_some()
            || self.head.is_some()
            || self.tail.is_some()
            || !self.roles.is_empty()
    }

    /// Numbering is opt-in for a full view (scripts parse today's format)
    /// and always on for a bounded one, whose output is new.
    pub fn numbered(&self) -> bool {
        self.number || self.bounded()
    }

    /// Rejects an ordinal the transcript does not have, naming the valid range.
    pub fn check(&self, messages: &[Message]) -> Result<(), String> {
        match self.around {
            Some(n) if messages.is_empty() => Err(format!(
                "message #{n} does not exist; this transcript has no messages"
            )),
            Some(n) if n >= messages.len() => Err(format!(
                "message #{n} does not exist; this transcript has messages 0..={}",
                messages.len() - 1
            )),
            _ => Ok(()),
        }
    }

    pub fn slots(&self, messages: &[Message]) -> Vec<Slot> {
        let context = self
            .context
            .unwrap_or(if self.around.is_some() { 2 } else { 0 });
        // Ordinals of the messages --role selects. Windows are ranges of this
        // list, so context counts selected messages, not skipped ones.
        let pool = self.selected(messages);
        let window =
            |k: usize| k.saturating_sub(context)..k.saturating_add(context + 1).min(pool.len());
        // --head/--tail keep the first/last N items: matches (each with its
        // context) under --grep, so a match is never cut off; else messages.
        let limit = |items: &mut Vec<usize>| {
            if let Some(n) = self.head {
                items.truncate(n);
            }
            if let Some(n) = self.tail {
                items.drain(..items.len().saturating_sub(n));
            }
        };
        let picked: Vec<usize> = if let Some(n) = self.around.filter(|&n| n < messages.len()) {
            // Message n itself is shown only when selected; either way the
            // window reaches `context` selected messages on each side.
            let at = pool.partition_point(|&i| i < n);
            let after = if pool.get(at) == Some(&n) { at + 1 } else { at };
            let mut picked = pool
                [at.saturating_sub(context)..after.saturating_add(context).min(pool.len())]
                .to_vec();
            limit(&mut picked);
            picked
        } else if self.grep.is_some() {
            let mut matches: Vec<usize> = self
                .matches(messages)
                .into_iter()
                .filter_map(|i| pool.binary_search(&i).ok())
                .collect();
            limit(&mut matches);
            let mut keep = vec![false; pool.len()];
            for k in matches {
                for j in window(k) {
                    keep[j] = true;
                }
            }
            (0..pool.len())
                .filter(|&k| keep[k])
                .map(|k| pool[k])
                .collect()
        } else if self.around.is_some() {
            Vec::new()
        } else {
            let mut picked = pool;
            limit(&mut picked);
            picked
        };
        let mut slots = Vec::with_capacity(picked.len());
        for (k, &i) in picked.iter().enumerate() {
            if k > 0 && picked[k - 1] + 1 != i {
                slots.push(Slot::Gap);
            }
            slots.push(Slot::Message(i));
        }
        slots
    }

    /// Ordinals of the messages --role selects, in order.
    fn selected(&self, messages: &[Message]) -> Vec<usize> {
        (0..messages.len())
            .filter(|&i| self.roles.is_empty() || self.roles.contains(&messages[i].role))
            .collect()
    }

    /// Ordinals of the selected messages --grep matches.
    pub fn matches(&self, messages: &[Message]) -> Vec<usize> {
        let Some(pattern) = &self.grep else {
            return Vec::new();
        };
        self.selected(messages)
            .into_iter()
            .filter(|&i| pattern.is_match(&view_text(&messages[i])))
            .collect()
    }

    /// Cuts a message to `max_chars` characters and says how much was left
    /// out. With --grep the kept window starts a little before the first
    /// match, so a match deep inside a long message is not cut away.
    fn clip(&self, text: &str) -> String {
        let Some(max) = self.max_chars else {
            return text.to_owned();
        };
        let total = text.chars().count();
        if total <= max {
            return text.to_owned();
        }
        let first_match = self
            .grep
            .as_ref()
            .and_then(|pattern| pattern.find(text))
            .map(|m| text[..m.start()].chars().count());
        let start = first_match
            .map(|at| at.saturating_sub(max / 4).min(total - max))
            .unwrap_or(0);
        let kept: String = text.chars().skip(start).take(max).collect();
        let before = if start > 0 {
            format!("[-{start} chars] … ")
        } else {
            String::new()
        };
        let rest = total - start - max;
        let after = if rest > 0 {
            format!(" … [+{rest} chars]")
        } else {
            String::new()
        };
        format!("{before}{kept}{after}")
    }
}

/// IDE Composer chats have no CLI resume. Tell the user how to open it.
pub fn cursor_ide_resume_hint(session: &Session) -> String {
    let title = title_of(&session.summary, &session.first_prompt, 100);
    if session.project.is_empty() {
        return format!(
            "You need to use the Cursor IDE UI to find this session.\n\
             This chat has no recorded workspace directory.\n\
             IDE chats cannot be resumed from the CLI.\n\
             \n\
             Title: {title}\n\
             Look for it in the Cursor sidebar after opening a related project.\n"
        );
    }
    let dir = tty(&abbreviate_home(&session.project));
    format!(
        "You need to use the Cursor IDE UI in the directory {dir} to find this session.\n\
         IDE chats cannot be resumed from the CLI.\n\
         \n\
         Title: {title}\n\
         Session ID: {}\n\
         Open that folder in Cursor and look for the chat in the sidebar.\n",
        session.id
    )
}

pub fn export_transcript(messages: &[Message], session: &Session, out_path: Option<&str>) -> bool {
    let summary = if session.summary.is_empty() {
        "(no summary)"
    } else {
        &session.summary
    };
    let mut lines = Vec::new();
    lines.push(format!("# {summary}\n"));
    lines.push(format!("- **Source:** {}", session.source));
    lines.push(format!("- **Date:** {}", session.date));
    lines.push(format!(
        "- **Branch:** {}",
        if session.branch.is_empty() {
            "-"
        } else {
            &session.branch
        }
    ));
    lines.push(format!(
        "- **Directory:** {}",
        if session.project.is_empty() {
            "-"
        } else {
            &session.project
        }
    ));
    lines.push(format!("- **Session ID:** {}\n\n---\n", session.id));
    for msg in messages {
        let role = role_style(&msg.role).1;
        let text = if msg.role == "user" {
            clean_prompt(&msg.content)
        } else {
            msg.content.clone()
        };
        lines.push(format!("## {role}\n\n{text}\n"));
    }
    let content = lines.join("\n");
    let path = out_path.map(String::from).unwrap_or_else(|| {
        let safe: String = summary
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
            .take(50)
            .collect();
        format!("{}_{safe}.md", session.date)
    });
    match std::fs::write(&path, &content) {
        Ok(_) => {
            println!("Exported to {path}");
            true
        }
        Err(e) => {
            eprintln!("Error writing {path}: {e}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::abbreviate_home_with;
    use super::compact_search_preview;
    use super::cursor_ide_resume_hint;
    use super::emphasize_search_preview;
    use crate::session::Session;

    #[test]
    fn ide_resume_hint_includes_directory() {
        let session = Session {
            source: "cursor-ide".into(),
            id: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into(),
            summary: "git branch analysis".into(),
            first_prompt: String::new(),
            created: String::new(),
            modified: String::new(),
            date: "2026-08-26".into(),
            messages: 0,
            branch: String::new(),
            project: "/home/alice/src/myapp".into(),
            file: String::new(),
            is_sidechain: false,
            also_ide: false,
        };
        let hint = cursor_ide_resume_hint(&session);
        assert!(hint.contains("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"));
        assert!(hint.contains("Cursor IDE UI"));
        assert!(hint.contains("/home/alice/src/myapp") || hint.contains("myapp"));
        assert!(hint.contains("git branch analysis"));
        assert!(hint.contains("sidebar"));
    }

    #[test]
    fn ide_resume_hint_omits_unknown_directory() {
        let session = Session {
            source: "cursor-ide".into(),
            id: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into(),
            summary: "untitled".into(),
            first_prompt: String::new(),
            created: String::new(),
            modified: String::new(),
            date: "2026-08-26".into(),
            messages: 1,
            branch: String::new(),
            project: String::new(),
            file: String::new(),
            is_sidechain: false,
            also_ide: false,
        };
        let hint = cursor_ide_resume_hint(&session);
        assert!(!hint.contains("(unknown directory)"));
        assert!(hint.contains("no recorded workspace"));
    }

    #[test]
    fn src_tag_labels_agent_as_cursor_agent() {
        assert!(super::src_tag("cursor", false).contains("cursor-agent"));
        assert!(super::src_tag("cursor-ide", false).contains("cursor-ide"));
        assert!(super::src_tag("cursor", true).contains("cursor-ide"));
        assert!(!super::src_tag("cursor", false).contains("cursor-ide"));
    }

    #[test]
    fn id_chip_shows_prefix_for_ide_rows_too() {
        let ide = Session {
            source: "cursor-ide".into(),
            id: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into(),
            summary: "sidebar title".into(),
            first_prompt: String::new(),
            created: String::new(),
            modified: String::new(),
            date: "2026-08-26".into(),
            messages: 2,
            branch: String::new(),
            project: "/tmp".into(),
            file: String::new(),
            is_sidechain: false,
            also_ide: false,
        };
        let mut agent = Session {
            source: "cursor".into(),
            id: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into(),
            ..ide.clone()
        };
        assert!(super::id_chip(&ide).contains("aaaaaaaa"));
        assert!(super::id_chip(&agent).contains("aaaaaaaa"));
        agent.also_ide = true;
        assert!(super::src_tag("cursor", true).contains("cursor-ide"));
        assert!(super::id_chip(&agent).contains("aaaaaaaa"));
        assert!(!super::id_chip(&agent).contains("--------"));
    }

    #[test]
    fn abbreviates_linux_home() {
        assert_eq!(
            abbreviate_home_with("/home/alice/src/app", Some("/home/alice")),
            "~/src/app"
        );
    }

    #[test]
    fn abbreviates_macos_home() {
        assert_eq!(
            abbreviate_home_with("/Users/alex/src/app", Some("/Users/alex")),
            "~/src/app"
        );
    }

    #[test]
    fn abbreviates_home_itself() {
        assert_eq!(
            abbreviate_home_with("/home/alice", Some("/home/alice")),
            "~"
        );
        assert_eq!(
            abbreviate_home_with("/home/alice/", Some("/home/alice")),
            "~"
        );
    }

    #[test]
    fn leaves_unrelated_paths_alone() {
        assert_eq!(
            abbreviate_home_with("/opt/tools", Some("/home/alice")),
            "/opt/tools"
        );
        assert_eq!(
            abbreviate_home_with("/home/alice-other/x", Some("/home/alice")),
            "/home/alice-other/x"
        );
    }

    #[test]
    fn abbreviates_windows_home() {
        assert_eq!(
            abbreviate_home_with(r"C:\Users\alex\proj", Some(r"C:\Users\alex")),
            "~/proj"
        );
    }

    #[test]
    fn compact_search_preview_collapses_newlines() {
        assert_eq!(
            compact_search_preview("alpha\n\n  uniquecli\tbeta"),
            "alpha uniquecli beta"
        );
    }

    #[test]
    fn empty_search_snippet_falls_back_to_message_text() {
        let content = "Service returned uniquecli during credential validation.";
        assert_eq!(
            super::usable_search_snippet(Some("   "), content, "uniquecli", 200),
            crate::parser::snippet_around_match(content, "uniquecli", 200)
        );
        assert_eq!(
            super::usable_search_snippet(Some("matched uniquecli here"), content, "uniquecli", 200),
            "matched uniquecli here"
        );
    }

    #[test]
    fn emphasize_search_preview_wraps_exact_query_tokens() {
        let marked =
            emphasize_search_preview("Fix uniquecli login in Waltham", "uniquecli WAL", true);
        assert!(marked.contains("\x1b[1muniquecli\x1b[0m"), "{marked}");
        assert!(
            !marked.contains("\x1b[1mWaltham\x1b[0m"),
            "must not treat WAL as a prefix of Waltham: {marked}"
        );
        assert_eq!(
            emphasize_search_preview("Fix uniquecli login", "uniquecli", false),
            "Fix uniquecli login"
        );
    }

    #[test]
    fn titles_and_previews_drop_terminal_controls() {
        let injected = "hi\u{1b}]0;evil title\u{7} there";
        assert_eq!(super::title_of(injected, "", 100), "hi there");
        let preview = super::format_search_preview("x\u{1b}]52;c;AAAA\u{7}secret", "needle");
        assert!(!preview.contains("]52"), "{preview:?}");
        assert!(!preview.contains('\u{7}'), "{preview:?}");
        assert!(preview.contains("xsecret"), "{preview}");
    }
}
