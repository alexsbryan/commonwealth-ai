# Query taxonomy — a MECE re-cut (design)

**Status:** design proposal (2026-06-09). Foundation for the role-layer refactor
(`~/.claude/plans/greedy-gliding-melody.md`). Not yet implemented.

## Problem

`Intent` (sovereign-core/src/types/routing.rs:13) fuses two orthogonal axes into one
label, and the seam fails empirically. An "exhaustive, section-by-section account of the
Greenwich bombing plot" gets `confidence=0.00` from the `EmbedRouter` — `top_sim < 0.55`
to *both* `knowledge_query` and `deep_query` exemplars. It isn't "closer to Deep"; it
falls in the **margin gap between them** and defaults to `KnowledgeQuery` → the fast 4B
slot, which leaks open-CoT and hallucinates. The 35B answers the same question cleanly.

The two welded axes:
- **Operation** — what the answer *does* (state a fact, compose an account, contrast entities).
- **Effort/tier** — how much model capability it needs (fast vs primary slot).

`KnowledgeQuery` ≈ (answer × *low* effort); `DeepQuery` ≈ (answer × *high* effort, plus an
implicit "causal/contested" connotation). "Exhaustive factual synthesis" = answer × *high*
effort on a *factual* subject — it has no box. This is `SynthesisRoute`'s conflation one
layer up: operation and tier welded, so neither is cleanly classifiable.

## What is already MECE — preserve

The enum's own doc-comments ground most of it; only the referential set is muddled.

| Layer | Variants | Basis | Verdict |
|---|---|---|---|
| Communicative function | `Metalingual`, `Conation`, `Commissive`, `Expressive` | Jakobson's functions + Searle's speech acts (cited in-code) | **MECE — keep** |
| Action / task | `SimpleAction{tool}`, `ComplexTask`, `Continuation{task_id}` | tool/task axis, orthogonal to "query" | **keep** |
| Referential knowledge | `Simple`, `Knowledge`, `Deep`, `Comparison` | **operation × effort, conflated** | **re-cut** |

So the re-cut touches only the referential subset — *not* the 247 `Intent::` refs wholesale
(most live in the unchanged Metalingual/Conation/Commissive/Expressive/action handlers).

## The MECE taxonomy

Three orthogonal axes; a classified query is a point in their product.

**Axis A — Communicative function (top level, unchanged).**
Referential · Metalingual · Conative · Expressive · Commissive. (Action/task is a sibling
top-level branch.) This is the Jakobson/Searle layer that already works.

**Axis B — Operation (within Referential only) — MECE on answer *structure*.**
- **Answer** — compose an answer from the corpus. *Collapses `Simple` + `Knowledge` + `Deep`*
  — they are one operation at different effort, not three operations.
- **Compare** — bounded contrast of ≥2 named entities (distinct *structure*: shared axes). Keep.
- **Enumerate** — a list/roster (distinct *structure*; today the gated `atom-enum` path). Promote
  to first-class.

**Axis C — Effort → tier (orthogonal, continuous → discrete).**
A scalar from {answer scope demanded (single fact ↔ exhaustive account), source breadth,
reasoning depth} → `{fast, primary}` slot. **The latent signal already exists**: the coarse
`LOOKUP`/`REASONING` label both the `EmbedRouter` and the LLM classifier emit *is* this axis
— it's currently fused into the `Knowledge`/`Deep` choice instead of kept separate. Lift it
out. Answer-scope cue words ("exhaustive / every / section-by-section / complete") feed it,
but as an **effort** signal, not as an intent label — so it generalizes and never teaches to
a bench.

## How this maps to the role layer

This *is* the slot/role separation at the classifier:
- **Operation = Role.** `Answer` → Synthesizer role; `Compare` → a Comparator profile;
  `Enumerate` → an Enumerator profile. Metalingual/Conation/… keep their handlers as roles.
- **Effort = tier**, resolved by the `RoleModelMap` (the CapabilityRouter). `Answer × high → primary`.
- **The escalation dissolves.** "Exhaustive Greenwich account" = `Answer × high → primary` —
  no knowledge-vs-deep boundary to misclassify, no exemplar tuning, no teach-to-the-test.

## Mapping onto today's handlers + the 247 refs

| Today | Becomes | Handler fate |
|---|---|---|
| `SimpleQuery` | `Answer` (effort:low, no/empty corpus) | fold `simple.rs` answer-path into Answer; keep the no-retrieval fast path |
| `KnowledgeQuery` | `Answer` (effort:low–mid) | `knowledge_query.rs` becomes the Answer handler |
| `DeepQuery` | `Answer` (effort:high) | merges into Answer; the always-primary stance becomes `effort:high → primary` |
| `ComparisonQuery` | `Compare` | unchanged handler; keep the fast-slot bounded-axes pin |
| (gated atom-enum) | `Enumerate` | promote from a flag to a first-class operation |
| `Metalingual`/`Conation`/`Commissive`/`Expressive` | unchanged | unchanged |
| `SimpleAction`/`ComplexTask`/`Continuation` | unchanged | unchanged |

