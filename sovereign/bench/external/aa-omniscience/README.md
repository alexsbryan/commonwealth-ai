<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# `aa-omniscience/` — the third industry ruler

RewardBench 2 rules on judgment, SWE-bench on agentic coding. This one rules on
**knowledge reliability**: what the model knows, and whether it knows what it
doesn't. Not a gate. A ruler.

600 public questions (10% of AA's 6,000), Apache-2.0, 6 domains x 100, 41 topics,
short exact answers. `AA_README_UPSTREAM.md` is the upstream card kept for
provenance; `prompts.py` is extracted from it verbatim and must never be
paraphrased — the answer prompt is what makes abstention a live option, and the
grader template is the rubric the published numbers were produced under.

## Run

```bash
python3 run.py --model Qwen3.8-27B-UD-Q6_K_XL                 # full 600, naked
python3 run.py --model <id> --limit 60                        # stratified cut, ~26 min
python3 run.py --model <id> --rejudge --out runs/naked--<id>  # re-grade, ask nothing
python3 test_score.py                                         # the tax's assertions
```

Measured 2026-08-21 on Qwen3.8-27B-UD-Q6_K_XL: **~26 s/item** end to end
(answer + judge), so a full arm is **~4.4 h** at concurrency 1. Cheaper than
RewardBench 2's 103 s/item because the official prompt asks for *just* the
answer — 9-20 completion tokens, no reasoning prefix. Long arms go out as
launchd one-shots.

## It is a (harness, model) leaderboard

Terminal-Bench's shape. Every submission there is a harness-model pair, because
a deployed agent *is* that composition and attributing the score to one factor
is a category error. The 600 items are harness-agnostic; a harness is a thin
adapter in `HARNESSES`. `naked` is a registered harness — the row that happens
to match AA's published protocol — not a privileged baseline. Adding the
calibrated and grounded arms is a function plus a registry line.

## Reading a number

- **`oi_taxed` is ours, `oi_official` is theirs, and both are always printed.**
  Official OI gives abstention weight 0, so declining everything scores 0 —
  which outranked all but three models at the Nov-2025 snapshot. We tax
  abstention at 0.1, which puts blanket abstention at -10 and moves the rational
  answer threshold 0.50 -> 0.45. `tax=0.0` recovers the official metric exactly.
  All of that is asserted in `test_score.py`, which also reproduces Opus 4.1's
  published 4.8 from its histogram.
- **Never read the index without the accuracy next to it.** The tax prices the
  degenerate strategy; it does not make a timid harness into a good one. Same
  reason `chaos_monkey` refuses a blended score.
- **Never read a score whose `coverage` is below 1.0**, and an error rate above
  2% exits 4 — could-not-judge, not a score.
- **`judge_is_official: false` in every summary.** The official grader is Gemini
  2.5 Flash Preview (09-2025) with reasoning; ours is the local daemon. That
  substitution is validated before any headline number — see `PREREG.md`.
- **A0 is not leaderboard-comparable** until the contamination and quantisation
  offsets in `PREREG.md` are priced. The A1-A0 *delta* survives both.

## Bars

In `PREREG.md`, registered 2026-08-21 before any arm was scored, including the
mechanism-level prediction that the *existing* grounding pipeline will lower the
NOT_ATTEMPTED count rather than raise it (invariant `0ee9fc42`).
