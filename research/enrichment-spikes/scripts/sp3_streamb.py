#!/usr/bin/env python3
"""Convert sp3_judge_probe results JSONL into the Stream B seed format.

One output row per scored claim: (member_chunks, claim, verdict) plus
provenance (corpus, model, node_id, level, max_support) — the faithfulness
lane's training-seed shape (sizing doc: lives at sovereign/bench/faithfulness/).

Usage:
    python3 scripts/sp3_streamb.py \
        --results runs/sp3/fast/results.jsonl \
        --corpus obsidian-vault-959ee8a8f330 \
        --model Qwopus3.5-4B-v3-MTP-Q8_0 \
        --out ../../sovereign/bench/faithfulness/obsidian_fast_seed.jsonl
"""

import argparse
import json
from pathlib import Path


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--results", required=True)
    ap.add_argument("--corpus", required=True)
    ap.add_argument("--model", required=True)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    n = 0
    with out.open("w") as f:
        for line in open(args.results):
            r = json.loads(line)
            for c in r["claims"]:
                f.write(
                    json.dumps(
                        {
                            "member_chunks": r["member_chunk_ids"],
                            "claim": c["claim"],
                            "verdict": "supported" if c["supported"] else "unsupported",
                            "max_support": c["max_support"],
                            "chunks_checked": c["chunks_checked"],
                            "corpus": args.corpus,
                            "node_id": r["node_id"],
                            "level": r["level"],
                            "judge_model": args.model,
                        },
                        ensure_ascii=False,
                    )
                    + "\n"
                )
                n += 1
    print(f"wrote {n} claim tuples -> {out}")


if __name__ == "__main__":
    main()
