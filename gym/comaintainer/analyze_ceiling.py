#!/usr/bin/env python3
"""Analyze the comaintainer ceiling study.

Runs the analysis fixed in PREREG_ceiling_study_20260818.md. Every threshold
and decision branch below was written BEFORE any rating was collected.

    python3 analyze_ceiling.py ceiling-ratings.jsonl [--key study_key.json]

Exits 3 if the ratings are incomplete enough that no verdict is honest —
a could-not-judge, reported rather than defaulted (ARCH 18.3).
"""
import argparse, collections, json, random, sys

BANDS = [(0.80, "RELIABLE"), (0.667, "TENTATIVE CONCLUSIONS ONLY")]
# the one item whose visible text contains its own gold verdict as prose;
# pre-specified sensitivity analysis, never a silent drop.
PROSE_ECHO = "cm-decision-00056-675d6388-t2"
BOOT_SEED, BOOT_N = 20260818, 4000


def kappa(a, b, ids):
    """Cohen's kappa, nominal, two raters over the same ids."""
    n = len(ids)
    if not n:
        return float("nan"), float("nan"), float("nan")
    po = sum(a[i] == b[i] for i in ids) / n
    ma, mb = collections.Counter(a[i] for i in ids), collections.Counter(b[i] for i in ids)
    pe = sum((ma[k] / n) * (mb[k] / n) for k in set(ma) | set(mb))
    return po, pe, (po - pe) / (1 - pe) if pe < 1 else float("nan")


def boot_ci(a, b, ids, seed=BOOT_SEED, n=BOOT_N):
    rng = random.Random(seed)
    vals = []
    for _ in range(n):
        s = [rng.choice(ids) for _ in ids]
        k = kappa(a, b, s)[2]
        if k == k:
            vals.append(k)
    if not vals:
        return float("nan"), float("nan")
    vals.sort()
    return vals[int(0.025 * len(vals))], vals[int(0.975 * len(vals))]


