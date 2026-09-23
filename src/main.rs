use chat_history::dates::parse_human_date;
use chat_history::search_index::{self, LegacyBackend, SearchBackend, SearchRequest};
use chat_history::session::{
    self, ResumeAction, SessionLookup, filter_sessions, load_sessions, lookup_session,
    parse_session, session_copies,
};
use chat_history::skill_install::{ensure_skills, install_skill};
use chat_history::{display, inspect, scoring, search};
use clap::{Parser, Subcommand};

fn at_least_one(value: &str) -> Result<usize, String> {
    match value.parse::<usize>() {
        Ok(0) => Err("must be at least 1".to_owned()),
        Ok(n) => Ok(n),
        Err(e) => Err(e.to_string()),
    }
}

fn view_pattern(value: &str) -> Result<regex::Regex, String> {
    // Line-oriented like grep: `^`/`$` match at every line of a message.
    regex::RegexBuilder::new(value)
        .case_insensitive(true)
        .multi_line(true)
        .build()
        .map_err(|e| e.to_string())
}

fn cli_timeframe(value: &str) -> Result<String, String> {
    search::parse_timeframe_duration(value)?;
    Ok(value.to_string())
}

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
    /// Search session content and metadata with BM25 relevance ranking
    #[command(
        after_help = "EXAMPLES:\n  chat-history search 'auth error' --json\n  chat-history search 'src/parser.rs' --scope files\n  chat-history search 'auth error' --engine legacy --deep\n\nExplicit flags override environment variables. BM25 searches transcripts by default."
    )]
    Search {
        /// Search query, or a full session UUID for direct lookup
        query: String,
        /// What to search within transcripts
        #[arg(long, default_value = "all", value_parser = ["all", "errors", "similar", "tools", "files"])]
        scope: String,
        /// Search full transcript content with legacy (already enabled with BM25)
        #[arg(long)]
        deep: bool,
        /// Ranking engine; legacy retains the previous metadata/deep search behavior
        #[arg(long, env = "CHAT_HISTORY_SEARCH_ENGINE", default_value = "bm25", value_parser = ["bm25", "legacy"])]
        engine: String,
        /// Group hits by session or message (BM25 defaults to session; legacy/similar to message)
        #[arg(long, env = "CHAT_HISTORY_SEARCH_GROUP_BY", value_parser = ["session", "message"])]
        group_by: Option<String>,
        /// Reparse all sessions and replace the BM25 index contents
        #[arg(long, env = "CHAT_HISTORY_REBUILD_INDEX")]
        rebuild_index: bool,
        /// Directory for the disposable BM25 index (defaults to ~/.chat-history/cache)
        #[arg(long, env = "CHAT_HISTORY_CACHE_DIR")]
        cache_dir: Option<std::path::PathBuf>,
        /// Build BM25 in memory; also enabled by CHAT_HISTORY_NO_CACHE
        #[arg(long)]
        no_cache: bool,
        /// Maximum results (sessions when grouped, otherwise messages)
        #[arg(long, default_value_t = 15)]
        limit: usize,
        /// Only messages newer than today, week, month, or Nd (e.g. 7d)
        #[arg(long, value_parser = cli_timeframe)]
        timeframe: Option<String>,
        /// Structured JSON output (session_id, score, snippet, tools, files)
        #[arg(long = "json")]
        json_output: bool,
    },
    /// Summarize sessions: accomplishments, tools, files, model, tokens
    Inspect {
        /// Session IDs or unique prefixes, inspected in the order given
        session_ids: Vec<String>,
        /// Inspect the most recent session
        #[arg(long, conflicts_with = "session_ids")]
        last: bool,
        /// A few lines per session: what was asked and how it ended
        #[arg(long)]
        brief: bool,
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
        /// Show message N (a search hit's `ordinal`) and --context messages around it
        #[arg(long, value_name = "N", conflicts_with = "grep")]
        around: Option<usize>,
        /// Messages matching a case-insensitive regex, with --context around each
        #[arg(long, value_name = "PATTERN", value_parser = view_pattern)]
        grep: Option<regex::Regex>,
        /// Messages to show on each side (default: 2 with --around, 0 with --grep)
        #[arg(short = 'C', long, value_name = "N")]
        context: Option<usize>,
        /// Only the first N messages (with --grep: the first N matches and their context)
        #[arg(long, value_name = "N", conflicts_with = "tail", value_parser = at_least_one)]
        head: Option<usize>,
        /// Only the last N messages (with --grep: the last N matches and their context)
        #[arg(long, value_name = "N", value_parser = at_least_one)]
        tail: Option<usize>,
        /// Cut each message to N characters and note how many were left out
        #[arg(long, value_name = "N")]
        max_chars: Option<usize>,
        /// Prefix messages with their ordinal ([#N]); implied by the selection flags
        #[arg(short = 'n', long)]
        number: bool,
        /// Only these roles, comma-separated: user (the person), assistant, tool (tool output)
        #[arg(long, value_name = "ROLES", value_delimiter = ',',
              value_parser = ["user", "assistant", "tool"])]
        role: Vec<String>,
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
    /// Record a Cursor stop-hook payload from stdin (optional local discovery)
    #[command(
        after_help = "EXAMPLES:\n  Configure a Cursor stop hook with command: chat-history cursor-hook\n  Replay a saved event: chat-history cursor-hook < event.json\n\nRecords only transcript paths and selected metadata in ~/.chat-history/cursor-hooks.db.\nDoes not install hooks. Outputs {} and never blocks Cursor on a recording error."
    )]
    CursorHook,
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
    resolve_session(sessions, sid).unwrap_or_else(|code| std::process::exit(code))
}

