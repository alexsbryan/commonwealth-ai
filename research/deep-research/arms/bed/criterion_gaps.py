#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Per-criterion gap map from a judge sidecar — which link is actually weakest.

The overall RACE number says an arm improved; it never says WHERE, and the
five-point evidence curve plateaus without saying why. This reads the sidecar
the judge already wrote (no inference, no cost) and prints ours-vs-reference
per criterion, so a plateau can be attributed to a dimension rather than
guessed at.

    python3 criterion_gaps.py <run-dir> [arm-to-highlight] [--why]

`--why` prints the judge's OWN analysis for every criterion where the focus arm
trails the reference by more than half a point. The scores say which link is
weak; the analysis says whether there is a mechanism to pull on, and it is the
difference between choosing a lever and guessing at one. It reads the sidecar
already on disk — no judge call, no cost.

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

WEAKEST IS NOT THE SAME AS MOST VALUABLE, and the per-criterion table above
cannot tell them apart because it is unweighted. Task 69 weights readability
0.15 and insight 0.35, so a point of readability is worth less than half a
point of insight in the overall. The `== GRADIENT ==` block prices each
dimension: how many OVERALL points sit between us and the reference, and
between us and a perfect 10. It uses the vendored `calculate_weighted_scores`
— the same arithmetic `score_one.py` and `score_race.py` run (§10.6), with
per-criterion weights, not the unweighted means printed above. Where the
criteria clone is not on disk the block is SKIPPED and says so; it is never
approximated from the means.
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
        m = re.search(r"arm-([0-9x]+(?:\.[a-z]+)*)\.md", d.get("article_path", ""))
        if m:
            rows[m.group(1)] = d["judge_output"]
    if not rows:
        sys.exit("REFUSED: sidecar has no arm-*.md rows")
    return rows


def main():
    argv = [a for a in sys.argv[1:] if a != "--why"]
    why = "--why" in sys.argv[1:]
    run_dir = argv[0] if argv else sys.exit(__doc__)
    rows = load(run_dir)
    known = [a for a in ARM_ORDER if a in rows]
    # A rendered variant sorts immediately after its own draft, so the pair
    # reads as a pair rather than at opposite ends of the table.
    arms = []
    for a in known:
        arms.append(a)
        for suffix in (".rendered", ".rerendered"):
            if a + suffix in rows:
                arms.append(a + suffix)
    arms += sorted(set(rows) - set(arms))
    focus = argv[1] if len(argv) > 1 else ("16x4" if "16x4" in rows else arms[-1])
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
    gradient(rows[focus], focus)
    if why:
        objections(rows[focus], focus)


def objections(judge_output, focus, floor=-0.5):
    """The judge's reasoning wherever the focus arm trails by more than `floor`.

    WHY THIS IS IN THE INSTRUMENT AND NOT A ONE-OFF. The scores rank the
    dimensions; only the analysis says whether a lever exists. On task 69 it
    named six readability objections, and TWO of them ("the density of
    citations [Source: ev-xx] can be visually cluttering", "long, complex
    sentences packed with data citations") are the RAW DRAFT's internal
    handles, which production strips before the deliverable ships — so part of
    the measured gap is an artifact of scoring `arm-N.md` rather than
    `arm-N.rendered.md`. That distinction is invisible in the numbers and
    obvious in the prose, and re-deriving it by hand every session is how it
    gets missed.

    It is the JUDGE's text, quoted, not a summary of it. Do not paraphrase a
    verdict into a stronger claim than the judge made.
    """
    print("\n== WHY == the judge's own analysis where %s trails by >%.1f"
          % (focus, abs(floor)))
    import textwrap
    for dim in DIMS:
        for c in judge_output.get(dim, []):
            try:
                gap = c["article_1_score"] - c["article_2_score"]
            except KeyError:
                continue
            if gap > floor:
                continue
            print("\n   %s / %s  (%.1f vs %.1f, %+.1f)"
                  % (dim, c["criterion"][:64], c["article_1_score"],
                     c["article_2_score"], gap))
            print(textwrap.fill(c.get("analysis", "(no analysis)"), 92,
                                initial_indent="      ",
                                subsequent_indent="      "))


