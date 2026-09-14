# BM25 implementation review

Reviewed before committing `feat/bm25-search`, 2026-09-14. Scope: the new backend,
query construction, cache lifecycle, source selection, existing search/display
integration, tests, documentation and bundled agent instructions.

## Guidance consulted

| Topic | Primary guidance | Application here |
|---|---|---|
| Review method | [Google: what to look for in a code review](https://google.github.io/eng-practices/review/reviewer/looking-for.html) | Review design, user behavior, concurrency, tests and documentation; reproduce faults before fixing them. |
| Search analysis | [Elastic: index and search analysis](https://www.elastic.co/docs/manage-data/data-store/text-analysis/index-search-analysis) | Use the index analyzer for query text too; test combining Unicode characters and structured identifiers. |
| Retrieval semantics | [Elastic: boolean query behavior](https://www.elastic.co/docs/reference/query-languages/query-dsl/query-dsl-bool-query) | Keep matching policy explicit and distinct from ranking; preserve adjacent filename components while allowing OR across query chunks. |
| SQLite consistency | [SQLite isolation](https://www.sqlite.org/isolation.html) | Keep synchronization and retrieval in one snapshot; handle writer contention without returning an inconsistent corpus. |
| Derived indexes | [SQLite external-content FTS pitfalls](https://www.sqlite.org/fts5.html#external_content_table_pitfalls) | Creating FTS tables/triggers does not backfill existing rows; rebuild missing derived assets atomically and verify external-content consistency. |
| Query performance | [SQLite query planning](https://www.sqlite.org/queryplanner.html) | Inspect the data carried through sorting; keep large payloads out of the ranked candidate stream. |
| File freshness | [Git's racy-index discussion](https://git-scm.com/docs/racy-git) | Retain conservative stat/racy-window handling and explicitly version extraction semantics. |
| CLI contracts | [Command Line Interface Guidelines](https://clig.dev/#output) | Keep JSON on stdout, cache/progress notices on stderr, and preserve the legacy engine option. |

These sources guide review criteria; the findings below are observations about
this implementation, not claims that an external author prescribed its design.

## Findings and fixes

| Finding | Fix and regression evidence |
|---|---|
| The reused fuzzy signature collapsed `E100` and `E200`, and ignored everything beyond the first 200 characters. | Deduplicate complete trimmed text with role/tools/files, preserving digits, case and quotes. `numeric_error_codes_are_distinct_search_results` failed before the fix. |
| The three-hit cap applied per file copy, allowing six hits from one session. | Count by provider and session ID, while retaining file-specific index identities. `per_session_cap_applies_across_transcript_copies` failed before the fix. |
| Fixed character cuts manufactured word prefixes at passage starts. | Align passage boundaries to whitespace and leave exceptionally long spans intact. `passage_boundaries_do_not_create_fake_word_prefixes` failed before the fix. |
| Rust-side splitting disagreed with Unicode61 for combining marks and broadened filenames into unrelated token matches. | Safely quote whole query chunks and let Unicode61 analyze them. `query_analysis_preserves_unicode_and_identifier_structure` failed before the fix. |
| Prompt previews borrowed the first user message's timestamp even when it was unrelated text. | Match cleaned preview content to a source message before using its timestamp. `prompt_preview_must_match_the_message_whose_timestamp_it_uses` failed before the fix. |
| The candidate sorter included the full serialized message once per matching passage. | Sort IDs/scores and hydrate only distinct eligible messages. This removes payload amplification for long messages; identified by inspecting the SQL projection. |
| A failure on final cache commit discarded otherwise valid results. | Warn, roll back cache changes, and return results already computed from the snapshot. `cache_commit_failure_does_not_discard_valid_results` injects a deferred constraint failure and failed before the fix. |
| Canonical Cursor merging could remove CLI membership when a store-only row became a readable IDE row. | Keep Agent membership independently of the canonical representation. `bm25_agent_filter_keeps_cli_stores_merged_into_readable_ide_rows` failed before the fix. |
| Recreating a missing FTS table left unchanged sessions unsearchable. | Initialize schema and rebuild derived postings in one transaction; reject incomplete payload schemas. `missing_fts_asset_is_rebuilt_from_existing_message_rows` failed before the fix and now checks FTS external-content integrity. |

Existing tests also cover WAL-only source edits, concurrent profile refreshes,
locked/corrupt-cache fallback, disabled persistence, UUID lookup, scope and date
filters, and preservation of scores across filtering.

## Remaining limits

No known blocking finding remains after these fixes. This is a source and test
review, not independent human review or a production-corpus relevance study.
Cold indexing and very broad query sorting remain proportional to corpus size.
Long unbroken tokens can exceed the passage target. Non-Unix stat fingerprints
cannot detect all same-size edits with restored mtimes. Completely corrupt cache
files fall back to memory until removed; missing derived FTS assets self-repair.
The synthetic timing and relevance results are recorded in
[the architecture document](search-architecture.md).

## Final verification

- `cargo test`: 408 passed; three ignored (two existing tests and the opt-in benchmark).
- The release benchmark was run separately and passed.
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `git diff --check` passed.
- The bundled skill validator passed.

## Follow-up: real-history evaluation

The subsequent [60-case binary comparison](search-evaluation.md) found two
issues that synthetic checks missed: prefix-only matches could outrank exact
acronyms, and shared Cursor database changes triggered redundant FTS writes.
Both now have regression tests that failed before the fixes. Changed-source
parsing also uses bounded parallel batches after refresh latency measurements.
The full suite now passes 410 tests, with the same three opt-in/ignored tests.
Real-history relevance, selectivity, cold/warm latency and remaining refresh
costs are reported separately; the earlier synthetic numbers are not substitutes
for these measurements.
- The judged-query fixture still returns BM25 Recall@5 8/8, MRR@5 1.000.
