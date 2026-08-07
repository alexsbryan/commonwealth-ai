#!/usr/bin/env python3
"""Prepare the held-out bank: filter the fresh harvest for training overlap,
then stratify the export for scoring.

Two mechanical disjointness gates (both must pass before any case is scored):
  1. WINDOW gate: drop any harvested claim whose evidence shares a CHUNK ID
     with any training case (cases_all.jsonl + discards cover every window
     the teacher ever saw, kept or not).
  2. The 13-gram content pass (scripts/contamination_pass.py) runs separately
     on the exported bank — this script only does the id-level gate.

Stratification (after `svrn bench verifier export` on the filtered harvest):
  keep ALL cases of the rare/decisive kinds; cap the abundant grounded kinds;
  DROP ocr_garble (referee bug, audited 2026-08-07 — cosmetic garbles don't
  change truth value, so the label is wrong for the gate's purpose).

Usage:
  heldout_prepare.py filter --harvest data/heldout-sep/claims.json \
      --train-cases data/stream_b/all/cases_all.jsonl \
      --out data/heldout-sep/claims.filtered.json
  heldout_prepare.py stratify --cases data/heldout-sep/stream_b.jsonl \
      --out data/heldout-sep/bank.jsonl --cap 500 --seed 43
"""
import argparse
import json
import random
import sys

DROP_KINDS = {"ocr_garble"}
KEEP_ALL_KINDS = {"entity_swap", "negation_flip", "number_perturb",
                  "cross_chunk_chimera", "distractor_absorption",
                  "unsupported_addition"}


def cmd_filter(args):
    train_chunks = set()
    for path in args.train_cases:
        with open(path) as f:
            for line in f:
                r = json.loads(line)
                ids = r.get("evidence_chunk_ids") or \
                    (r.get("meta") or {}).get("evidence_chunk_ids") or []
                train_chunks.update(ids)
    print(f"training evidence chunk ids: {len(train_chunks)}")

    h = json.load(open(args.harvest))
    before = len(h["items"])
    kept, dropped = [], 0
    for it in h["items"]:
        if set(it.get("evidence_chunk_ids") or []) & train_chunks:
            dropped += 1
        else:
            kept.append(it)
    h["items"] = kept
    json.dump(h, open(args.out, "w"))
    print(f"harvest claims: {before} -> {len(kept)} "
          f"({dropped} dropped for chunk overlap with training)")
    if dropped > before * 0.2:
        print("WARNING: >20% overlap — stride collision worse than expected; "
              "check the window arithmetic before trusting the bank",
              file=sys.stderr)


def cmd_stratify(args):
    rng = random.Random(args.seed)
    by_kind = {}
    with open(args.cases) as f:
        for line in f:
            r = json.loads(line)
            by_kind.setdefault(r["kind"], []).append(r)
    out, report = [], {}
    for kind, rs in sorted(by_kind.items()):
        if kind in DROP_KINDS:
            report[kind] = f"0 (dropped, of {len(rs)})"
            continue
        if kind in KEEP_ALL_KINDS or len(rs) <= args.cap:
            take = rs
        else:
            take = rng.sample(rs, args.cap)
        out.extend(take)
        report[kind] = f"{len(take)} of {len(rs)}"
    rng.shuffle(out)
    with open(args.out, "w") as f:
        for r in out:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
    print(json.dumps({"total": len(out), "by_kind": report}, indent=1))


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    f = sub.add_parser("filter")
    f.add_argument("--harvest", required=True)
    f.add_argument("--train-cases", nargs="+", required=True)
    f.add_argument("--out", required=True)
    s = sub.add_parser("stratify")
    s.add_argument("--cases", required=True)
    s.add_argument("--out", required=True)
    s.add_argument("--cap", type=int, default=500)
    s.add_argument("--seed", type=int, default=43)
    args = ap.parse_args()
    {"filter": cmd_filter, "stratify": cmd_stratify}[args.cmd](args)


if __name__ == "__main__":
    main()
