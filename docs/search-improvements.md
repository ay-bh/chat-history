# Search follow-up: implementation and evidence

Evaluated on 2026-09-14, macOS arm64, following `b0ce038` on
`feat/bm25-search`. The [earlier comparison](search-evaluation.md) remains a
historical record. Private queries, transcripts and result JSON stay outside
the repository.

## Changes shipped

| Problem | Change | Verification after the change |
|---|---|---|
| An excerpt from the start of a long message could hide the passage responsible for its score. | Compute FTS5 excerpts from the winning passage, after accepting the result; preserve the original message. | Regression covers a distant match, accented text and the shared index analyzer. All 60 existing real-history cases rerun; labeled target ranks preserved. |
| Partial mentions could beat fuller explanations, and reversed word order tied the requested phrase. | Keep OR recall, with a 25% complete-query bonus and a 15% ordered-phrase bonus for multi-chunk queries. | Regression checks ordered versus reversed terms and retains partial matches. A separate coverage case checks a complete passage against a short partial mention. Existing 60 cases rerun with target ranks preserved. |
| Repeated message rows consumed the result limit. | Default BM25 to conversations, each with a best message and up to two additional matches; offer `--group-by message`. | Check distinct-session filling, a one-session limit with children, CLI JSON/human output, explicit flag precedence and legacy opt-in. Existing 60 cases rerun with target ranks preserved. |
| Any change to Cursor's shared database reparsed many unchanged conversations. | Hash each conversation's indexed raw rows in a read snapshot; reuse hashes while database observations are stable. | Unrelated writes leave both fixture sessions unchanged; a same-size WAL edit updates only its conversation. Atomic database replacement remains visible. Existing 60 cases rerun with target ranks preserved. |

Each implementation step passed its relevant regression and CLI tests before the
next step. Final validation passes **417 tests**, with three explicitly ignored
tests, plus `cargo clippy --all-targets -- -D warnings`, formatting and the
bundled skill validator. Release binaries were rebuilt for the final comparison.

The bonuses are small, explicit policy choices. The regression cases establish
their intended behavior; unchanged navigation results do **not** establish that
these weights are optimal or that overall relevance increased. Grouping and
match-aware excerpts improve what is presented, independently of target rank.

## Final binary comparison

All **216 commands** succeeded: 72 cases each against the pre-BM25 binary
(`ce77c67`) in default and deep modes and against the final BM25 release build.
The 72 comprise the earlier 60 cases plus the 12 predeclared paraphrases below.
Source history remained live and read-only; `--to 2026-09-13` excluded the ongoing
session. JSON grouping invariants also passed: distinct conversation rows and no
more than two additional matches.

| Conversation retrieval | Old default | Old deep | Final BM25 |
|---|---:|---:|---:|
| Topic target first, 24 queries | 23 | 7 | 23 |
| Topic target in top 5 | 24 | 17 | 24 |
| Literal-match target first, 18 queries | 18 | 18 | 18 |
| Paraphrase target first, 12 queries | 4 | 3 | 5 |
| Paraphrase target in top 5 | 8 | 7 | 8 |
| Paraphrase MRR@15 | 0.507 | 0.432 | 0.509 |

This table collapses repeated session IDs before measuring rank. Earlier report
tables counted returned message rows; the old-deep top-five figures therefore
differ. Each binary still returned at most 15 rows, so ungrouped engines could
expose fewer than 15 distinct conversations. The grouped UI's larger conversation
window should not be mistaken for a pure scoring improvement. Unlabeled control
queries are excluded from accuracy totals.

### Cost against the previous BM25 build

Separate caches were primed for `b0ce038` and the new build. Twelve representative
cases were run three times per binary, alternating order: **72 whole commands**.
No builds or model jobs ran during measurement; ordinary applications and source
writers remained active. Timings include discovery, refresh, retrieval and JSON
rendering, with each version's default grouping behavior.

| Whole-command timing | Previous BM25 | New BM25 |
|---|---:|---:|
| Median, 36 commands each | 62 ms | 82 ms |
| Empirical p95, nearest rank | 146 ms | 295 ms |
| Maximum | 153 ms | 441 ms |

The richer default costs time in this warm sample. Grouping can accept more
messages and traverse more candidates, and excerpts and coverage checks add
work; these measurements do not isolate each component. The 12 paraphrases were
also compared directly against the previous BM25 binary: both placed 5 targets
first and 8 in the top five. Thus this follow-up has no demonstrated aggregate
top-five relevance gain over that lexical build.

