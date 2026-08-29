---
name: chat-history
description: Search, inspect, and export Claude Code, Cursor, and Codex conversation history. Use when the user asks about past conversations, wants to find a previous session, needs to search chat history, wants a summary of what they worked on, or asks to resume a session. Also use when the user says "what did I work on", "find that conversation where I...", "show me my recent sessions", or "search my history for...".
---

# chat-history

Search, inspect, and export Claude Code, Cursor, and Codex conversation history. The short alias `ch` works identically. If the command is not found: `cargo install chat-history`.

## When to use

- User asks about past conversations or sessions
- User wants to find something they discussed before
- User needs a summary of recent work / accomplishments
- User wants to resume or export a previous session

## Process

**Keyword question** ("find that conversation where I..."):

1. `chat-history search "<query>" --deep --json` — always `--deep --json` from agents. `--deep` searches full transcript content; `--json` returns structured results (`session_id`, `score`, `snippet`, `tools`, `files`). Note `--json` exists only on `search`.
2. Shortlist by snippet, not by raw score (see "Choosing the best hit").
3. `chat-history inspect <partial-uuid>` on the top 2–3 candidates to confirm before answering.
4. `view` / `export` only if the user needs the actual content.

**Temporal question** ("what did I work on yesterday?") — list, don't search:

1. `chat-history --from yesterday --to yesterday` — every row shows a short session ID; `-s` groups by day for multi-day overviews.
   - `--from X` alone means **X through today**. Always pair with `--to` when the user means a specific day.
   - Short IDs work everywhere a session ID is accepted (`inspect`, `view`, `export`, `find`); `-v` adds full IDs and file paths. `resume` works for `claude` / `codex` rows and for Cursor Agent CLI chats; `cursor-ide` rows (and Agent transcripts without a CLI store) print the **title** and `DIR:` — tell the user to open that folder in Cursor and pick the chat in the sidebar.
2. `chat-history inspect <id>` for accomplishments, tools, files touched.

## Choosing the best hit

- Scores rank keyword density, not intent. Use scores only to shortlist; decide from snippets and `inspect`.
- The conversation you are currently in can match its own query and score highest. Ignore hits whose session is the current one.
- When candidates are close, `inspect` each before picking — don't answer from the top score alone.

## Common mistakes

- Only `search` accepts `--json`; the session list and `inspect` reject it.
- The only subcommands are `search`, `inspect`, `view`, `export`, `resume`, `find`, `install-skill`, `completions`. Do not guess others; run `chat-history --help` when unsure.
- Don't dump raw JSON or full transcripts at the user — summarize, cite the session ID and date (or title + directory for `cursor-ide`).
- Do not `resume` a session whose human tag is `cursor-ide`. `--json` may still report `"source": "cursor"` for IDE Agent chats that also have a jsonl; `resume` will print a sidebar hint instead of launching the Agent CLI.
- Some Cursor sessions have thin metadata (`(no summary)`, `duration: 0min`, raw first-message titles). If `inspect` is thin, fall back to `chat-history view <id> --plain`.

## Commands

```bash
# List sessions
chat-history                                      # newest first; short IDs on every row (-v for full IDs + paths)
chat-history --from yesterday --to yesterday -s   # a specific day, grouped
chat-history --from "3 days ago"                  # natural-language dates
chat-history --source claude                      # claude | cursor | cursor-agent | cursor-ide | codex
chat-history -L                                   # current workspace only
chat-history --branch feature-xyz -k "auth" -v    # branch / keyword filters

# Search (always --deep --json from agents)
chat-history search "auth error" --deep --json
chat-history search "fix" --scope errors --deep --json   # only messages with error patterns
chat-history search <full-uuid>                          # direct session lookup
chat-history search "q" --deep --json --limit 30         # default limit is 15

# Inspect / View / Export / Resume / Find
chat-history inspect --last                # accomplishments, tools, model, tokens, files
chat-history inspect <partial-uuid>
chat-history view <id> --plain             # transcript, pipe-friendly (--tools for tool names)
chat-history export <id> -o session.md
chat-history resume <id>                   # Claude Code, Cursor Agent CLI, or Codex — not IDE sidebar
chat-history find <id>                     # print transcript file path for scripting
chat-history completions zsh               # shell completions (bash/zsh/fish/elvish/powershell)
```

- `--scope` values: `all` (default), `errors` (messages with error patterns or the word "error"), `similar` (user messages only — similar past queries), `tools` (messages with tool calls), `files` (messages referencing files). Any scope other than `all` always searches full transcripts.
- Shared filters on every subcommand: `--from`/`--to`, `--source`, `--project`, `--branch`, `-k`, `--sidechains` (include hidden subagent sessions).

## Interpreting output

- Display tags: `claude` = Claude Code, `cursor-ide` = Cursor IDE sidebar (SQLite; IDE Agent also writes jsonl with the same id — still listed once as `cursor-ide`), `cursor-agent` = Agent CLI / jsonl-only, `codex` = Codex; `★ N.N` = relevance score. `--source cursor` / `cursor-agent` = jsonl store; `--source cursor-ide` = SQLite.
- Header line has `DIR:` (spawn directory) and, for index search, `INDEX_FIELD:` (`summary` / `first_prompt` / `branch`). Title is on the next line. Pass the short ID to `inspect` / `view` / `export` / `find` for any row, and to `resume` for `claude` / `codex` rows and Cursor Agent CLI chats (`cursor-agent` rows that still have a `~/.cursor/chats` store). Other Cursor rows are found in the sidebar by title + `DIR:`.
- `COPIES: N` means the same Cursor Agent session id exists in more than one project folder; `inspect`/`resume`/`find` pick one copy (cwd match, else newest) and print the others.
- Accepted dates: `YYYY-MM-DD`, `today`, `yesterday`, `"3 days ago"`, `"last week"`, `"last month"`.
