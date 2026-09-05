# Pre-registration — is `retrieval-prod` a substrate an evolutionary loop can climb?

WRITTEN BEFORE ANY DATA. Bars are falsifiable and stated as the OBSERVATION.

## The question

Settled, not under test: AVO's mechanism works in its own setting
(arXiv:2603.24517 — 40 committed kernel versions, 7 days, +3.5% over cuDNN).

Under test, and prior to building any of it here: **does the `retrieval-prod`
lane behave like a fitness function?** Three properties are required and none
has been measured:

- **Deterministic** — `sovereign/docs/RUNBOOK.md` §6 asserts retrieval recall is "exact —
  any delta is real". Asserted, never verified by a repeat run.
- **Has a gradient** — some reachable variation beats HEAD's defaults. If the
  declared knob space is already at its optimum there is nothing to climb.
- **Not trivially overfittable** — a gain on the banks used for search
  survives on banks never scored during search. This is the one property the
  paper got for free (a faster kernel is faster) and we do not.

A NO on any one kills a different amount of the programme. The bars below say
which.

## What is already known — the headroom, from the committed baselines

Aggregate of `sovereign/bench/*/baselines/*-prod-isolated/latest.json` as of
2026-09-04. `src`/`fact` are the mean of per-question `source_score.ratio` and
`fact_score.ratio`; `<1` counts questions short of full marks. `search_s` is
the sum of recorded `search_ms`.

| bank | n | src | src<1 | fact | fact<1 | search_s | captured |
|---|---|---|---|---|---|---|---|
| sep-core-v1.1 | 21 | 0.882 | 8 | 0.956 | 7 | 46.5 | 2026-08-10 |
| wikipedia-core-v2 | 20 | 0.679 | 13 | 0.819 | 10 | 67.0 | 2026-07-17 |
| sep-summarize-v1 | 8 | 1.000 | 0 | 0.625 | 7 | 13.8 | 2026-08-10 |
| sep-summarize-obscure-v1 | 6 | 1.000 | 0 | 0.783 | 6 | 10.5 | 2026-08-10 |
| wikipedia-summarize-pilot-v1 | 8 | n/a | 0 | 0.562 | 8 | 20.7 | 2026-07-17 |
| wikipedia-newsworthy-smoke | 6 | 0.806 | 3 | 1.000 | 0 | 20.6 | 2026-07-17 |
| single-atomic-bombings | 1 | 1.000 | 0 | 0.167 | 1 | 5.7 | 2026-07-17 |
| single-causal-roman | 1 | 0.667 | 1 | 1.000 | 0 | 4.6 | 2026-07-17 |

71 questions, ~189 s of recorded search across all eight banks. Headroom is
not in doubt: `wikipedia-core-v2` misses at least one expected source on 13 of
20 questions, and 7 of 8 `sep-summarize-v1` questions are short on facts. The
question is whether any of it is reachable.

## The split, fixed here and not revisited

**EVOLUTION set** (search may score these): `sep/questions` (21),
`wikipedia/questions` (20) = 41 questions.

**HELD-OUT set** (scored exactly once, at Stage 2, by the winning cell only):
`sep/summarize` (8), `sep/summarize_obscure` (6), `wikipedia/summarize` (8),
`wikipedia/newsworthy_smoke` (6), `wikipedia/single_atomic` (1),
`wikipedia/single_roman` (1) = 30 questions.

The split is question-shape, not corpus: both sets span both corpora, so a
transfer failure indicts the search rather than a corpus. It mirrors the
paper's MHA -> GQA check — same pipeline, a configuration never seen during
evolution.

## PRECONDITIONS

**P1 — BASELINES RE-CAPTURED AT HEAD.** The wikipedia banks were captured
2026-07-17 (49 days) and the sep banks 2026-08-10 (25 days), both past the
14-day `SOVEREIGN_BASELINE_MAX_AGE_DAYS` default. A stale baseline measures
weeks of drift, not a cell. Re-capture all eight at HEAD before any cell runs.
If not re-captured the run is **could-not-judge**, not a result.

