# D1 — HARD-lane baseline re-mint adjudication (2026-08-10)

Order: native-grounding-step3-tuning, deliverable D1. Funds backlog item de5aad99.
Path: RUNBOOK §6 "legitimate re-mint" — this file is the required per-probe
adjudication, recorded BEFORE the `--update-baseline` runs it authorizes.

## What is being re-minted, and why re-mint is legitimate

Three HARD lanes fail on clean main (2c1ab2d1), confirmed by two independent
runs (step2-order run 2026-08-09 ~22:00 and seat control run 2026-08-09/10,
identical lane-for-lane signature; see backlog item de5aad99):

| Lane | Regressed benches | Baseline minted | Age |
|---|---|---|---|
| retrieval:wikipedia | newsworthy_smoke, questions | 2026-07-16 | 25d |
| retrieval-prod:sep | summarize, summarize_obscure | 2026-07-17 | 24d |
| routing | cells_v1_paraphrases, skills_migration_smoke | 2026-07-16 | 25d |

The staleness threshold is 14d; every failing lane's output carries the stale
warning. 394 commits landed on main between the baseline mint dates and today.
The deltas below are attributable to landed, individually-adjudicated main-line
changes — not to the Step 2 grounding branch (merge-base diff: zero
routing/retrieval source; the failures reproduce identically on clean main).
The named mechanism commits:

- 4d589963 `feat(router): give the classifier stack its own embedding
  instruction` — changes every embed-router cosine, so probes that previously
  resolved at the embed/lookup layer near the margin now fall through to the
  coarse-LLM layer.
- dc970a1b, da9b5aa5, f7719278 — router taxonomy/margin/bank changes.
- d04a1100 `fix(retrieval): move atom_enum after the reweight`, f40f8e72
  `gate the atom-enum overview injection on corpus aboutness` — recompose the
  evidence pool the prod-pipeline lane diffs.

No probe below is a new, unexplained failure of current code; every delta is a
25-day accumulation against a ruler minted before those changes. Per RUNBOOK
§6, the fix is a dated re-mint, not code reverts.

## Per-probe adjudication

Source: seat control run reports, `target/ci-bench/{retrieval-wikipedia,
retrieval-prod-sep,routing}.json` (main worktree, 2026-08-09 23:00-23:12),
`baseline.results` vs `current.results` per question.

### retrieval:wikipedia / newsworthy_smoke

| probe | src ratio | fact ratio | delta detail | decision |
|---|---|---|---|---|
| newsworthy-iran-war-ceasefire | 1.00 -> 0.67 | 1.00 -> 1.00 | lost source title '2026 Iran war'; all 4 expected facts still retrieved | ACCEPT — pool recomposition, answer-bearing content intact |
| newsworthy-lebanon-war-context | 1.00 -> 0.67 | 1.00 -> 1.00 | lost source title 'Middle Eastern crisis (2023–present)'; facts intact | ACCEPT — same shape |

### retrieval:wikipedia / questions

| probe | src ratio | fact ratio | delta detail | decision |
|---|---|---|---|---|
| contested_atomic_bombings_morality | 1.00 -> 1.00 | 0.17 -> 0.33 | gained fact 'demonstration' | ACCEPT (improvement) |
| contested_colonialism_legacy | 0.67 -> 0.00 | 0.57 -> 0.86 | lost source titles 'Colonialism', 'Scramble for Africa'; gained facts 'exploitation', 'extraction' | ACCEPT — fact coverage (the answer-bearing metric) up +0.29; content now arrives via different articles |
| contested_globalization_effects | 0.67 -> 0.67 | 0.88 -> 0.62 | lost facts 'labor', 'outsourcing' | ACCEPT with note — real coverage step-down on this probe; new baseline becomes the ruler that would catch any further loss |

### retrieval-prod:sep / summarize

