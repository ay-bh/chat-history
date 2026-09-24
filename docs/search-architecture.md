# Search architecture

`chat-history search` ranks conversations with BM25 over a disposable SQLite
FTS5 index. This document describes the goals of that design, the components
and their boundaries, how the index stays fresh, how queries are matched and
ranked, how failures are handled, and what the design deliberately leaves out.

## Goals

The previous default search returned early when a metadata match crossed a
fixed score threshold, so relevant transcript matches could be omitted
entirely. Deep search reparsed every selected transcript on each query and
scored it with hand-maintained technology terms, substring matches and
multiplicative boosts.

The replacement has four goals:

- Rank metadata and transcript passages in one candidate stream, so a metadata
  match can never suppress transcript results.
- Reparse a transcript only when its source changed.
- Keep source files authoritative. The index is a cache that can be deleted at
  any time.
- Require no background service, model download or network access.

The previous engine has since been removed; only `--scope similar` keeps its
word-overlap scorer.

## Components

```mermaid
flowchart TD
    A[Live discovery across providers] --> B[Complete session snapshot]
    B --> C[Source fingerprints and metadata changes]
    C --> D[Parse changed sessions and recover timestamps]
    D --> E[Atomic message and FTS update]
    B --> F[Source, project, branch, date and sidechain filters]
    Q[Plain-text query] --> G[SearchRequest]
    F --> G
    E --> H[BM25 backend]
    G --> H
    G --> L[Similar-scope scorer]
    H --> I[Deduplicate and group ranked messages]
    I --> J[Match-aware excerpts and shared renderers]
    L --> J
```

The code lives in `src/search_index.rs` (index and BM25 retrieval) and
`src/search.rs` (result types, grouping and the `--scope similar` scorer).

- `SearchBackend::search(&SearchRequest)` is the boundary between the CLI and
  an engine. The backend can be replaced without touching the renderers.
- `Bm25Backend` owns index synchronization and lexical retrieval.
- `SearchResult` carries one message, an optional excerpt and up to two
  `SearchMatch` children. Renderers keep a fallback path for results without
  an excerpt, which `--scope similar` produces.

Two operations bypass BM25. Exact session UUID lookup runs before indexing; a
UUID absent from the selected sessions falls through to a literal
token-sequence search. `--scope similar` keeps the old user-message
similarity calculation so the meaning of that option does not change.

## Storage

The index is `search-v3.db` under the cache directory, separate from the
metadata cache. New directories and files use Unix modes 0700 and 0600.
When the default cache directory is not writable, both caches are served from
a user-private copy under the temp directory, seeded from the existing files
and re-verified session by session on the next sync (`cache_dir.rs`).

| Relation | Purpose |
|---|---|
| `sessions` | Composite source/ID/path identity and freshness fingerprint. |
| `messages` | Original serialized message, ordinal, timestamp and scope flags. |
| `passages` | Message reference and bounded searchable text or metadata fields. |
| `passages_fts` | External-content full-text index over the passage fields. |
| `allowed_sessions` (temporary) | Per-request filter selection, avoiding large SQL `IN` lists. |

Messages are indexed whole: no per-session size cap. Tool output (Claude
`tool_result` blocks, Codex `function_call_output` and `custom_tool_call_output`
records) is a `tool` message whose passages fill the `tool` column instead of
`content`; each tool output keeps its first 12 KiB and last 4 KiB. A tool
result whose call ran `chat-history` or `ch` (the first word of a simple
command, so `cd ~/src/chat-history` does not count) is kept for `view` but not
indexed: it repeats other sessions and made them match twice.

Transcript passages target 1,600 Unicode characters with up to 200 characters
of overlap, aligned to whitespace boundaries. A single longer span stays intact
so chunking never invents a word prefix. Titles, project, branch and
first-prompt previews are separate metadata passages. A first prompt takes the
timestamp of a matching source user message, never an unrelated first message
or the session modification time. Title metadata uses session activity time.

Ranking sorts only message IDs and scores. Original payloads are loaded for
selected candidates alone, and each message is hydrated once from its strongest
matching passage. The FTS5 `snippet()` excerpt is computed from that passage
with the same analyzer, after the message is accepted, so neither payloads nor
snippet text enter the candidate sort. The original message is returned intact
for API consumers.

Changes replace a session's rows. Cascading deletes and FTS triggers maintain
postings in the same transaction. The database filename versions the schema. An
extraction version stored in each fingerprint invalidates unchanged sources when
parser, timestamp, chunking or tokenization policy changes. Missing derived FTS
assets are rebuilt transactionally from existing passage rows. Incomplete
payload tables are treated as an unavailable cache, not an empty corpus.

## Freshness

Discovery supplies the complete corpus before search filters are applied, so
changing a project or source filter does not change collection statistics.

**File-backed sources.** Fingerprints reuse the metadata cache's size, mtime,
Unix ctime/inode/mode and two-second racy-file policy. SQLite WAL and journal
files are included. Non-Unix filesystems lack the ctime and inode checks, so a
same-size edit with a restored mtime can require `--rebuild-index` there.
Fingerprints are versioned objects; older serialized arrays trigger a one-time
conservative refresh.

