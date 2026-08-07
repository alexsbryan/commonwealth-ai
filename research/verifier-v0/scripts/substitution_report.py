#!/usr/bin/env python3
"""Read a `substitution_study.py` artifact and answer the one question slice 1
was armed to answer: could our checkpoint stand in for the shipped 35B critic?

THREE READINGS, BECAUSE ONE WOULD MISLEAD.

1. AGREEMENT AT THE ARGMAX. What fraction of claims does the candidate label the
   same way the incumbent did. This is the headline everyone wants and the one
   most likely to flatter: on a corpus where the incumbent calls ~90% of claims
   supported, a model that says "supported" always scores ~90%. So the majority
   baseline is printed beside it, and agreement that fails to beat it is
   reported as a failure however high it looks.

2. DISCRIMINATION OVER THE INCUMBENT'S VERDICT (AUC on `margin`). Argmax
   agreement is one point on a curve; a candidate can disagree everywhere at its
   own threshold and still rank claims exactly as the incumbent does, which is
   what matters for a judge slot whose threshold gets recalibrated anyway
   (tau=0.9 is pinned to the PRIMARY's logit -- a different judge shifts what
   tau means, `grounding/config.rs:50-61`).

3. THE ASYMMETRY, which is the product question. The gate exists to catch
   unsupported claims. Missing one the incumbent caught is a REGRESSION IN THE
   THING THE GATE IS FOR; flagging one the incumbent released costs a rescue
   and, at scale, user trust. These are not interchangeable and are never summed
   into one number here.

COST is reported as measured wall-clock per claim on both sides, because
"cheaper" without a denominator is a lead, not a finding.
"""
import argparse
import collections
import json
import math


def auc(pairs):
    """Rank-based AUC of score vs binary label; ties get average ranks."""
    pairs = [(s, y) for s, y in pairs if s is not None]
    pos = [s for s, y in pairs if y == 1]
    neg = [s for s, y in pairs if y == 0]
    if not pos or not neg:
        return None, len(pos), len(neg)
    order = sorted(range(len(pairs)), key=lambda i: pairs[i][0])
    rank = [0.0] * len(pairs)
    k = 0
    while k < len(order):
        j = k
        while j + 1 < len(order) and pairs[order[j + 1]][0] == pairs[order[k]][0]:
            j += 1
        avg = (k + j) / 2 + 1
        for t in range(k, j + 1):
            rank[order[t]] = avg
        k = j + 1
    s_pos = sum(rank[i] for i in range(len(pairs)) if pairs[i][1] == 1)
    n1, n0 = len(pos), len(neg)
    return (s_pos - n1 * (n1 + 1) / 2) / (n1 * n0), n1, n0


