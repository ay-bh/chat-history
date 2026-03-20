mod dates;
mod display;
mod inspect;
mod parser;
mod scoring;
mod search;
mod session;

use clap::{Parser, Subcommand};
use dates::parse_human_date;
use session::{filter_sessions, find_session, load_all_sessions, parse_session};

#[derive(Parser)]
#[command(name = "chat-history", about = "Search Claude Code + Cursor conversation history")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(long = "from", global = true, help = "Start date (YYYY-MM-DD, today, yesterday, '3 days ago')")]
    from_date: Option<String>,

    #[arg(long = "to", global = true, help = "End date")]
    to_date: Option<String>,

    #[arg(long, global = true, help = "Filter by source (claude/cursor)")]
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

    #[arg(short = 'L', long = "local", help = "Only show sessions from current workspace")]
    local: bool,
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
    /// Install the agent skill for Claude Code and Cursor
    #[command(name = "install-skill")]
    InstallSkill,
}

const SKILL_CONTENT: &str = include_str!("../SKILL.md");

fn skill_targets() -> Vec<(std::path::PathBuf, &'static str)> {
    let Ok(home) = std::env::var("HOME") else { return vec![] };
    let home = std::path::Path::new(&home);
    vec![
        (home.join(".cursor/skills/chat-history"), "Cursor"),
        (home.join(".claude/skills/chat-history"), "Claude Code"),
    ]
}

fn write_skill(dir: &std::path::Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() { return false; }
    std::fs::write(dir.join("SKILL.md"), SKILL_CONTENT).is_ok()
}

fn install_skill() {
    let targets = skill_targets();
    if targets.is_empty() {
        eprintln!("Could not determine home directory");
        std::process::exit(1);
    }

    let mut any_installed = false;
    for (dir, name) in &targets {
        if write_skill(dir) {
            println!("  installed → {}", dir.join("SKILL.md").display());
            any_installed = true;
        } else {
            eprintln!("  skip {name}: could not write to {}", dir.display());
        }
    }

    if any_installed {
        println!("\nDone. The skill is active immediately — no restart needed.");
    } else {
        eprintln!("\nNo skills were installed.");
        std::process::exit(1);
    }
}

fn parse_date_arg(val: &Option<String>) -> Option<chrono::NaiveDate> {
    val.as_ref().and_then(|v| {
        parse_human_date(v).or_else(|| {
            eprintln!("Invalid date: '{}'. Try: YYYY-MM-DD, today, yesterday, '3 days ago', 'last week'", v);
            std::process::exit(1);
        })
    })
}

fn main() {
    let cli = Cli::parse();

    if matches!(cli.command, Some(Commands::InstallSkill)) {
        install_skill();
        return;
    }

    let sessions = load_all_sessions();
    let from_d = parse_date_arg(&cli.from_date);
    let to_d = parse_date_arg(&cli.to_date);

    match cli.command {
        Some(Commands::Search { query, scope, deep, limit, timeframe, json_output }) => {
            let pre = filter_sessions(
                &sessions, from_d, to_d, cli.keyword.as_deref(),
                cli.source.as_deref(), cli.project.as_deref(), cli.branch.as_deref(),
            );

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
                        eprintln!("Index matches too weak (best: ★ {:.1}) — searching transcripts...", idx_results[0].score);
                    } else {
                        eprintln!("No index matches — searching transcripts...");
                    }
                }
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
                sessions.iter().max_by_key(|s| {
                    if s.modified.is_empty() { &s.created } else { &s.modified }
                })
            } else if let Some(sid) = &session_id {
                find_session(&sessions, sid)
            } else {
                eprintln!("Provide a session ID or use --last");
                std::process::exit(1);
            };
            let Some(session) = session else {
                eprintln!("Session not found");
                std::process::exit(1);
            };
            match inspect::inspect_session(session) {
                Some(info) => display::print_inspect(&info),
                None => eprintln!("Could not inspect session (transcript may be expired)."),
            }
        }
        Some(Commands::View { session_id, last, tools, plain }) => {
            let session = if last {
                sessions.iter().max_by_key(|s| {
                    if s.modified.is_empty() { &s.created } else { &s.modified }
                })
            } else if let Some(sid) = &session_id {
                find_session(&sessions, sid)
            } else {
                eprintln!("Provide a session ID or use --last");
                std::process::exit(1);
            };
            let Some(session) = session else {
                eprintln!("Session not found");
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
            let Some(session) = find_session(&sessions, &session_id) else {
                eprintln!("Session not found: {session_id}");
                std::process::exit(1);
            };
            let (messages, _) = parse_session(session, false);
            display::export_transcript(&messages, session, output.as_deref());
        }
        Some(Commands::Resume { session_id }) => {
            let Some(session) = find_session(&sessions, &session_id) else {
                eprintln!("Session not found: {session_id}");
                std::process::exit(1);
            };
            if session.source != "claude" {
                eprintln!("Resume is only supported for Claude Code sessions.");
                std::process::exit(1);
            }
            println!("Resuming: {}", if session.summary.is_empty() { &session.id } else { &session.summary });
            if !session.project.is_empty() {
                let project = std::path::Path::new(&session.project);
                if project.is_dir() {
                    if let Err(e) = std::env::set_current_dir(project) {
                        eprintln!("Warning: could not cd to {}: {e}", session.project);
                    }
                }
            }
            use std::os::unix::process::CommandExt;
            let err = std::process::Command::new("claude")
                .args(["--resume", &session.id])
                .exec();
            eprintln!("Failed to exec claude: {err}");
            std::process::exit(1);
        }
        Some(Commands::Find { session_id }) => {
            let Some(session) = find_session(&sessions, &session_id) else {
                eprintln!("Session not found: {session_id}");
                std::process::exit(1);
            };
            println!("{}", session.file);
        }
        Some(Commands::InstallSkill) => unreachable!(),
        None => {
            let mut project_filter = cli.project.as_deref().map(String::from);
            if cli.local && project_filter.is_none() {
                if let Ok(cwd) = std::env::current_dir() {
                    project_filter = cwd.file_name().map(|n| n.to_string_lossy().to_string());
                }
            }
            let filtered = filter_sessions(
                &sessions, from_d, to_d, cli.keyword.as_deref(),
                cli.source.as_deref(), project_filter.as_deref(), cli.branch.as_deref(),
            );
            if cli.summarize {
                display::print_summarized(&filtered);
            } else {
                display::print_list(&filtered, cli.verbose);
            }
        }
    }
}
