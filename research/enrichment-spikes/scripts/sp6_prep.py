#!/usr/bin/env python3
"""SP6 fixture prep: docs + chunks + recall queries for the late-chunking spike.

Selects 20 SEP articles from the union of `expected_sources` in
bench/sep/questions.toml (ranked by how many questions expect them, then by
length), reconstructs each article's text from the source parquet
(~/.svrnmesh/indexes/_downloads/sep.parquet, rows in file order per
`category` slug), pulls the production chunks for those articles from
sep/chunks.lance, and emits the query set restricted to questions with at
least one expected article in the pool.

Chunk spans are NOT computed here — the Rust harness re-locates each chunk
in the doc text itself (exact find, then whitespace-normalized fallback)
and reports the unlocatable count as part of the offsets-plumbing finding.

Usage:
    .venv/bin/python scripts/sp6_prep.py \
        --questions ../../sovereign/bench/sep/questions.toml \
        --parquet ~/.svrnmesh/indexes/_downloads/sep.parquet \
        --chunks ~/.svrnmesh/indexes/sep/chunks.lance \
        --n-docs 20 --out-dir data
"""

import argparse
import json
import tomllib
from collections import Counter, defaultdict
from pathlib import Path

import lance
import pyarrow.parquet as pq


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--questions", required=True)
    ap.add_argument("--parquet", required=True)
    ap.add_argument("--chunks", required=True)
    ap.add_argument("--n-docs", type=int, default=20)
    ap.add_argument("--out-dir", default="data")
    args = ap.parse_args()

    bank = tomllib.loads(Path(args.questions).read_text())
    questions = bank["questions"] if "questions" in bank else bank.get("question", [])
    if not questions:
        # bank uses [[questions]] array-of-tables; tomllib maps to key "questions"
        raise SystemExit(f"no questions found; top-level keys: {list(bank)}")

    freq: Counter[str] = Counter()
    for q in questions:
        for slug in q.get("expected_sources", []):
            freq[slug.lower()] += 1
    print(f"questions: {len(questions)}, unique expected slugs: {len(freq)}")

    # Article text from parquet, preserving row order per slug.
    pf = pq.ParquetFile(Path(args.parquet).expanduser())
    texts: defaultdict[str, list[str]] = defaultdict(list)
    wanted = set(freq)
    for batch in pf.iter_batches(columns=["category", "text"]):
        for slug, text in zip(
            batch.column("category").to_pylist(), batch.column("text").to_pylist()
        ):
            if slug and slug.lower() in wanted and text:
                texts[slug.lower()].append(text)
    docs = {slug: "\n".join(rows) for slug, rows in texts.items()}
    missing = wanted - set(docs)
    if missing:
        print(f"WARNING: {len(missing)} expected slugs absent from parquet: {sorted(missing)[:5]}")

    # Rank: most-expected first, then longest (long docs are the point of SP6).
    ranked = sorted(docs, key=lambda s: (-freq[s], -len(docs[s]), s))
    pool = ranked[: args.n_docs]
    pool_set = set(pool)

    # Production chunks for the pool.
    ds = lance.dataset(str(Path(args.chunks).expanduser()))
    quoted = ", ".join(f"'{s}'" for s in pool)
    tbl = ds.to_table(columns=["id", "title", "content"], filter=f"title IN ({quoted})")
    rows = tbl.to_pylist()
    by_slug: defaultdict[str, int] = defaultdict(int)
    for r in rows:
        by_slug[r["title"].lower()] += 1

    out = Path(args.out_dir)
    out.mkdir(exist_ok=True)

    with open(out / "sp6_docs.jsonl", "w") as f:
        for slug in pool:
            f.write(json.dumps({"slug": slug, "text": docs[slug]}) + "\n")
    with open(out / "sp6_chunks.jsonl", "w") as f:
        for r in sorted(rows, key=lambda r: r["id"]):
            f.write(
                json.dumps(
                    {"chunk_id": r["id"], "slug": r["title"].lower(), "text": r["content"]}
                )
                + "\n"
            )

    kept = 0
    with open(out / "sp6_queries.jsonl", "w") as f:
        for q in questions:
            expected_in_pool = sorted(
                {s.lower() for s in q.get("expected_sources", [])} & pool_set
            )
            if not expected_in_pool:
                continue
            kept += 1
            f.write(
                json.dumps(
                    {
                        "qid": q["id"],
                        "question": q["question"],
                        "expected_slugs": expected_in_pool,
                    }
                )
                + "\n"
            )

    print(f"pool: {len(pool)} docs, {len(rows)} chunks, {kept}/{len(questions)} queries kept")
    for slug in pool:
        print(
            f"  {slug:<40} freq={freq[slug]}  chars={len(docs[slug]):>7}  chunks={by_slug[slug]}"
        )


if __name__ == "__main__":
    main()
