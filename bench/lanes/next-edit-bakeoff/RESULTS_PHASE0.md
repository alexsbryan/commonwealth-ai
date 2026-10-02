# Phase 0 results — next-edit build vs adopt

Run 2026-08-05 on the M-series box, llama.cpp Metal, `--ctx-size 8192`,
concurrency 1. Spec: [`NEXT_EDIT_BAKEOFF.md`](../../docs/specs/NEXT_EDIT_BAKEOFF.md)
§7. Driver: `scripts/next_edit_bakeoff.py`. Raw runs: `runs/phase0-*`.

**Verdict: Sweep-1.5B is the champion, and it is the only arm that
clears the §6 bar.** It matches the best quality in the field at **1/5
the residency and 1/5 the latency** of the next-best arm. Scaling up
does not buy quality here: Sweep's own 7B is *worse* than their 1.5B on
a neutral ruler, and Zeta-2 at 8B only ties the 1.5B while costing 4.8×
the latency.

**But the ruler is saturated, and that is the second finding.** Three
arms land within 2 cases of each other at n=30 with **zero wrong edits
each**. The existing bank can no longer separate these models on
quality — it separates them on cost. Any decision that turns on "which
model is better" now needs the golden set (§2); any decision that turns
on "what does it cost to run" is already answered.

## The table

| Arm | Params | Quant | Format | Useful | Wrong | Fires | p95 | Resident | Verdict |
|---|---|---|---|---|---|---|---|---|---|
| **sweep-1.5b** | 1.5B | Q8_0 | `sweep` | **27/30** | **0/28** | 28 | **749 ms** | **1.54 GB** | **pass — champion** |
| zeta-2 | 8B | Q6_K | `zeta2` | 27/30 | 0/27 | 27 | 3578 ms | 7.02 GB | pass |
| sweep-v2-7b | 7B | Q6_K | `sweep` | 25/30 | 0/25 | 25 | 2688 ms | 6.25 GB | pass |
| instinct-7b | 7B | Q4_K_M | `region_instruct` | 0/30 | 0/8 | 8 | 2345 ms | 4.70 GB | could-not-judge |
| *Mellum2 (incumbent)* | *12B* | *Q6_K* | *`region_instruct`* | *29/30* | *0* | — | *1807 ms* | *10.88 GB* | *not re-run — see caveats* |

All arms: GM1 structural 0 malformed · GM2 gate determinism 60/60 · GM3
wrong-edit 0. Every model-bearing arm is Apache-2.0.

## Against the pre-registered champion bar (§6)

Field-best useful-fire is 27/30 (90%), so "within 3 points" is ≥26.1/30.

| Criterion | sweep-1.5b | zeta-2 | sweep-v2-7b |
|---|---|---|---|
| useful within 3 pts of best | ✓ 27/30 | ✓ 27/30 | ✗ 25/30 (83%) |
| wrong-fire ≤1% | ✓ 0% | ✓ 0% | ✓ 0% |
| p95 ≤1500 ms | ✓ 749 | ✗ 3578 | ✗ 2688 |
| resident ≤4 GB | ✓ 1.54 | ✗ 7.02 | ✗ 6.25 |
| Apache-2.0 | ✓ | ✓ | ✓ |
| no FIM regression | **not measured** | — | — |

**Sweep-1.5B clears every bar that was measured.** One bar was not
measured: `gym/fim/` never ran, and the FIM seat shares this slot, so
"can Sweep-1.5B also serve inline completion without regressing it" is
an open question and the single remaining blocker on adoption. That is
a could-not-judge on one criterion, not a pass.

Displacing Mellum2-12B at its validated q6_k rung returns **~9.3 GB of
residency on every fleet node** — on the slot that competes with the 35B
primary — and cuts proposal p95 from 1807 ms to 749 ms.

## What the run found beyond the numbers

**1. Our `zeta2` dialect was wrong, and the bakeoff is how we learned.**
Zeta-2's first arm scored **0/30** with 19 `invalid` + 11 `truncated` —
a 100% parse failure. `build_prompt_zeta2` emitted `<|marker_1|>` /
`<|marker_2|>` sentinels; the canonical `sample.prompt` published in
`zed-industries/zeta-2` uses **git-merge markers** (`<<<<<<< CURRENT` /
`=======`, model resumes after `<[fim-middle]>` and terminates with
`>>>>>>> UPDATED`). The format had been written from a prose model-card
description and **never run against the weights**. Corrected, the same
arm scores **27/30**. `NEXT_EDIT.md` claimed this format was built; it
was built against a dialect that does not exist.

**2. Sweep-1.5B improved materially since our July measurement.**
`NEXT_EDIT.md` §9b records 22/30 useful, 0/24 fires, p95 1112 ms. The
current published GGUF — named `q8_0.**v2**` — scores **27/30, 0/28
fires, p95 749 ms**, reproduced bit-stable across two runs. The consult
gate made identical decisions both times (GM2 60/60), so this is new
weights, not harness drift. The comparison our own spec rests on was
stale.

**3. Bigger is not better on this task.** Sweep publish their 7B at
81.28% against the 1.5B's 67.82% on their own five-benchmark suite. On
a neutral ruler the ordering **inverts**: 25/30 vs 27/30, at 4× the
params and 3.6× the latency. This is precisely the self-graded-benchmark
problem §0 was written about, and it is now observed rather than
predicted.

