# Speculative-decoding experiment

**Status (2026-05-12):** **closed — not shipping in current form.**
Measured wall-time A/B against the production Primary model on Strix
Halo found classic-draft net-negative (0.87×) and n-gram only
marginal on average (1.06×, though 1.40× on highly-repetitive
prompts). The integration cost — re-architecting how pipeline ingest
distributes work through the commonwealth daemon / mesh to drive an
external `llama-server` — far exceeds the measured benefit. Closing
out with all artifacts kept for re-use if the model lineup changes
or MTP support lands in `llama-cpp-2`.

← related: §4.3 (Inference / slots) in `SYSTEM_OVERVIEW.md` and
the pipeline driver at `sovereign/crates/sovereign-core/src/pipeline/`.

---

## TL;DR

| Config | Avg tok/s | vs baseline | Per-prompt range |
|---|---|---|---|
| Baseline (no SD) | 45.82 | 1.00× | 45.4–46.0 (tight) |
| `--spec-type ngram-cache --draft-max 5` | 48.54 | **1.06×** | 43.1–64.1 (high variance) |
| `-md Qwen3.5-0.8B-UD-Q6_K_XL.gguf --draft-max 5` | 39.92 | **0.87×** | 36.5–45.3 (consistent loss) |

Workload: 5 Drafter-shaped pipeline prompts (knowledge / reasoning /
comparison / factual / synthesis), 300-token decode cap each, greedy
(temperature=0). Target: `Qwen3.6-35B-A3B-UD-Q4_K_XL.gguf` on Strix
Halo (Radeon 8060S, ROCm, 124 GB UMA). `llama-server` version 8910
(187a45637), system-installed at `/usr/local/bin/llama-server`.

The classic-draft loss confirms the prior research that SD against
**A3B-class MoE** targets (3B active params per token) is dominated
by the target's already-low per-token cost — verification overhead
swamps the savings regardless of memory-bandwidth regime. The 1.40×
on prompt 2 (TCP vs UDP comparison) shows n-gram pays off when
content has high structural repetition, but the four free-form
synthesis prompts were at or below baseline.

## Goal

Speed up synthesis on the Primary slot (`Qwen3.6-35B-A3B` or
`FINAL-Bench_Darwin-36B-Opus`) for the in-house pipeline path —
specifically the Drafter (`pipeline/runner.rs:536`) and Presenter
(`pipeline/presenter.rs:325`) call sites where 240–1000 tokens of
free decode dominate per-turn wall time. Original target: 1.4–2×
wall-time speedup on long-form decode.

The chat path was explicitly out-of-scope: chat is BYOM and each
user provisions their own model pair, so SD's tokenizer-matched
draft requirement doesn't compose with a community deployment
model. Pipeline is in-house and tuneable; if SD pays off, it's
shippable there.

## Phases attempted

### Phase 0 — kill-switch (≤1.5 days planned, completed in hours)

Built a standalone Rust example
(`sovereign/crates/sovereign-inference/examples/sd_smoke.rs`) that:
1. Loads a draft model in causal mode and verifies non-degenerate
   next-token logits.
2. Verifies the draft and target share a tokenizer
   (`LlamaModel::str_to_token` equality on probe strings).
3. Runs a hand-port of the core draft/verify/accept loop from
   `common/speculative.cpp`.

This surfaced two architectural blockers before any integration
work, exactly as intended:

1. **`FINAL-Bench_Darwin-36B-Opus-Q4_K_L`** has `n_vocab = 248,320`
   and is a hybrid Mamba/SSM + MoE model — not Qwen-family despite
   the naming. The plan's assumed draft (`Qwen3-Embedding-0.6B`,
   vocab 151,669) is structurally incompatible. See
   `memory/project_darwin_not_qwen_family.md`.
2. **`Qwen3.6-35B-A3B-UD-Q4_K_XL`** ALSO has `n_vocab = 248,320` —
   Qwen3.6 is a new tokenizer generation, NOT the same vocab as
   Qwen3. The on-disk Qwen3-family embed model is incompatible with
   both Primary candidates.