def wilson(k, n, z=1.96):
    if n == 0:
        return (0.0, 0.0)
    p = k / n
    d = 1 + z * z / n
    c = (p + z * z / (2 * n)) / d
    h = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d
    return (100 * (c - h), 100 * (c + h))


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--study", required=True)
    ap.add_argument("--faith", help="the source faithfulness artifact, for the "
                                    "incumbent's own cost/verdict tallies")
    args = ap.parse_args()

    rows = [json.loads(l) for l in open(args.study)]
    total = len(rows)
    scorable = [r for r in rows if r["verifier_margin"] is not None
                and r["incumbent_verdict"] in ("supported", "unsupported")]
    unscorable = total - len(scorable)

    print(f"claims in artifact: {total}")
    print(f"  scorable (candidate produced a verdict AND incumbent labelled it): {len(scorable)}")
    if unscorable:
        print(f"  UNSCORABLE: {unscorable} — reported, never dropped silently (ARCH §18.3)")
    if not scorable:
        print("\nNO SCORABLE CLAIMS — this is a could-not-judge, not a pass.")
        return 4

    models = collections.Counter(r.get("incumbent_model") for r in scorable)
    print(f"  incumbent judge: {', '.join(f'{m} x{n}' for m, n in models.items())}")
    lv = collections.Counter(r.get("level") for r in scorable)
    print(f"  RAPTOR levels: {dict(sorted(lv.items(), key=lambda x: (x[0] is None, x[0])))}")

    # ---------- 1. agreement at the argmax
    inc = [1 if r["incumbent_verdict"] == "supported" else 0 for r in scorable]
    # The candidate's verdict under the INCUMBENT'S OWN decision rule
    # (max_support >= SUPPORTED_TAU), which is what a judge-slot swap ships.
    # `verifier_pred` (the raw argmax) is a different question and is not it.
    cand = [None if r.get("verifier_verdict") is None
            else (1 if r["verifier_verdict"] == "supported" else 0)
            for r in scorable]
    recon = sum(1 for r in scorable if r.get("ranking_reconstructed"))
    print(f"  chunk ranking reconstructed on {recon}/{len(scorable)} claims "
          f"(the artifact records a COUNT, not the judged SET — see "
          f"substitution_study.py's header)")
    agree = sum(1 for a, b in zip(inc, cand) if b is not None and a == b)
    n_cmp = sum(1 for b in cand if b is not None)
    base_rate = sum(inc) / len(inc)
    majority = max(base_rate, 1 - base_rate)
    lo, hi = wilson(agree, n_cmp)
    print("\n--- 1. AGREEMENT AT THE ARGMAX -------------------------------")
    print(f"  agreement            {100 * agree / n_cmp:6.2f}%  [95% CI {lo:.2f}-{hi:.2f}]  n={n_cmp}")
    print(f"  always-'{'supported' if base_rate >= 0.5 else 'unsupported'}' baseline  {100 * majority:6.2f}%   "
          f"(incumbent calls {100 * base_rate:.1f}% supported)")
    if agree / n_cmp <= majority:
        print("  VERDICT: FAILS — does not beat the constant classifier on this corpus.")
    else:
        print(f"  lift over the constant classifier: +{100 * (agree / n_cmp - majority):.2f} pts")

    # ---------- 2. discrimination
    a, n1, n0 = auc([(r["verifier_margin"], y) for r, y in zip(scorable, inc)])
    print("\n--- 2. DISCRIMINATION OVER THE INCUMBENT'S VERDICT -----------")
    if a is None:
        print(f"  AUC undefined — incumbent labelled only one class "
              f"({n1} supported / {n0} unsupported). This corpus cannot "
              f"adjudicate the substitution; pick one with both classes.")
    else:
        print(f"  AUC(margin vs incumbent verdict)  {a:.4f}   "
              f"({n1} supported / {n0} unsupported)")
        print("  0.5 = the candidate's ranking carries no information about "
              "what the incumbent decided.")

    # ---------- 3. the asymmetry
    print("\n--- 3. THE ASYMMETRY (the product question) ------------------")
    both = [(y, c) for y, c in zip(inc, cand) if c is not None]
    tp = sum(1 for y, c in both if y == 0 and c == 0)   # caught what incumbent caught
    fn = sum(1 for y, c in both if y == 0 and c == 1)   # MISSED an unsupported claim
    fp = sum(1 for y, c in both if y == 1 and c == 0)   # flagged a released claim
    tn = sum(1 for y, c in both if y == 1 and c == 1)
    n_uns = tp + fn
    n_sup = fp + tn
    if n_uns:
        print(f"  unsupported claims the incumbent caught: {n_uns}")
        print(f"    candidate ALSO catches   {tp:5d}  ({100 * tp / n_uns:5.1f}%)  <- recall of the gate's purpose")
        print(f"    candidate MISSES         {fn:5d}  ({100 * fn / n_uns:5.1f}%)  <- regression in what the gate is FOR")
    else:
        print("  incumbent flagged NO claims unsupported on this sample — "
              "recall of the gate's purpose is UNMEASURED here, not 100%.")
    if n_sup:
        print(f"  supported claims the incumbent released: {n_sup}")
        print(f"    candidate agrees         {tn:5d}  ({100 * tn / n_sup:5.1f}%)")
        print(f"    candidate FLAGS anyway   {fp:5d}  ({100 * fp / n_sup:5.1f}%)  <- false-alarm cost, paid per turn")

    # ---------- cost
    print("\n--- COST (measured, this run) --------------------------------")
    cand_s = [r["verifier_seconds_total"] for r in scorable
              if r.get("verifier_seconds_total")]
    chunk_s = [c["seconds"] for r in scorable for c in r.get("per_chunk", [])
               if c.get("seconds")]
    if cand_s:
        cand_s.sort()
        chunk_s.sort()
        print(f"  candidate: {sum(cand_s) / len(cand_s):.2f}s per claim "
              f"(median {cand_s[len(cand_s) // 2]:.2f}s) over "
              f"{sum(r['chunks_scored'] for r in scorable) / len(scorable):.1f} chunks/claim")
        print(f"             {sum(chunk_s) / len(chunk_s):.3f}s per chunk-judgement "
              f"(median {chunk_s[len(chunk_s) // 2]:.3f}s), n={len(chunk_s)}")
    print("  incumbent: see the faithfulness run's wall-clock — this artifact "
          "does not carry per-claim timings for the 35B, so any speedup claim "
          "must cite that run, not this one.")

    # ---------- contamination sensitivity
    # SEP is 89% of Stream B, so this corpus is partly the model's TRAINING
    # domain. If agreement is materially higher on claims whose evidence
    # overlaps training text, the headline is contaminated and must not stand.
    flagged = [r for r in scorable if r.get("train_overlap")]
    clean = [r for r in scorable if not r.get("train_overlap")]
    if flagged and clean:
        def agr(rs):
            p = [(1 if r["incumbent_verdict"] == "supported" else 0,
                  None if r.get("verifier_verdict") is None
                  else (1 if r["verifier_verdict"] == "supported" else 0))
                 for r in rs]
            p = [(a, b) for a, b in p if b is not None]
            return (100 * sum(1 for a, b in p if a == b) / len(p), len(p)) if p else (None, 0)
        af, nf = agr(flagged)
        ac, nc = agr(clean)
        print("\n--- CONTAMINATION SENSITIVITY (SEP is 89% of Stream B) -------")
        print(f"  evidence OVERLAPS training text: agreement {af:6.2f}%  n={nf}")
        print(f"  evidence CLEAN of training text: agreement {ac:6.2f}%  n={nc}")
        d = af - ac
        print(f"  delta {d:+.2f} pts — a large positive delta means the headline "
              f"is riding on memorised text.")
    else:
        print("\n--- CONTAMINATION SENSITIVITY --------------------------------")
        print("  NOT RUN — rows carry no `train_overlap` field. Annotate with "
              "the 13-gram pass before quoting an agreement number on SEP.")

    print("\n--- WHAT THIS DOES NOT SETTLE --------------------------------")
    print("  * The incumbent's verdict is a COMPARISON, not ground truth. High")
    print("    agreement means substitutable, not correct — both can be wrong")
    print("    together, and neither was checked against a human here.")
    print("  * One corpus. Disagreement rates are corpus-shaped.")
    print("  * Evidence was held FIXED, so this says nothing about the")
    print("    retrieval coupling that killed the 2026-06 attempt.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
