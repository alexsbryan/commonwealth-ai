#!/usr/bin/env python3
"""Score the retrieval head-to-head and decide whether a champion may be crowned.

Consumes one eval JSON per arm (from `svrn eval run --prod-pipeline --output`)
plus the sidecar from `bridge-bank-gen.py`, and reports a verdict.

THE ENDPOINT is reciprocal rank of the HARDER required conversation (B — the
one with fewer mentions of the bridging entity). Conversation A is retrieved at
rank 1 almost always, so A carries no signal; B is where arms separate.
Reciprocal rank is bounded, handles "absent from the pool" as a clean 0, and
rewards movement at the top of the list where the prompt's character budget is
actually spent. `source_ratio` (the harness's own score) is reported alongside
as a coarse cross-check, never as the primary.

WHY PAIRED: the sizing gate measured 48% bucket instability from question
wording alone. Between-question variance dwarfs the effects being compared, so
every arm answers the SAME questions and every test is on per-question deltas.

=== THE TWO GATES THAT MUST PASS BEFORE ANY VERDICT IS READ (ARCH §18.4) ===

1. VACUITY. An arm whose retrieved ordering is bit-identical to baseline on
   (nearly) every question DID NOT ENGAGE. It is reported VACUOUS and excluded
   — never as "no effect". This is not hypothetical: the `conv_ppr_off` arm was
   vacuous once already (it ran on a corpus where PPR never fires), and
   `SOVEREIGN_RERANK_DEDUP_CORPORA` defaults to {"sep"} so a dedup arm on
   `conversations-anthropic` silently no-ops unless the var is set.

2. COMPLETENESS. An arm missing questions relative to baseline is reported
   PARTIAL. Aggregates over a different question set are not comparable.

A champion is crowned ONLY IF it beats every other non-vacuous arm on the
primary endpoint with a two-sided sign test at p < 0.05. Otherwise the verdict
is an explicit NO CHAMPION — which is a result, not a failure.

Usage:
  python3 scripts/bridge-arms-analyze.py <sidecar.json> <baseline_arm=path.json> [arm=path.json ...]
"""

import json
import sys
from math import comb


def rank_of(retrieved, title):
    """1-based rank of the first chunk whose title matches; 0 = absent."""
    for i, c in enumerate(retrieved, start=1):
        if c.get("title") == title:
            return i
    return 0


def rr(rank):
    return 1.0 / rank if rank > 0 else 0.0


def sign_test(deltas, eps=1e-9):
    """Two-sided exact binomial sign test on non-zero paired deltas."""
    pos = sum(1 for d in deltas if d > eps)
    neg = sum(1 for d in deltas if d < -eps)
    n = pos + neg
    if n == 0:
        return pos, neg, 1.0
    k = min(pos, neg)
    tail = sum(comb(n, i) for i in range(0, k + 1)) / (2**n)
    return pos, neg, min(1.0, 2 * tail)