**P2 — THE LANE IS ACTUALLY DETERMINISTIC.** Run the default cell twice at the
same commit. Every per-question `source_score.ratio` and `fact_score.ratio`
must match exactly across all eight banks.

If P2 fails, **every bar below is void** and the finding is that `sovereign/docs/RUNBOOK.md`
§6 is wrong — retrieval is a noisy lane, single-run fitness is invalid, and
the whole AVO programme on this substrate needs a repeat-count and a measured
noise band before it can restart. That finding is worth the run on its own.

**P3 — LLM-TOUCHED CELLS FLAP-CHECKED SEPARATELY.** Cells C5, C6 and C7 below
put a fast-slot model call inside the retrieval path (`SOVEREIGN_DEMAND_PLAN`
plans the turn; `SOVEREIGN_TITLE_EXPAND` names article titles). P2's
determinism claim covers embeddings and ANN, not these. Run each of the three
3x. Any that does not produce identical scores across all three is
reclassified NOISY, excluded from B1/B2/B3, and recorded as a finding: an
LLM-in-the-loop knob cannot be judged by single-run fitness.

## The cells — 13 arms, fixed here

Drawn from the experimental-OFF and tunable buckets of
`sovereign/docs/retrieval-pipeline.md` (generated, freshness-gated). Each is
one `sovereign bench all --prod-pipeline --isolate` run over the EVOLUTION set
with the named environment set and everything else at HEAD default.

| id | cell | why this one |
|---|---|---|
| C0 | (no overrides) | reference |
| C1 | `SOVEREIGN_QUERY_DECOMP=1` | experimental-OFF, never A/B'd on prod pipeline |
| C2 | `SOVEREIGN_QUERY_DECOMP=1 SOVEREIGN_DECOMP_DECAY=0.8` | registry: decay<1 augments instead of displacing |
| C3 | `SOVEREIGN_GRAPH_NEIGHBOR_EXPAND=1` | experimental-OFF |
| C4 | `SOVEREIGN_ATOM_ENUM=1` | experimental-OFF; 2026-06-04 bench called it net-negative — expected to regress, so it doubles as a sanity check on the harness |
| C5 | `SOVEREIGN_TITLE_EXPAND=1` | experimental-OFF (LLM; see P3) |
| C6 | `SOVEREIGN_DEMAND_PLAN=1` | default-OFF (LLM; see P3) |
| C7 | `SOVEREIGN_DEMAND_PLAN=1 SOVEREIGN_DEMAND_PLAN_FANOUT=1 SOVEREIGN_DECOMP_DECAY=0.8` | the exact pairing the registry says was never tried (LLM; see P3) |
| C8 | `SOVEREIGN_EXPANSION_SCOPE_CORPORA=16` | scale-vs-recall dial; registry records a too-narrow scope costing wikipedia 3 sources / 4 facts |
| C9 | `SOVEREIGN_EXPANSION_SCOPE_CORPORA=4` | the other direction |
| C10 | `SOVEREIGN_META_BRIDGE=1` | experimental-OFF, cross-corpus |
| C11 | `SOVEREIGN_RAPTOR_LATE=0` | position flip on a validated-ON feature |
| C12 | `SOVEREIGN_MERGE_SELECT=0` | **negative control.** Restores the pre-`MERGE_SELECT` heuristic pile. If this does NOT regress, the validated-ON verdict is stale and the harness is not measuring what the registry claims. |

Budget: 13 cells x ~114 s of EVOLUTION-set search ~= 25 min of search plus
process overhead, single-threaded. C5/C6/C7 cost 3x. If the measured total
exceeds 2 h, stop and report the cost rather than trimming cells.

## BARS