3. **`Qwen3.6-35B-A3B-DFlash-Q8_0`** uses a custom architecture
   identifier (`dflash`) that `llama-cpp-2 0.1.146` does not
   recognize. DFlash is a Feb 2026 Z Lab (UCSD) paper targeting
   vLLM/SGLang; the open llama.cpp PR (#22105) has known issues on
   MoE/hybrid models.

Pivot: `Qwen3.5-0.8B-UD-Q6_K_XL` shares the Qwen3.6 tokenizer
(`n_vocab = 248,320`) and is on disk. Tokenizer match passed; the
hand-ported SD loop then hit a KV-position off-by-one bug (Risk 3
in the original plan, materialized as predicted). Rather than debug
the port, pivoted to `llama-server` for the measurement.

### Phase 1 (revised) — `llama-server` A/B measurement

Built `sovereign/bench/sd/bench_sd_ab.sh` — a bash harness that
stands up `llama-server` in a chosen SD configuration, polls
`/health`, sends 5 Drafter-shaped prompts via HTTP, captures the
per-request `timings` payload, and reports averages. Reusable for
any future SD measurement on this hardware.

Results in the TL;DR. Per-config evidence is preserved in
`sovereign/bench/sd/results_{baseline,ngram-cache,classic}.jsonl`.

## Why we closed it

1. **Classic draft is net-negative on A3B regardless of hardware
   regime.** The prior research warned this for consumer Ampere;
   our measurement confirms it on Strix Halo UMA too. The active-
   parameter math (3B active per token on the target) makes target
   decode so cheap that draft + verify overhead always wins. Not a
   bandwidth question; not solvable by switching engines within the
   same family.
2. **N-gram averages 1.06×.** A 6% speedup is below the noise
   floor of normal run-to-run variance and nowhere near the
   integration cost.
3. **The integration cost is large.** Pipeline ingest relies on the
   commonwealth daemon for slot management, peer routing, and mesh
   work distribution. Shipping SD via `llama-server` requires
   either (a) replacing the daemon as the pipeline inference
   backend or (b) teaching the daemon to delegate to a co-located
   `llama-server` for SD-eligible models. Both are non-trivial
   re-architectures.
4. **The in-process Rust port** of `common/speculative.cpp` into
   `sovereign-inference` was originally planned as the Phase 1
   integration path. The hand-port surfaced a KV-rollback off-by-
   one bug on the first run — exactly the failure class flagged as
   Risk 3 in the original plan. A correct port is achievable but
   2-4 days of careful work that buys nothing the `llama-server`
   harness doesn't already provide, and ships nothing the
   measurement doesn't already say isn't worth shipping.

## What stays / what's kept reusable

- `sovereign/crates/sovereign-inference/examples/sd_smoke.rs` — the
  Phase 0 standalone. Useful as a diagnostic if a new model pair is
  proposed: verifies causal-mode load, tokenizer match, and runs a
  toy SD loop. The hand-port has the off-by-one bug noted above; if
  reused, the `measure_acceptance` function needs the KV-rollback
  fix before its numbers are trustworthy.
- `sovereign/bench/sd/bench_sd_ab.sh` — the `llama-server` A/B
  harness. Untouched by the off-by-one bug because it delegates the
  SD loop to upstream's battle-tested implementation. Supports
  `baseline`, `ngram-cache`, `ngram-mod`, `classic` configurations.
  Would be the right tool for measuring **MTP** (the unmeasured
  upside path) once `--spec-type mtp` lands or once someone builds
  llama.cpp from the `am17an/llama.cpp` `mtp-clean` branch.
- `sovereign/bench/sd/results_*.jsonl` — per-run timings as
  evidence.
- `memory/project_sd_spike2_measurements.md` — the empirical
  findings.
- `memory/project_darwin_not_qwen_family.md` — the architectural
  finding about the production Primary alias.

## What would re-open this

Three concrete triggers that would change the recommendation:

1. **MTP support lands.** Either upstream `llama-cpp-2` exposes
   it, or someone is willing to build llama.cpp from
   `am17an/llama.cpp` branch `mtp-clean` and run the existing
   harness against `unsloth/Qwen3.6-35B-A3B-MTP-GGUF`. The
   research suggests ~1.5–2× decode — at that level, the
   integration cost is worth paying.
2. **Primary slot rotates to a non-A3B target.** The active-
   parameter math is the dominant negative-result driver. A dense
   target in the 13–35B range (or a higher-active MoE like A10B)
   would shift the math. If the Primary alias moves to such a
   model, re-run `bench_sd_ab.sh classic` with a vocab-matched
   draft.
3. **Pipeline workload sample turns out heavily repetitive.** The
   n-gram 1.40× on one prompt suggests that workloads with strong
   structural repetition (atlas JSON extraction, citation-heavy
   synthesis, recipe templates) might consistently see 1.2–1.3×.
   A targeted re-measure using a real atlas extraction recipe
   (not the synthetic Drafter prompts in this bench) would tell
   us whether to ship n-gram as a recipe-specific opt-in.

## Reproducing the run

```
sovereign/bench/sd/bench_sd_ab.sh baseline
sovereign/bench/sd/bench_sd_ab.sh ngram-cache
sovereign/bench/sd/bench_sd_ab.sh classic       # loads draft model
sovereign/bench/sd/bench_sd_ab.sh all           # runs all three sequentially
```

Defaults: `Qwen3.6-35B-A3B-UD-Q4_K_XL` target, `Qwen3.5-0.8B-UD-
Q6_K_XL` draft, port 8765, 300-token decode cap, `--draft-max 5`.
Override via env: `TARGET_MODEL`, `DRAFT_MODEL`, `PORT`, `N_PREDICT`,
`DRAFT_MAX`, `SERVER_BIN`.

Per-run output: `sovereign/bench/sd/results_<config>.jsonl` (per-
prompt timings) + stdout summary line.

The daemon at `localhost:9741` can stay running during the bench —
the harness uses port 8765 and the Strix Halo UMA has enough headroom
for both `llama-server`'s 21 GB target + 0.6 GB draft and the
daemon's resident slots.