/// The session `sid` names, or the exit code after explaining why there is
/// none (not found, or an ambiguous prefix).
fn resolve_session<'a>(
    sessions: &'a [session::Session],
    sid: &str,
) -> Result<&'a session::Session, i32> {
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
            Ok(s)
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
            Err(2)
        }
        SessionLookup::NotFound => {
            eprintln!("Session not found: {sid}");
            Err(1)
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

/// The session a transcript-reading command (inspect, view, export) works
/// on: by id, or with `--last` the newest readable one. Exits with an
/// explanation when only Cursor CLI metadata exists for it.
fn transcript_or_exit<'a>(
    sessions: &'a [session::Session],
    filtered: &'a [session::Session],
    session_id: Option<&str>,
    last: bool,
) -> &'a session::Session {
    let session = if last {
        let newest = |readable_only: bool| {
            filtered
                .iter()
                .filter(|s| !readable_only || !s.is_cursor_store_only())
                .max_by_key(|s| session::recency_key(s))
        };
        // Fall back to a metadata-only row so the message below explains
        // it, instead of claiming nothing matched what the listing showed.
        let (readable, any) = (newest(true), newest(false));
        if let (Some(r), Some(a)) = (readable, any)
            && session::recency_key(a) > session::recency_key(r)
        {
            eprintln!(
                "Note: skipped newer Cursor CLI session {} (metadata only, no transcript)",
                a.id
            );
        }
        match readable.or(any) {
            Some(s) => s,
            None => {
                eprintln!("Session not found");
                std::process::exit(1);
            }
        }
    } else if let Some(sid) = session_id {
        resolve_session_or_exit(sessions, sid)
    } else {
        eprintln!("Provide a session ID or use --last");
        std::process::exit(2);
    };
    readable(session).unwrap_or_else(|code| std::process::exit(code))
}