**Cursor's shared database.** Each conversation depends on a BLAKE3 digest of
its raw bubble, composer-data and composer-header rows, read in one SQLite
snapshot. Indexed range and point queries stream those values without
deserializing JSON or sorting large payloads. IDE sessions depend on these rows;
Agent JSONL sessions also depend on the database for timestamp recovery.
Session metadata and the enrichment profile remain fingerprinted.

Stable database and WAL observations allow digest reuse. After the database
changes, digests are recomputed and only conversations whose digest changed are
parsed. An unchanged conversation has its stored observation refreshed in one
best-effort write so later searches skip the digest again until the next
change. This still reads conversation bytes; it is not a change feed. Cached
read connections reopen when the database file identity changes, including
atomic replacement on Unix.

**Parsing.** Stable unchanged sessions skip transcript parsing. Changed sources
are checked before and after parsing, and unstable or failed reads are retried
on the next invocation. An empty parse of a SQLite-backed source also retries.
An empty parse is accepted only for deliberately metadata-only CLI stores and
for plain transcript files that read cleanly from start to end, such as a lone
`turn_ended` record. Changed sources parse in parallel batches of at most eight
sessions on the existing Rayon pool, which bounds the number of transcripts held
in memory at once. Index writes stay serial and transactional.

**Retention.** After a source changes, identical extracted message payloads keep
their postings. This avoids rewriting every Cursor transcript when unrelated
data changes in the shared database. Complete payloads and ordinals are
compared; rebuild requests and extraction-version changes always regenerate
passages. Sessions absent from the discovery snapshot are removed from the
index. An unavailable source is therefore indexed again when it returns. Source
files are never modified.

## Query and ranking

**Analysis.** Input is plain text. There is no executable FTS syntax, stopword
list, technology vocabulary, stemming, typo correction or arbitrary infix
matching. Whitespace-delimited chunks are deduplicated and safely quoted,
including embedded quotation marks. At most 128 distinct chunks or analyzer
tokens become clauses, so a pasted log cannot build an unbounded expression.
The `unicode61` tokenizer with `remove_diacritics 2` analyzes both indexed text
and query chunks, so composed and combining diacritics fold to the same tokens.

**Matching.** Punctuation separates identifier and path components inside one
adjacent token sequence, so a filename query does not become a broad OR over a
directory, a basename and an extension. A chunk with at least two alphanumeric
characters matches both its exact sequence and a prefix of its final token.
Exact matches contribute their own IDF on top of the prefix contribution, so
`WAL` outranks `wall` or `Waltham`. Single-character chunks match exactly.
Chunks are ORed for recall.

**Scoring.** Column weights are content 1, title 3, first prompt 2, project 0.5,
branch 0.5 and tool output 0.3, with BM25 length normalization over passages. FTS5 reports BM25
as an ascending, often tiny negative number; the backend negates it so higher
positive scores win. The score is then multiplied by
`1 + 0.25 * complete + 0.15 * phrase`, where `complete` means every distinct
query chunk matched the passage, exactly or by prefix, and `phrase` means the
query's analyzed token sequence occurs in order. Both bonuses apply only to
multi-chunk queries and come from separate match sets, so partial matches stay
eligible. These weights are explicit initial policy, not learned or proven
optimal. Newer timestamps break equal scores, then stable session and ordinal
order. JSON preserves the score without rounding tiny values to zero. Scores
are ranking signals, not probabilities, and are not comparable across
queries.

**Tool-output weight.** Chosen on 185 real agent searches whose relevant
sessions were the ones the agent opened next. At weight 1 indexing Codex tool
output lowered MRR (0.267 to 0.250); from 0.5 down to 0.15 MRR was flat at
0.285 to 0.289, and 0.3 sits in the middle. Labels come from what agents were
shown, so they favour the old ranking.

**The calling session.** Search drops the session named by
`CLAUDE_CODE_SESSION_ID`, `CODEX_THREAD_ID` or `CURSOR_CONVERSATION_ID` (Cursor
covers `cursor` and `cursor-ide` rows) before ranking, unless the query is a
UUID. It contains the question being asked and ranked first in 21 of 31 Claude
and 81 of 129 Cursor cases; excluding it raised MRR from 0.287 to 0.398.

**Selection and grouping.** Source, timestamp and scope predicates are applied
before the ranked stream is consumed. Within a conversation, duplicate passage
IDs and identical trimmed text, role, tool and file tuples collapse without
erasing numbers, case, quotes or suffixes. The same text in another session is a
separate hit. At most three messages per source and session ID are accepted
across file copies.

BM25 returns one result per conversation by default: the highest-ranked message
plus up to two additional matches. `--limit` counts conversations, and
`--group-by message` restores individual message rows. There is no fixed
candidate cutoff, so one busy session cannot starve others. Grouped retrieval
continues through the stream to fill children for selected groups and stops
when every selected group has three matches or the stream is exhausted. Sparse
groups can therefore traverse the full ranked stream, although hydration is
capped at three messages per group.

