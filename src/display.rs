use crate::inspect::InspectInfo;
use crate::parser::{clean_prompt, snippet_around_match};
use crate::search::{IndexResult, SearchResult};
use crate::session::{Message, Session};
use std::collections::BTreeMap;

fn is_color() -> bool {
    std::io::IsTerminal::is_terminal(&std::io::stdout()) && std::env::var("NO_COLOR").is_err()
}

macro_rules! c {
    ($name:expr) => {
        if is_color() {
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
                "bg_cyan" => "\x1b[46m",
                _ => "",
            }
        } else {
            ""
        }
    };
}

fn src_tag(source: &str) -> String {
    if source == "claude" {
        format!("{}{} CC {}", c!("bg_cyan"), c!("bold"), c!("reset"))
    } else {
        format!("{}{} CR {}", c!("bg_blue"), c!("bold"), c!("reset"))
    }
}

pub fn print_list(sessions: &[Session], verbose: bool) {
    if sessions.is_empty() {
        println!("{}No sessions found.{}", c!("dim"), c!("reset"));
        return;
    }
    println!("\n{}{} sessions{}\n", c!("bold"), sessions.len(), c!("reset"));
    for (i, s) in sessions.iter().enumerate() {
        let tag = src_tag(&s.source);
        let summary = if !s.summary.is_empty() {
            s.summary.chars().take(100).collect::<String>()
        } else if !s.first_prompt.is_empty() {
            clean_prompt(&s.first_prompt).chars().take(100).collect::<String>()
        } else {
            "(empty)".into()
        };
        let branch = if s.branch.is_empty() {
            String::new()
        } else {
            format!(" {}({}){}", c!("magenta"), s.branch, c!("reset"))
        };
        let msgs = if s.messages > 0 {
            format!(" {}[{} msgs]{}", c!("dim"), s.messages, c!("reset"))
        } else {
            String::new()
        };
        println!(
            "  {}{:3}.{} {} {}{}{}  {}{}{}{}{}",
            c!("dim"), i + 1, c!("reset"), tag, c!("cyan"), s.date, c!("reset"),
            c!("bold"), summary, c!("reset"), branch, msgs
        );
        if verbose {
            println!("       {}id: {}{}", c!("dim"), s.id, c!("reset"));
            println!("       {}file: {}{}", c!("dim"), s.file, c!("reset"));
        }
    }
    println!();
}

