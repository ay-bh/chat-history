# Search comparison on a real local profile

Evaluated on 2026-09-14, macOS arm64. This is a local engineering evaluation,
not an independent relevance benchmark or a claim about every user's history.

## Binaries and method

- Baseline: release binary built from `ce77c67`, before the BM25 upgrade.
- Candidate: `feat/bm25-search`, including the fixes discovered in this evaluation.
- Compare baseline default, baseline `--deep`, and candidate BM25. All searches
  use JSON, limit 15, and `--to 2026-09-13` to exclude the ongoing conversation.
- The initial profile had 658 visible sessions, 636 eligible before the cutoff.
  The complete discovery corpus, including hidden sidechains, contained 970
  sessions. The initial index had 40,188 message/metadata rows and 64,712 passages,
  occupying approximately 224 MiB including the metadata cache.
- Sources remained live and read-only. Separate disposable caches were used;
  this is not a frozen-corpus or isolated-machine benchmark. BM25 collection
  statistics still include discovered sessions excluded by the date filter.
- Cases were chosen before viewing their rankings: 24 known-session topic
  queries, 18 transcript-only error/file/identifier queries, and 18 exploratory
  cases covering scopes, source filters, prefixes, infixes, natural language,
  punctuation and absent terms. The two exploratory natural-language queries
  have no predeclared relevance labels and are excluded from accuracy totals.
- Topic targets came from session titles and were checked against source content.
  This deliberately tests navigation and favors the old metadata shortcut.
  Body targets are every eligible indexed session containing the literal query
  in a parsed non-metadata message, ignoring case. One annotation was corrected
  consistently for all engines: `apply_migration` inside a tool name such as
  `mcp__supabase__apply_migration` is a valid literal occurrence, although the
  initial word-boundary extractor missed it.
- Private queries, session IDs, source excerpts and raw result JSON remain in a
  local evaluation directory, outside the repository. This document contains
  aggregate results and generic technical examples only.

## Retrieval results

| Measure | Old default | Old deep | BM25 after fixes |
|---|---:|---:|---:|
| Intended topic session first, 24 queries | 23/24 | 7/24 | 23/24 |
| Intended topic session in first 5 hits | 24/24 | 13/24 | 24/24 |
| Topic MRR@15 | 0.979 | 0.436 | 0.979 |
| Literal-match session first, 18 body queries | 18/18 | 18/18 | 18/18 |
| Hits outside literal-match session set, among up to 5 returned hits per body query | 17/71 | 17/71 | 1/54 |

Ranks count returned messages, including repeated sessions, rather than silently
collapsing them into a different UI. The last row measures lexical selectivity;
it is neither semantic precision nor precision@5 with missing results treated as
failures. A session without the literal string can still discuss a related topic.
The engines return different numbers of hits, which is why denominators differ.

What these establish:

- BM25 retains the old default's strength on title-oriented navigation while
  substantially improving the same tasks over legacy deep ranking.
- Both engines find these exact body terms. BM25 typically avoids padding the
  list with matches to isolated pieces of an error code or identifier.
- `ERR_CONNECTION_REFUSED` returned 15 legacy hits versus one BM25 hit;
  `ERR_MODULE_NOT_FOUND` returned 15 versus two. Their first hits were useful in
  both engines. Searching absent `SQLITE_BUSY` returned seven legacy hits versus
  none with BM25's adjacent-token matching.
- The original BM25 implementation ranked `wall`, `walking` and `Waltham` above
  SQLite discussion for `WAL`. After the exact-term fix, the first five hits
  refer to WAL/database behavior. Prefix lookup still works.
- `dentification` finds `identification` with legacy but returns no BM25 hits.
  BM25 intentionally lacks arbitrary infix matching. Neither engine corrected
  the tested typo. A random absent token returned no hits in either engine.
- Natural-language searches found relevant sessions, but BM25 sometimes surfaced
  tool output instead of a useful explanation. These results do not establish
  semantic search quality or snippet quality.

