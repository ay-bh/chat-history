#!/usr/bin/env python3
"""Compare release binaries on a local, predeclared query set.

Cases are a JSON array: {"query": "...", "category": "...", "args": [],
"targets": ["known-session-id"]}. Targets are optional; never infer relevance
from score or result overlap. Keep private cases and output outside the repo.
"""

import argparse
import json
import os
from pathlib import Path
import statistics
import subprocess
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", required=True, type=Path)
    parser.add_argument("--candidate", required=True, type=Path)
    parser.add_argument("--cases", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--cache-root", required=True, type=Path)
    parser.add_argument("--through", required=True, help="Exclude newer sessions (YYYY-MM-DD)")
    parser.add_argument("--rounds", type=int, default=1)
    parser.add_argument("--limit", type=int, default=15)
    parser.add_argument("--timeout", type=float, default=180)
    parser.add_argument("--candidate-only", action="store_true")
    args = parser.parse_args()
    if args.rounds < 1 or args.limit < 1:
        parser.error("rounds and limit must be positive")
    cases = json.loads(args.cases.read_text())
    engines = {
        "old-default": (args.baseline.resolve(), []),
        "old-deep": (args.baseline.resolve(), ["--deep"]),
        "bm25": (args.candidate.resolve(), ["--engine", "bm25", "--deep"]),
    }
    if args.candidate_only:
        engines = {"bm25": engines["bm25"]}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    records = []
    for round_index in range(args.rounds):
        for case_index, case in enumerate(cases):
            names = list(engines)
            shift = (round_index + case_index) % len(names)
            names = names[shift:] + names[:shift]
            for name in names:
                binary, flags = engines[name]
                env = dict(os.environ)
                for key in ("CHAT_HISTORY_SEARCH_ENGINE", "CHAT_HISTORY_SEARCH_GROUP_BY", "CHAT_HISTORY_REBUILD_INDEX", "CHAT_HISTORY_NO_CACHE"):
                    env.pop(key, None)
                env["CHAT_HISTORY_CACHE_DIR"] = str(args.cache_root.resolve() / name)
                command = [str(binary), "search", case["query"], "--json", "--limit", str(args.limit),
                           "--to", args.through, *flags, *case.get("args", [])]
                start = time.perf_counter()
                try:
                    run = subprocess.run(command, env=env, capture_output=True, text=True, timeout=args.timeout)
                    elapsed = (time.perf_counter() - start) * 1000
                    data = json.loads(run.stdout)
                    record = dict(code=run.returncode, stderr=run.stderr, data=data, ms=elapsed)
                except (subprocess.TimeoutExpired, json.JSONDecodeError) as error:
                    record = dict(error=str(error), ms=(time.perf_counter() - start) * 1000)
                targets = case.get("targets", [case["target"]] if case.get("target") else [])
                results = record.get("data", {}).get("results", [])
                # Keep row rank for UI comparisons and collapsed rank for fair
                # conversation retrieval comparisons across grouping policies.
                session_ids = list(dict.fromkeys(hit["session_id"] for hit in results))
                record.update(engine=name, round=round_index, case=case, command=command,
                              target_rank=next((i + 1 for i, hit in enumerate(results)
                                                if hit["session_id"] in targets), None),
                              target_session_rank=next((i + 1 for i, sid in enumerate(session_ids)
                                                        if sid in targets), None))
                records.append(record)
                # Save each result so interrupted or timed-out runs remain inspectable.
                args.output.write_text(json.dumps(records, indent=2) + "\n")
                print(f"round {round_index + 1}, case {case_index + 1}/{len(cases)}, "
                      f"{name}: {record['ms']:.0f} ms, rank {record['target_rank']}", flush=True)
    for name in engines:
        times = [r["ms"] for r in records if r["engine"] == name]
        print(f"{name}: {len(times)} runs, median {statistics.median(times):.1f} ms, max {max(times):.1f} ms")
    if any("error" in r or r.get("code") != 0 for r in records):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