Effective blast radius ≈ the referential routing (`route_from_evidence`/`SynthesisRoute`,
evidence.rs) + the EmbedRouter exemplar labels + `knowledge_query.rs`/`simple.rs`/`retrieval.rs`
synthesis sites — **not** the Jakobson/action handlers.

## Migration path (behavior-preserving, ARCH §10)

1. **Introduce the axes as types without changing behavior.** Add `Operation{Answer,Compare,
   Enumerate}` and an `Effort`/tier signal. Provide a pure `Intent → (Operation, Effort)`
   mapping (Simple/Knowledge→Answer/low, Deep→Answer/high, Comparison→Compare, atom-enum→Enumerate).
   Existing handlers keep consuming `Intent` via the inverse map. **Test:** the round-trip
   agrees with current routing on a truth table.
2. **Move the tier decision onto Effort.** The CapabilityRouter resolves `(Operation, Effort)
   → (role, tier)`, reproducing current routes (equivalence test) — this is where the role
   layer's `RoleModelMap` lands.
3. **Re-label the EmbedRouter exemplars by Operation, not by knowledge/deep.** `deep_query`
   exemplars become `answer` exemplars; the effort axis (coarse LOOKUP/REASONING) carries the
   tier. Validate on held-out questions (chaos stays held-out — never an exemplar source).
4. **Collapse `Simple`/`Knowledge`/`Deep` in the enum** only after 1–3 are green, behind the
   compatibility map so consumers migrate incrementally (ARCH §3.2 façade pattern).

## Resolved decisions

**1. `SimpleQuery` collapses into `Answer × low`; retrieval-skip becomes an Effort property.**
`SimpleQuery` is not a no-retrieval branch today — it retrieves and escalates to Slow when it
finds chunks (per-intent head in `runtime/retrieval_pipeline.rs`, ~line 845 — `runtime/retrieval.rs`
was split into `retrieval_pipeline.rs` + the `runtime/retrieval/` directory; exact escalation line
not re-verified in this pass). It conflates *trivial chitchat* (phatic) with *low-effort
factual lookup*. Resolution: map it to `Answer × low`. The "skip retrieval" optimization moves
to the **Effort** signal (effort:trivial → skip retrieval, fast slot, conversational tone), not
a separate operation — keeping the Operation axis clean. A dedicated **Phatic** communicative-
function intent (Jakobson; "hi"/"thanks") is **deferred** — trivial-effort Answer-with-no-corpus
covers it for v1; add Phatic only if small-talk volume warrants (note, don't pre-build).

**2. Effort = the existing coarse label, OR-combined and escalation-biased; no new classifier for v1.**
The coarse `REASONING`/`LOOKUP` signal already exists and is plumbed (`LlmRouter::parse_coarse
{intent, confidence}`; router.rs already has a pre-check that force-routes the deep path on a
coarse reasoning signal). This session *proved* it works: the LLM classifier emitted
`{"intent":"REASONING"}` for the maximal questions — a correct high-effort call that simply
wasn't driving the tier. Resolution: **`effort:high` = (coarse==REASONING from embed-router OR
LLM classifier) OR scope-cue match** ("exhaustive / every / section-by-section / complete / in
depth"), **biased toward escalation** because the cost is asymmetric — under-serving a hard
question (fast 4B → open-CoT leak + hallucination, proven) is worse than over-serving an easy
one (primary latency). Scope cues feed **Effort**, never an intent label → cannot teach to a
bench. No dedicated effort classifier for v1 (the signal is free); add one only if the role-bench
calibration metric (Brier/ECE) shows the coarse label is miscalibrated.

**3. `Compare` stays fast in v1 (preserve the bench learning); `Compare × high → primary` is a guarded follow-on.**
The fast-pin is bundled with comparison-aware retrieval (entity-anchored, knowledge_query.rs:390/
397/614), breadth expansion (evidence.rs:429), and `COMPARISON_DIRECTIVE` — a bench-validated
bundle. The MECE model *permits* `Compare × high → primary`, but v1 **keeps `Compare → fast` as
the default** rather than overturn a validated pin on theory. The prior "escalation regressed
comparisons" learning most plausibly came from escalation *dropping* the `COMPARISON_DIRECTIVE`,
not from the larger model — so the follow-on must **preserve the bounded-axes directive +
comparison-aware retrieval at BOTH tiers** and is gated on the comparison bench before it flips.

## Validation / standing guard
The full `--synth` matrix gates every migration step: chaos is the improve-target; sep / wiki /
marathon / obsidian / enron must not regress beyond ±0.04–0.06 (`answer-equiv` judge). Marathon
is the v32 variance tripwire (run ≥2×). The `Intent → (Operation, Effort)` map ships with an
equivalence test before any consumer migrates (behavior-preserving, ARCH §10).

## Status: spec complete — ready to build
Operation axis `{Answer, Compare, Enumerate}` + Effort→tier, with the three decisions above.
This is the foundation of the role-layer plan (operation=role, effort=tier); it replaces the
"add deep_query exemplars" escalation (empirically blocked + teach-to-the-test risk) with
"effort:high → primary," which falls out of the Effort axis for free.
