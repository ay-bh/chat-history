use chat_history::dates::parse_human_date;
use chat_history::session::{
    self, ResumeAction, SessionLookup, filter_sessions, load_sessions, lookup_session,
    parse_session, session_copies,
};
use chat_history::skill_install::{ensure_skills, install_skill};
use chat_history::{display, inspect, scoring, search};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "chat-history",
    about = "Search Claude Code + Cursor + Codex conversation history",
    long_about = "Search Claude Code + Cursor + Codex conversation history.\n\n\
        With no command, lists sessions newest first; every row shows a short\n\
        session ID usable with inspect/view/export/find. resume works for\n\
        claude, codex and Cursor Agent CLI chats (ids with a store under\n\
        ~/.cursor/chats); other Cursor rows print how to open the chat in\n\
        the Cursor sidebar instead.",
    version,
    after_help = "EXAMPLES:\n  \
        chat-history                                  list sessions, newest first\n  \
        chat-history --from yesterday --to yesterday  sessions from a specific day\n  \
        chat-history search \"auth error\" --deep --json\n  \
        chat-history --source cursor-ide              IDE sidebar chats only\n  \
        chat-history inspect 6b1094cd                 summarize by short ID\n  \
        chat-history view 6b1094cd --plain | less\n\n\
        EXIT CODES:\n  \
        0 success, 1 not found / IO error, 2 usage error or ambiguous session ID"
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

    #[arg(
        long,
        global = true,
        value_parser = ["claude", "cursor", "cursor-agent", "cursor-ide", "codex"],
        help = "Filter by source"
    )]
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
    /// Search session content and metadata (add --deep --json from agents)
    Search {
        /// Search query, or a full session UUID for direct lookup
        query: String,
        /// What to search within transcripts
        #[arg(long, default_value = "all", value_parser = ["all", "errors", "similar", "tools", "files"])]
        scope: String,
        /// Search full transcript content instead of the fast index
        #[arg(long)]
        deep: bool,
        /// Maximum number of results
        #[arg(long, default_value_t = 15)]
        limit: usize,
        /// Only messages newer than this (e.g. "2 days", "1 week")
        #[arg(long)]
        timeframe: Option<String>,
        /// Structured JSON output (session_id, score, snippet, tools, files)
        #[arg(long = "json")]
        json_output: bool,
    },
    /// Summarize a session: accomplishments, tools, files, model, tokens
    Inspect {
        /// Session ID or unique prefix
        session_id: Option<String>,
        /// Inspect the most recent session
        #[arg(long)]
        last: bool,
    },
    /// Print a session transcript
    View {
        /// Session ID or unique prefix
        session_id: Option<String>,
        /// View the most recent session
        #[arg(long)]
        last: bool,
        /// Show tool call names inline
        #[arg(long)]
        tools: bool,
        /// Plain text without formatting, pipe-friendly
        #[arg(long)]
        plain: bool,
    },
    /// Export a session transcript as markdown
    Export {
        /// Session ID or unique prefix
        session_id: String,
        /// Output file (stdout if omitted)
        #[arg(short = 'o', long)]
        output: Option<String>,
    },
    /// Resume Claude Code, Codex, or a Cursor Agent CLI chat (IDE chats print how to open them)
    Resume {
        /// Session ID or unique prefix
        session_id: String,
    },
    /// Print the transcript file path for scripting
    Find {
        /// Session ID or unique prefix
        session_id: String,
    },
    /// Install the agent skill for Claude Code, Cursor, and Codex
    #[command(name = "install-skill")]
    InstallSkill {
        /// Overwrite existing skills even if they were user-edited
        #[arg(long)]
        force: bool,
    },
    /// Generate shell completions (bash, zsh, fish, elvish, powershell)
    Completions {
        /// Shell to generate completions for
        shell: clap_complete::Shell,
    },
}

