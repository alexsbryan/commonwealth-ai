# Calibrated decisions

One body of work: replacing the pipeline's label-only decisions with calibrated
distributions, and building one small decision model, the same weights on
every client, to produce them. Started 2026-09-22. No results yet.

## The documents

Read them in this order:

| Document | Kind | What it holds |
|---|---|---|
| [`CALIBRATED_DECISIONS_PREREG_20260922.md`](CALIBRATED_DECISIONS_PREREG_20260922.md) | pre-registration, append-only | Track A (self-assessment) and Track B (the crisis classifier) with fixed bars; the N-way forced-choice funnel; instrument validation. Addenda: "Also considered", and R1–R6, the retrieval and gate items read against Jev's architecture |
| [`CALIBRATED_DECISIONS_MODEL_20260923.md`](CALIBRATED_DECISIONS_MODEL_20260923.md) | plan | The goal (one model, every client), the thesis (the pipeline as a decision process), which decisions go where, F1 (readouts with no training), the trained decision model, F2 (does distilling the base help?), tradeoffs T1–T9, the order |
| [`CALIBRATED_DECISIONS_DATA_20260923.md`](CALIBRATED_DECISIONS_DATA_20260923.md) | plan | The data strategy: rules, teacher roles, licence tiers, six sources (public labelled sets, widened Stream B over recipe corpora, manufactured traffic, self-knowledge labels, the journal, the distillation corpus), evaluation banks, milestones D0–D6 |

## Conventions

- **The pre-registration is append-only.** Its bars were fixed when it was
  committed. Changes go under a dated heading at the end.
- **The plans are revised in place until their first result.** Each carries a
  revision note at the top. What was set aside is kept in the model doc's
  "Tradeoffs considered" (and the data doc's "Deferred" notes), with the
  reason and what would reopen it, so a later reader does not have to
  rediscover it.
- **Anything that runs gets its own appended pre-registration** before its
  first run, with bars fixed before data exists, under
  `bench/VERIFIER_ECONOMY_PREREG_20260819.md` §"Shared discipline".
- **Results are committed beside these files.** A killed line gets a
  `DEFAULTS_LEDGER.md` REJECTED row naming the measurement and the bar it
  failed.

## Related work outside this folder

- `docs/specs/VERIFIER_V0.md`: the verifier training plan this work amends.
- `research/verifier-v0/findings/`: the measurements this work leans on,
  especially `VERIFICATION_SCALING_AXES.md` §13–21, `BASELINES.md` and
  `M2_STREAM_B_VOLUME.md`.
- `bench/VERIFIER_ECONOMY_PREREG_20260819.md`: the shared discipline, and the
  decorrelation argument behind keeping certification in separate weights.

## Open for the operator

Each document ends with its own list. The decisions that block work soonest:

1. The latency bar per hardware profile, before F1's data exists (model doc).
2. Whether non-commercial evaluation sets, FaithBench included, are admissible
   for internal measurement (data doc).
3. Which genre is held out entirely as the off-distribution set (data doc).
4. Whether Qwen3.8-Max-50k's traces may be used for training. Its prompts and
   the comparison uses do not wait on this (data doc, S6).
