# chat-history

A fast Rust CLI to search, inspect, and export **Claude Code** and **Cursor** conversation history.

Scoring logic ported from [claude-historian-mcp](https://github.com/Vvkmnn/claude-historian-mcp), [claude-history](https://github.com/raine/claude-history), and [search-sessions](https://github.com/sinzin91/search-sessions), with Cursor transcript parsing inspired by [cursor-history](https://github.com/S2thend/cursor-history).

## Install

Requires [Rust](https://rustup.rs/). Then:

```bash
cargo install chat-history
```

This installs both `chat-history` and `ch` (short alias) into `~/.cargo/bin/`.

### Install the agent skill (recommended)

```bash
chat-history install-skill
```

This writes the bundled `SKILL.md` to `~/.cursor/skills/chat-history/` and `~/.claude/skills/chat-history/`, giving Claude Code and Cursor agents the ability to search your conversation history automatically. Re-run after upgrading to pick up skill updates.

### Build from source

```bash
git clone https://github.com/ay-bh/chat-history.git
cd chat-history
cargo install --path .
```

## Quick start

```bash
chat-history                               # list all sessions, newest first
chat-history --from yesterday -s           # yesterday's sessions, grouped by day
chat-history search "auth error"           # fast index search (sub-second)
chat-history search "auth error" --deep    # full transcript search (thorough)
chat-history inspect --last                # session summary with accomplishments
chat-history view --last --plain | head    # pipe transcript to other tools
```

`ch` works as a drop-in alias for `chat-history`:

```bash
ch search "docker config"
ch inspect --last
```

## Commands

### List sessions (default)

```bash
chat-history                               # all sessions
chat-history -L                            # current workspace only
chat-history --source claude               # Claude Code sessions only
chat-history --source cursor               # Cursor sessions only
chat-history --from 2026-03-01 --to 2026-03-20
chat-history --from yesterday              # natural language dates
chat-history --from "3 days ago"           # relative dates
chat-history --from "last week" --to today
chat-history --branch feature-xyz          # filter by git branch
chat-history -k "auth" -v                  # keyword filter, show IDs/paths
chat-history -s                            # group by day
```

### Scored search

By default, search checks session metadata (summary, first prompt, branch) without parsing transcript files. This is fast — sub-second. Weak index results (★ < 5.0) automatically fall through to deep transcript search. Use `--deep` to force full transcript search, or `--scope` for specialized searches.

```bash
# Fast index search (sub-second, checks summary/prompt/branch)
chat-history search "docker auth"
chat-history search "trade_assets"         # _ treated as word separator
chat-history search "auth"                 # prefix: matches "authentication"

# Deep transcript search (searches inside messages)
chat-history search "docker auth" --deep
chat-history search "fix" --scope errors   # only error patterns
chat-history search "trade" --scope similar  # find similar past queries
chat-history search "Edit" --scope tools   # tool usage patterns
chat-history search "config" --scope files # file operations
chat-history search "e7d318b1-..."         # UUID direct lookup
chat-history search "auth" --timeframe week  # time window: today/week/month/Nd

# Structured output for programmatic/agent use
chat-history search "cache fix" --json
chat-history search "auth" --deep --json
```

Search results include an 8-char session UUID prefix (e.g. `[e363d98d]`) so you can immediately drill into a result with `chat-history inspect e363d98d` or `chat-history view e363d98d`.

### Inspect

Session summary showing accomplishments, key decisions, tools used, files touched, model name, and token count.

```bash
chat-history inspect --last                # most recent session
chat-history inspect 2df5                  # by partial UUID
```

### View transcript

```bash
chat-history view --last                   # full transcript
chat-history view 2df5 --tools             # include tool call names
chat-history view --last --plain           # plain text (pipe-friendly)
```

### Export / Resume / Find

```bash
chat-history export 2df5 -o session.md     # export as markdown
chat-history resume 2df5                   # resume Claude Code session
chat-history find e912                     # print file path (for scripting)
```

### Install agent skill

```bash
chat-history install-skill                 # installs SKILL.md for Claude Code + Cursor
```

## Data sources

| Source | Path | What it contains |
|---|---|---|
| Claude Code index | `~/.claude/projects/*/sessions-index.json` | Summary, dates, branch, message count |
| Claude Code JSONL | `~/.claude/projects/*/*.jsonl` | Full conversations with tool calls |
| Cursor agent transcripts | `~/.cursor/projects/*/agent-transcripts/` | JSONL or plain-text transcripts |

## Search scoring

### Index search (default)

Searches `sessions-index.json` metadata with field-weighted scoring:

- **Summary match** — 3x weight
- **First prompt match** — 2x weight
- **Branch / project match** — 1x weight
- **Recency multiplier** — 3x today, 2x this week, 1.5x this month
- All query words must match (AND logic)
- Shows which field matched (`[summary]`, `[first_prompt]`, etc.)

### Deep search (`--deep`)

Deep search parses transcript files in parallel using [rayon](https://github.com/rayon-rs/rayon). Snippets show context **around the match**, not the first N characters.

Full transcript scoring combines signals from multiple open-source projects:

- **Core tech term matching** — exact match on framework/tool names (10pts)
- **Word-level scoring** — exact word boundary match (2pts), substring fallback (1pt)
- **Supporting terms** — 5+ character non-generic terms (3pts)
- **Exact phrase bonus** — full query appears verbatim (5pts)
- **Prefix matching** — `auth` matches `authentication` at word boundaries
- **Separator normalization** — `_`, `-`, `/` treated as spaces
- **Recency multiplier** — 3x today, 2x this week, 1.5x this month
- **Importance boost** — decisions (2.5x) > bugfixes (2x) > features (1.5x)
- **Semantic boosts** — error queries boost error content (3x), fix queries boost solutions (2.8x)
- **Content deduplication** — normalized signatures prevent duplicate results
- **Per-session cap** — max 3 matches per session to prevent domination

## Filtering

The following are automatically filtered out:

- Warmup/handshake messages (`Warmup`, `/clear`)
- Noise patterns (`"I'm Claude"`, `"ready to help"`, etc.)
- Clear-only conversations
- Structural config content (settings listings)
- Full-text capped at 4MB per conversation to prevent lag

## Credits

Search and scoring logic ported from:
- [claude-historian-mcp](https://github.com/Vvkmnn/claude-historian-mcp) — multi-signal relevance scoring, query similarity, importance heuristics
- [claude-history](https://github.com/raine/claude-history) — prefix matching, separator normalization, recency multiplier, cwd-based project path resolution
- [search-sessions](https://github.com/sinzin91/search-sessions) — two-tier index/deep search, field-weighted scoring, natural language dates, per-session cap
- [cursor-history](https://github.com/S2thend/cursor-history) — multi-format Cursor transcript parsing

## License

MIT