## Whole-command latency

After the final fixes, 12 representative cases were run three times per engine,
rotating engine order: 108 commands, 36 per engine. No builds, test suites or
parallel benchmark jobs ran during this final measurement. Normal applications
and source writers remained active. Caches already existed; refresh costs are
included. The cases mix three navigation queries, identifiers/files/errors,
one broad query, and absent terms. They are not weighted by actual user traffic.

| Whole-command timing | Old default | Old deep | BM25 |
|---|---:|---:|---:|
| Median | 1,138 ms | 1,196 ms | 104 ms |
| Empirical p95, nearest rank | 1,498 ms | 1,578 ms | 2,898 ms |
| Maximum | 1,506 ms | 1,631 ms | 3,836 ms |

BM25 is much faster on the typical command in this sample, but slower at the
refresh-heavy tail. The old metadata shortcut remains faster for easy title
matches; for example, the exact filename navigation query took 48 and 52 ms
with the old default in two repetitions, though its other repetition took
890 ms. This is not a uniform latency improvement.

Controlled refresh check: copy the real disposable cache, invalidate the
fingerprints of its 574 Cursor entries, and run the same query. Source transcripts
are untouched. Alternate before/after order over two repetitions. The original
`f471f4d` BM25 binary took **25.38 and 25.86 seconds**; the final candidate took
**3.44 and 3.41 seconds**. This confirms the refresh fixes reduce redundant work,
while showing that revalidating this many sources still costs seconds. Discovery
grew from 970 to 971 sessions during the live evaluation.

A separate final invocation with an empty cache directory took **13.33 seconds**
to discover, index and search the profile. This is cold application caching,
not a cold filesystem/page-cache measurement. Neither this single cold run nor
36 latency samples establishes a production latency distribution.

## Fixes prompted by the comparison

1. Include both exact and prefix forms in the FTS expression. Exact matches now
   receive the exact term's IDF as well as the broader prefix contribution.
   The acronym regression test failed before this change and passes afterward.
2. When a source fingerprint changes but complete extracted messages are equal,
   update freshness without deleting/reinserting the same FTS postings. Changes
   to unrelated Cursor data previously caused large redundant index writes.
   A regression test forbids deletion of unchanged messages; it failed before
   the fix. The same test confirms real text edits, forced rebuilds and changed
   extraction policies still replace postings.
3. Parse changed transcripts in batches of at most eight using the existing Rayon
   pool. Writes remain serial in the original transaction; parsed memory is
   bounded by a batch instead of the entire corpus. This addresses the serial
   parsing cost exposed by repeated refresh-heavy searches.

## Reproduction

Build the baseline in an isolated checkout/archive and build the candidate with
`cargo build --release --bin chat-history`. The standard-library-only runner
accepts local JSON cases and saves every command, timing, result and error:

```sh
python3 tools/compare_search.py \
  --baseline /path/to/old/chat-history \
  --candidate target/release/chat-history \
  --cases /private/local/cases.json \
  --output /private/local/results.json \
  --cache-root /private/local/caches \
  --through 2026-09-13 --rounds 3
```

Case format: `{"query":"...","category":"...","args":[],"targets":["session-id"]}`.
Omit targets for exploratory cases. Choose targets before seeing results;
never infer relevance from raw scores. The runner rotates engine order and
records whole CLI latency, including discovery, refresh, retrieval and JSON.

## Limits and validation

The cases are selected, correlated and partly title-derived; no significance
claim or population-wide percentage improvement is justified. There was no
blind human grading of a pooled result set. One live profile cannot establish
latency distributions for other machines or histories. Cold indexing and broad
Cursor refreshes still have costs that warm synthetic timings hide.

All 60 candidate searches returned valid JSON with successful exit codes and
respected the date cutoff. The complete Rust suite passed: 410 tests, three
ignored. Formatting, Clippy with warnings denied, and diff checks passed.
