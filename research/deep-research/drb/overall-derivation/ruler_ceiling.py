#!/usr/bin/env python3
"""How our ruler behaves at the top — and the ceiling claim it FALSIFIED.

RACE scores an article against a fixed reference article in one pairwise judge
call: `overall = T/(T+R)`, where T and R are weighted means of per-criterion
scores the official prompt defines as continuous 0-10
(`prompt/score_prompt_en.py:36`).

THE CLAIM THIS SCRIPT WAS WRITTEN TO MAKE, AND WHY IT IS WRONG.
Across 20 scored task-69 draws of our OWN articles, our judge's `reference_total`
sat at 9.19 +- 0.15, and across the 10-task Perplexity A/B arm at 9.34 +- 0.18 —
so nearly invariant that it read as a constant of the instrument. Treating it as
one gives `overall <= 10/(10+R)` ~= 51.7: a hard ceiling that would have put
AIQ's 56.02 and the campaign's 57.55 / 54.81 bars off the scale entirely.

**Measured the same morning, that ceiling is false.** AIQ's own task-56 article
through this exact ruler scores **57.22** — T=9.458, R=7.072. The reference
score fell **2.09 points** from the 9.163 the same judge gave the same reference
on the same task when the target was Perplexity's article. R is not a property
of the ruler. It is a property of the COMPARISON: a pairwise judge scores the
reference relative to the target in front of it, and our own draws all sat in a
narrow band of quality, so R's apparent stability was a restricted-range
artifact. The corr(T, R) = -0.60 already visible within the task-69 draws was
the signal, and it was read as noise.

WHAT THE NUMBERS BELOW ARE, THEN. They are the ruler's behaviour AGAINST OUR
OWN ARTICLES, which is a real and useful thing to know — it is why a 3-point
lever is hard to see and why the offset measured at 43 cannot be extrapolated
to 56 — but it is NOT a bound on what a better article can score. The `R_off <=
10*(1-s)/s` column is a valid bound on the official judge's reference score for
that system's own comparison; comparing it against our R measured against a
DIFFERENT target is apples to oranges, and the "our judge is more generous"
line it used to print has been removed for that reason.

The only sound way to place AIQ on our ruler is to run AIQ's own articles
through it: `score_race.py --arm ab --peer aiq --arm-label ab-aiq`.
"""
import json
import statistics as st
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
CLONE = Path("/home/alexbryan/dev/deep_research_bench")
sys.path.insert(0, str(CLONE))
from utils.score_calculator import calculate_weighted_scores      # noqa: E402

SUBSET_IDS = [56, 58, 59, 62, 65, 69, 78, 83, 90, 95]
PUBLISHED = ["aiq", "gemini-deepresearch", "openai-deepresearch", "perplexity"]
# Our judge's reference_total, per task, from the arm that ran Perplexity's own
# published articles through our pinned 27B (2026-08-24, greedy pin N6).
OUR_JUDGE_ARM = ("flights-pinned-perplexity/race-20260824T152102/ab/"
                 "judge_output.jsonl")


def main() -> None:
    crit = {json.loads(l)["id"]: json.loads(l)
            for l in open(CLONE / "data" / "criteria_data" / "criteria.jsonl")}
    our_r = {}
    for line in open(HERE / OUR_JUDGE_ARM):
        d = json.loads(line)
        s = calculate_weighted_scores(d["judge_output"], crit[d["id"]])
        our_r[d["id"]] = s["reference"]["total"]
    published = {
        name: {json.loads(l)["id"]: json.loads(l)["overall_score"]
               for l in open(HERE / "inputs" / f"{name}-raw_results.jsonl")}
        for name in PUBLISHED
    }

    print("task | our R  | official R <= | gap   | our ceiling (T=10)")
    gaps, ceilings = [], []
    for t in SUBSET_IDS:
        bound = min(10 * (1 - published[n][t]) / published[n][t]
                    for n in PUBLISHED)
        ceiling = 10 / (10 + our_r[t]) * 100
        gaps.append(our_r[t] - bound)
        ceilings.append(ceiling)
        print(f"{t:4d} | {our_r[t]:6.3f} | {bound:13.3f} | {our_r[t]-bound:+6.3f} "
              f"| {ceiling:6.2f}")

    print(f"\nApparent ceiling if R were fixed at its against-OUR-articles value: "
          f"{st.mean(ceilings):.2f} "
          f"(per-task {min(ceilings):.2f}-{max(ceilings):.2f})")
    print("  THIS IS NOT A CEILING. Measured 2026-08-26: AIQ's own task-56")
    print("  article scores 57.22 on this ruler with R=7.072 — the reference")
    print("  score fell 2.09 when the target improved. R moves with the")
    print("  comparison; the apparent stability above is restricted range,")
    print("  because every draw behind it was one of our own weak articles.")
    print(f"  (gaps column retained as diagnostics only: mean "
          f"{st.mean(gaps):+.2f} — NOT a judge-generosity claim, since our R "
          f"and the bound are measured against different targets.)")


if __name__ == "__main__":
    main()
