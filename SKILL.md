---
name: chat-history
description: Search, inspect, and export Claude Code and Cursor conversation history. Use when the user asks about past conversations, wants to find a previous session, needs to search chat history, wants a summary of what they worked on, or asks to resume a session. Also use when the user says "what did I work on", "find that conversation where I...", "show me my recent sessions", or "search my history for...".
---

# chat-history

Repo: https://github.com/ay-bh/chat-history

## Install

```bash
cargo install chat-history
```

This installs both `chat-history` and `ch` (short alias) into `~/.cargo/bin/`.

## When to use

- User asks about past conversations or sessions
- User wants to find something they discussed before
- User needs a summary of recent work / accomplishments
- User wants to search across all Claude Code or Cursor history
- User wants to resume or export a previous session

## Commands

```bash
# List sessions
chat-history                               # all sessions, newest first
chat-history --from yesterday -s           # grouped by day
chat-history --from "3 days ago"           # natural language dates
chat-history --source claude               # Claude Code only
chat-history --source cursor               # Cursor only
chat-history -L                            # current workspace only
chat-history --branch feature-xyz          # filter by branch
chat-history -k "auth" -v                  # keyword filter, show IDs

# Search (fast index search by default, sub-second)
chat-history search "auth error"           # searches summary/prompt/branch
chat-history search "auth error" --deep    # full transcript search (rayon-parallelized)
chat-history search "fix" --scope errors   # error patterns only
chat-history search "trade" --scope similar  # similar past queries
chat-history search <full-uuid>            # direct UUID lookup
chat-history search "cache fix" --json     # structured JSON output for agents

# Inspect (session summary)
chat-history inspect --last                # accomplishments, tools, model, tokens
chat-history inspect <partial-uuid>

# View / Export / Resume / Find
chat-history view --last --plain           # pipe-friendly plain text
chat-history view <id> --tools             # show tool call names
chat-history export <id> -o session.md     # export as markdown
chat-history resume <id>                   # resume Claude Code session
chat-history find <id>                     # print file path for scripting
```

The short alias `ch` works identically: `ch search "auth"`, `ch inspect --last`, etc.

## Search behavior

- **Index search** (default): Searches session metadata (summary, first prompt, branch) — sub-second.
  - Weak index results (★ < 5.0) automatically fall through to deep transcript search.
- **Deep search** (`--deep`): Searches full transcript content across all messages.
  - Parses sessions in parallel using rayon.
  - Snippets show context **around the match**, not the first N characters of the message.
- **JSON output** (`--json`): Returns structured JSON with `session_id`, `score`, `snippet`, `tools`, `files` — ideal for programmatic/agent consumption.

## Interpreting output

- `CC` = Claude Code session, `CR` = Cursor session
- Score (`★ N.N`) = relevance score (higher = better match)
- `[summary]`, `[first_prompt]`, `[branch]` = which field matched in index search
- `inspect` shows: duration, message count, model name, token count, tools used, files touched, accomplishments, key decisions

## Date formats accepted

`YYYY-MM-DD`, `today`, `yesterday`, `"3 days ago"`, `"last week"`, `"last month"`
