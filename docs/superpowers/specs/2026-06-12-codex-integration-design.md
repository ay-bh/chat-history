# Codex CLI Chat-History Integration — Design

**Date:** 2026-06-12
**Status:** Approved

## Goal

Add OpenAI Codex CLI as a third conversation source alongside Claude Code and
Cursor. Machines without Codex installed must work exactly as before — no
crashes, no warnings, just zero Codex sessions.

## Codex on-disk format (verified locally against Codex CLI 0.136.0)

- Sessions live at `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl`
  (default `CODEX_HOME` is `~/.codex`).
- Every line is `{"timestamp": "<RFC3339>", "type": <record>, "payload": {...}}`.
- Record types:
  - `session_meta` (first line): `payload.id`, `payload.timestamp` (created),
    `payload.cwd` (project path), `payload.git.branch` (optional),
    `payload.source.subagent` (present only for sub-agent rollouts).
  - `response_item` with `payload.type == "message"`: `payload.role`
    (`user`/`assistant`/`developer`), `payload.content` — array of
    `{type: "input_text"|"output_text", text}` blocks.
  - `response_item` with `payload.type == "function_call"`: `payload.name` is
    the tool name.
  - Other records (`turn_context`, `event_msg`, `function_call_output`,
    `reasoning`) are ignored.
- Noise that must be filtered from user messages: injected
  `<environment_context>`, `<permissions ...>`, and `<user_instructions>`
  wrappers; `developer`-role messages are skipped entirely.
- Older Codex versions wrote flat records without the `payload` wrapper
  (`{"type":"message","role":...,"content":[...]}`); the parser falls back to
  reading the record itself when `payload` is absent.
- Prior art confirming this format: coding_agent_session_search (cass),
  codex-trace, agent-sessions, codex-history-list.

## Design

Mirror the existing per-source pattern in `src/session.rs`.

### Discovery: `load_codex_sessions()`

- `codex_home()` honors `$CODEX_HOME`, defaults to `~/.codex` (same pattern as
  `CLAUDE_CONFIG_DIR`).
- If `<codex_home>/sessions` does not exist, return `Vec::new()` — identical
  graceful-absence behavior to the Claude and Cursor loaders.
- Glob `sessions/**/rollout-*.jsonl`. For each file, read the first line
  (`session_meta`) for: session id, created timestamp, project (`cwd`), git
  branch, and the sub-agent marker.
- **Sub-agent rollouts are skipped**, matching how Claude `agent-*.jsonl`
  sidechain files are skipped today.
- `modified`/`date` come from file mtime (same as Cursor). `first_prompt` is
  the first `user` message that survives noise filtering, truncated to 300
  chars (same as other sources).
- Sessions get `source: "codex"` and are appended in `load_all_sessions()`.

### Parsing: `parse_codex_jsonl()`

- Iterate lines; extract `user`/`assistant` messages from `response_item`
  message records (with the no-`payload` legacy fallback).
- Skip `developer` role and env-context/permissions/user-instructions noise.
- `function_call` records append the tool name to the **preceding assistant
  message's** `tool_uses` so search scoring sees tool context (Claude gets
  this via `extract_context`; Codex tool calls are separate records).
- Timestamps come from each record's top-level `timestamp`; session id from
  the session_meta line.
- `parse_session()` dispatches on `source == "codex"`.

### Shared changes

- `extract_text()` in `src/parser.rs` learns `"input_text"` and
  `"output_text"` block types (additive; no effect on Claude/Cursor parsing).
- `--source` help text becomes `claude/cursor/codex`.
- `install-skill` additionally writes `SKILL.md` to
  `<codex_home>/skills/chat-history/` (honoring `$CODEX_HOME`) so Codex
  agents can use the tool.
- README and SKILL.md document the new source.

### Error handling

- Unreadable files, malformed JSON lines, and missing fields are skipped
  silently (existing convention throughout `session.rs`).
- Absent `~/.codex` (or `$CODEX_HOME`) directory yields zero sessions, never
  an error.

### Testing

- Unit tests in `src/session.rs`: parse a Codex fixture (message extraction,
  developer/noise filtering, tool-name attachment, legacy flat-record
  fallback).
- CLI integration tests in `tests/cli.rs`: `setup_codex_fixture()` writing a
  rollout file under a temp `HOME/.codex/sessions/...` (same pattern as
  `setup_cursor_fixture`), covering listing, `--source codex` filtering,
  session view, deep search, and the no-`.codex`-dir case.