def band(k):
    for thr, name in BANDS:
        if k >= thr:
            return name
    return "NOT RELIABLE"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("ratings")
    ap.add_argument("--key", default="study_key.json")
    ap.add_argument("--interim", action="store_true",
                    help="instrument diagnostics on a partial set. Reports what "
                         "n supports (under-specification rate, cost, verdict "
                         "mix) and REFUSES the ceiling, which needs the full set.")
    a = ap.parse_args()

    rows = [json.loads(l) for l in open(a.ratings) if l.strip()]
    meta = next((r for r in rows if r.get("record") == "meta"), {})
    rat = {r["id"]: r for r in rows if r.get("record") == "rating"}
    key = json.load(open(a.key))

    ids = sorted(set(rat) & set(key))
    missing = len(key) - len(ids)

    if a.interim:
        n = len(ids)
        if not n:
            sys.exit("no ratings in that file")
        op = {i: rat[i]["verdict"] for i in ids}
        gold = {i: key[i]["gold"] for i in ids}
        flags = [i for i in ids if rat[i].get("unjudgeable")]
        opened = [i for i in ids if rat[i].get("expanded")]
        times = sorted(rat[i]["ms"] for i in ids)
        print("=" * 72)
        print(f"INTERIM — {n} of {len(key)} rated. NOT the study.")
        print("=" * 72)
        print("\nWhat this n CAN support (properties of items, not statistics "
              "needing power):")
        print(f"  under-specified as written   {len(flags)}/{n} = {len(flags)/n:.0%}"
              f"   {'<-- >=25% pre-registered branch WOULD FIRE' if len(flags)/n >= .25 else ''}")
        print(f"  needed the background field  {len(opened)}/{n} = {len(opened)/n:.0%}")
        print(f"  median {times[len(times)//2]/1000:.0f}s per item, "
              f"{sum(times)/60000:.1f} min so far, "
              f"{(sum(times)/n)*(len(key)-n)/60000:.0f} min left at this pace")
        mix = collections.Counter(op[i] for i in ids)
        print("\n  your verdict mix so far vs the bank's own class balance:")
        bank_mix = collections.Counter(key[i]["gold"] for i in key)
        for v in sorted(set(mix) | set(bank_mix)):
            print(f"    {v:18} you {mix.get(v,0):>2}/{n} = {mix.get(v,0)/n:>4.0%}"
                  f"    bank {bank_mix.get(v,0)/len(key):>4.0%}")
        po, pe, k = kappa(op, gold, ids)
        lo, hi = boot_ci(op, gold, ids)
        print(f"\nWhat this n CANNOT support:")
        print(f"  kappa point estimate {k:.3f}, 95% CI [{lo:.3f}, {hi:.3f}] — "
              f"width {hi-lo:.2f}.")
        print(f"  That interval spans {band(lo)} to {band(hi)}. It cannot place the")
        print(f"  ceiling in a band, so no decision branch fires and none is printed.")
        print(f"  Raw agreement {po:.0%} is shown for completeness and is inflated "
              f"by base rates.")
        if flags:
            print(f"\nItems you flagged under-specified:")
            for i in flags:
                print(f"  {i}")
        return

    if len(ids) < 0.9 * len(key):
        print(f"COULD-NOT-JUDGE: only {len(ids)}/{len(key)} items rated. The "
              f"pre-registration fixes n=46; a partial set is not the study.",
              file=sys.stderr)
        sys.exit(3)

    op = {i: rat[i]["verdict"] for i in ids}
    gold = {i: key[i]["gold"] for i in ids}
    fro = {i: key[i]["frontier"] for i in ids}
    loc = {i: key[i]["local"] for i in ids}

    print("=" * 72)
    print("COMAINTAINER CEILING STUDY — analysis fixed in advance")
    print(f"  n rated {len(ids)}/{len(key)}" + (f"  ({missing} missing)" if missing else ""))
    print(f"  seed {meta.get('seed')}  started {meta.get('started','?')[:19]}")
    print("=" * 72)

    # ---- PRIMARY -------------------------------------------------------
    po, pe, k = kappa(op, gold, ids)
    lo, hi = boot_ci(op, gold, ids)
    print("\nPRIMARY — operator vs recorded verdict (test-retest ceiling)")
    print(f"  raw agreement {po:.1%}   chance {pe:.1%}")
    print(f"  Cohen's kappa {k:.3f}   95% CI [{lo:.3f}, {hi:.3f}]   -> {band(k)}")

    sens = [i for i in ids if i != PROSE_ECHO]
    ks = kappa(op, gold, sens)[2]
    print(f"  sensitivity, excluding the one prose-echo item: kappa {ks:.3f} "
          f"(n={len(sens)}, delta {ks-k:+.3f})")

    # ---- SECONDARY -----------------------------------------------------
    print("\nSECONDARY — the three-way picture (same items throughout)")
    hdr = f"  {'pair':34} {'raw':>7} {'kappa':>7}  {'95% CI':>18}"
    print(hdr); print("  " + "-" * (len(hdr) - 2))
    for nm, x, y in (("operator vs gold  [CEILING]", op, gold),
                     ("frontier vs gold", fro, gold),
                     ("local    vs gold", loc, gold),
                     ("operator vs frontier", op, fro),
                     ("operator vs local", op, loc),
                     ("frontier vs local", fro, loc)):
        p_, _, kk = kappa(x, y, ids)
        l_, h_ = boot_ci(x, y, ids)
        print(f"  {nm:34} {p_:>6.1%} {kk:>7.3f}  [{l_:>6.3f}, {h_:>6.3f}]")

    # ---- per-class -----------------------------------------------------
    print("\nPer-class recall — where the operator and the record diverge")
    classes = sorted({gold[i] for i in ids})
    print(f"  {'gold class':18} {'n':>3} {'op agrees':>10}   most common operator answer when not")
    for c in classes:
        sub = [i for i in ids if gold[i] == c]
        agree = sum(op[i] == c for i in sub)
        other = collections.Counter(op[i] for i in sub if op[i] != c)
        alt = f"{other.most_common(1)[0][0]} ({other.most_common(1)[0][1]})" if other else "-"
        print(f"  {c:18} {len(sub):>3} {agree/len(sub):>9.0%}   {alt}")

    # ---- label precision ----------------------------------------------
    dis = [i for i in ids if op[i] != gold[i]]
    print(f"\nLabel-precision estimate (secondary outcome 3)")
    print(f"  operator disagrees with the record on {len(dis)}/{len(ids)} = "
          f"{len(dis)/len(ids):.0%} of items.")
    print(f"  Upper bound on achievable agreement for ANY rater against these "
          f"labels: {1-len(dis)/len(ids):.0%}.")
    print(f"  (bank's last measured label precision: 86%, PRE-FIX bank, 2026-08-06)")

    # ---- cost + calibration -------------------------------------------
    times = sorted(rat[i]["ms"] for i in ids)
    med = times[len(times) // 2] / 1000
    print(f"\nCost of the task: median {med:.1f}s per item, "
          f"{sum(times)/60000:.1f} min total.")
    print("Calibration — is the rater's confidence informative?")
    for c in ("high", "medium", "low"):
        sub = [i for i in ids if rat[i].get("confidence") == c]
        if sub:
            acc = sum(op[i] == gold[i] for i in sub) / len(sub)
            print(f"  confidence {c:7} n={len(sub):>3}  agrees with record {acc:.0%}")

    flags = [i for i in ids if rat[i].get("unjudgeable")]
    print(f"\nFlagged under-specified: {len(flags)}/{len(ids)} = {len(flags)/len(ids):.0%}")

    # ---- summary layer (Amendment 1) -----------------------------------
    seen_only = [i for i in ids if not rat[i].get("expanded")]
    opened = [i for i in ids if rat[i].get("expanded")]
    # Amendment 1 (generated summaries) was WITHDRAWN by Amendment 2; the
    # presentation path has no model in it. `expanded` now records whether
    # the rater needed the background field to judge.
    print("\nBACKGROUND USE (Amendment 2)")
    print(f"  judged on proposal+evidence alone  {len(seen_only)}")
    print(f"  needed the background field        {len(opened)}")
    if seen_only and opened:
        ks_ = kappa(op, gold, seen_only)[2]
        ko_ = kappa(op, gold, opened)[2]
        print(f"  kappa | claim+evidence only {ks_:.3f} (n={len(seen_only)})"
              f"   with background {ko_:.3f} (n={len(opened)})   delta {ks_-ko_:+.3f}")
        print("  (these are DIFFERENT ITEMS, not a within-item contrast: the rater "
              "chose\n   which to open, so a delta reports item difficulty, not the "
              "background's value)")
    if opened:
        exp_ms = [rat[i]["ms_to_expand"] for i in opened if rat[i].get("ms_to_expand")]
        if exp_ms:
            exp_ms.sort()
            print(f"  median time before opening background: "
                  f"{exp_ms[len(exp_ms)//2]/1000:.1f}s")

    # ---- PRE-REGISTERED DECISION --------------------------------------
    print("\n" + "=" * 72)
    print("DECISION — the branch fixed before the data")
    print("=" * 72)
    if len(flags) / len(ids) >= 0.25:
        print(f"  FIRES: >=25% flagged under-specified ({len(flags)/len(ids):.0%}).")
        print("  The EPISODES are the problem, not the raters. The bank needs a")
        print("  specification pass before any further scoring.")
    if k >= 0.80:
        print(f"  FIRES: ceiling kappa {k:.3f} >= 0.80 — the task IS reliably")
        print("  specifiable. Charter iteration is the right lever; restate")
        print(f"  OW1/OW2 in kappa against a ceiling of {k:.3f}.")
    elif k >= 0.667:
        print(f"  FIRES: 0.667 <= kappa {k:.3f} < 0.80 — tentatively specifiable.")
        print(f"  Model targets are set AT {k:.3f}, never at 1.0. Any claim about")
        print("  a model is bounded by that.")
    else:
        print(f"  FIRES: ceiling kappa {k:.3f} < 0.667 — the 6-way nominal verdict")
        print("  task is NOT reliably specifiable, and no charter prose fixes it.")
        print("  -> Move to COMAINTAINER.md 6.6 forced-choice probe decomposition.")
        print("  -> STOP iterating the charter against exact-6.")
        print("  -> Charter v1->v8's measured gains are movement inside the noise")
        print("     of an unreliable instrument, and must be re-reported as such.")
    kf, kl = kappa(fro, gold, ids)[2], kappa(loc, gold, ids)[2]
    print(f"\n  Model numbers in context: frontier {kf:.3f}, local {kl:.3f}, "
          f"ceiling {k:.3f}")
    if kf > k:
        print("  NOTE: the frontier scores ABOVE the human ceiling. That does not")
        print("  mean it judges better — it means it has learned the label-generating")
        print("  process, and the ceiling is the wrong reference for it.")
    print()


if __name__ == "__main__":
    main()