/// Resolve a session id or prefix, or exit: candidates are listed on an
/// ambiguous prefix so a short id never silently picks the wrong session.
fn resolve_session_or_exit<'a>(
    sessions: &'a [session::Session],
    sid: &str,
) -> &'a session::Session {
    match lookup_session(sessions, sid) {
        SessionLookup::Found(s) => {
            let copies = session_copies(sessions, s);
            if copies.len() > 1 {
                eprintln!(
                    "Note: session {} is stored in {} project folders; using DIR: {}",
                    s.id,
                    copies.len(),
                    display::abbreviate_home(&s.project)
                );
                for c in copies {
                    if c.file == s.file {
                        continue;
                    }
                    eprintln!(
                        "  also: {}  {}",
                        display::abbreviate_home(&c.project),
                        display::abbreviate_home(&c.file)
                    );
                }
            }
            s
        }
        SessionLookup::Ambiguous(candidates) => {
            eprintln!("Session ID \"{sid}\" is ambiguous — it matches:");
            for s in &candidates {
                eprintln!(
                    "  {}  {}  {}  DIR: {}",
                    s.id,
                    s.date,
                    s.source,
                    display::abbreviate_home(&s.project)
                );
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
            std::process::exit(2);
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

    if let Some(Commands::Completions { shell }) = &cli.command {
        use clap::CommandFactory;
        // Use the invoked binary name so the `ch` alias gets working
        // completions too, not a `_chat-history` function it never triggers.
        let bin = std::env::args()
            .next()
            .as_deref()
            .map(std::path::Path::new)
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "chat-history".to_string());
        clap_complete::generate(*shell, &mut Cli::command(), bin, &mut std::io::stdout());
        return;
    }

    if let Some(Commands::InstallSkill { force }) = &cli.command {
        install_skill(*force);
        return;
    }

    // Auto-install / refresh managed agent skills on first use (and later
    // upgrades). Never overwrites user-edited skills.
    ensure_skills();

    let id_lookup = matches!(
        &cli.command,
        Some(Commands::Inspect {
            session_id: Some(_),
            last: false
        }) | Some(Commands::View {
            session_id: Some(_),
            last: false,
            ..
        }) | Some(Commands::Export { .. })
            | Some(Commands::Resume { .. })
            | Some(Commands::Find { .. })
    );
    let mut sessions = if id_lookup {
        load_sessions(None)
    } else {
        load_sessions(cli.source.as_deref())
    };
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
                std::process::exit(2);
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
                std::process::exit(2);
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
            if session.is_ide_ui() {
                print!("{}", display::cursor_ide_resume_hint(session));
                std::process::exit(1);
            }
            let action = match session::resume_command(session) {
                Some(a) => a,
                None if session.source == "cursor" => {
                    eprintln!(
                        "No Agent CLI chat store for this id under ~/.cursor/chats, so \
                         `agent --resume` cannot load it (it would start a blank chat)."
                    );
                    print!("{}", display::cursor_ide_resume_hint(session));
                    std::process::exit(1);
                }
                None => {
                    eprintln!("Resume is not supported for {} sessions.", session.source);
                    std::process::exit(1);
                }
            };
            if let ResumeAction::Print { cmdline } = action {
                eprintln!(
                    "Not running `cursor agent` (it can install the Agent CLI). Run:\n  {cmdline}"
                );
                std::process::exit(1);
            }
            let ResumeAction::Exec { bin, args } = action else {
                unreachable!();
            };
            println!(
                "Resuming: {}",
                if session.summary.is_empty() {
                    &session.id
                } else {
                    &session.summary
                }
            );
            let workdir = session::resume_working_dir(session);
            if workdir.is_none() && !session.project.is_empty() {
                if session.source == "claude" {
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
                } else {
                    eprintln!(
                        "Warning: session directory {} no longer exists; resuming from the current directory",
                        display::abbreviate_home(&session.project)
                    );
                }
            }
            use std::os::unix::process::CommandExt;
            let mut cmd = std::process::Command::new(&bin);
            cmd.args(&args);
            if let Some(dir) = &workdir {
                println!("cd {}", display::abbreviate_home(&dir.to_string_lossy()));
                cmd.current_dir(dir);
            }
            let err = cmd.exec();
            eprintln!("Failed to exec {bin}: {err}");
            std::process::exit(1);
        }
        Some(Commands::Find { session_id }) => {
            let session = resolve_session_or_exit(&sessions, &session_id);
            println!("{}", session.file);
        }
        Some(Commands::InstallSkill { .. }) | Some(Commands::Completions { .. }) => unreachable!(),
        None => {
            if cli.summarize {
                display::print_summarized(&filtered);
            } else {
                display::print_list(&filtered, cli.verbose);
            }
        }
    }
}
