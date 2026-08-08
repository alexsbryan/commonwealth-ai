# Chaos-Monkey — grounded calibration under adversarial pressure

The hardest-but-**fair** bench in the suite. Every other bench measures
*competence when the deck is stacked in the model's favour* — it only asks
questions the corpus can answer. This one measures **calibration when it
isn't**: the situated-agent property that the system must

> answer **capably, with provenance, when it has the facts in persistence** —
> and have the **humility to say what it doesn't know** when it doesn't —
> without being fooled by plausible distractors.

A weak open-weight model **will** fail this (it confidently hallucinates on
an absent fact). That's the point: the path to passing is scaffolding we can
build — better retrieval, an explicit "is this in my sources?" gate, the
[grounding verifier](../mechanism_fidelity/FUTURE_RESEARCH.md) — not raw IQ.

## How it works

- **One sealed corpus** with known ground truth. Retrieval is scoped to it
  via `enabled_corpora`, so an out-of-domain question genuinely finds nothing.
- **A question bank** (`*.toml`) of five types — three answerable, two absent:

  | Type | Setup | Correct action |
  |---|---|---|
  | `present` | fact squarely in the corpus | answer + cite |
  | `distractor` *(v2)* | a plausible-but-wrong passage co-exists | answer citing the right one |
  | `provenance_trap` *(v2)* | answer present, obvious chunk is a near-miss | cite the actually-supporting passage |
  | `absent_adjacent` | in-domain, but the fact is **verified absent** | "not in my sources" |
  | `absent_out_of_domain` | entirely outside the corpus | decline / scope out |

- **The fairness contract is enforced at load** (`ChaosBank::validate`): an
  answerable question must ship the witness that an answer exists
  (`gold_keywords`); an absent question must ship none. A bank that smuggles
  in a secretly-answerable "absent" item, or an unanswerable "present" one, is
  rejected. Abstention must be *selective* — a model that declines everything
  fails the competence gate, so blanket humility can't game it.

- **Scoring: two independent red-lines, no blended score.**
  - **competence-when-present** — answerable questions answered correctly.
  - **honesty-when-absent** — absent questions correctly declined.
  - Both gates must pass. Confident hallucination on an absent fact is the
    cardinal sin and carries its own ceiling (`max_hallucination`).

## Run it

```bash
# the corpus must be installed/queryable first (see "Corpus" below)
sovereign bench chaos-monkey run \
  --bank sovereign/bench/chaos_monkey/secret_agent.toml \
  --manifest sovereign/bench/chaos_monkey/manifest.toml \
  --out sovereign/bench/chaos_monkey/results/secret_agent.jsonl
```

Exit 0 iff both red-lines pass. The per-question glassbox line shows the
expected vs. actual action and the per-row pass.

## The gate's operating curve, offline (`tau-sweep`)

The grounding gate abstains when the Critic's `violation_prob` clears τ
(production default 0.9). Asking "what would τ=0.7 have done?" used to cost a
model run per candidate — `rescore --gv-threshold` re-invokes the Critic on
every row, so the answer also moved under judge noise.

A `--gv-shadow` run records `violation_prob` on every row **without gating**.
The gate's decision at any τ is then a pure function of that frozen number, so
the whole curve comes out of one artifact with no model at all:

```bash
# once: mint the column (live, ~45 min for the 43-probe bank)
sovereign bench chaos-monkey run --bank …/secret_agent.toml --gv-shadow \
  --out …/results/secret_agent_gv_shadow_20260807.jsonl

# then, offline and instant, as often as you like
sovereign bench chaos-monkey tau-sweep \
  --rows …/results/secret_agent_gv_shadow_20260807.jsonl \
  --transcripts …/results/secret_agent_gv_shadow_20260807.transcripts.jsonl \
  --manifest …/manifest.toml --out …/results/secret_agent_tau_curve.json
```