def load_arm(path):
    d = json.load(open(path))
    return {r["question_id"]: r for r in d["results"]}


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        sys.exit(2)
    sidecar = json.load(open(sys.argv[1]))

    arms = {}
    order = []
    for spec in sys.argv[2:]:
        name, path = spec.split("=", 1)
        arms[name] = load_arm(path)
        order.append(name)
    base_name = order[0]
    base = arms[base_name]

    # Per-arm, per-question metrics.
    metrics = {}
    for name, res in arms.items():
        m = {}
        for qid, r in res.items():
            sc = sidecar.get(qid)
            if not sc:
                continue
            ra = rank_of(r["retrieved"], sc["title_a"])
            rb = rank_of(r["retrieved"], sc["title_b"])
            m[qid] = {
                "rank_a": ra,
                "rank_b": rb,
                "rr_b": rr(rb),
                "both_top10": 1 if (0 < ra <= 10 and 0 < rb <= 10) else 0,
                "ratio": r["source_score"]["ratio"],
                "sig": tuple(c.get("title") for c in r["retrieved"]),
            }
        metrics[name] = m

    print("=" * 72)
    print("GATE 1 — VACUITY (did the arm change retrieval at all?)")
    print("=" * 72)
    status = {}
    for name in order:
        m = metrics[name]
        shared = [q for q in m if q in metrics[base_name]]
        if name == base_name:
            print(f"  {name:16} BASELINE           n={len(m)}")
            status[name] = "baseline"
            continue
        diff = sum(1 for q in shared if m[q]["sig"] != metrics[base_name][q]["sig"])
        pct = 100 * diff / len(shared) if shared else 0
        if diff == 0:
            status[name] = "VACUOUS"
            print(f"  {name:16} *** VACUOUS ***    identical ordering on all {len(shared)} — arm did not engage")
        else:
            status[name] = "live"
            print(f"  {name:16} live               ordering differs on {diff}/{len(shared)} ({pct:.0f}%)")

    print()
    print("=" * 72)
    print("GATE 2 — COMPLETENESS")
    print("=" * 72)
    nbase = len(metrics[base_name])
    for name in order:
        n = len(metrics[name])
        if n != nbase:
            status[name] = "PARTIAL" if status.get(name) != "VACUOUS" else status[name]
            print(f"  {name:16} *** PARTIAL ***    {n}/{nbase} questions — not comparable")
        else:
            print(f"  {name:16} complete           {n}/{nbase}")

    eligible = [n for n in order if status.get(n) in ("baseline", "live")]

    print()
    print("=" * 72)
    print("DESCRIPTIVES (eligible arms only)")
    print("=" * 72)
    print(f"  {'arm':16} {'mean RR_b':>10} {'B in pool':>10} {'both@10':>9} {'src ratio':>10}")
    for name in eligible:
        m = metrics[name]
        n = len(m)
        mrr = sum(v["rr_b"] for v in m.values()) / n
        inpool = 100 * sum(1 for v in m.values() if v["rank_b"] > 0) / n
        both = 100 * sum(v["both_top10"] for v in m.values()) / n
        ratio = sum(v["ratio"] for v in m.values()) / n
        print(f"  {name:16} {mrr:10.4f} {inpool:9.1f}% {both:8.1f}% {ratio:10.4f}")

    print()
    print("=" * 72)
    print("PAIRED HEAD-TO-HEAD — primary endpoint: reciprocal rank of conversation B")
    print("=" * 72)
    wins = {n: 0 for n in eligible}
    for i, a in enumerate(eligible):
        for b in eligible[i + 1 :]:
            shared = [q for q in metrics[a] if q in metrics[b]]
            deltas = [metrics[a][q]["rr_b"] - metrics[b][q]["rr_b"] for q in shared]
            pos, neg, p = sign_test(deltas)
            if p < 0.05 and pos > neg:
                verdict, win = f"{a} WINS", a
            elif p < 0.05 and neg > pos:
                verdict, win = f"{b} WINS", b
            else:
                verdict, win = "no separation", None
            if win:
                wins[win] += 1
            print(f"  {a:16} vs {b:16} {a[:6]}+{pos:3} {b[:6]}+{neg:3} p={p:.4f}  {verdict}")

    print()
    print("=" * 72)
    contenders = [n for n in eligible if n != "baseline"]
    champs = [n for n in eligible if wins[n] == len(eligible) - 1 and len(eligible) > 1]
    if champs:
        print(f"CHAMPION: {champs[0]} — beat every other eligible arm at p<0.05.")
    else:
        best = max(eligible, key=lambda n: wins[n]) if eligible else None
        print("NO CHAMPION. No arm beat every other eligible arm at p<0.05.")
        if best is not None:
            print(f"  Most pairwise wins: {best} ({wins[best]}/{len(eligible)-1}).")
        print("  This is a result. Do not promote an arm on descriptives alone.")
    vac = [n for n in order if status.get(n) == "VACUOUS"]
    if vac:
        print(f"  EXCLUDED as vacuous (did not engage): {', '.join(vac)}")
    print("=" * 72)


if __name__ == "__main__":
    main()
