#!/usr/bin/env python3
"""M0 contamination pass: training streams vs external test sets.

Two checks, per VERIFIER_V0.md section 3:
  1. Canary strings: LLM-AggreFact rows embed a `contamination_identifier`
     canary. If any appears in a training stream, that stream contains
     benchmark rows verbatim.
  2. 13-gram word-shingle overlap (the GPT-3/Llama dedup convention):
     shingles of every *test* document, hashed into a set; every *training*
     document is scanned for a colliding shingle. A collision means the
     training doc shares a >=13-word verbatim span with a test doc.

Streams checked today: HalluGuard-Preferences-76k (Stream A). Stream B goes
through the same pass when it exists.
Test sets: LLM-AggreFact test parquet + FaithBench data_for_release sources.

Output: findings/contamination_report.json (collision counts + examples).
"""

import glob
import hashlib
import json
import os
import re
import sys

N = 13
ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
WORD_RE = re.compile(r"[a-z0-9]+")


def shingles(text: str):
    words = WORD_RE.findall(text.lower())
    for i in range(len(words) - N + 1):
        yield hash(" ".join(words[i : i + N]))


def main() -> int:
    # --- test-set shingle index -------------------------------------------
    import pyarrow.parquet as pq

    test_docs = {}  # doc text -> (benchmark, subset/sample id)
    t = pq.read_table(os.path.join(ROOT, "data/llm-aggrefact/test.parquet"))
    for ds, doc in zip(t.column("dataset").to_pylist(), t.column("doc").to_pylist()):
        test_docs.setdefault(doc, ("LLM-AggreFact", ds))
    canaries = set(t.column("contamination_identifier").to_pylist())

    fb_files = sorted(glob.glob(os.path.join(ROOT, "data/FaithBench/data_for_release/batch_*.json")))
    for fp in fb_files:
        for s in json.load(open(fp))["samples"]:
            test_docs.setdefault(s["source"], ("FaithBench", os.path.basename(fp)))

    print(f"unique test docs: {len(test_docs)}")
    index = {}  # shingle hash -> (benchmark, subset) of first-seen doc
    for doc, origin in test_docs.items():
        for h in shingles(doc):
            index.setdefault(h, origin)
    print(f"test shingle index: {len(index):,} 13-grams")

    # --- scan the training stream -----------------------------------------
    src = glob.glob(
        os.path.expanduser(
            "~/.cache/huggingface/hub/datasets--lrsbrgrn--HalluGuard-Preferences-76k/"
            "snapshots/*/halluguard-main.jsonl"
        )
    )[0]

    canary_hits = 0
    collisions = []  # (row_index, benchmark, subset)
    per_benchmark = {"LLM-AggreFact": 0, "FaithBench": 0}
    rows = 0
    with open(src) as f:
        for i, line in enumerate(f):
            rows += 1
            d = json.loads(line)
            pj = json.loads(d["prompt"][0]["content"])
            doc = pj["document"]
            hit = None
            for h in shingles(doc):
                if h in index:
                    hit = index[h]
                    break
            if hit:
                per_benchmark[hit[0]] += 1
                # Record every collision: the exclusion list for the training
                # stream is built from train_row, so this must be complete.
                collisions.append({"train_row": i + 1, "benchmark": hit[0], "subset": hit[1], "claim": pj["claim"][:120]})

    # canary grep over the raw file (verbatim-row check)
    raw = open(src, "rb").read()
    for c in canaries:
        if c and c.encode() in raw:
            canary_hits += 1

    report = {
        "training_stream": "HalluGuard-Preferences-76k",
        "training_rows": rows,
        "ngram": N,
        "test_sets": {
            "LLM-AggreFact": "test.parquet (29,320 rows)",
            "FaithBench": f"{len(fb_files)} release batches",
        },
        "unique_test_docs": len(test_docs),
        "test_shingles": len(index),
        "canary_hits": canary_hits,
        "colliding_training_rows": per_benchmark,
        "collision_examples": collisions,
        "verdict": (
            "CLEAN" if canary_hits == 0 and sum(per_benchmark.values()) == 0 else "COLLISIONS FOUND"
        ),
    }
    out = os.path.join(ROOT, "findings", "contamination_report.json")
    with open(out, "w") as f:
        json.dump(report, f, indent=2)
    print(json.dumps({k: v for k, v in report.items() if k != "collision_examples"}, indent=2))
    print(f"report -> {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