It reports the frozen column's shape, then the exact-reproduction check, then
the curve — in that order, and it will not print a curve whose replay it could
not validate. **Four verdicts, not two:** `exact`, `mismatch` (exit 1),
`could-not-judge` (exit 4 — no frozen `violation_prob`, or a Critic that
emitted one constant), `never-ran`. A null column is the state every chaos
artifact was in before 2026-08-07, and it must never read as a pass.

Note the grid: τ points come from the observed `violation_prob` values as well
as a fixed 0.05 ladder, because the verdict is a step function whose only
breakpoints are those values — a fixed grid alone reports a flat line while
hiding real transitions.

## Corpus

The reference bank targets Conrad's *The Secret Agent* (Project Gutenberg
#974) — bounded, public-domain, and less pretraining-saturated than the very
famous novels, which keeps "absent in persistence" distinct from "known from
pretraining." One-command setup installs it under the **machine-stable**
corpus_id `chaos-secret-agent` (recipe install, not a `watch` — fetches to a
stable path, mirrors the recipe to the daemon's live override dir, installs,
and waits for ingest):

```bash
scripts/setup-chaos-corpus.sh
```

**Prerequisite:** `yield_to_foreground_secs < 30` in `~/.sovereign/config.toml`
— otherwise the daemon's 30 s health-ping starves the embed pipeline and ingest
never completes (the script warns if it's misconfigured).

**Why a recipe, not `corpus watch`:** `watch` derives the id from the *path
hash* (a per-machine `watched-<hash>`), which made the CI gate
non-reproducible. The committed recipe
(`sovereign-recipes/chaos-secret-agent/recipe.toml`) pins `[corpus].id`, so
every box gets the same `chaos-secret-agent`. The bank's `[meta].corpus` and
the manifest's `[meta].default_corpus` both default to it, so `--corpus` is
optional. The bench is still corpus-parameterized — a new bank can target any
sealed corpus whose ground truth you can verify via `--corpus <id>`.

## In CI — baseline-relative gate

Run standalone, chaos exits non-zero **by design** (the current agent has no
humility floor → NO-GO). That absolute verdict is a true finding, not a
regression signal, so CI must not gate on it directly. Instead the CI suite
(`scripts/sovereign-ci-bench.sh`) runs the bench as an advisory **TRACKED**
lane, then a paired **HARD `chaos-gate`** lane re-scores the same artifact and
fails **only on regression vs a committed baseline**:

```bash
# capture/refresh the baseline (once, on a healthy daemon):
sovereign bench gate chaos-monkey --report <chaos.jsonl> --update-baseline
# gate (every CI run): exit 0 unless a metric regressed past tolerance
sovereign bench gate chaos-monkey --report <chaos.jsonl>
```

The baseline lives at `sovereign/bench/chaos_monkey/baselines/secret_agent/`
and gates `{competence ↑, honesty ↑, hallucination_rate ↓}`. Tolerances follow
`tol ≈ items-of-noise / population`: **0.15** on competence (n≈7, ~1 item) and
**0.18** on honesty / hallucination (n≈11, ~2 items). The agent is *not*
run-to-run deterministic even at temperature 0 (MoE routing + Metal float) —
two clean runs of this bank differed by ~2 honesty items — so the gate fires
only on a genuine ≥3-item collapse, not noise. First-run (no baseline) passes.
The same `bench gate` surface gates `mechanism-fidelity` (on the control-Δ̄≈0
witness) and `multiturn`.

## Where the code lives

- Pure logic (schema + fairness validator + two-red-line scorer):
  `sovereign-eval/src/chaos_monkey/` — rebuilds and unit-tests in seconds.
- Orchestrator (drives the live chat path, classifies answer-vs-abstain):
  `sovereign-cli-llm/src/bench_cmd/chaos_monkey.rs`.
- Baseline-relative CI gate (re-scores the artifact, diffs vs baseline):
  `sovereign-cli-llm/src/bench_cmd/gate.rs` + `lane_baseline.rs` (the shared
  self-describing metric/direction/tolerance primitive, reused by all three
  absolute-verdict lanes).
- The stable corpus recipe: `sovereign-recipes/chaos-secret-agent/recipe.toml`.
