#!/usr/bin/env python3
"""Dump N real chunks from an installed corpus's chunks.lance to JSONL.

SP1 fixture: 50 real chunks for the GLiNER2-vs-v1 throughput/quality probe.

Usage:
    .venv/bin/python scripts/dump_chunks.py \
        --corpus ~/.svrnmesh/indexes/sep/chunks.lance \
        --n 50 --seed 7 --out data/chunks_50.jsonl
"""

import argparse
import json
import random
from pathlib import Path

import lance


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", required=True, help="path to a chunks.lance dataset")
    ap.add_argument("--n", type=int, default=50)
    ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--out", required=True)
    ap.add_argument("--min-chars", type=int, default=200, help="skip trivially short chunks")
    args = ap.parse_args()

    ds = lance.dataset(str(Path(args.corpus).expanduser()))
    names = ds.schema.names
    total = ds.count_rows()
    print(f"dataset: {args.corpus}  rows={total}  columns={names}")

    text_col = next(c for c in ("content", "text", "chunk") if c in names)
    keep = [c for c in (text_col, "id", "chunk_id", "title", "source_id") if c in names]

    rng = random.Random(args.seed)
    # Oversample indices to survive the min-chars filter, single take() call.
    idxs = sorted(rng.sample(range(total), min(total, args.n * 4)))
    tbl = ds.take(idxs, columns=keep).to_pylist()

    rows = []
    for r in tbl:
        if len(r[text_col] or "") >= args.min_chars:
            r["text"] = r.pop(text_col)
            rows.append(r)
        if len(rows) == args.n:
            break
    if len(rows) < args.n:
        raise SystemExit(f"only {len(rows)} rows >= {args.min_chars} chars (wanted {args.n})")

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    with out.open("w") as f:
        for r in rows:
            f.write(json.dumps(r, ensure_ascii=False, default=str) + "\n")
    lens = sorted(len(r["text"]) for r in rows)
    print(f"wrote {len(rows)} chunks -> {out}  chars p50={lens[len(lens)//2]} max={lens[-1]}")


if __name__ == "__main__":
    main()