| probe | fact ratio | delta detail | decision |
|---|---|---|---|
| summary_idealism | 0.60 -> 0.50 | lost 'Berkeley', 'Hegel absolute idealism'; gained 'rejection of mind-independent matter' | ACCEPT with note |
| summary_cosmological_argument | 0.90 -> 1.00 | gained 'Hume objection' | ACCEPT (improvement) |
| summary_problem_of_evil | 0.80 -> 0.70 | lost 'natural versus moral evil' | ACCEPT with note |
| summary_descartes_epistemology | 0.40 -> 0.70 | gained 'cogito ergo sum', 'foundationalism', 'rationalism' | ACCEPT (improvement) |
| summary_mill_moral_political | 0.80 -> 0.50 | lost 'harm principle', 'higher and lower pleasures', 'qualitative hedonism' | ACCEPT with note |
| summary_conservatism | 0.80 -> 0.40 | lost 'Oakeshott', 'anti-rationalism', 'gradual incremental change', 'skepticism of abstract reason' | ACCEPT with note |

All source ratios 1.00 -> 1.00 on this bench (the right articles are found;
composition of the evidence pool shifted). Net fact delta across the six moved
probes: -0.50 ratio points — a real, known step-down in conceptual-summary fact
coverage vs the 2026-07-17 pipeline state, mechanism-consistent with the
atom-enum gating/reorder (d04a1100, f40f8e72; hypothesis, not proven here).
Recorded as a KNOWN level; the re-minted baseline is the ruler for any further
loss. If Step 3's D3 decomposition attributes grounding failures to retrieval,
this step-down is a named candidate.

### retrieval-prod:sep / summarize_obscure

| probe | fact ratio | delta detail | decision |
|---|---|---|---|
| summary_proof_theory | 1.00 -> 0.90 | lost 'cut elimination' | ACCEPT with note |
| summary_recursive_functions | 0.80 -> 0.70 | lost 'Ackermann function', 'minimization operator'; gained 'the halting problem' | ACCEPT with note |
| summary_common_knowledge | 0.80 -> 0.60 | lost 'agreement theorem', 'coordination problems' | ACCEPT with note |

### routing / cells_v1_paraphrases (25/27, embed-layer 63%)

| probe | baseline | current | decision |
|---|---|---|---|
| commissive_p_flag_for_friday | commissive_query via EMBED_ROUTER (cosine 0.598, margin 0.178 — marginal) | knowledge_query via coarse-LLM, conf 1.00, 575ms | ACCEPT into baseline as failing; the embed cosine shift (4d589963) dropped a marginal probe through to the LLM layer, which misroutes it. Correct fix per RUNBOOK §6 is an embed exemplar, filed as backlog follow-up, not a re-run and not a revert |
| metalingual_p_seps_framing | deep_query via LOOKUP, conf 1.0 | knowledge_query via coarse-LLM, conf 1.00, 827ms | ACCEPT into baseline as failing; same fall-through mechanism; embed-exemplar follow-up filed |

### routing / skills_migration_smoke (9/10, embed-layer 80%)

| probe | baseline | current | decision |
|---|---|---|---|
| research_survey | deep_query (passing, 2026-07-16) | knowledge_query via coarse-LLM, conf 1.00, 970ms | ACCEPT into baseline as failing; embed-exemplar follow-up filed |

The three routing misroutes were stable across both 2026-08-09/10 runs (not
single-run flap). All three are coarse-LLM verdicts (575-970ms, no embed
exemplar in the rationale) — the exact class RUNBOOK §6 says wants an embed
exemplar rather than a re-run. The exemplar fix is out of this order's scope
block; it is filed in the seat's backlog (key routing-exemplar-fallthrough) so
the misses are owned, not silently baked in.

## What the re-mint does NOT touch

- retrieval:sep (raw) and retrieval-prod:wikipedia — PASSING with stale
  baselines (24-25d). Left un-minted: the order funds re-mint of the three
  failing lanes; the stale-age warning on the passing lanes is warn-only.
- synth:sep / synth:wikipedia SOFT baselines — advisory under --quick; not
  re-minted here.
- chaos-gate, agent-coding, enrichment:literary — passing, untouched.

## Re-mint commands (identical invocations to the lanes, + --update-baseline)

```
target/debug/sovereign-cli-llm bench all --bench-root sovereign/bench --filter wikipedia --update-baseline
target/debug/sovereign-cli-llm bench all --bench-root sovereign/bench --filter sep --prod-pipeline --isolate --update-baseline
target/debug/sovereign-cli-llm bench all --bench-root sovereign/bench --routing-only --filter routing --update-baseline
```

Then one confirming `./scripts/sovereign-ci-bench.sh --quick`; D1 lands only if
its HARD verdicts are green (or knowingly-red with this file updated to say
which probe and why).
