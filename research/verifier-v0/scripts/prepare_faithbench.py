#!/usr/bin/env python3
"""Flatten FaithBench release batches into binary eval rows for the harness.

FaithBench annotates (source, summary) pairs with span labels:
Unwanted(.Intrinsic/.Extrinsic), Questionable, Benign. Binary mappings:
  label        (strict):  0 (hallucinated) iff any annotation is Unwanted*
  label_lenient        :  0 iff any annotation is Unwanted* or Questionable*
No annotations => 1 (consistent). Both labels are kept so the eval can report
either; the harness reads `label` (strict) by default.

Output: data/faithbench/test.jsonl with {id, dataset, doc, claim, label,
label_lenient}. Deduped on (source, summary).
"""

import glob
import json
import os
import sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))


def main() -> int:
    files = sorted(glob.glob(os.path.join(ROOT, "data/FaithBench/data_for_release/batch_*.json")))
    if not files:
        sys.exit("no FaithBench batches found; clone vectara/FaithBench into data/")
    seen = set()
    rows = []
    counts = {"unwanted": 0, "questionable_only": 0, "benign_only": 0, "clean": 0}
    for fp in files:
        for s in json.load(open(fp))["samples"]:
            key = (s["source"], s["summary"])
            if key in seen:
                continue
            seen.add(key)
            labels = [l for a in s.get("annotations", []) for l in a.get("label", [])]
            unwanted = any(l.startswith("Unwanted") for l in labels)
            questionable = any(l.startswith("Questionable") for l in labels)
            benign = any(l.startswith("Benign") for l in labels)
            if unwanted:
                counts["unwanted"] += 1
            elif questionable:
                counts["questionable_only"] += 1
            elif benign:
                counts["benign_only"] += 1
            else:
                counts["clean"] += 1
            rows.append(
                {
                    "id": f"FaithBench:{os.path.basename(fp)}:{s['sample_id']}",
                    "dataset": "FaithBench",
                    "doc": s["source"],
                    "claim": s["summary"],
                    "label": 0 if unwanted else 1,
                    "label_lenient": 0 if (unwanted or questionable) else 1,
                }
            )
    outdir = os.path.join(ROOT, "data", "faithbench")
    os.makedirs(outdir, exist_ok=True)
    out = os.path.join(outdir, "test.jsonl")
    with open(out, "w") as f:
        for r in rows:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
    strict0 = sum(1 for r in rows if r["label"] == 0)
    print(f"rows: {len(rows)} | strict hallucinated: {strict0} | {counts}")
    print(f"-> {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
