#!/usr/bin/env python3
"""Report for headroom_study.py output: the incumbent-vs-ours table, per kind.

Ground truth is the constructed label — the one referee neither model
supplies (§18.1). Three readings, in order of how much they assume:

1. AUC per side (incumbent: max_support; ours: margin), threshold-free.
2. Operating points as shipped: both at tau 0.5.
3. Ours at MATCHED false-alarm: pick the margin threshold whose FA rate on
   grounded cases equals the incumbent's, then compare catch on ungrounded.
   This is the number a judge-slot swap ships.

ocr_garble is EXCLUDED from every aggregate (referee bug — cosmetic garbles
don't change truth value; audited 2026-08-07) but still printed, flagged, at
the bottom. Errors/unscored rows are reported, never silently dropped
(§18.3).
"""
import argparse
import collections
import json
import math
import random


def wilson(k, n, z=1.96):
    if n == 0:
        return (0.0, 0.0)
    p = k / n
    d = 1 + z * z / n
    c = p + z * z / (2 * n)
    s = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n))
    return ((c - s) / d, (c + s) / d)


def auc(pos, neg):
    """P(score(pos) > score(neg)), ties 0.5. pos = should-score-higher."""
    if not pos or not neg:
        return None
    wins = ties = 0
    sn = sorted(neg)
    import bisect
    for p in pos:
        lo = bisect.bisect_left(sn, p)
        hi = bisect.bisect_right(sn, p)
        wins += lo
        ties += hi - lo
    return (wins + 0.5 * ties) / (len(pos) * len(neg))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--scored", required=True)
    ap.add_argument("--tau", type=float, default=0.5)
    ap.add_argument("--boot", type=int, default=2000)
    args = ap.parse_args()

    # Dedupe by case id: kill-tolerant reruns append tombstone rows (null
    # verdicts) that a later attempt completes. Prefer the complete row;
    # among completes, last write wins.
    by_id = {}
    corrupt = 0
    for l in open(args.scored):
        try:
            r = json.loads(l)
        except json.JSONDecodeError:
            corrupt += 1  # interleaved concurrent append; case was rescored
            continue
        prev = by_id.get(r["id"])
        complete = r["incumbent_verdict"] is not None \
            and r["our_verdict"] is not None
        prev_complete = prev is not None \
            and prev["incumbent_verdict"] is not None \
            and prev["our_verdict"] is not None
        if prev is None or complete or not prev_complete:
            by_id[r["id"]] = r
    rows = list(by_id.values())
    total = len(rows)
    unscored = [r for r in rows if r["incumbent_verdict"] is None
                or r["our_verdict"] is None or r["our_margin"] is None]
    rows = [r for r in rows if r not in unscored]
    core = [r for r in rows if r["kind"] != "ocr_garble"]
    ocr = [r for r in rows if r["kind"] == "ocr_garble"]

    if corrupt:
        print(f"note: {corrupt} corrupt line(s) skipped (concurrent-append "
              f"interleave; affected cases were rescored)")
    print(f"rows: {total} scored, {len(unscored)} unscored/errored "
          f"(reported, excluded), {len(ocr)} ocr_garble (referee bug, "
          f"excluded from aggregates)")

    # ── 1. AUC, threshold-free, per side ─────────────────────────────
    g = [r for r in core if r["label"] == "grounded"]
    u = [r for r in core if r["label"] == "ungrounded"]
    inc_auc = auc([r["incumbent_max_support"] for r in g],
                  [r["incumbent_max_support"] for r in u])
    our_auc = auc([r["our_margin"] for r in g],
                  [r["our_margin"] for r in u])
    print(f"\nAUC vs constructed labels (n={len(core)}: {len(g)} grounded / "
          f"{len(u)} ungrounded)")
    print(f"  incumbent (max_support): {inc_auc:.4f}")
    print(f"  ours      (margin):      {our_auc:.4f}")

    # ── 2. shipped operating points ──────────────────────────────────
    def verdict_err(r, side):
        v = r[f"{side}_verdict"]
        return (v == "supported") if r["label"] == "ungrounded" \
            else (v == "unsupported")

    # ── 3. matched-FA threshold for ours ─────────────────────────────
    # multi_hop_conjunction is excluded from the FA pool: the production
    # procedure judges per-chunk and a two-chunk synthesis claim has no
    # single supporting chunk, so BOTH sides fail ~structurally (measured
    # 99.7% both-fail vs 8.5% when the teacher judged the JOINED window —
    # teacher_label.py:88). It stays in the per-kind table as its own
    # finding about the gate, but letting it set the operating point would
    # match ours to a meaninglessly lax threshold.
    g_fa = [r for r in g if r["kind"] != "multi_hop_conjunction"]
    inc_fa = sum(verdict_err(r, "incumbent") for r in g_fa)
    fa_rate = inc_fa / len(g_fa) if g_fa else 0.0
    margins_g = sorted((r["our_margin"] for r in g_fa), reverse=True)
    # our verdict at threshold t: supported iff margin >= t. FA on grounded =
    # margin < t. Pick smallest t giving FA count <= inc_fa.
    k = max(0, len(margins_g) - inc_fa)
    matched_t = margins_g[k - 1] if k > 0 else float("-inf")
    print(f"\n(FA pool excludes multi_hop_conjunction — structural "
          f"per-chunk failure, see per-kind table; n_grounded for "
          f"matching = {len(g_fa)})")

    def our_matched_err(r):
        sup = r["our_margin"] >= matched_t
        return (not sup) if r["label"] == "grounded" else sup

    print(f"\nincumbent FA on grounded: {inc_fa}/{len(g)} ({100*fa_rate:.1f}%) "
          f"-> matched margin threshold {matched_t:.3f}")

    hdr = (f"{'kind':24s} {'n':>5s} | {'inc err':>7s} {'95% CI':>13s} | "
           f"{'ours@0.5':>8s} | {'ours@mFA':>8s} | {'both':>4s} "
           f"{'inc only':>8s} {'ours only':>9s}")
    print(f"\nper-kind error rates (miss for ungrounded / FA for grounded)")
    print(hdr)
    print("-" * len(hdr))

    def kind_block(rows_k):
        by = collections.OrderedDict()
        for r in rows_k:
            by.setdefault((r["label"], r["kind"]), []).append(r)
        for (label, kind), rs in sorted(by.items()):
            n = len(rs)
            ie = sum(verdict_err(r, "incumbent") for r in rs)
            oe = sum(verdict_err(r, "our") for r in rs)
            me = sum(our_matched_err(r) for r in rs)
            both = sum(verdict_err(r, "incumbent") and our_matched_err(r)
                       for r in rs)
            inc_only = ie - both
            ours_only = me - both
            lo, hi = wilson(ie, n)
            print(f"{kind:24s} {n:5d} | {100*ie/n:6.1f}% "
                  f"[{100*lo:4.1f}-{100*hi:4.1f}%] | {100*oe/n:7.1f}% | "
                  f"{100*me/n:7.1f}% | {both:4d} {inc_only:8d} {ours_only:9d}")

    kind_block(core)

    # headline: catch on ungrounded at matched FA
    inc_miss = sum(verdict_err(r, "incumbent") for r in u)
    our_miss_m = sum(our_matched_err(r) for r in u)
    both_miss = sum(verdict_err(r, "incumbent") and our_matched_err(r)
                    for r in u)
    print(f"\nHEADLINE (ungrounded, n={len(u)}, FA matched at "
          f"{100*fa_rate:.1f}%):")
    print(f"  incumbent catches {len(u)-inc_miss}/{len(u)} "
          f"({100*(len(u)-inc_miss)/len(u):.1f}%)  misses {inc_miss}")
    print(f"  ours      catches {len(u)-our_miss_m}/{len(u)} "
          f"({100*(len(u)-our_miss_m)/len(u):.1f}%)  misses {our_miss_m}")
    print(f"  miss overlap: both {both_miss} | incumbent-only "
          f"{inc_miss-both_miss} | ours-only {our_miss_m-both_miss}")

    # bootstrap CI on catch delta at matched FA
    rng = random.Random(17)
    deltas = []
    for _ in range(args.boot):
        s = [u[rng.randrange(len(u))] for _ in range(len(u))]
        d = (sum(verdict_err(r, "incumbent") for r in s)
             - sum(our_matched_err(r) for r in s)) / len(s)
        deltas.append(d)
    deltas.sort()
    lo, hi = deltas[int(0.025*len(deltas))], deltas[int(0.975*len(deltas))]
    print(f"  catch delta (ours - incumbent): "
          f"{100*(inc_miss-our_miss_m)/len(u):+.1f} pts "
          f"[{100*lo:+.1f}, {100*hi:+.1f}] (bootstrap 95%)")

    if ocr:
        print(f"\nocr_garble (EXCLUDED — referee bug, kept for the record):")
        kind_block(ocr)


if __name__ == "__main__":
    main()