The synthetic title and first-prompt passages usually repeat the first user
message. Within a conversation, a synthetic row whose cleaned text is a prefix
of, or prefixed by, an accepted user message or another synthetic row is
dropped. A user message that arrives after its synthetic echo replaces it at
the same rank, so one sentence cannot occupy all three slots.

`--source cursor-ide` includes canonical Agent transcript rows marked
`also_ide`. `--source cursor-agent` also retains CLI stores represented by
readable IDE rows, which requires a separate cached Agent membership discovery.

## Failure handling

- WAL mode supports concurrent readers. Synchronization commits before
  retrieval so no write lock is held across ranking.
- If another profile replaces the cached corpus between synchronization and
  retrieval, the search is retried, then falls back to an in-memory index.
- Lock contention or an unreadable or corrupt cache also falls back to an
  in-memory rebuild with a warning on stderr. If a cache write cannot be saved,
  results still come from the ephemeral index.
- A database that reports corruption while opening, synchronizing or matching is
  reset in place once per invocation and refilled, unless `quick_check` and the
  FTS `integrity-check` show another process already repaired it. The file is
  never unlinked. If the reset fails, search falls back to memory.
- `--rebuild-index` reparses every session into a usable index.
- `CHAT_HISTORY_NO_CACHE=1` bypasses both disk caches. `search --no-cache`
  bypasses the search index only.

## Compatibility

- BM25 is the only engine. `--deep` and `--engine bm25` are still accepted,
  hidden, and ignored; any other `--engine` value exits with a usage error that
  says the engine was removed. Any other `CHAT_HISTORY_SEARCH_ENGINE` value is
  ignored with a warning.
- JSON always includes the former deep-search result fields. Message results
  expose `additional_matches`; ungrouped rows use an empty array.
  `CHAT_HISTORY_SEARCH_GROUP_BY` controls grouping, with an explicit flag taking
  precedence. Similarity searches default to message rows.
- Relevance ordering and result counts changed from the previous engine. Short
  messages remain eligible, multiple terms need not all match, and a metadata
  match cannot suppress transcript retrieval.

## Alternatives considered

| Candidate | Decision |
|---|---|
| SQLite FTS5 | Selected. Already bundled through `rusqlite`, and one transaction can update payloads and postings together. |
| Tantivy | A reasonable future backend if profiling demands it. Its configurable analyzers and segment-based indexes would add a separate index lifecycle. |
| Replacing only `score_relevance` | Rejected. It would still parse every transcript per query and leave the metadata early return in place. |

[CASS](https://github.com/Dicklesworthstone/coding_agent_session_search)
was studied as prior art. Three of its choices carried over: an explicit engine
boundary, incremental refresh of derived state, and atomic publication of
related records. Its daemon, semantic models, async runtime bridge and storage
stack were not adopted. The [FTS5 documentation](https://www.sqlite.org/fts5.html)
and the [BM25 chapter of Introduction to Information Retrieval](https://nlp.stanford.edu/IR-book/html/htmledition/okapi-bm25-a-non-binary-model-1.html)
cover the ranking function itself.

## Known limitations

- Cold indexing has an up-front parsing and storage cost. Warm searches still
  stat every discovered source, so they are not constant-time in the number of
  sessions.
- Extremely broad queries may sort many matching passages. Specialized top-k
  retrieval, native code tokenizers and semantic fusion should follow
  measurements and relevance judgments rather than be added preemptively.
- Source parsers expose empty results rather than structured read errors, which
  is why empty parses are trusted only for plain files that were re-read
  successfully.
- The shared Cursor database has no change feed. While Cursor is writing, each
  search after a write re-hashes every dependent conversation before digests
  can be reused again.
- FTS damage that raises no SQLite error, such as a segment that decodes to
  nothing, is not detected at query time. `--rebuild-index` repairs it.

## Testing

`tests/search_bm25.rs` covers rare-term ranking, metadata and transcript
recall, Unicode, prefix and path queries, literal query syntax, passage
collapse, per-session limits, filtering and score stability, timestamp
provenance, scopes, persistent refresh, rebuild and deletion, WAL-only edits,
cache failures, UUID lookup, CLI selection, winning-passage excerpts, coverage
and phrase bonuses, group filling and JSON children, unrelated Cursor writes,
same-size WAL changes, and atomic database replacement. The CLI and Cursor
reliability suites exercise the same path.

An eight-query synthetic fixture spans rare identifiers, filenames, error
codes, acronyms, Unicode accents, separator normalization, multiple terms and
prefixes. It asserts intended behavior, not relevance on real histories:

```sh
cargo test --test search_bm25 judged_query_fixture
```

A synthetic benchmark builds 250 sessions and 5,000 messages in a temporary
directory, then measures cold synchronization, a reopened warm index and one
BM25 query, and verifies that warm synchronization reparses zero sessions:

```sh
cargo test --release --test search_bm25 benchmark_cold_and_warm -- --ignored --nocapture
```

Before tuning ranking weights, collect representative queries with expected
sessions and compare Recall@5 and MRR before and after, especially for exact
errors, filenames, short acronyms, verbose queries and old but relevant
conversations.
