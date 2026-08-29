use crate::inspect::InspectInfo;
use crate::parser::{clean_prompt, display_title, snippet_around_match};
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

fn dir_label(project: &str) -> String {
    labeled("DIR", &abbreviate_home(project))
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

fn print_title_line(title: &str) {
    println!("        {}{}{}", c!("bold"), title, c!("reset"));
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
        return t;
    }
    let t = display_title(first_prompt, max);
    if !t.is_empty() {
        return t;
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
        let branch = labeled("BRANCH", &s.branch);
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
        print_title_line(&title);
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
                labeled("BRANCH", &s.branch)
            );
            print_title_line(&title);
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
        print_title_line(&title);
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
            "{}★ {:.1}{}",
            c!("yellow"),
            r.message.final_score,
            c!("reset")
        );
        let role_str = if r.message.role == "user" {
            format!("{}You{}", c!("green"), c!("reset"))
        } else {
            format!("{}Assistant{}", c!("blue"), c!("reset"))
        };
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
        print_title_line(&title);
        let snippet = snippet_around_match(&r.message.content, query, 200);
        println!("        {}: {}", role_str, snippet);
        if !r.message.tool_uses.is_empty() {
            let tools: String = r
                .message
                .tool_uses
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            println!("       {}tools: {}{}", c!("dim"), tools, c!("reset"));
        }
        if !r.message.files_referenced.is_empty() {
            let files: String = r
                .message
                .files_referenced
                .iter()
                .take(3)
                .map(|f| abbreviate_home(f))
                .collect::<Vec<_>>()
                .join(", ");
            println!("       {}files: {}{}", c!("dim"), files, c!("reset"));
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
                "also_ide": r.session.also_ide,
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
                "also_ide": r.session.also_ide,
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
    let cleaned = display_title(&info.summary, 120);
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
        abbreviate_home(&info.project)
    };
    println!("  {}id: {}{}", c!("dim"), info.session_id, c!("reset"));
    println!(
        "  {}date: {}  cwd: {}  branch: {}{}",
        c!("dim"),
        info.date,
        cwd,
        if info.branch.is_empty() {
            "-"
        } else {
            &info.branch
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
    println!(
        "  {}duration: {}min  messages: {} ({} user, {} assistant){}{}{}",
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
            println!("    • {t}");
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
            println!("    • {}", abbreviate_home(f));
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
            println!("    ✓ {a}");
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
            println!("    → {d}");
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
            println!("    ✗ {truncated}");
        }
        println!();
    }
}

pub fn print_transcript(messages: &[Message], session: &Session, show_tools: bool) {
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
    for msg in messages {
        if msg.role == "user" {
            println!("{}{}▌ You{}", c!("green"), c!("bold"), c!("reset"));
        } else {
            println!("{}{}▌ Assistant{}", c!("blue"), c!("bold"), c!("reset"));
        }
        if show_tools && !msg.tool_uses.is_empty() {
            println!(
                "  {}tools: {}{}",
                c!("dim"),
                msg.tool_uses.join(", "),
                c!("reset")
            );
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
    let dir = abbreviate_home(&session.project);
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
        let role = if msg.role == "user" {
            "You"
        } else {
            "Assistant"
        };
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
    use super::cursor_ide_resume_hint;
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
}
