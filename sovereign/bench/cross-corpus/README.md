# Cross-corpus bench — SEP × Wikipedia bridge (whole-game gate)

The whole-game test for the meta-atlas bridge: does routing typed SEP↔Wikipedia
edges (`SOVEREIGN_META_BRIDGE=1`, the `bridge_boost` retrieval step) lift source +
fact coverage on questions that need **both** corpora — over a bridge-off
baseline? The gate is **QA lift**, never alignment-F1 in isolation (no metric
overfitting; see `feedback_whole_game_quality`).

`questions.toml` — 8 questions, each answerable well only with SEP's argument
structure *and* Wikipedia's encyclopedic framing. `expected_sources` span both
corpora (SEP slugs lowercase, Wikipedia titles title-case).

## Prerequisites

1. Built bridge edges: `sovereign meta-atlas align --k=8` (persists
   `~/.svrnmesh/meta-atlas/bridge_edges.json`). Inspect with
   `sovereign meta-atlas explain "<concept>"`.
2. Both `sep` and `wikipedia` corpora installed.
3. Daemon up (`sovereign daemon start`, inside the inference toolbox).

## The A/B

Bridge-OFF baseline vs bridge-ON, holding everything else fixed:

```
# OFF (baseline) — bridge step is a no-op
SOVEREIGN_META_BRIDGE=0  sovereign eval run --bank sovereign/bench/cross-corpus/questions.toml --synth ...
# ON — bridge_boost pulls the linked corpus's framing through typed edges
SOVEREIGN_META_BRIDGE=1  sovereign eval run --bank sovereign/bench/cross-corpus/questions.toml --synth ...
```

Compare source-coverage and fact-coverage. A positive delta is the stereo-view
win. Freeze the ON result as the committed baseline under `baselines/` and gate
regressions with `sovereign bench gate` (first run passes).

## Corpus scope — do NOT pass `--isolate`

The eval runner only seals retrieval to `bank.corpus` under `--isolate`
(`eval_cmd/runner.rs:1719`). The default (no `--isolate`) leaves
`enabled_corpora = None`, so retrieval reaches **all** installed corpora —
exactly what a cross-corpus bank needs. `bridge_boost` then fetches the linked
corpus through the typed edge. So run the A/B WITHOUT `--isolate`; passing it
would seal to SEP and make the bridge a no-op.

`bridge_boost` injects are traceable: chunks carry `metadata["bridge_relation"]`
+ `["bridge_confidence"]`, and the pipeline emits
`retrieval.pipeline step=bridge_boost note="bridge: +N cross-corpus chunks"`.

## Alignment-precision spot-check (NOT the gate)

A sacred held-out set of hand-verified SEP↔Wikipedia correspondences gives a
precision read on the edges themselves (Train/Dev/Test discipline, reuse
`mechanism_fidelity::PeekBudget`). It informs τ tuning; it does not gate.