**4. Four verdicts earned their keep on the first run.** Recording
Zeta-2's 0/30 as `fail` would have published a confident, permanent, and
*false* verdict about a live competitor's model based on our own bug.
The driver now discriminates protocol-boundary drops (`invalid`,
`truncated`) from content drops (`noop`, `inconsistent`) and returns
`could-not-judge` when a dialect never reached the model
(`format_fidelity` in `scripts/next_edit_bakeoff.py`), verified against
the archived broken run.

## Caveats — read before quoting any number above

- **n=30 positives. A 2-case spread is noise.** Do not read 27 vs 25 as
  a quality ranking. The defensible quality claim is that three arms are
  indistinguishable on this bank; the defensible cost claim is that they
  are 5× apart.
- **Mellum2 was not re-run.** Its 29/30 / 1807 ms comes from `NEXT_EDIT.md`
  §9b, measured through the daemon in July on a different harness
  generation. It is in the table for orientation, not comparison. Since
  Sweep moved under us in three weeks, assume Mellum2's row is stale too.
- **Instinct is unmeasured, not beaten.** It was run on
  `region_instruct` — a dialect it was never trained on — and responded
  with 18 `noop` (echoing the region unchanged, which our prompt
  explicitly instructs when the pattern does not apply). It ships its own
  chat template and its own published dataset. A real verdict needs an
  Instinct adapter; the 0/30 above is a statement about our integration.
- **Quantization is not equalised** and cannot be from what the field
  publishes: Q8_0 for the 1.5B, Q6_K for the 7–8B class, Q4_K_M for
  Instinct. Read the quant column before comparing rows.
- **Community quants for two arms.** `sweep-v2-7b` (fl0rm) and `zeta-2`
  (bartowski) are third-party GGUF conversions; the vendors publish
  safetensors only. A conversion defect would land as a quality loss and
  is not separable here.
- **Silence was scored only by the bank's 20 negatives**, all of which
  every arm handled (GM3 0 wrong across the board). That is a floor, not
  a measurement of restraint at scale.

## Statistical power — what this bank can and cannot decide

Computed 2026-08-05 from the run above. **The bank supports the cost
decision and no other.**

**The safety criterion is unmeasured by an order of magnitude.** The §6
champion bar sets wrong-fire ≤1%. Sweep-1.5B fired 28 times with zero
wrong edits — by the rule of three, that establishes only that
wrong-fire is **below 10.7%**, not below 1%. Certifying the bar as
written needs ~300 fires with zero wrong; we have 28. This is the
precision-critical axis (`NEXT_EDIT.md` §1) and it is the one the bank
is furthest from resolving.

| 0 wrong in N fires | 95% upper bound on wrong-fire |
|---|---|
| 28 (what we have) | 10.71% |
| 100 | 3.00% |
| 300 | 1.00% ← the bar |
| 1000 | 0.30% |

**Quality differences here are noise.** 27/30 carries a 95% CI of
**[74.4%, 96.5%]** — 22 points wide. 27/30 vs 25/30 is z=0.76, p≈0.45:
statistically indistinguishable. Sample size to resolve a real gap:

| Gap to detect | Positives needed **per arm** |
|---|---|
| 10 points | 199 |
| 5 points | 685 |
| 3 points | 1,772 |

We have 30.

**And the deepest limit is not sample size.** All 30 positives are drawn
from three categories — `signature_fanout`, `param_insert`, `field_init`
— which are exactly the three shapes `should_consult` admits
(`fanout_insert`, `param_insert`, `multiline_fanout`). **The bank is a
mirror of the gate, not a sample of the world.** It therefore cannot
discover that the gate's taxonomy is incomplete: an episode the gate
declines by construction never becomes a measurable missed-fire. No
value of n fixes this; only new *shapes* do.

Coverage gaps that follow from that, none currently measured at all:
rename-across-casing (declined by design), import addition following a
new symbol, delete-propagation, type change fanning to annotations,
error-handling conversion, API migration with argument reorder,
test-follows-impl, enum variant → match arm, doc/comment sync, guard
insertion, interleaved concurrent patterns, revert-shaped edits that
must stay silent, edits far from the cursor, and **all cross-file work**
— the axis Zeta-2 competes on.

Two further skews: the rule bank is 48% Rust and ~31% not code at all
(json/markdown/yaml/toml), and the gen bank averages ~5 cases per
language. Neither bank contains a single real editing session.

**No floor and no ceiling were measured** (§5 items 3–4), so we do not
know whether 27/30 is good. If an untrained `Qwen2.5-Coder-1.5B-base`
also scores near 27/30, the bank is not measuring next-edit skill and
every number above is uninterpretable. That check costs ~20 minutes and
should run before any of this is used to decide anything.

## What this authorizes

1. **Adopt Sweep-1.5B for the model lane**, pending the FIM
   non-regression run — the one unmeasured bar. Retires the Mellum2-12B
   dependency and ~9.3 GB of residency.
2. **Do not train yet.** The §6 TRAIN trigger is not met: no candidate
   failed the wrong-fire bar, and there is no measured frontier ceiling
   to establish a 10-point gap against. The ceiling and floor checks
   (§5 items 3–4) were not run and are the next cheap thing.
3. **The golden set is now justified by evidence, not anticipation.**
   Phase 0's second predicted outcome landed: the field does not separate
   on the existing ruler. A 60-case bank at 90% cannot adjudicate a
   training run, which is exactly what §2 exists to fix.
