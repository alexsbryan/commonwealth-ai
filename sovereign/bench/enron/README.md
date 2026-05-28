# Enron entity-resolution bench

The first calibration target for the architecture-over-Enron push.
Measures **multi-origin entity reconciliation** (Phase 4 substrate
primitive) against a ground-truth set of canonical entities from the
public Enron organisational record.

## Scope

- ~50 hand-curated canonical entities seeded from the public Enron
  org chart (executives, key counterparties, common-knowledge places).
- Each ground-truth entry pins one canonical id + a list of accepted
  surface forms (`"Ken Lay"`, `"Kenneth L. Lay"`, `"klay@enron.com"`,
  `"K. Lay"`).
- Bench answer model: a *predicted* clustering of mention-ids →
  cluster-ids; the runner computes B³ + pairwise-F1 against ground
  truth via `sovereign_eval::entity_resolution_score`.
- Calibrated judge: `corpus-engine/assets/judges/business_entity_v1/`
  with the pinned `JUDGE_TEMPERATURE=0.0` / `JUDGE_SEED=0xA705`
  consistent with `sovereign-eval::judge`. Used by Phase 4
  reconciliation when two surface forms hit the
  `judge_when_uncertain` threshold.

## Splits

Each question entry declares a `split`. The runner enforces:

| Split | Use |
|---|---|
| `train` | Tune the reconciliation policy (`name_similarity_threshold`, `judge_when_uncertain`, signal weights). Free to run as often as you like. |
| `test`  | Score once per tuned policy; result commits to `baselines/enron-entity-resolution/`. Re-running `test` after seeing the number is **leakage** — do it deliberately and own the next-iteration's seed change. |
| `holdout` | **Sealed.** Refuses to run without `--unseal-holdout`. That flag burns a peek-budget counter in `baselines/enron-entity-resolution/peek_budget.json`; the public-release plan (out of scope for this push) reads the counter and decides whether the holdout is still credible as a generalisation estimate. |

## Pre-reconciliation floor

The intentionally-bad baseline every Phase 4 tuning move must beat:
every surface form is its own cluster. Captured in
`baselines/enron-entity-resolution/pre_reconciliation.json` as the
B³ floor. The floor's precision is trivially 1.0 (singletons are
purely pure); its recall is `1 / mean_cluster_size`. Tuning earns
its keep on **F1 delta vs the floor**, not on precision alone.

## Layout

```
sovereign/bench/enron/
  README.md                       # this file
  questions.toml                  # ~50 ground-truth entities × splits
  ground_truth_entities.jsonl     # canonical_id → surface forms +
                                  # provenance + sealed-holdout flag
```

## Authoring conventions

1. **Surface forms come from the corpus, not from imagination.**
   Run an initial atlas extraction with reconciliation OFF (every
   surface form is its own atom) and let the dispatcher tell you
   what shapes appear; pick the canonical id from that set.
2. **Holdout entries are sealed at author-time.** Set
   `split = "holdout"`, fill in the canonical id + 1-2 surface forms,
   then drop further detail. The runner reveals nothing about
   holdout entries until `--unseal-holdout` is invoked.
3. **One canonical id per real-world entity.** Don't split "Ken Lay
   (CEO 1985-2002)" and "Ken Lay (defendant 2006)" — same person,
   same id; the temporal slice is encoded on the *mention*, not the
   canonical entity.

## Running

(CLI surface lands as part of Phase 4's reconciliation work — bench
runner extension. This README is the canary scaffold so the runner
has a target to bind to.)

```sh
sovereign bench enron run --split train --judge-trials 3
sovereign bench enron run --split test
sovereign bench enron run --split holdout --unseal-holdout
```
