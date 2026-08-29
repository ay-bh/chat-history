# chat-history

CLI for **agents** to search, inspect, and export **Claude Code**, **Cursor**, and **OpenAI Codex** conversation history.

Most usage is via an agent skill (`SKILL.md`) that tells Claude Code / Cursor / Codex when and how to call this tool — not interactive human browsing.

<img src="docs/assets/usage.png" alt="Agent using chat-history to summarize past project work" width="420" />

Scoring logic ported from [claude-historian-mcp](https://github.com/Vvkmnn/claude-historian-mcp), [claude-history](https://github.com/raine/claude-history), and [search-sessions](https://github.com/sinzin91/search-sessions), with Cursor transcript parsing inspired by [cursor-history](https://github.com/S2thend/cursor-history).

## Setup (for agents)

```bash
cargo install chat-history
```

`cargo install` puts `chat-history` and `ch` on `PATH` (`~/.cargo/bin/`).

The first time you run `chat-history` / `ch`, it quietly installs the bundled agent skill to:

- `~/.cursor/skills/chat-history/SKILL.md`
- `~/.claude/skills/chat-history/SKILL.md`
- `$CODEX_HOME/skills/chat-history/SKILL.md` (or `~/.codex/...`)

On Windows, the same paths are under `%USERPROFILE%`. Later CLI upgrades refresh **managed** copies automatically (those written by this CLI, tracked via a `.chat-history-managed` sidecar). User-edited skills are left alone. Skills installed by older versions without that sidecar are left alone by the quiet path — run `install-skill` once after upgrading to adopt them.

Optional explicit install / reinstall:

```bash
chat-history install-skill           # refresh managed + adopt legacy (pre-sidecar) installs
chat-history install-skill --force   # overwrite even user-edited skills
```

The skill is active immediately — no restart needed.

### Build from source

```bash
git clone https://github.com/ay-bh/chat-history.git
cd chat-history
cargo install --path .
```

Run any `chat-history` command once to install the skill (or `chat-history install-skill`).

## Agent usage

When a user asks about past conversations, recent work, or something they discussed before, call the CLI. For search, prefer JSON output so the agent can parse results reliably. `inspect` and `view` return text.

### Preferred invocations

```bash
# List recent sessions
chat-history
chat-history --from yesterday -s
chat-history -L                         # current workspace only

# Search — use --json; add --deep for full-transcript, all-scope search
chat-history search "auth error" --deep --json
chat-history search "timeout" --scope errors --json
chat-history search "trade" --scope similar --json
chat-history search <full-uuid> --json

# Summarize a session
chat-history inspect --last
chat-history inspect <partial-uuid>

# Read / export / resume / locate
chat-history view --last --plain
chat-history view <id> --tools
chat-history export <id> -o session.md
chat-history resume <id>                  # Claude / Codex / Cursor Agent CLI chats — IDE chats print a hint
chat-history find <id>                    # absolute path for further tooling

# Shell completions (bash, zsh, fish, elvish, powershell)
chat-history completions zsh
```

`ch` is a drop-in alias: `ch search "auth" --deep --json`.

### Narrowing scope

These filters apply to session listing, `search`, and `--last` selection. Explicit session-id lookups (`inspect` / `view` / `export` / `resume` / `find`) resolve by id and ignore date/source/project filters. `--sidechains` still controls whether sidechain sessions are loaded at all.

```bash
# Listing / search / --last filters
chat-history --source claude              # claude | cursor | cursor-agent | cursor-ide | codex
                                          # cursor = cursor-agent = jsonl; cursor-ide = sidebar
chat-history --project chat-history
chat-history --branch feature-xyz
chat-history --from "3 days ago" --to today
chat-history --sidechains                 # include hidden subagent/sidechain sessions

# Apply workspace scope to a search (-L works before or after the subcommand)
chat-history search "auth" --deep --json -L

# Listing-only presentation flags
chat-history --from yesterday -s          # group by day
chat-history -k "auth" -v                 # show IDs and paths
```

Date formats: `YYYY-MM-DD`, `today`, `yesterday`, `"3 days ago"`, `"last week"`, `"last month"`.

### Search behavior for agents

| Flag | Why |
|---|---|
| `--deep` | Force full transcript search for the default `all` scope. Specialized scopes already scan transcript content. Snippets are match-centered, not message prefixes. |
| `--json` | Machine-readable search output. This flag is available on `search`, not `inspect` or `view`. |
| (default index) | Fast metadata-only search (title/summary, first prompt, branch, project). Weak results (★ < 5.0) fall through to deep search, except curated summary/title hits at ★ 4.5+. |

All JSON output uses a `{ "query", "count", "results" }` envelope. Deep-search result items include `session_id`, `source`, `also_ide`, `date`, `summary`, `project`, `score`, `role`, `snippet`, `tools`, and `files`. Index result items include `matched_field` instead of `role`, `tools`, and `files`, and the envelope includes `"search_type": "index"`.

Scopes: `all` (default), `errors`, `similar`, `tools`, `files`. Use `--timeframe today|week|month|Nd` and `--limit N` (default 15) to constrain results.

Human-readable results include an 8-char UUID prefix — pass that to `inspect`, `view`, `export`, or `find` for any source, and to `resume` for `claude`, `codex` and Cursor Agent CLI chats (ids that have a store under `~/.cursor/chats`; the CLI is launched in that chat's own workspace). Any other Cursor row — `cursor-ide`, or an Agent transcript whose CLI store is gone — prints the chat's **title** and `DIR:` so you can open it in the Cursor sidebar: the Agent CLI cannot load IDE chats and would start a blank chat that reuses the id.

Example (index search, not `--json`):

```
  1.  cursor-ide    2026-07-30 3f9c1a2e ★ 7.5 DIR: ~/proj INDEX_FIELD: summary
        Restore optimization
  2.  cursor-agent  2026-08-20 2f5bb25d ★ 4.0 DIR: ~/tmp INDEX_FIELD: first_prompt
        Please read over the setup guide…
```

`--json` still includes `session_id`. For IDE Agent chats the JSON `source` is `cursor` (the jsonl store) even though the human tag is `cursor-ide`; those items carry `"also_ide": true`, and `resume` refuses them.

### Interpreting output

- Display tags: `claude` = Claude Code, `cursor-ide` = Cursor IDE sidebar, `cursor-agent` = Agent CLI / jsonl-only, `codex` = Codex
- Cursor IDE **Agent mode** writes SQLite **and** a jsonl with the same composer id. Search lists that pair **once** as `cursor-ide`. `--source cursor` / `cursor-agent` is the jsonl store; `--source cursor-ide` is SQLite.
- Header line: source, date, short id, score, `DIR:` spawn directory (`$HOME` shown as `~`), and (index search) `INDEX_FIELD:`
- Title / match text is on the following indented line
- `★ N.N` = relevance (higher is better)
- `INDEX_FIELD:` is `summary`, `first_prompt`, or `branch`
- `inspect` → duration, messages, model, tokens, tools, files, accomplishments, key decisions
- Claude Code titles come from `ai-title` / `custom-title` JSONL when available
- Subagent/sidechain sessions are omitted unless `--sidechains`; this includes Cursor transcripts under `agent-transcripts/*/subagents/` (tagged `[subagent]`)
- Codex commentary-phase messages are hidden once a turn has a final answer; interrupted turns keep commentary

## Data sources

| Source | Path |
|---|---|
| Claude Code | `~/.claude/projects/*/*.jsonl` (+ optional legacy `sessions-index.json`) |
| Cursor Agent | `~/.cursor/projects/*/agent-transcripts/` (`--source cursor`, alias `cursor-agent`) |
| Cursor IDE | `.../Cursor/User/globalStorage/state.vscdb` (override with `CURSOR_USER_DIR`; `--source cursor-ide`). IDE Agent chats also get a jsonl in the Agent path above; they still list as `cursor-ide`. |
| Cursor subagents | `.../agent-transcripts/*/subagents/*.jsonl` |
| Codex | `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-*.jsonl` (default `~/.codex`) |

Noise filtered automatically: warmups, handshake fluff, clear-only sessions, structural config dumps. Transcripts capped at 4MB each.

## Search scoring (summary)

**Index (default):** field-weighted — summary 3×, first prompt 2×, branch/project 1× — with recency multipliers (3× today / 2× week / 1.5× month). AND across query words.

**Deep (`--deep`):** parallel transcript parse with tech-term boosts, word/prefix/phrase scoring, separator normalization (`_`/`-`/`/` → spaces), importance/semantic boosts, dedup, and a per-session cap of 3 matches.

## Credits

- [claude-historian-mcp](https://github.com/Vvkmnn/claude-historian-mcp) — multi-signal relevance, query similarity, importance heuristics
- [claude-history](https://github.com/raine/claude-history) — prefix matching, separator normalization, recency, cwd project paths
- [search-sessions](https://github.com/sinzin91/search-sessions) — two-tier search, field weights, natural-language dates, per-session cap
- [cursor-history](https://github.com/S2thend/cursor-history) — multi-format Cursor transcript parsing

## License

MIT
