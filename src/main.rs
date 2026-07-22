use chat_history::dates::parse_human_date;
use chat_history::session::{
    self, SessionLookup, filter_sessions, load_all_sessions, lookup_session, parse_session,
};
use chat_history::skill_install::{ensure_skills, install_skill};
use chat_history::{display, inspect, scoring, search};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "chat-history",
    about = "Search Claude Code + Cursor + Codex conversation history",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(
        long = "from",
        global = true,
        help = "Start date (YYYY-MM-DD, today, yesterday, '3 days ago')"
    )]
    from_date: Option<String>,

    #[arg(long = "to", global = true, help = "End date")]
    to_date: Option<String>,

    #[arg(long, global = true, help = "Filter by source (claude/cursor/codex)")]
    source: Option<String>,

    #[arg(long, global = true, help = "Filter by project path substring")]
    project: Option<String>,

    #[arg(long, global = true, help = "Filter by git branch substring")]
    branch: Option<String>,

    #[arg(short = 'k', long, global = true, help = "Quick keyword filter")]
    keyword: Option<String>,

    #[arg(short = 's', long, help = "Group sessions by day")]
    summarize: bool,

    #[arg(short = 'v', long, help = "Show session IDs and file paths")]
    verbose: bool,

    #[arg(
        short = 'L',
        long = "local",
        global = true,
        help = "Only show sessions from current workspace"
    )]
    local: bool,

    #[arg(
        long,
        global = true,
        help = "Include subagent/sidechain sessions (hidden by default)"
    )]
    sidechains: bool,
}

#[derive(Subcommand)]
enum Commands {
    Search {
        query: String,
        #[arg(long, default_value = "all")]
        scope: String,
        #[arg(long)]
        deep: bool,
        #[arg(long, default_value_t = 15)]
        limit: usize,
        #[arg(long)]
        timeframe: Option<String>,
        #[arg(long = "json")]
        json_output: bool,
    },
    Inspect {
        session_id: Option<String>,
        #[arg(long)]
        last: bool,
    },
    View {
        session_id: Option<String>,
        #[arg(long)]
        last: bool,
        #[arg(long)]
        tools: bool,
        #[arg(long)]
        plain: bool,
    },
    Export {
        session_id: String,
        #[arg(short = 'o', long)]
        output: Option<String>,
    },
    Resume {
        session_id: String,
    },
    Find {
        session_id: String,
    },
    /// Install the agent skill for Claude Code, Cursor, and Codex
    #[command(name = "install-skill")]
    InstallSkill {
        /// Overwrite existing skills even if they were user-edited
        #[arg(long)]
        force: bool,
    },
}

/// Resolve a session id or prefix, or exit: candidates are listed on an
/// ambiguous prefix so a short id never silently picks the wrong session.
fn resolve_session_or_exit<'a>(
    sessions: &'a [session::Session],
    sid: &str,
) -> &'a session::Session {
    match lookup_session(sessions, sid) {
        SessionLookup::Found(s) => s,
        SessionLookup::Ambiguous(candidates) => {
            eprintln!("Session ID \"{sid}\" is ambiguous — it matches:");
            for s in &candidates {
                eprintln!("  {}  {}  {}", s.id, s.date, s.source);
            }
            eprintln!("Use a longer prefix.");
            std::process::exit(2);
        }
        SessionLookup::NotFound => {
            eprintln!("Session not found: {sid}");
            std::process::exit(1);
        }
    }
}

fn parse_date_arg(val: &Option<String>) -> Option<chrono::NaiveDate> {
    val.as_ref().and_then(|v| {
        parse_human_date(v).or_else(|| {
            eprintln!(
                "Invalid date: '{}'. Try: YYYY-MM-DD, today, yesterday, '3 days ago', 'last week'",
                v
            );
            std::process::exit(1);
        })
    })
}

