#!/usr/bin/env python3
"""Dump a CONTIGUOUS slice of wikipedia chunks.lance to JSONL.

SP5 fixture: chunks.lance is article-ordered, so a contiguous slice yields
whole articles — a random sample would give ~1 chunk/article and an
artificially thin co-occurrence graph. Offset is fixed and recorded so the
fixture is reproducible.

Usage:
    .venv/bin/python scripts/sp5_dump_wiki.py \
        --corpus ~/.svrnmesh/indexes/wikipedia/chunks.lance \
        --offset 500000 --n 10000 --out data/sp5_wiki_10k.jsonl
"""

import argparse
import json
from pathlib import Path

import lance


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", required=True)
    ap.add_argument("--offset", type=int, default=500000)
    ap.add_argument("--n", type=int, default=10000)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    ds = lance.dataset(str(Path(args.corpus).expanduser()))
    total = ds.count_rows()
    assert args.offset + args.n <= total, f"slice exceeds {total} rows"
    tbl = ds.take(
        list(range(args.offset, args.offset + args.n)),
        columns=["id", "title", "content"],
    ).to_pylist()

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    titles = set()
    with out.open("w") as f:
        for r in tbl:
            titles.add(r["title"])
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
    print(f"wrote {len(tbl)} chunks ({len(titles)} distinct articles) -> {out}")


if __name__ == "__main__":
    main()