/// `session`, or exit code 1 after explaining that only Cursor CLI metadata
/// exists for it.
fn readable(session: &session::Session) -> Result<&session::Session, i32> {
    if session.is_cursor_store_only() {
        let reopen = match chat_history::cursor_cli::unresumable_reason(session) {
            Some(reason) => format!("It cannot be resumed either: {reason}"),
            None => format!(
                "Use `chat-history resume {}` to reopen it in Cursor Agent.",
                session.id
            ),
        };
        eprintln!(
            "Only metadata is available for Cursor CLI session {}. Its store.db format is not a readable transcript. {reopen}",
            session.id
        );
        return Err(1);
    }
    Ok(session)
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

    if matches!(&cli.command, Some(Commands::CursorHook)) {
        use std::io::IsTerminal;
        let result = if std::io::stdin().is_terminal() {
            Err("cursor-hook expects a Cursor JSON payload on stdin".to_owned())
        } else {
            chat_history::cursor_hooks::record_hook(std::io::stdin().lock())
        };
        if let Err(error) = result {
            eprintln!("Warning: {error}");
        }
        // Observational hook: no follow-up prompt, permission decision, skill
        // installation, or history scan, including when recording fails.
        println!("{{}}");
        return;
    }

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
        Some(Commands::Inspect { session_ids, .. }) if !session_ids.is_empty()
    ) || matches!(
        &cli.command,
        Some(Commands::View {
            session_id: Some(_),
            last: false,
            ..
        }) | Some(Commands::Export { .. })
            | Some(Commands::Resume { .. })
            | Some(Commands::Find { .. })
    );
    let bm25_search =
        matches!(&cli.command, Some(Commands::Search { engine, .. }) if engine == "bm25");
    let mut sessions = if id_lookup || bm25_search {
        load_sessions(None)
    } else {
        load_sessions(cli.source.as_deref())
    };
    // Stable collection statistics and freshness across project/source filters.
    let search_corpus = bm25_search.then(|| sessions.clone());
    if !cli.sidechains {
        sessions.retain(|s| !s.is_sidechain);
    }
    let source_filter = if bm25_search
        && session::normalize_source_filter(cli.source.as_deref()).as_deref() == Some("cursor")
    {
        // Canonical discovery can replace a CLI metadata-only row with readable
        // IDE bubbles. Preserve CLI membership independently of representation.
        let agent_ids: std::collections::HashSet<String> = load_sessions(Some("cursor"))
            .into_iter()
            .map(|s| s.id.to_lowercase())
            .collect();
        sessions.retain(|s| {
            s.source == "cursor"
                || (s.source == "cursor-ide" && agent_ids.contains(&s.id.to_lowercase()))
        });
        None
    } else {
        cli.source.as_deref()
    };
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
        source_filter,
        project_filter.as_deref(),
        cli.branch.as_deref(),
    );

    match cli.command {
        Some(Commands::Search {
            query,
            scope,
            deep,
            engine,
            group_by,
            rebuild_index,
            cache_dir,
            no_cache,
            limit,
            timeframe,
            json_output,
        }) => {
            let mut pre = filtered;
            // The calling conversation contains the question being searched
            // for and would rank first; asking for it by id still finds it.
            if !scoring::is_uuid(&query) {
                let calling = session::calling_sessions();
                pre.retain(|s| {
                    !calling.iter().any(|(source, id)| {
                        s.source.starts_with(source) && s.id.eq_ignore_ascii_case(id)
                    })
                });
            }

            if engine == "legacy" && !deep && scope == "all" && !scoring::is_uuid(&query) {
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
            let request = SearchRequest {
                sessions: &pre,
                query: &query,
                scope: &scope,
                limit,
                timeframe: timeframe.as_deref(),
                group_by_session: group_by
                    .as_deref()
                    .map_or(engine == "bm25" && scope != "similar", |value| {
                        value == "session"
                    }),
            };
            let result = if engine == "legacy" || scope == "similar" {
                LegacyBackend.search(&request)
            } else {
                let directory = if no_cache || std::env::var_os("CHAT_HISTORY_NO_CACHE").is_some() {
                    None
                } else {
                    cache_dir
                        .filter(|p| !p.as_os_str().is_empty())
                        .or_else(|| chat_history::cache_dir::prepare(search_index::INDEX_FILENAME))
                };
                search_index::search_corpus(
                    search_corpus.as_deref().unwrap_or(&sessions),
                    &request,
                    directory.as_deref(),
                    rebuild_index,
                )
            };
            let results = result.unwrap_or_else(|error| {
                eprintln!("Search failed: {error}");
                std::process::exit(1);
            });
            if scoring::is_uuid(&query)
                && timeframe.is_some()
                && results.is_empty()
                && pre.iter().any(|s| s.id.eq_ignore_ascii_case(query.trim()))
            {
                eprintln!(
                    "That session exists but has no activity in the --timeframe; drop the flag to open it."
                );
            }
            if json_output {
                display::print_search_results_json(&results, &query);
            } else {
                display::print_search_results(&results, &query);
            }
        }
        Some(Commands::Inspect {
            session_ids,
            last,
            brief,
        }) => {
            let targets: Vec<Result<&session::Session, i32>> = if last || session_ids.is_empty() {
                vec![Ok(transcript_or_exit(&sessions, &filtered, None, last))]
            } else {
                session_ids
                    .iter()
                    .map(|sid| resolve_session(&sessions, sid).and_then(readable))
                    .collect()
            };
            let mut failure = None;
            for (n, target) in targets.into_iter().enumerate() {
                let Ok(session) = target.map_err(|code| failure = failure.or(Some(code))) else {
                    continue;
                };
                match inspect::inspect_session(session) {
                    Some(info) if brief => {
                        if n > 0 {
                            println!();
                        }
                        display::print_inspect_brief(&info);
                    }
                    Some(info) => display::print_inspect(&info),
                    None => {
                        eprintln!(
                            "Could not inspect session {} (transcript may be expired).",
                            session.id
                        );
                        failure = failure.or(Some(1));
                    }
                }
            }
            if let Some(code) = failure {
                std::process::exit(code);
            }
        }
        Some(Commands::View {
            session_id,
            last,
            tools,
            plain,
            around,
            grep,
            context,
            head,
            tail,
            max_chars,
            number,
            role,
        }) => {
            let session = transcript_or_exit(&sessions, &filtered, session_id.as_deref(), last);
            let (messages, _) = parse_session(session, false);
            let opts = display::ViewOptions {
                around,
                context,
                grep,
                head,
                tail,
                number,
                max_chars,
                roles: role,
            };
            if let Err(error) = opts.check(&messages) {
                eprintln!("{error}");
                std::process::exit(1);
            }
            if let Some(pattern) = &opts.grep
                && opts.matches(&messages).is_empty()
            {
                eprintln!(
                    "No messages match {} in this transcript ({} messages).",
                    pattern.as_str(),
                    messages.len()
                );
                return;
            }
            if plain {
                display::print_plain(&messages, &opts);
            } else {
                display::print_transcript(&messages, session, tools, &opts);
            }
        }
        Some(Commands::Export { session_id, output }) => {
            let session = transcript_or_exit(&sessions, &filtered, Some(&session_id), false);
            let (messages, _) = parse_session(session, false);
            if !display::export_transcript(&messages, session, output.as_deref()) {
                std::process::exit(1);
            }
        }
        Some(Commands::Resume { session_id }) => {
            let session = resolve_session_or_exit(&sessions, &session_id);
            // A CLI store wins even when the IDE also indexes the chat; the
            // sidebar hint is for chats no store can reopen.
            let action = match session::resume_command(session) {
                Some(a) => a,
                None if session.source.starts_with("cursor") => {
                    // Two independent decisions: which stderr line explains
                    // the missing or unusable store, and whether the sidebar
                    // pointer follows (IDE-indexed chats, and chats with no
                    // store at all, may still open in the IDE).
                    let reason = chat_history::cursor_cli::unresumable_reason(session);
                    match &reason {
                        Some(reason) => {
                            eprintln!("Cannot resume Agent CLI chat {}: {reason}", session.id)
                        }
                        None if !session.is_ide_ui() => eprintln!(
                            "No Agent CLI chat store for this id under ~/.cursor/chats, so \
                             `agent --resume` cannot load it (it would start a blank chat)."
                        ),
                        None => {}
                    }
                    if reason.is_none() || session.is_ide_ui() {
                        print!("{}", display::cursor_ide_resume_hint(session));
                    }
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
            let ResumeAction::Exec { bin, args, workdir } = action else {
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
            if session.source.starts_with("cursor")
                && let Some(dir) = &workdir
                && !session.project.is_empty()
                && !chat_history::cursor_cli::same_workspace(
                    &session.project,
                    &dir.to_string_lossy(),
                )
            {
                // The listed workspace (a path, or a slug no store matched)
                // has no resumable store; say where the chat is reopened.
                let listed = if std::path::Path::new(&session.project).is_absolute() {
                    display::abbreviate_home(&session.project)
                } else {
                    format!("workspace slug {}", session.project)
                };
                eprintln!(
                    "Note: no resumable Agent CLI store in {listed}; resuming in {}",
                    display::abbreviate_home(&dir.to_string_lossy())
                );
            }
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
        Some(Commands::InstallSkill { .. })
        | Some(Commands::Completions { .. })
        | Some(Commands::CursorHook) => unreachable!(),
        None => {
            if cli.summarize {
                display::print_summarized(&filtered);
            } else {
                display::print_list(&filtered, cli.verbose);
            }
        }
    }
}