Scored on the EVOLUTION set only, per-question, against the P1-refreshed C0.
"Improves" and "regresses" count QUESTIONS whose `ratio` moved, not mean
deltas — 41 questions make one flip worth 0.024 and a mean would hide which
direction the pipeline actually moved.

**B1 — GRADIENT EXISTS.** At least one non-LLM cell improves >= 3 questions on
`source_score` or `fact_score` with 0 regressions on either.
MEANS: the declared knob space is not at its optimum. Proceed to Stage 2.

**B2 — NO GRADIENT.** No cell clears B1.
MEANS: knob-space search is exhausted at HEAD defaults. This does NOT kill the
programme — it argues for rung-1 AVO (structural edits to `kq_pipeline`), since
the cheap surface is demonstrably spent. It does kill the cheap version.

**B3 — GRADIENT, PARETO-CONFLICTED.** Some cell improves one score and
regresses the other, and no cell clears B1 cleanly.
MEANS: `f` is genuinely multi-objective. The monotone commit gate must be
defined over the vector with an explicit written rule before any loop is
built; a scalarised gate would hide the trade.

**B4 — NEGATIVE CONTROL HELD.** C12 regresses.
MEANS: the harness measures what the registry claims. If C12 does NOT
regress, B1-B3 are downgraded to could-not-judge pending an explanation, and
the finding is about `SOVEREIGN_MERGE_SELECT`'s stale verdict.

Exactly one of B1/B2/B3 is the Stage-1 result. B4 is independent and is
checked first.

## Stage 2 — the transfer bar (only if B1)

Take the single best cell from Stage 1. Score it on the HELD-OUT set, once.

**B5 — TRANSFERS.** Held-out improves, or holds with 0 regressions.
MEANS: **GREEN.** The bank is not trivially overfittable at this search depth.
Build the score-on-commit note, the monotone gate, and the supervisor, in that
order.

**B6 — DOES NOT TRANSFER.** Held-out regresses while the evolution set
improved.
MEANS: **RED, and this is the kill bar.** A fixed question bank is an
overfittable fitness function, and an AVO loop on this substrate would produce
40 committed versions of bank-fitting that read as progress. Do not build the
loop against a static bank. The programme restarts only with a redesigned `f`
— rotating banks per generation, or synthesized queries — and that redesign
gets its own pre-registration.

One cell overfitting proves overfittability at depth 1. Forty agent-driven
generations would search far harder, so B6 at depth 1 is strictly worse news
than it looks.

## Procedure

1. Record HEAD sha. Confirm daemon up, both corpora indexed
   (`sovereign doctor`).
2. **P1:** `sovereign bench all --bench-root sovereign/bench --filter sep
   --prod-pipeline --isolate --update-baseline`, same for `wikipedia`. Commit
   the dated snapshots.
3. **P2:** run C0 twice; diff per-question ratios across all eight banks. Void
   everything below on any mismatch.
4. **T0 (fact, not a bar):** record wall-clock of one full 8-bank evaluation
   and one EVOLUTION-set evaluation. Every downstream decision about loop
   shape — generations/day, subset-vs-full fitness, launchd vs interactive —
   falls out of this number.
5. **B4:** run C12. Check it regresses before spending on C1-C11.
6. Run C1-C11 over the EVOLUTION set. C5/C6/C7 3x each (P3).
7. Score B1/B2/B3.
8. If B1: run the winner once over the HELD-OUT set. Score B5/B6.
9. Append `RESULTS-2026-09-04-<host>.md` with the raw report JSONs beside it.

## What this experiment does NOT test

The agent. No AVO agent runs here. Stage 1 is a fixed sweep on purpose: it
establishes whether the substrate has the three properties an agentic loop
would need, at a cost of hours rather than days, before any agent time is
spent. A green result licenses the build order in `README.md`; it does not
itself demonstrate that an agent finds more than a sweep does. That is the
next pre-registration.
