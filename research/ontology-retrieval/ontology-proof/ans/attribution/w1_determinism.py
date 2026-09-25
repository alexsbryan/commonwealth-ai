#!/usr/bin/env python3
"""W1 attribution: run-to-run input differences in the atlas-grounded path.

For each arm, per question, across its runs:
  answer_identical   — synth.answer byte-equal across runs
  retrieved_equal    — ordered retrieved url list byte-equal across runs
  walk_equal         — atlas_walk (seeds, edges_followed, nodes) byte-equal across runs
  chunk_set_equal    — unordered retrieved membership equal across runs

Classification per question (across runs of one arm):
  fully_deterministic        — answer and retrieved equal
  synth_only                 — retrieved equal, answer differs (sampling)
  retrieval_input_differs    — retrieved differs, answer differs
  retrieval_diff_answer_same — retrieved differs, answer happens to match
"""
import json, sys, os
from collections import Counter

BASE = os.path.dirname(os.path.abspath(__file__))
PROOF = os.path.dirname(BASE)

def load(arm, run):
    p = os.path.join(PROOF, "runs", arm, f"run-{run}", "eval.json") if arm in ("bare", "full") \
        else os.path.join(PROOF, f"runs-{arm}", f"run-{run}", "eval.json")
    return {r["question_id"]: r for r in json.load(open(p))["results"]}

def run_count(arm):
    d = os.path.join(PROOF, "runs", arm) if arm in ("bare", "full") else os.path.join(PROOF, f"runs-{arm}")
    return len([x for x in os.listdir(d) if x.startswith("run-")])

ARMS = sys.argv[1:] or ["bare", "full", "grounding-only"]

def walk_sig(r):
    w = r.get("atlas_walk")
    if not w:
        return None
    return json.dumps({k: w.get(k) for k in ("seeds", "edges_followed", "nodes", "nodes_reached", "requests")},
                      sort_keys=True)

def retrieved_sig(r, ordered=True):
    urls = [c.get("url") or c.get("title") for c in r["retrieved"]]
    return urls if ordered else tuple(sorted(urls))

for arm in ARMS:
    n = run_count(arm)
    if n < 2:
        print(f"== {arm}: only {n} run, skipping"); continue
    runs = [load(arm, i + 1) for i in range(n)]
    qids = sorted(runs[0].keys())
    cls = Counter()
    detail = []
    for q in qids:
        rows = [run[q] for run in runs if q in run]
        ans_eq = len({r["synth"]["answer"] for r in rows}) == 1
        ret_eq = len({json.dumps(retrieved_sig(r)) for r in rows}) == 1
        ret_set_eq = len({json.dumps(retrieved_sig(r, ordered=False)) for r in rows}) == 1
        walk_eq = len({walk_sig(r) for r in rows}) == 1
        if ans_eq and ret_eq:
            c = "fully_deterministic"
        elif ret_eq and not ans_eq:
            c = "synth_only"
        elif not ans_eq and not ret_set_eq:
            c = "retrieval_membership_differs"
        elif not ans_eq:
            c = "retrieval_order_differs"
        else:
            c = "retrieval_diff_answer_same"
        if c != "fully_deterministic":
            w = [walk_sig(r) is not None for r in rows]
            detail.append((q, c, ans_eq, ret_eq, ret_set_eq, walk_eq, all(w)))
        cls[c] += 1
    print(f"== {arm} ({n} runs, {len(qids)} questions)")
    for k, v in sorted(cls.items()):
        print(f"   {k}: {v}")
    for q, c, ans_eq, ret_eq, ret_set_eq, walk_eq, has_walk in detail:
        w = "walk_present" if has_walk else "no_walk"
        print(f"   {q:40s} {c:28s} ans_eq={ans_eq} ret_ord_eq={ret_eq} ret_set_eq={ret_set_eq} walk_eq={walk_eq} {w}")