pub fn print_summarized(sessions: &[Session]) {
    if sessions.is_empty() {
        println!("{}No sessions found.{}", c!("dim"), c!("reset"));
        return;
    }
    let mut by_day: BTreeMap<&str, Vec<&Session>> = BTreeMap::new();
    for s in sessions {
        by_day.entry(&s.date).or_default().push(s);
    }
    println!(
        "\n{}{} sessions across {} days{}\n",
        c!("bold"), sessions.len(), by_day.len(), c!("reset")
    );
    for (day, ds) in by_day.iter().rev() {
        println!(
            "  {}{}{}{}  {}({} sessions){}",
            c!("cyan"), c!("bold"), day, c!("reset"), c!("dim"), ds.len(), c!("reset")
        );
        for s in ds {
            let title = if !s.summary.is_empty() {
                s.summary.chars().take(80).collect::<String>()
            } else if !s.first_prompt.is_empty() {
                clean_prompt(&s.first_prompt).chars().take(80).collect::<String>()
            } else {
                "(empty)".into()
            };
            let branch = if s.branch.is_empty() {
                String::new()
            } else {
                format!(" {}({}){}", c!("magenta"), s.branch, c!("reset"))
            };
            println!("    {} {}{}", src_tag(&s.source), title, branch);
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
        c!("bold"), results.len(), query, c!("reset"), c!("dim"), c!("reset")
    );
    for (i, r) in results.iter().enumerate() {
        let tag = src_tag(&r.session.source);
        let score = format!("{}★ {:.1}{}", c!("yellow"), r.score, c!("reset"));
        let field = format!("{} [{}]{}", c!("dim"), r.matched_field, c!("reset"));
        let summary_or_display = if !r.session.summary.is_empty() {
            r.session.summary.chars().take(80).collect::<String>()
        } else {
            r.display.chars().take(80).collect::<String>()
        };
        let sid_short: String = r.session.id.chars().take(8).collect();
        let branch = if r.session.branch.is_empty() {
            String::new()
        } else {
            format!(" {}({}){}", c!("magenta"), r.session.branch, c!("reset"))
        };
        println!(
            "  {}{:3}.{} {} {}{}{} {}{} {}{}{}{}  {}[{}]{}",
            c!("dim"), i + 1, c!("reset"), tag, c!("cyan"), r.session.date,
            c!("reset"), score, field, c!("bold"), summary_or_display,
            c!("reset"), branch, c!("dim"), sid_short, c!("reset")
        );
    }
    println!();
}

pub fn print_search_results(results: &[SearchResult], query: &str) {
    if results.is_empty() {
        println!("{}No results for \"{}\".{}", c!("dim"), query, c!("reset"));
        return;
    }
    println!("\n{}{} results for \"{}\"{}\n", c!("bold"), results.len(), query, c!("reset"));
    for (i, r) in results.iter().enumerate() {
        let tag = src_tag(&r.session.source);
        let score = format!("{}★ {:.1}{}", c!("yellow"), r.message.final_score, c!("reset"));
        let sid_short: String = r.session.id.chars().take(8).collect();
        let role_str = if r.message.role == "user" {
            format!("{}You{}", c!("green"), c!("reset"))
        } else {
            format!("{}Assistant{}", c!("blue"), c!("reset"))
        };
        println!(
            "  {}{:3}.{} {} {}{}{} {}  {}{}{}  {}[{}]{}",
            c!("dim"), i + 1, c!("reset"), tag, c!("cyan"), r.session.date, c!("reset"),
            score, c!("bold"), r.session.summary, c!("reset"),
            c!("dim"), sid_short, c!("reset")
        );
        let snippet = snippet_around_match(&r.message.content, query, 200);
        println!("       {}: {}", role_str, snippet);
        if !r.message.tool_uses.is_empty() {
            let tools: String = r.message.tool_uses.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
            println!("       {}tools: {}{}", c!("dim"), tools, c!("reset"));
        }
        if !r.message.files_referenced.is_empty() {
            let files: String = r.message.files_referenced.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
            println!("       {}files: {}{}", c!("dim"), files, c!("reset"));
        }
        if !r.message.error_patterns.is_empty() {
            let err: String = r.message.error_patterns[0].chars().take(80).collect();
            println!("       {}errors: {}{}", c!("red"), err, c!("reset"));
        }
        println!();
    }
}

pub fn print_search_results_json(results: &[SearchResult], query: &str) {
    let items: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            serde_json::json!({
                "session_id": r.session.id,
                "source": r.session.source,
                "date": r.session.date,
                "summary": r.session.summary,
                "project": r.session.project,
                "score": (r.message.final_score * 10.0).round() / 10.0,
                "role": r.message.role,
                "snippet": snippet_around_match(&r.message.content, query, 300),
                "tools": r.message.tool_uses,
                "files": r.message.files_referenced,
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
    let tag = src_tag(&info.source);
    let summary = if info.summary.is_empty() { "(no summary)" } else { &info.summary };
    println!("\n{}", "─".repeat(80));
    println!("  {}  {}{}{}", tag, c!("bold"), summary, c!("reset"));
    println!("  {}id: {}{}", c!("dim"), info.session_id, c!("reset"));
    println!(
        "  {}date: {}  project: {}  branch: {}{}",
        c!("dim"), info.date, info.project,
        if info.branch.is_empty() { "-" } else { &info.branch },
        c!("reset")
    );
    let model_str = if info.model.is_empty() { String::new() } else { format!("  model: {}", info.model) };
    let token_str = if info.total_tokens == 0 { String::new() } else { format!("  tokens: {}", info.total_tokens) };
    println!(
        "  {}duration: {}min  messages: {} ({} user, {} assistant){}{}{}",
        c!("dim"), info.duration_minutes, info.message_count,
        info.user_messages, info.assistant_messages, model_str, token_str, c!("reset")
    );
    println!("{}\n", "─".repeat(80));

    if !info.tools_used.is_empty() {
        println!("  {}{}Tools Used:{}", c!("cyan"), c!("bold"), c!("reset"));
        for t in &info.tools_used { println!("    • {t}"); }
        println!();
    }
    if !info.files_modified.is_empty() {
        println!("  {}{}Files Touched:{}", c!("green"), c!("bold"), c!("reset"));
        for f in &info.files_modified { println!("    • {f}"); }
        println!();
    }
    if !info.accomplishments.is_empty() {
        println!("  {}{}Accomplishments:{}", c!("yellow"), c!("bold"), c!("reset"));
        for a in &info.accomplishments { println!("    ✓ {a}"); }
        println!();
    }
    if !info.decisions.is_empty() {
        println!("  {}{}Key Decisions:{}", c!("magenta"), c!("bold"), c!("reset"));
        for d in &info.decisions { println!("    → {d}"); }
        println!();
    }
    if !info.errors.is_empty() {
        println!("  {}{}Errors Encountered:{}", c!("red"), c!("bold"), c!("reset"));
        for e in &info.errors {
            let truncated: String = e.chars().take(100).collect();
            println!("    ✗ {truncated}");
        }
        println!();
    }
}

pub fn print_transcript(messages: &[Message], session: &Session, show_tools: bool) {
    let tag = src_tag(&session.source);
    let summary = if session.summary.is_empty() { "(no summary)" } else { &session.summary };
    println!("\n{}", "─".repeat(80));
    println!("  {}  {}{}{}", tag, c!("bold"), summary, c!("reset"));
    println!(
        "  {}id: {}  date: {}  branch: {}  project: {}{}",
        c!("dim"), session.id, session.date,
        if session.branch.is_empty() { "-" } else { &session.branch },
        if session.project.is_empty() { "-" } else { &session.project },
        c!("reset")
    );
    println!("{}\n", "─".repeat(80));
    if messages.is_empty() {
        println!("  {}(no messages — transcript may be expired){}", c!("dim"), c!("reset"));
        return;
    }
    for msg in messages {
        if msg.role == "user" {
            println!("{}{}▌ You{}", c!("green"), c!("bold"), c!("reset"));
        } else {
            println!("{}{}▌ Assistant{}", c!("blue"), c!("bold"), c!("reset"));
        }
        if show_tools && !msg.tool_uses.is_empty() {
            println!("  {}tools: {}{}", c!("dim"), msg.tool_uses.join(", "), c!("reset"));
        }
        let text = if msg.role == "user" {
            clean_prompt(&msg.content)
        } else {
            msg.content.clone()
        };
        for line in text.lines() {
            println!("  {line}");
        }
        println!();
    }
}

pub fn print_plain(messages: &[Message]) {
    for msg in messages {
        let role = if msg.role == "user" { "You" } else { "Claude" };
        let text = if msg.role == "user" {
            clean_prompt(&msg.content)
        } else {
            msg.content.clone()
        };
        if !text.trim().is_empty() {
            println!("{role}: {text}\n");
        }
    }
}

pub fn export_transcript(messages: &[Message], session: &Session, out_path: Option<&str>) {
    let summary = if session.summary.is_empty() { "(no summary)" } else { &session.summary };
    let mut lines = Vec::new();
    lines.push(format!("# {summary}\n"));
    lines.push(format!("- **Source:** {}", session.source));
    lines.push(format!("- **Date:** {}", session.date));
    lines.push(format!("- **Branch:** {}", if session.branch.is_empty() { "-" } else { &session.branch }));
    lines.push(format!("- **Project:** {}", if session.project.is_empty() { "-" } else { &session.project }));
    lines.push(format!("- **Session ID:** {}\n\n---\n", session.id));
    for msg in messages {
        let role = if msg.role == "user" { "You" } else { "Assistant" };
        let text = if msg.role == "user" { clean_prompt(&msg.content) } else { msg.content.clone() };
        lines.push(format!("## {role}\n\n{text}\n"));
    }
    let content = lines.join("\n");
    let path = out_path
        .map(String::from)
        .unwrap_or_else(|| {
            let safe: String = summary.chars().filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_').take(50).collect();
            format!("{}_{safe}.md", session.date)
        });
    match std::fs::write(&path, &content) {
        Ok(_) => println!("Exported to {path}"),
        Err(e) => eprintln!("Error writing {path}: {e}"),
    }
}
