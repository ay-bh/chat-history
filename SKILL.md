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

1. `chat-history search "<query>" --compact` — one line per conversation: short id, date, source, directory, `#ordinal` of the best match (`-` for a title or first-prompt match), title, then `—  Role: excerpt` and `(also #a #b)` for its other matches. Role is `You` (the person), `Assistant` or `Tool` (command output or file contents). BM25 searches metadata and transcripts together (`--deep` is not needed). Use `--json` instead only when a script parses the results: each hit then carries `session_id`, `score`, `snippet`, `role`, `ordinal`, `timestamp`, `tools`, `files`, and `additional_matches` with the same message fields.
2. Shortlist by title and excerpt (see "Choosing the best hit").
3. Read the hit in context, not the whole transcript: `chat-history view <id> --plain --around <ordinal>` (2 messages each side; `-C N` to widen, `--max-chars 1500` to cap long messages), or `--around` one of the `also` ordinals. A title/prompt match has no ordinal — use `inspect` for those. `view --json` gives the same selection as data for scripts.
4. To find something inside one session, use `chat-history view <id> --plain --grep "<regex>" --max-chars 600 --head 12` instead of piping `view` through `grep`/`sed`/`head`. To read just the conversation, add `--role user,assistant` (tool output is most of a transcript); `--role user` lists what the person asked.
5. `chat-history inspect <id> <id> <id> --brief` on the top 2–3 candidates to confirm before answering: one call, a few lines each (what was asked, the latest substantive result, files it read or edited — the project's first). Drop `--brief` for one session's full detail. Full `view` / `export` only if the user needs the whole conversation.

**Temporal question** ("what did I work on yesterday?") — list, don't search:

1. `chat-history --from yesterday --to yesterday` — every row shows a short session ID; `-s` groups by day for multi-day overviews.
   - `--from X` alone means **X through today**. Always pair with `--to` when the user means a specific day.
   - Short IDs work everywhere a session ID is accepted (`inspect`, `view`, `export`, `find`); `-v` adds full IDs and file paths. `resume` works for `claude` / `codex` rows and for Cursor Agent CLI chats; Cursor rows whose id has no `~/.cursor/chats` store (whatever their tag) print the **title** and `DIR:` — tell the user to open that folder in Cursor and pick the chat in the sidebar.
2. `chat-history inspect <id>…` for accomplishments (how each turn ended), tools, files touched; `--brief` to compare several sessions.

## Choosing the best hit

- Results are ranked by lexical relevance (BM25 by default), not intent. `--json` scores are not confidence values or comparable across queries or engines. Use the order only to shortlist; decide from excerpts and `inspect`.
- BM25 groups matches by conversation: `--limit` counts conversations, the excerpt follows the strongest matching passage, and `(also #a #b)` names up to two more matching messages — read them with `view --around` before inspecting the session (`--json` includes their excerpts under `additional_matches`). Use `--group-by message` if individual matching messages are needed; legacy and `--scope similar` retain their previous message-row default.
- Search leaves out the conversation you are running in (Claude Code, Codex and Cursor's agent tell it which one), since it would match its own query. Searching for its full UUID still finds it.
- Tool output (`role: tool`) ranks below conversation text with the same words; chat-history's own output is never indexed.
- When candidates are close, `inspect` them together (`inspect a b c --brief`) before picking — don't answer from the top score alone, and don't loop over ids in the shell.

## Common mistakes

- `search` and `view` accept `--json`; the session list and `inspect` reject it.
- The subcommands are `search`, `inspect`, `view`, `export`, `resume`, `find`, `install-skill`, `completions`, and the optional `cursor-hook` receiver. Do not guess others; run `chat-history --help` when unsure. `cursor-hook` is for configured Cursor hooks, not normal history queries.
- Cursor CLI rows labeled `[metadata only]` (in `--compact` too) support `find`, `resume`, and title search when Cursor recorded a title (print-mode chats usually have none); their internal `store.db` cannot be viewed or exported as a transcript. Do not claim that metadata-only search covers message content.
- Cursor message timestamps may be unavailable. `search --timeframe` excludes unknown message times and untimed first-prompt previews. Titles use session activity times. With `--engine legacy`, add `--deep` to bypass its metadata shortcut; use session `--from` / `--to` filters for activity-date questions. File modification times are not message timestamps.
- A stderr line starting `Note:` about the cache directory not being writable means the sandbox blocked `~/.chat-history`; results are still complete and later searches stay fast. Mention it only if the user asks why; the README section "Running inside agent sandboxes" has the one-line setting per tool.
- Don't dump raw JSON or full transcripts at the user — summarize, cite the session ID and date (or title + directory for `cursor-ide`).
- `cursor-ide` rows (and `--json` items with `"also_ide": true`) resume only when the Agent CLI has a `~/.cursor/chats` store for the id; otherwise `resume` prints a sidebar hint instead of launching the Agent CLI. Run `resume` and follow its output rather than assuming.
- Some Cursor sessions have thin metadata (`(no summary)`, `duration: 0min`, raw first-message titles). If `inspect` is thin, fall back to `chat-history view <id> --plain --tail 6` (or `--grep`) rather than the whole transcript.
- Don't pipe `view` through `grep -A40 | head` or `sed -n 'A,Bp'`: use `--around`, `--grep`, `--head`/`--tail` (with `--grep` these count matches, each kept whole with its context) and `--max-chars`. Messages are numbered `[#N]` in those modes (`-n` numbers a full view); `[-K chars] …` / `… [+K chars]` mark cut text, `…` on its own line marks skipped messages.

## Commands

```bash
# List sessions
chat-history                                      # newest first; short IDs on every row (-v for full IDs + paths)
chat-history --from yesterday --to yesterday -s   # a specific day, grouped
chat-history --from "3 days ago"                  # natural-language dates
chat-history --source claude                      # claude | cursor | cursor-agent | cursor-ide | codex
chat-history -L                                   # current workspace only
chat-history --branch feature-xyz -k "auth" -v    # branch / keyword filters

# Search (--compact from agents; --json for scripts)
chat-history search "auth error" --compact
chat-history search "fix" --scope errors --compact       # only messages with error patterns
chat-history search <full-uuid>                          # direct session lookup (with --timeframe: only if active in the window)
chat-history search "q" --compact --limit 30            # default limit is 15
chat-history search "q" --json                          # structured, for scripts
chat-history search "auth error" --engine legacy --deep --json  # compare previous ranking

# Inspect / View / Export / Resume / Find
chat-history inspect --last                # accomplishments, tools, model, tokens, files
chat-history inspect <partial-uuid>
chat-history inspect a1b2c3d4 e5f6a7b8 --brief  # several sessions, a few lines each: asked, outcome, files
chat-history view <id> --plain --around 42 # message #42 (a hit's ordinal) ± 2 messages; -C N to change
chat-history view <id> --plain --grep "cloudflare|dns" --max-chars 600 --head 12  # matching messages, excerpt centred on the match
chat-history view <id> --plain --tail 6    # last 6 messages (--head N for the first N)
chat-history view <id> --plain --role user # only what the person said (user | assistant | tool, comma-separated)
chat-history view <id> --plain             # whole transcript, pipe-friendly (--tools for tool names; -n numbers messages)
chat-history view <id> --json --around 42  # the same selections as data: ordinal, role, timestamp, content, tools
chat-history export <id> -o session.md
chat-history resume <id>                   # Claude Code, Codex, or any Cursor chat with a CLI store
chat-history find <id>                     # print transcript file path for scripting
chat-history completions zsh               # shell completions (bash/zsh/fish/elvish/powershell)
```

- `--scope` values: `all` (default), `errors` (messages with error patterns or the word "error"), `similar` (user messages only — similar past queries), `tools` (messages with tool calls), `files` (messages referencing files). Any scope other than `all` always searches full transcripts.
- Shared filters on every subcommand: `--from`/`--to`, `--source`, `--project`, `--branch`, `-k`, `--sidechains` (include hidden subagent sessions).

## Interpreting output

- Display tags: `claude` = Claude Code, `cursor-ide` = Cursor IDE sidebar (SQLite; IDE Agent also writes jsonl with the same id — still listed once as `cursor-ide`), `cursor-agent` = Agent CLI / jsonl-only, `codex` = Codex; `★ N.N` = relevance score. `--source cursor` / `cursor-agent` = Agent transcripts plus CLI chat metadata and hook-registered paths; `--source cursor-ide` = SQLite.
- Header line has `DIR:` (spawn directory) and, for legacy metadata search, `INDEX_FIELD:` (`summary` / `first_prompt` / `branch`). Title is on the next line. Pass the short ID to `find` for any row and to `inspect` / `view` / `export` for rows with a transcript (`[metadata only]` rows refuse those three), and to `resume` for `claude` / `codex` rows and any Cursor row whose id has a `~/.cursor/chats` store. Cursor rows without one are found in the sidebar by title + `DIR:`.
- `COPIES: N` means the same Cursor Agent session id exists in more than one project folder; `inspect`/`resume`/`find` pick one copy (cwd match, else newest) and print the others.
- Accepted dates: `YYYY-MM-DD`, `today`, `yesterday`, `"3 days ago"`, `"last week"`, `"last month"`.