For a controlled Cursor refresh, force the old cache's 574 Cursor fingerprints
to retry; force the new cache's 573 conversation database observations to retry
while preserving their content signatures. The new implementation intentionally
excludes one metadata-only store from the database enrichment dependency.
Sources were untouched. This emulates invalidation after a shared-database change,
not an actual source write or an isolated I/O benchmark.

| Alternating refresh trial | Previous BM25 | New BM25 |
|---|---:|---:|
| First | 2.396 s | 1.453 s |
| Second, reverse order | 1.348 s | 0.704 s |

This shows reduced refresh work in both trials, with substantial variation from
filesystem caching and live activity. The synthetic WAL regression separately
establishes correctness with actual unrelated and same-size source writes.
A new empty-cache invocation took **12.77 seconds**. Cache priming and format
migration are excluded from the warm/controlled measurements. These results do
not establish production percentiles or a universal latency improvement.

## Engineering guidance and decisions

- [SQLite's FTS5 documentation](https://www.sqlite.org/fts5.html#the_snippet_function)
  supplies match-aware excerpts using the index analyzer. Computing excerpts
  only for accepted hits keeps large text out of the ranking sort.
- [Elasticsearch's Boolean query guidance](https://www.elastic.co/docs/reference/query-languages/query-dsl/query-dsl-bool-query)
  separates optional scoring signals from required matches. Here, query coverage
  adds a bonus; it does not impose a hard AND that would discard partial matches.
  The guidance does not prescribe our numeric weights.
- [Field collapse and inner hits](https://www.elastic.co/docs/reference/elasticsearch/rest-apis/collapse-search-results)
  provide a useful model for conversation results: one representative hit and
  additional evidence from its group. Our backend implements that policy without
  adding an Elasticsearch service.
- [SQLite isolation](https://www.sqlite.org/isolation.html) supports reading
  each conversation's dependencies in one snapshot. Persisting
  [`PRAGMA data_version`](https://www.sqlite.org/pragma.html#pragma_data_version)
  across CLI processes would be incorrect: its values are comparable on the
  same connection, not across different connections. File/WAL observations are
  an optimization; the conversation's content digest determines its dependency
  identity after a database change.
- [BLAKE3's streaming interface](https://docs.rs/blake3/latest/blake3/struct.Hasher.html)
  hashes raw SQLite values without JSON deserialization. Indexed range and point
  queries avoid a temporary sort carrying large Cursor payloads. A digest check
  still reads those bytes; this is not a free notification mechanism.
- [Sentence Transformers' retrieve-and-rerank guide](https://www.sbert.net/examples/sentence_transformer/applications/retrieve_rerank/README.html)
  and [reciprocal rank fusion](https://www.elastic.co/docs/reference/elasticsearch/rest-apis/reciprocal-rank-fusion)
  motivate the experiment below: combine lexical and dense candidates by rank,
  then test whether a cross-encoder earns its additional cost. These are design
  options, not guarantees of gains on this history.

## Frozen-corpus neural experiment

This is a local prototype, **not an installed search engine**. No history was
sent to an embedding API. Public model weights were downloaded into an isolated
evaluation environment; the runner then operated offline on a frozen SQLite
backup. No neural package was added to the Rust application.

- 51,237 eligible passages, containing approximately 38.4 MiB of content/title/
  first-prompt text. All passage IDs and text were checked against the backup.
- 54 queries: the existing 24 topic targets and 18 literal body targets, plus
  12 paraphrases declared before observing their rankings. The paraphrases refer
  to known sessions; they are new queries, not held-out users or documents.
- Dense encoder:
  [`sentence-transformers/all-MiniLM-L6-v2`](https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2),
  revision `1110a243fdf4706b3f48f1d95db1a4f5529b4d41`, normalized 384-dimensional
  vectors and its default 256-wordpiece truncation.
- Reranker:
  [`cross-encoder/ms-marco-MiniLM-L6-v2`](https://huggingface.co/cross-encoder/ms-marco-MiniLM-L6-v2),
  revision `233902d25c440f23af6f7d6e94d2946bac0bee0a`, with query/passage pairs
  truncated at 384 tokens in this trial.
- Lexical retrieval uses FTS5 exact/prefix clauses and the production field
  weights, **without the new coverage/phrase bonuses**. It contributes up to
  100 eligible passages; exhaustive dense scoring contributes 100. RRF uses
  `k=60`; the cross-encoder reranks its first 50 passages. Each final ranking is
  collapsed to 15 distinct sessions.
- Four CPU threads, Python 3.12, sentence-transformers 6.0.1, transformers 5.17.0,
  torch 2.14.0 and numpy 2.5.3. The models were loaded without remote code using
  safetensors weights.

| Known target measure | Lexical prototype | Dense | Hybrid RRF | Hybrid + reranker |
|---|---:|---:|---:|---:|
| Topic target first, 24 queries | 23 | 22 | 23 | 22 |
| Topic target in top 5 | 24 | 24 | 24 | 24 |
| Literal-match target first, 18 queries | 18 | 8 | 17 | 17 |
| Literal-match target in top 5 | 18 | 12 | 18 | 18 |
| Paraphrase target first, 12 queries | 5 | 8 | 5 | 5 |
| Paraphrase target in top 5 | 8 | 11 | 11 | 11 |
| Paraphrase target in top 15 | 9 | 12 | 12 | 12 |
| Paraphrase MRR@15 | 0.498 | 0.750 | 0.570 | 0.621 |

Encoding the corpus took **192.4 seconds**. The two downloaded models occupied
approximately **175 MiB**, and the passage vectors approximately **75 MiB**.
With models and vectors already resident, dense query encoding plus exhaustive
scoring took a median **8.2 ms**; reranking added **373 ms** median, **620 ms**
empirical p95. These exclude Python/library startup, model downloads, lexical
retrieval, source refresh and CLI rendering; they are not whole-command latency.

Dense retrieval helps these paraphrases and damages literal lookup. RRF retains
the top-five literal results while recovering three paraphrase targets; it also
moves one literal target off rank one. The reranker does not increase any
top-five total here and has meaningful CPU cost. This supports an **optional
hybrid experiment**, not replacing the default with dense search or automatically
adding this reranker.

This trial cannot rank every search architecture. It uses one small English
encoder, one reranker, fixed candidate budgets and truncation settings, and a
single user's history. Labels are known targets rather than exhaustive graded
relevance judgments; unjudged hits are not necessarily irrelevant. It does not
measure multilingual quality, typo recovery, end-user satisfaction or incremental
embedding freshness. The lexical prototype also differs from the final CLI.

## Extension boundary and remaining work

`SearchBackend::search(&SearchRequest)` remains the replacement boundary. A
future optional hybrid backend can reuse the canonical passage IDs, payloads,
filters, grouping and renderers. It would need a versioned embedding cache keyed
by model revision and passage content, incremental updates/deletions, explicit
local model provisioning, candidate fusion before hydration, and measured
startup/refresh costs. Query routing should be judged on an independent set of
technical identifiers and paraphrases before introducing a classifier or another
handwritten rule list.

The current implementation is a stronger lexical baseline, not a proven optimum.
Broad queries still sort many candidates; grouped results may scan the ranked
stream to fill additional matches. Cursor digest refreshes still read substantial
data after global writes, and non-Unix file identity checks are weaker. Typo
correction, arbitrary infix matching and semantic retrieval are not shipped.

## Reproduction

`tools/compare_search.py` compares actual binaries and records both returned-row
rank and collapsed-session rank. Relevance targets must be declared separately
from the outputs. Use separate caches and exclude the ongoing conversation with
`--through`; alternate binary order for repeated latency measurements.

`tools/evaluate_semantic.py` accepts a private frozen index, eligible document
JSON (`passage_id`, `session_id`, `text`) and query cases (`query`, `targets`).
Extract documents from the same SQLite backup, joining `passages` to `messages`
and restricting to the intended session allowlist. Use content/title/prompt text
and original passage IDs. Keep the files private. Install the versions listed
above in an isolated Python environment and pre-download the pinned public
model revisions, then run:

```sh
python tools/evaluate_semantic.py \
  --documents /private/evaluation/documents.json \
  --index /private/evaluation/search-v1.db \
  --cases /private/evaluation/cases.json \
  --output /private/evaluation/semantic-results.json \
  --model-cache /private/evaluation/models \
  --encoder-revision 1110a243fdf4706b3f48f1d95db1a4f5529b4d41 \
  --reranker-revision 233902d25c440f23af6f7d6e94d2946bac0bee0a
```

The runner caches vectors by document hash and pinned encoder configuration,
records package versions and retrieval budgets, and saves rankings after each
query. Cached reruns report vector-loading time rather than cold encoding time.
The pinned-model replay completed all 54 queries with identical target ranks
across all four retrieval methods.
