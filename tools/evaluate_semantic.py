#!/usr/bin/env python3
"""Local research experiment, not a runtime dependency of chat-history.

Requires numpy, torch and sentence-transformers in an isolated environment.
Pre-download the two public models; this runner operates offline. Documents are
[{"passage_id": 1, "session_id": "...", "text": "..."}], extracted from a
frozen search index and filtered to the intended evaluation corpus. Cases use
the same target/targets format as compare_search.py. Keep inputs/output private.
"""

import argparse
import hashlib
from importlib.metadata import version
import json
import os
from pathlib import Path
import sqlite3
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("documents", "index", "cases", "output", "model-cache"):
        parser.add_argument("--" + name, required=True, type=Path)
    parser.add_argument("--device", default="cpu")
    parser.add_argument("--encoder-revision", required=True, help="Pinned model commit already in the local cache")
    parser.add_argument("--reranker-revision", required=True, help="Pinned model commit already in the local cache")
    args = parser.parse_args()
    os.environ.update(HF_HOME=str(args.model_cache), HF_HUB_OFFLINE="1", HF_HUB_DISABLE_TELEMETRY="1")
    import numpy as np
    import torch
    from sentence_transformers import CrossEncoder, SentenceTransformer

    torch.set_num_threads(4)
    raw = args.documents.read_bytes()
    documents = json.loads(raw)
    cases = json.loads(args.cases.read_text())
    if not documents or not cases:
        parser.error("documents and cases must be nonempty arrays")
    by_id = {d["passage_id"]: i for i, d in enumerate(documents)}
    if len(by_id) != len(documents):
        parser.error("passage_id must be unique")
    model_name = "sentence-transformers/all-MiniLM-L6-v2"
    started = time.perf_counter()
    model = SentenceTransformer(model_name, revision=args.encoder_revision,
                                device=args.device, local_files_only=True,
                                model_kwargs={"use_safetensors": True})
    load_seconds = time.perf_counter() - started
    cache = args.output.with_suffix(".embeddings.npy")
    stamp = args.output.with_suffix(".embeddings.json")
    identity = dict(documents_sha256=hashlib.sha256(raw).hexdigest(), model=model_name,
                    revision=args.encoder_revision,
                    max_seq_length=model.max_seq_length)
    started = time.perf_counter()
    if cache.exists() and stamp.exists() and json.loads(stamp.read_text()) == identity:
        vectors = np.load(cache)
    else:
        batches = []
        for offset in range(0, len(documents), 512):
            batches.append(model.encode([d["text"] for d in documents[offset:offset + 512]],
                                        batch_size=64, normalize_embeddings=True, show_progress_bar=False))
            print(f"Encoded {min(offset + 512, len(documents))}/{len(documents)} passages", flush=True)
        vectors = np.concatenate(batches)
        np.save(cache, vectors)
        stamp.write_text(json.dumps(identity, indent=2))
    index_seconds = time.perf_counter() - started
    started = time.perf_counter()
    reranker = CrossEncoder("cross-encoder/ms-marco-MiniLM-L6-v2", revision=args.reranker_revision, device=args.device,
                           local_files_only=True, max_length=384, model_kwargs={"use_safetensors": True})
    reranker_load_seconds = time.perf_counter() - started
    configuration = dict(reranker="cross-encoder/ms-marco-MiniLM-L6-v2",
                         reranker_revision=args.reranker_revision, device=args.device,
                         lexical_candidates=100, dense_candidates=100, rrf_k=60,
                         rerank_candidates=50, rerank_max_length=384,
                         packages={name: version(name) for name in
                                   ("sentence-transformers", "transformers", "torch", "numpy")})
    conn = sqlite3.connect(f"file:{args.index.resolve()}?mode=ro", uri=True)
    records = []

    def session_ranking(indices):
        seen = set()
        result = []
        for i in indices:
            sid = documents[i]["session_id"]
            if sid not in seen:
                seen.add(sid)
                result.append(sid)
            if len(result) == 15:
                break
        return result

    for case in cases:
        query = case["query"]
        # Plain-text FTS clauses, preserving adjacent identifier components.
        terms = sorted({t.lower() for t in query.split() if any(c.isalnum() for c in t)})
        clauses = []
        for term in terms:
            literal = '"' + term.replace('"', '""') + '"'
            clauses.append(f"({literal} OR {literal}*)" if sum(c.isalnum() for c in term) >= 2 else literal)
        expression = " OR ".join(clauses)
        lexical = []
        if expression:
            # Baseline lexical candidate generation; no learned model or target
            # labels enter this stage. Filter to the frozen eligible documents.
            for (pid,) in conn.execute("SELECT rowid FROM passages_fts WHERE passages_fts MATCH ? ORDER BY bm25(passages_fts,1,3,2,.5,.5),rowid", (expression,)):
                if pid in by_id:
                    lexical.append(by_id[pid])
                if len(lexical) == 100:
                    break
        started = time.perf_counter()
        vector = model.encode(query, normalize_embeddings=True, show_progress_bar=False)
        scores = vectors @ vector
        dense = np.argsort(-scores, kind="stable")[:100].tolist()
        semantic_ms = (time.perf_counter() - started) * 1000
        fused = {}
        for ranking in (lexical, dense):
            for rank, i in enumerate(ranking, 1):
                fused[i] = fused.get(i, 0) + 1 / (60 + rank)
        hybrid = sorted(fused, key=lambda i: (-fused[i], i))
        candidates = hybrid[:50]
        started = time.perf_counter()
        scores = reranker.predict([(query, documents[i]["text"]) for i in candidates],
                                  batch_size=32, show_progress_bar=False)
        reranked = [candidates[i] for i in np.argsort(-scores, kind="stable")]
        rerank_ms = (time.perf_counter() - started) * 1000
        rankings = {name: session_ranking(ids) for name, ids in
                    [("lexical", lexical), ("dense", dense), ("hybrid", hybrid), ("reranked", reranked)]}
        targets = set(case.get("targets", [case["target"]] if case.get("target") else []))
        ranks = {name: next((i + 1 for i, sid in enumerate(ids) if sid in targets), None)
                 for name, ids in rankings.items()}
        records.append(dict(case=case, rankings=rankings, ranks=ranks, semantic_ms=semantic_ms, rerank_ms=rerank_ms))
        args.output.write_text(json.dumps(dict(identity=identity, load_seconds=load_seconds,
                                              configuration=configuration,
                                              reranker_load_seconds=reranker_load_seconds,
                                              index_seconds=index_seconds, document_count=len(documents),
                                              records=records), indent=2))
        print(f"Query {len(records)}/{len(cases)}: {ranks}; semantic {semantic_ms:.0f}ms, rerank {rerank_ms:.0f}ms", flush=True)


if __name__ == "__main__":
    main()
