# Reasoning-Fidelity — Work Summary & Future Research

**Created:** 2026-06-06 · **Companion to:** `HANDOFF.md` (build state / how to pick up) and
`README.md` (what the bench is). This doc is the **"where does this go"** layer: a summary of
what exists, the one constraint that governs how we may use it, and the research/product
directions to return to.

---

## 1. What this system is (in one breath)

A **measurement layer** that decides, per *reasoning class*, whether a frozen model reasons
from the **causal mechanism** or from **memorized association with the label** — cheaply
enough to characterize a model **once** (cached as a *fidelity card*) and read **free** at
query time. It is metamorphic testing: generate cases, perturb them, and check that the
model's decision moves the way the structural prior says it must (DIR), stays put when it
must (INV), and that a feature-blind **negative control** *fails* (the instrument-validity
guard).

It is **not** a benchmark of accuracy. See §3 — that boundary shapes everything downstream.

---

## 2. What we built (state as of 2026-06-06)

The harness went from a single hard-coded wealth-tax instrument to a **registry of reasoning
classes** behind one generic, self-terminating orchestrator that emits per-model capability
cards.

- **Class registry** — `ReasoningClass` trait (`class.rs`) + `registry.rs`. A class owns its
  label set, target-probability read, system prompt, and probe matrix. Adding a class is one
  trait impl + one registry line; the scorer, pools, early-stopping, and cards are all generic
  over it.
- **Three classes ship:**
  - `wealth_tax_relocation` — synthetic, logistic structural prior (the reference instrument).
  - `attribution_support` — **corpus-grounded**: mines `Claim` atoms + their evidence from a
    corpus's `atlas/atoms.json`; **exact 0/1 oracle** (label known by construction); negate /
    reframe / distractor perturbations; **blindfold control** (passage withheld) + a
    `control_cannot_cheat` guard. No lance dependency.
  - `aggregation_threshold` — synthetic counting-under-a-threshold; exact 0/1 oracle.
- **Forced-choice logprob elicitation** — one forward pass per probe (the candidate set rides
  inside `structured_output` as a sentinel the daemon reads off the masked next-token logits).
  **Elicitation is sequential** — this is load-bearing, not cosmetic (§3, the control witness).
- **Anytime-valid early-stopping** (`stopping.rs` + `decide_at`) — empirical-Bernstein
  intervals at a pre-registered checkpoint schedule; stops the instant the **overall verdict**
  is decided (any required band fails → NO-GO; all pass → GO). Verified: an n=200 dev run
  stopped at 64/200.
- **Fidelity cards** (`card.rs`) — per-`(model, class)` grade + metrics →
  `~/.svrnmesh/model-fidelity-cards/<model>.json`, stamped with a manifest fingerprint so
  stale bands invalidate the card.
- **Hardened `verdict.py`** — no longer crashes on `null` (failed-probe) `d_agent`; reports an
  honest logprob power line (deterministic, K=1).

**Live finding (this daemon's `primary`, a Darwin-36B):** a *fidelity spectrum* —
counting **−0.868**, attribution **−0.218**, wealth-tax **−0.006** (NO-GO / Unfaithful), with a
perfectly inert negative control on all three. The same model reasons mechanistically about
*counts*, somewhat about *evidence*, and not at all about the *wealth-tax differential*. That
per-class differentiation is the whole point.

Tests: 41 `sovereign-eval` + 3 `sovereign-cli-llm` green; full-workspace lint clean; only the
3 known pre-existing failures in untouched crates.

---

## 3. The constraint that governs everything: consistency ≠ correctness

The cards measure **agreement with a structural prior**, plus **instrument validity** (the
control fails). They do **not** measure correspondence with reality. A `Faithful` grade means
"reasons mechanism-consistently on synthetic probes," **not** "is accurate in production."

Two consequences we must hold to:

1. **Cards are a *negative* signal today.** "This model does not reason from the mechanism on
   class X" is well-supported (it failed a test a faithful reasoner passes) — so *"don't fully
   trust it here"* is sound. The positive claim *"do trust it here"* needs the real-holdout
   calibration layer (§5-C) before it earns an end-user-facing promise.
2. **The grounding verifier (§5-A) is exempt** — checking whether a *generated answer's cited
   passage actually supports the claim* is a direct correctness check on that specific output,
   not a synthetic proxy. It is the one piece that delivers user value without waiting on
   calibration.

> **The model-independent regression witness** for the whole pipeline is: the negative
> control's `d_agent` must be **exactly 0.000** (byte-identical blindfold prompts + sequential,
> deterministic elicitation). If that moves, the scoring join or determinism broke. This is
> why elicitation is sequential — concurrent same-slot requests batch together and the daemon's
> batched matmuls are not bit-invariant to batch composition, which drifts the control off 0.