fn main() {
    // Rust ignores SIGPIPE by default, turning writes to a closed pipe
    // (e.g. `chat-history ... | head`) into println! panics. Restore the
    // conventional Unix behavior of terminating quietly.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    let cli = Cli::parse();

    if let Some(Commands::InstallSkill { force }) = &cli.command {
        install_skill(*force);
        return;
    }

    // Auto-install / refresh managed agent skills on first use (and later
    // upgrades). Never overwrites user-edited skills.
    ensure_skills();

    let mut sessions = load_all_sessions();
    if !cli.sidechains {
        sessions.retain(|s| !s.is_sidechain);
    }
    let from_d = parse_date_arg(&cli.from_date);
    let to_d = parse_date_arg(&cli.to_date);

    let project_filter = if cli.local && cli.project.is_none() {
        std::env::current_dir()
            .ok()
            .and_then(|cwd| cwd.file_name().map(|n| n.to_string_lossy().to_string()))
    } else {
        cli.project.clone()
    };

    // The filter flags are global, so honor them everywhere they can apply:
    // search scoping, `--last` selection, and the default listing.
    let filtered = filter_sessions(
        &sessions,
        from_d,
        to_d,
        cli.keyword.as_deref(),
        cli.source.as_deref(),
        project_filter.as_deref(),
        cli.branch.as_deref(),
    );

    match cli.command {
        Some(Commands::Search {
            query,
            scope,
            deep,
            limit,
            timeframe,
            json_output,
        }) => {
            let pre = filtered;

            if !deep && scope == "all" && !scoring::is_uuid(&query) {
                let idx_results = search::index_search(&pre, &query, limit);
                if search::index_quality_ok(&idx_results) {
                    if json_output {
                        display::print_index_results_json(&idx_results, &query);
                    } else {
                        display::print_index_results(&idx_results, &query);
                    }
                    return;
                }
                if !json_output {
                    if !idx_results.is_empty() {
                        eprintln!(
                            "Index matches too weak (best: ★ {:.1}) — searching transcripts...",
                            idx_results[0].score
                        );
                    } else {
                        eprintln!("No index matches — searching transcripts...");
                    }
                }
            }

            if scoring::is_uuid(&query)
                && !json_output
                && !pre.iter().any(|s| s.id.eq_ignore_ascii_case(query.trim()))
            {
                eprintln!("No session with that ID — searching transcripts...");
            }
            let results = search::scored_search(&pre, &query, &scope, limit, timeframe.as_deref());
            if json_output {
                display::print_search_results_json(&results, &query);
            } else {
                display::print_search_results(&results, &query);
            }
        }
        Some(Commands::Inspect { session_id, last }) => {
            let session = if last {
                let Some(s) = filtered.iter().max_by_key(|s| session::recency_key(s)) else {
                    eprintln!("Session not found");
                    std::process::exit(1);
                };
                s
            } else if let Some(sid) = &session_id {
                resolve_session_or_exit(&sessions, sid)
            } else {
                eprintln!("Provide a session ID or use --last");
                std::process::exit(1);
            };
            match inspect::inspect_session(session) {
                Some(info) => display::print_inspect(&info),
                None => eprintln!("Could not inspect session (transcript may be expired)."),
            }
        }
        Some(Commands::View {
            session_id,
            last,
            tools,
            plain,
        }) => {
            let session = if last {
                let Some(s) = filtered.iter().max_by_key(|s| session::recency_key(s)) else {
                    eprintln!("Session not found");
                    std::process::exit(1);
                };
                s
            } else if let Some(sid) = &session_id {
                resolve_session_or_exit(&sessions, sid)
            } else {
                eprintln!("Provide a session ID or use --last");
                std::process::exit(1);
            };
            let (messages, _) = parse_session(session, false);
            if plain {
                display::print_plain(&messages);
            } else {
                display::print_transcript(&messages, session, tools);
            }
        }
        Some(Commands::Export { session_id, output }) => {
            let session = resolve_session_or_exit(&sessions, &session_id);
            let (messages, _) = parse_session(session, false);
            if !display::export_transcript(&messages, session, output.as_deref()) {
                std::process::exit(1);
            }
        }
        Some(Commands::Resume { session_id }) => {
            let session = resolve_session_or_exit(&sessions, &session_id);
            if session.source != "claude" && session.source != "codex" {
                eprintln!("Resume is only supported for Claude Code and Codex sessions.");
                std::process::exit(1);
            }
            println!(
                "Resuming: {}",
                if session.summary.is_empty() {
                    &session.id
                } else {
                    &session.summary
                }
            );
            if !session.project.is_empty() {
                let project = std::path::Path::new(&session.project);
                if project.is_dir() {
                    if let Err(e) = std::env::set_current_dir(project) {
                        eprintln!("Warning: could not cd to {}: {e}", session.project);
                    }
                } else if session.source == "claude" {
                    eprintln!(
                        "Project dir {} no longer exists, copying session to current directory...",
                        session.project
                    );
                    match std::env::current_dir() {
                        Ok(cwd) => {
                            let encoded = session::encode_path_for_claude(&cwd);
                            let target = session::claude_projects_dir().join(&encoded);
                            if let Err(e) = session::copy_session_to_dir(session, &target) {
                                eprintln!("Warning: failed to copy session files: {e}");
                            }
                        }
                        Err(e) => {
                            eprintln!(
                                "Warning: could not determine current directory; skipping session copy: {e}"
                            );
                        }
                    }
                }
            }
            use std::os::unix::process::CommandExt;
            let (bin, args) = if session.source == "codex" {
                ("codex", ["resume", session.id.as_str()])
            } else {
                ("claude", ["--resume", session.id.as_str()])
            };
            let err = std::process::Command::new(bin).args(args).exec();
            eprintln!("Failed to exec {bin}: {err}");
            std::process::exit(1);
        }
        Some(Commands::Find { session_id }) => {
            let session = resolve_session_or_exit(&sessions, &session_id);
            println!("{}", session.file);
        }
        Some(Commands::InstallSkill { .. }) => unreachable!(),
        None => {
            if cli.summarize {
                display::print_summarized(&filtered);
            } else {
                display::print_list(&filtered, cli.verbose);
            }
        }
    }
}
