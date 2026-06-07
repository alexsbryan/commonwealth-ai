# Mechanism-Fidelity Validation Harness — decisive slice

Decides, per policy mechanism, whether a frozen LLM agent reasons from the
**causal mechanism** or from **memorized association with the label**. The
framing is metamorphic testing: we can't score the agent against an oracle,
so we check relations that must hold under input transformations —
**invariance** to identity-preserving changes and **directional
responsiveness** to mechanism-feature changes — on synthetic cases the
model cannot have memorized.

Reference mechanism: **relocation under a wealth tax**.

## Where the code lives

| Piece | Location |
|---|---|
| Case schema, structural prior, perturbation engine, scorer, pools | `sovereign-eval/src/mechanism_fidelity/` (pure Rust, fast tests) |
| Elicitation adapter + orchestration | `sovereign-cli-llm/src/bench_cmd/mechanism_fidelity.rs` |
| Pre-registration (frozen thresholds) | `manifest.toml` (this dir) |
| Tiered, power-annotated verdict | `verdict.py` (this dir; the only throwaway component) |
| Run outputs | `results/*.jsonl` |
| Sacred-test peek ledger | `baselines/mechanism_fidelity/peek_budget.json` |

## The go/no-go question

> Does a mechanism-faithful agent show the **P1 collapse** (~0.95 → ~0.01
> when exit becomes expensive), stay **flat on P2** (saturation: home and
> destination rates both rise, differential unchanged) and **I1**
> (identity swap), while the **feature-stripped negative control sits at
> chance**?

If the control passes the sensitivity tests (a leak) or even a faithful
agent is insensitive, the program needs rework **before** any corpus or
simulation investment.

## Running it

The orchestrator drives the running daemon's OpenAI-compatible
`/v1/chat/completions`; the models under test are whatever stems you pass
to `--models` (see `/v1/models`). Probability is estimated by **repeated
sampling** — K structured draws of a forced ternary choice
(`relocate`/`stay`/`indifferent`) at temperature — because no logprobs are
exposed on either the local or frontier path.

```bash
# Smoke — a handful of cases, low K, two open-weight models:
sovereign bench mechanism-fidelity run \
  --models <stem_a>,<stem_b> --pool dev --n-cases 5 --k 8 \
  --out sovereign/bench/mechanism_fidelity/results/smoke.jsonl

# Full dev run (K=64) — the instrument-validity (Tier 0) check:
sovereign bench mechanism-fidelity run \
  --models <stem_a>,<stem_b> --pool dev --n-cases 200 --k 64 \
  --manifest sovereign/bench/mechanism_fidelity/manifest.toml \
  --out sovereign/bench/mechanism_fidelity/results/dev.jsonl

# Read the tiered, power-annotated verdict:
python3 sovereign/bench/mechanism_fidelity/verdict.py \
  sovereign/bench/mechanism_fidelity/results/dev.jsonl \
  --manifest sovereign/bench/mechanism_fidelity/manifest.toml
```

The sacred `test` pool refuses to run without `--unseal-test --reason "…"`,
which burns a peek in `baselines/mechanism_fidelity/peek_budget.json`. Use
`--k 200` there to certify the `<0.05` flat band.

## Honest boundary

The synthetic loop measures mechanism **consistency** (agreement with the
structural prior) and **instrument validity** (the control fails while a
sanity agent passes). It does **not** measure correctness — only a real,
scarce holdout (a later work package) tests correspondence to reality. The
harness is built so the speed of the synthetic loop can never be mistaken
for the agent getting closer to truth.

## Adding a mechanism

The core is mechanism-agnostic. A new mechanism registers a `(feature
schema, structural_prior, perturbation set)` in
`sovereign-eval/src/mechanism_fidelity/`; the generator, adapter, scorer,
pools, and manifest are unchanged.