---

## 4. From measurement to end-user value — the read side

Two reusable assets fall out of the harness:

- **The fidelity card** = a per-class map of *where a model is weak*. → a routing / abstention /
  escalation signal.
- **The forced-choice primitives** (`"does this passage support this claim?"`,
  `"is this count > N?"`) = one-pass verifiers that were built as *test probes* but work just as
  well as **runtime verifiers of the assistant's own output**.

**The combination is the thesis:** *the card tells you which verifier to run; the verifier does
the work.* Run the attribution check on a RAG answer's citations **when the active model is
graded weak on attribution**; skip it when it's strong. Measurement directs effort.

**Where it plugs in:** the chat path is `runtime.handle_message_stream` (sovereign-server +
desktop) — intent classifier → router → retrieval → synthesis → sensitive filter. A query→class
classifier follows the **embed-centroid** pattern already preferred here
(`router_embed.rs` / `scope_classifier.rs`), not keywords. The verifier slots in after
synthesis; the card consumer reads `~/.svrnmesh/model-fidelity-cards/`.

---

## 5. Future Research (the backlog to return to)

Roughly ordered by *value-now ÷ prerequisite-cost*. Each: **what**, **why it's safe/valuable**,
**prereq**, rough **effort**.

### A. Runtime grounding verifier  ·  effort: M  ·  prereq: none (works on the single-model daemon)
Reuse the `attribution_support` forced-choice primitive at answer time: after synthesis, for
each cited claim, ask the model `"does the cited passage support this claim?"` and **flag or
strike** unsupported claims; surface a per-answer **grounding score**.
- **Why first:** sidesteps the consistency≠correctness boundary (§3.2) — it's a direct check on
  the actual output against the actual evidence. Highest immediate user value (fewer
  confidently-wrong cited answers), no calibration or multi-model needed.
- **Open questions:** verify-with-same-model vs. a second model (self-verification bias);
  threshold for "strike" vs. "soft-flag"; cost budget (one extra forward pass per citation).

### B. Class-aware caveats + fidelity-directed routing  ·  effort: M–L  ·  prereq: classifier; routing needs the daemon fix (§6)
Build the read side: a query→reasoning-class classifier (embed-centroid, abstain+margin gates,
tuned on a small harness), a card lookup, and a Runtime policy:
- weak class → **calibrated caveat** ("I'm less reliable at this kind of question") and/or
  **escalate to verification** (§5-A);
- multi-model available → **route** the query to the model graded faithful on its class.
- **Why:** turns the cards into live behavior; the abstention path is honest *today* (negative
  signal); routing is the long-game payoff once the daemon serves distinct, name-routed slots.
- **Open questions:** class taxonomy granularity vs. classifier reliability; what fraction of
  real user queries even fall into a characterized class (coverage); caveat fatigue.

