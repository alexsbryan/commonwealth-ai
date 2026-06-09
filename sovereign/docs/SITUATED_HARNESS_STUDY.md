# Situated-Harness Study — where raw models trip, and how the harness lifts any tier

_2026-06-09. Method, data, and design principles for tuning the core system as a
situated harness that guides many model tiers (4B → 36B) toward success on the
system's work. Companion to `CI_GATE_HANDOFF.md`._

## Thesis

Our value-add is the **harness** — retrieval, structured prompts, verification
loops, routing — not the model. A good harness should **externalize what a weak
model can't do internally**, so the *weaker* the model, the *more* the harness
carries it, converging different tiers toward a shared ceiling. The control that
makes this measurable is **`chaos-monkey --naked`** (commit `00e33f1b`): the bare
model with no system prompt, no retrieval, no router/synthesis — so `full − naked`
is the measured value-add, per tier.

## Method — the tier × harness matrix

Run the same bench (chaos-monkey on `chaos-secret-agent`, IQ4 temp 0) in four
cells: {weak 4B `Qwopus3.5-4B-v3-MTP-Q8_0`, strong 35B `Qwen3.6-35B-A3B-IQ4`} ×
{naked, full}. `--naked` = bare model; full = the Runtime (router → retrieval →
`KNOWLEDGE_SYNTHESIS_SYSTEM` synthesis). `--chat-model fast` selects the 4B.

## Data — chaos grounded-calibration

| cell | competence (≥0.60) | honesty (≥0.70) | hallu | citation |
|---|---|---|---|---|
| 4B naked | 0.21 (5/24) FAIL | 0.27 (3/11) | 0.73 | 0.00 |
| 35B naked | 0.42 (10/24) FAIL | 0.36 (4/11) | 0.64 | 0.00 |
| **4B full** | **0.67 (16/24) PASS** | 0.45 (5/11) FAIL | 0.55 | 0.25 |
| **35B full** | **0.67 (16/24) PASS** | 0.45 (5/11) FAIL | 0.55 | 0.25 |

4B-full and 35B-full are **byte-identical** (same per-question breakdown:
present 15/17, provenance 1/4, distractor 0/3, OOD 5/5).

## Findings

1. **The harness EQUALIZES tiers on grounded competence.** 4B+harness == 35B+harness
   (both 0.67). Once the supporting chunk is in context, *extracting* the answer is
   tier-insensitive — the harness substitutes external grounding for the parametric
   knowledge the small model lacks. Naked, the 4B knows half what the 35B does
   (present 5/17 vs 10/17); harnessed, that gap vanishes.
2. **The harness lifts the weak model MORE** (+0.46 vs +0.25 competence; +0.18 vs
   +0.09 honesty), converging to a shared ceiling. The situated-harness thesis,
   quantified: the weaker the model, the more the harness carries it.
3. **The shared ceiling is the HARNESS's frontier, not a tier gap.** Both tiers
   stall at the same honesty 0.45 (gate-fail) under the same scaffolding. So the
   next lever must be built at the harness level — it will help every tier at once.

## Failure-mode taxonomy (the "where models trip" map)

| # | Raw failure mode | Evidence | Harness lever | Generalizes across tiers? |
|---|---|---|---|---|
| 1 | **Specific-fact grounding** — answers specifics from memory, gets them wrong | naked provenance 0/4, distractor 0/3 | retrieval + synthesis | ✅ EQUALIZES tiers (the headline win) |
| 2 | **Present-vs-absent calibration** — fabricates on in-domain absent facts | adjacent 0/6 at BOTH tiers, naked+full | — (open frontier; blunt prompt rule over-abstains, costs competence) | ⚠ harness-bounded: same 0.45 ceiling both tiers |
| 3 | **Provenance discipline** — omits "general knowledge" caveat | naked OOD 3-4/5 → full 5/5 | prompt (cheap) | ✅ |
| 4 | **Output discipline** — narrates, emits no `<think>` | chaos + knowledge-gym 06 | weak (model won't suppress; no tag to strip) | ❌ model-bounded |
| 5 | **Agentic loop** — forced-tool → write-thrash | agent-bench pi | TDD test-feedback loop (`--agent search`) | ✅ (proven python 8/9) |
| 6 | **Language syntax** (Rust) — can't reach a compiling candidate | probe 0/3 compile | compile-feedback loop | ⚠ bounded by the raw syntax floor |

## Design principles (for the situated harness)

- **Externalize, don't exhort.** The levers that work give the model an external
  artifact it couldn't produce internally: retrieved chunks (grounding),
  test/compile results (verification), structured fields (shape). The levers that
  fail try to *instruct* the model into a behavior it can't do (abstain reliably,
  stop narrating) — those hit a model wall.
- **Scale lever strength to tier.** A weak model needs the externalization more.
  The harness should detect tier/uncertainty and lean harder on grounding +
  verification for weaker slots (future: adaptive scaffolding).
- **Tune at the ceiling, not the floor.** Per-tier floors differ; the harnessed
  ceiling is shared. Spend harness effort on the shared ceiling (here: honesty /
  abstention calibration) — it lifts every tier.
- **The open frontier: tier-agnostic abstention.** Both tiers fabricate on
  in-domain-absent specifics under the current harness. A blunt "if absent, abstain"
  prompt over-triggers (the model can't tell present-vs-absent — see
  CI_GATE_HANDOFF.md Step 2 #2). The needed mechanism is an *external* present/absent
  signal (e.g., a grounding-verifier that checks whether the answer's claim is
  actually supported by a retrieved chunk, and gates the assertion) — externalized,
  so it works for any tier. That is the highest-leverage next build.

## Reproduce

```bash
# naked (bare model) — per tier
sovereign bench chaos-monkey run --bank sovereign/bench/chaos_monkey/secret_agent.toml \
  --manifest sovereign/bench/chaos_monkey/manifest.toml --corpus chaos-secret-agent \
  --naked [--chat-model fast]   --out target/ci-bench/chaos-naked.jsonl
# full (harness) — per tier
sovereign bench chaos-monkey run --bank ... --corpus chaos-secret-agent \
  [--chat-model fast]           --out target/ci-bench/chaos-full.jsonl
```