CLONE = "/home/alexbryan/dev/deep_research_bench"


def criteria_for(task):
    """The task's weights, from the bench clone. `None` if it is not on disk."""
    path = os.path.join(CLONE, "data/criteria_data/criteria.jsonl")
    if not os.path.exists(path):
        return None
    for line in open(path):
        r = json.loads(line)
        if int(r.get("id", -1)) == task:
            return r
    return None


def gradient(judge_output, focus, task=69):
    """Price each dimension in OVERALL points — the number that ranks work.

    Two prices per dimension, both computed by moving ONLY that dimension and
    re-running the real ratio (never a derivative — `overall` is non-linear in
    the target total, and at these magnitudes the linear estimate is off by
    enough to reorder adjacent dimensions):

      to-parity   what closing the gap to the reference is worth. 0.00 when we
                  are already ahead — there is nothing there to win.
      to-ceiling  what a perfect 10 on that dimension is worth, HOLDING THE
                  REFERENCE FIXED. Read it as a LOWER BOUND, not a ceiling —
                  see below.

    THE REFERENCE IS NOT A CONSTANT, AND THAT MAKES to-ceiling A FLOOR. RACE
    judges both articles in one prompt, so a stronger target DEPRESSES the
    reference's own scores in the same call. Measured (drb/bars.json,
    2026-08-26): on task 56 the same judge scored the same reference 9.163
    against a Perplexity-class target and 7.072 against AIQ's — R moved 2.09
    points because the TARGET changed. On task 69, R = 8.56 against our 16x4
    draft and 7.92 against AIQ's article. So real improvement is paid twice,
    once in T and once in a falling R, and every number in this block
    understates it. It is still the right ranking signal: the understatement
    applies to all four dimensions at once.

    NOT A PREDICTION. It says what a dimension is WORTH, never that it can be
    moved — readability was immune to a 7.5x evidence sweep (2026-08-27) while
    being the most under-parity dimension on the board. Read it with the
    per-criterion table above, which says whether there is a mechanism.
    """
    cd = criteria_for(task)
    if cd is None:
        print("\n   == GRADIENT == SKIPPED: no criteria for task %d at %s"
              % (task, CLONE))
        return
    sys.path.insert(0, CLONE)
    try:
        from utils.score_calculator import calculate_weighted_scores
    except ImportError as e:                                    # noqa: BLE001
        print("\n   == GRADIENT == SKIPPED: %s" % e)
        return
    sc = calculate_weighted_scores(judge_output, cd)
    T, R = sc["target"]["total"], sc["reference"]["total"]
    if T + R <= 0:
        print("\n   == GRADIENT == SKIPPED: the judge scored nothing")
        return
    now = T / (T + R)
    w = cd["dimension_weight"]

    def moved(dim, new_t):
        """Overall after this dimension alone moves to `new_t`."""
        t_d = sc["target"]["dims"].get("%s_weighted_avg" % dim, 0.0)
        t2 = T + w.get(dim, 0.0) * (new_t - t_d)
        return t2 / (t2 + R)

    print("\n== GRADIENT == overall points on the table, at %s (now %.4f)"
          % (focus, 100 * now))
    print("   %-22s %6s %7s %7s %7s %11s %11s"
          % ("dim", "weight", "ours", "ref", "gap", "to-parity", "to-ceiling"))
    rows_out = []
    for dim in DIMS:
        key = "%s_weighted_avg" % dim
        if key not in sc["target"]["dims"]:
            continue
        t_d = sc["target"]["dims"][key]
        r_d = sc["reference"]["dims"].get(key, 0.0)
        parity = 100 * (moved(dim, r_d) - now) if r_d > t_d else 0.0
        ceiling = 100 * (moved(dim, 10.0) - now)
        rows_out.append((parity, ceiling, dim, w.get(dim, 0.0), t_d, r_d))
    for parity, ceiling, dim, wd, t_d, r_d in sorted(rows_out, reverse=True):
        print("   %-22s %6.2f %7.2f %7.2f %+7.2f %+11.2f %+11.2f"
              % (dim, wd, t_d, r_d, t_d - r_d, parity, ceiling))
    tot_p = sum(r[0] for r in rows_out)
    tot_c = sum(r[1] for r in rows_out)
    print("   %-22s %6s %7s %7s %7s %+11.2f %+11.2f"
          % ("ALL (moved together)", "", "", "", "", tot_p, tot_c))
    print("   parity would put the overall at %.2f; a perfect 10 at %.2f "
          "or ABOVE (R falls as T rises — see the docstring)"
          % (100 * now + tot_p, 100 * now + tot_c))
    peer_block(cd, sc, now, task)