### C. Real-holdout calibration: consistency → correctness  ·  effort: L  ·  prereq: a scarce real test pool
Build the sacred `Test`-pool layer (the `PeekBudget` discipline already exists): natural
experiments + post-cutoff events with known outcomes, so a grade can finally claim
*correspondence with reality*, not just internal consistency. This is what upgrades cards from a
negative signal to a **positive, trustworthy** one — and what lets §5-B make end-user promises.
- **Why:** removes the §3 ceiling on everything downstream.
- **Open questions:** sourcing genuinely post-cutoff / uncontaminated cases per class;
  prior calibration (the structural coefficients are placeholders — `structural.rs` `HORIZON/
  FRICTION_K/SCALE`); how a synthetic-faithful-but-holdout-failing model signals *mechanism
  revision* (per the `structural.rs` "political-capture channel" note) rather than model
  retraining.

### D. Glassbox transparency surface  ·  effort: S–M  ·  prereq: A or B producing signals
A desktop "why" panel showing the detected reasoning class, the active model's grade on it, and
(when run) the grounding score — *before* changing any answer behavior. Lowest-risk way to build
trust in the signal itself, and it embodies the project's glassbox/observability principle.

### E. Remaining reasoning classes  ·  effort: M each  ·  prereq: confirm atom schemas first
Each is one `ReasoningClass` impl in the shared `base/dir_p1/dir_p2/inv_i1 × full/stripped`
shape (so scorer/verdict/cards/early-stopping all work unchanged), plus the mandatory
"control-can't-cheat" + deterministic-in-seed tests.
- **Comparative** — corpus-grounded on `EdgeType::Tension` + `Position.salience`; **INV =
  order-invariance** (swap A/B → same answer).
- **Identity** — `entity_resolution_bench::GroundTruthEntity` + a deterministic name-paraphrase
  engine; dir = swap a discriminating attribute, INV = paraphrase the name.
- **Stretch:** temporal (needs date extraction), argument-structure.
- **Caveat learned the hard way:** confirm field names against a *real* `atoms.json` before
  coding — the documented atom struct path was stale (the real `Claim` schema is
  `{content, evidence:[{chunk_id, passage_preview}], quotable_excerpt, …}`).

### F. Infrastructure unblockers (gate the above)
- **Multi-model daemon routing.** This daemon's BYOM OICP adapter routes forced-choice by
  *latency class*, not model name, so `--models a,b` collapse onto one slot. Distinct,
  name-routed forced-choice slots are required before any *comparative* characterization or
  *routing* (§5-B) is real.
- **Frontier-model witness.** A strong, known-faithful model (API key via
  `--base-url`/`--api-key-env`) is both the positive control for the instrument and the
  throughput path. Currently absent.
- **Mesh fan-out.** Characterization is embarrassingly parallel across (model × class × case);
  the orchestrator is single-box today.

---

## 6. The honest open questions (worth stating plainly)

1. **Coverage:** what fraction of real user queries map cleanly to a characterized reasoning
   class? If it's small, §5-B's routing helps few queries and §5-A (output verification, which
   needs no class match) is the better lever.
2. **Self-verification bias:** can a model usefully grade the groundedness of its *own* output,
   or does §5-A need a second (cheaper/different) model?
3. **Class atomicity:** real questions blend reasoning types. Is per-class characterization the
   right unit, or do we need compositional fidelity?
4. **Mechanism vs. model revision:** when a faithful model misses a real holdout, the design
   says *revise the mechanism, not the model*. We have no tooling for that loop yet.
5. **Connection to the Public Knowledge Desk** (separate plan): grounding-verified, caveated
   answers are exactly what a public, anonymous-client surface needs to be trustworthy — §5-A is
   a natural dependency for that product if it proceeds.

---

## 7. Pointers

| Thing | Where |
|---|---|
| Build state / how to pick up | `HANDOFF.md` |
| Pure logic (classes, scorer, stopping, cards) | `sovereign-eval/src/mechanism_fidelity/` |
| Orchestrator (inference-coupled) | `sovereign-cli-llm/src/bench_cmd/mechanism_fidelity.rs` |
| Pre-registration bands | `manifest.toml` |
| Verdict reader | `verdict.py` |
| Cards | `~/.svrnmesh/model-fidelity-cards/<model>.json` |
| System map entry | `sovereign/SYSTEM_OVERVIEW.md` (Reasoning-Fidelity Validation Harness) |
