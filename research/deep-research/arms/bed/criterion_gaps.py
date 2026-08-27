#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Per-criterion gap map from a judge sidecar — which link is actually weakest.

The overall RACE number says an arm improved; it never says WHERE, and the
five-point evidence curve plateaus without saying why. This reads the sidecar
the judge already wrote (no inference, no cost) and prints ours-vs-reference
per criterion, so a plateau can be attributed to a dimension rather than
guessed at.

    python3 criterion_gaps.py <run-dir> [arm-to-highlight]

article_1 is OURS, article_2 is the reference the judge was shown.

THE REFERENCE IS RE-SCORED IN EVERY CALL. RACE judges both articles in ONE
prompt, so the reference's absolute scores drift with whatever it was paired
against — measured spread across six arms on task 69: comprehensiveness 0.67,
insight 1.25, instruction_following 0.70, readability 0.21. Every gap is
therefore computed against THAT ARM'S OWN paired reference, never against a
single arm's copy of it. Using one arm's reference as a fixed yardstick (this
script's first version did) silently rewrites the other arms' gaps by up to
1.25 points, in the direction of whichever arm was chosen.

This is also why `overall = t/(t+r)` is the trustworthy number: it is a ratio
against the reference scored in the same call, so the drift cancels.
"""
import json, re, sys, os

DIMS = ["comprehensiveness", "insight", "instruction_following", "readability"]
ARM_ORDER = ["4x2", "8x3", "16x4", "28x5", "44x6", "60x8"]


def load(run_dir):
    side = os.path.join(run_dir, "judge-sidecar.jsonl")
    if not os.path.exists(side):
        sys.exit("REFUSED: no judge-sidecar.jsonl in %s" % run_dir)
    rows = {}
    for line in open(side):
        d = json.loads(line)
        # `.rendered` is part of the arm identity, not noise: it is the SAME
        # draft with production's citation numbering applied, and the pair is
        # the whole point (how much of the readability gap is render clutter).
        # A regex of `[0-9x]+\.md` silently drops it — skipping exactly the row
        # the comparison exists for.
        m = re.search(r"arm-([0-9x]+(?:\.rendered)?)\.md", d.get("article_path", ""))
        if m:
            rows[m.group(1)] = d["judge_output"]
    if not rows:
        sys.exit("REFUSED: sidecar has no arm-*.md rows")
    return rows


def main():
    run_dir = sys.argv[1] if len(sys.argv) > 1 else sys.exit(__doc__)
    rows = load(run_dir)
    known = [a for a in ARM_ORDER if a in rows]
    # A rendered variant sorts immediately after its own draft, so the pair
    # reads as a pair rather than at opposite ends of the table.
    arms = []
    for a in known:
        arms.append(a)
        if a + ".rendered" in rows:
            arms.append(a + ".rendered")
    arms += sorted(set(rows) - set(arms))
    focus = sys.argv[2] if len(sys.argv) > 2 else ("16x4" if "16x4" in rows else arms[-1])
    if focus not in rows:
        sys.exit("REFUSED: arm %s not in sidecar (have %s)" % (focus, ", ".join(arms)))

    head = "  ".join("%-6s" % a for a in arms)
    print("ours (article_1) vs reference (article_2); gap = ours - ref at %s" % focus)
    print("ref/gap use %s's OWN paired reference — it is re-scored every call\n" % focus)
    for dim in DIMS:
        if dim not in rows[arms[0]]:
            continue
        print("== %s ==" % dim.upper())
        print("   %-58s %s   ref    gap" % ("criterion", head))
        for i, c in enumerate(rows[arms[0]][dim]):
            # A criterion the judge scored for one arm but not another is a
            # COULD-NOT-COMPARE, never a zero (18.1) — print it as a blank.
            def sc(a):
                try:
                    return "%-6.1f" % rows[a][dim][i]["article_1_score"]
                except (IndexError, KeyError):
                    return "%-6s" % "--"
            try:
                ref = rows[focus][dim][i]["article_2_score"]
                gap = rows[focus][dim][i]["article_1_score"] - ref
                refs, gaps = "%4.1f" % ref, "%+5.2f" % gap
            except (IndexError, KeyError):
                refs, gaps = "  --", "   --"
            print("   %-58s %s  %s  %s"
                  % (c["criterion"][:58], "  ".join(sc(a) for a in arms), refs, gaps))
        print()

    print("== DIMENSION MEANS ==")
    print("   %-22s %s   ref    gap" % ("dim", head))
    worst = None
    for dim in DIMS:
        if dim not in rows[arms[0]]:
            continue
        mean = lambda a: sum(c["article_1_score"] for c in rows[a][dim]) / len(rows[a][dim])
        # THIS ARM'S OWN paired reference — see the module docstring.
        ref = sum(c["article_2_score"] for c in rows[focus][dim]) / len(rows[focus][dim])
        gap = mean(focus) - ref
        print("   %-22s %s  %4.2f  %+5.2f"
              % (dim, "  ".join("%-6.2f" % mean(a) for a in arms), ref, gap))
        if worst is None or gap < worst[1]:
            worst = (dim, gap)
    print("\n   weakest link at %s: %s (%+.2f)" % (focus, worst[0], worst[1]))


if __name__ == "__main__":
    main()