# The peer whose number the DRB-I objective is defined against, and the
# artifact that measurement wrote. Named here rather than passed in: there is
# one bar, it is already measured, and re-deriving it is the failure mode the
# frame warns about ("THE BAR HAS ALREADY BEEN RUN — find it").
PEER_JUDGE_OUTPUT = ("research/deep-research/drb/overall-derivation/"
                     "flights-aiq-bar/race-20260826T092917/ab-aiq/"
                     "judge_output.jsonl")
PEER_NAME = "AIQ"


def peer_block(cd, ours, now, task):
    """Ours vs the peer, per dimension, same judge and same reference.

    THE ONLY COMPARISON THAT SETTLES THE OBJECTIVE. `to-parity` above measures
    us against the RACE reference, which is not what we are trying to beat.
    This measures us against the article we are trying to beat, scored by the
    same pinned judge on the same task — no offset, no translation.

    Read the two reference columns as data, not as noise. They are the SAME
    reference article scored in two different calls, and the difference is the
    contrast effect: a lower `ref` under the peer means the peer's article
    made the judge grade the reference down. That gap is a second, hidden way
    the peer is ahead, and closing it needs no separate work — it moves when
    our target scores move.
    """
    import os
    if not os.path.exists(PEER_JUDGE_OUTPUT):
        print("\n   == vs %s == SKIPPED: no judge output at %s"
              % (PEER_NAME, PEER_JUDGE_OUTPUT))
        return
    from utils.score_calculator import calculate_weighted_scores
    peer = None
    for line in open(PEER_JUDGE_OUTPUT):
        d = json.loads(line)
        if int(d.get("id", d.get("task_id", -1))) != task:
            continue
        jo = d.get("judge_output") or d
        if isinstance(jo, str):
            jo = json.loads(jo)
        peer = calculate_weighted_scores(jo, cd)
        break
    if peer is None:
        print("\n   == vs %s == SKIPPED: task %d is not in %s"
              % (PEER_NAME, task, PEER_JUDGE_OUTPUT))
        return
    pT, pR = peer["target"]["total"], peer["reference"]["total"]
    p_overall = 100 * pT / (pT + pR) if pT + pR > 0 else 0.0
    print("\n== vs %s == same judge, same task, same reference (%s %.4f, "
          "ours %.4f, gap %+.4f)"
          % (PEER_NAME, PEER_NAME, p_overall, 100 * now, 100 * now - p_overall))
    print("   %-22s %8s %8s %8s %10s %9s"
          % ("dim", "ours", PEER_NAME, "delta", "ref(ours)", "ref(%s)" % PEER_NAME))
    for dim in DIMS:
        k = "%s_weighted_avg" % dim
        if k not in ours["target"]["dims"] or k not in peer["target"]["dims"]:
            continue
        o, p = ours["target"]["dims"][k], peer["target"]["dims"][k]
        print("   %-22s %8.2f %8.2f %+8.2f %10.2f %9.2f"
              % (dim, o, p, o - p, ours["reference"]["dims"].get(k, 0.0),
                 peer["reference"]["dims"].get(k, 0.0)))
    print("   totals                 %8.4f %8.4f %+8.4f %10.4f %9.4f"
          % (ours["target"]["total"], pT, ours["target"]["total"] - pT,
             ours["reference"]["total"], pR))


if __name__ == "__main__":
    main()
