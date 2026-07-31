# sovereign/handoff/

External-audience material: product walkthroughs, prospect demos,
deployment briefs you hand to someone outside the contributor circle.

Different from `sovereign/docs/`. Docs are for people building the
system. Handoff is for people *seeing* it.

## What lives here

- [`CLINICAL_TELEMED_DEMO.md`](CLINICAL_TELEMED_DEMO.md) — NIDA R34
  grant planning walkthrough; clinical telemedicine context.
- [`CODE_INTEL_DEMO.md`](CODE_INTEL_DEMO.md) — Panicked Engineer
  Demo: 64GB Mac, 400K-line monorepo, three P0s.
- [`ENRICHMENT_CANARY_DEMO.md`](ENRICHMENT_CANARY_DEMO.md) — quality
  regressions can no longer ship silently: break the resolver, watch
  the build red in three minutes (2026-07-31).
- [`FAITHFULNESS_LANE_DEMO.md`](FAITHFULNESS_LANE_DEMO.md) — the
  knowledge tier audits its own summaries: per-corpus
  unsupported-claim rate, gated in CI (2026-07-31).
- [`VERSION_STAMPED_TREES_DEMO.md`](VERSION_STAMPED_TREES_DEMO.md) —
  the knowledge tier knows who wrote it: per-node prompt/model
  provenance, `--refresh-stale` rebuilds only what's outdated
  (2026-07-31).
- [`EXTRACTIVE_FLOOR_DEMO.md`](EXTRACTIVE_FLOOR_DEMO.md) — summaries
  that cannot make things up: verbatim-sentence trees on demand, and
  failed summary calls fall back to extractive instead of thinning
  the tree (2026-07-31).

## When to add a file here

- A deployment brief for one identified peer / cohort
  (e.g., "what to send a lab onboarding next week").
- An end-to-end demo script tied to a specific use case the user
  shows on a call.
- A pitch artifact a prospect reads outside the repo.

## When NOT

- Operator runbooks → `sovereign/docs/` (e.g. `TOOLBOX_SETUP.md`).
- Feature deep-dives → `sovereign/docs/`.
- Architecture or principle docs → top-level `sovereign/`.

## Cadence

Handoff docs are one-shots. Each carries a date stamp; they age
out, not get maintained. If a demo would be evergreen, write the
underlying feature doc in `sovereign/docs/` and have the demo
reference it.
