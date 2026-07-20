<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# Epistemic State — program status (snapshot 2026-07-19)

Companion to [`EPISTEMIC_STATE.md`](./EPISTEMIC_STATE.md) (the vision +
§9 roadmap). Maps each initiative to what has shipped, what remains, and
priority. Branch `v1`. Six commits landed this session:
`adfb5600` (I2+I3) · `5e5690f3` (I4-A) · `cdc7b44b` (I4-B r2) ·
`b537f2bf` (I2-C parity flip) · `90059493` (I4-C gap-check) ·
`62d353fa` (coverage_probe scope).

## Where each initiative stands

### I1 — "The honest turn" (P0 assembly · P1a deterministic demands · P2 resolver) — ✅ DONE
Implemented before this session. Typed `EpistemicState` assembled by pure
collation on every KQ/Deep/Simple turn; deterministic demand set +
coverage residue; catalog-only acquisition resolver; coverage probe.

### I2 — "Rendering honesty" (P3 render · P5 surface completion · P4b) — ✅ SHIPPED
- **I2-A (P5)** — attached-doc + complex-task retain gate claims and
  persist `epistemic_state`; complex-task emits `ToolDerived` holdings.
- **I2-B (P3)** — `EpistemicFooter.svelte` (provenance badges, memory
  band, verdict receipt, abstention panel) rendered on ledger-bearing
  messages; `ResearchGapCard` deleted. vitest + Playwright green.
- **I2-C** — server projects `epistemic_state` (REST + WS). Chaos scorer:
  **answer-vs-abstain flipped to the typed verdict as PRIMARY** (43/43
  parity, structural); caveat kept on the judge (parity caught the typed
  GK verdict as unfaithful).
- **I2-D (P4b)** — `NotebookOpenQuestions` Explore-tab panel.

### I3 — "Unasked questions persist" (P4a) — ✅ DONE
`field_engine` binds `detect_open_questions` + threads it into the JSON
skeleton; `SkeletonOpenQuestion.question_type` (additive). Only
field_model corpora re-enriched after this carry the data.

### I4 — "Demand intelligence" (P1b demand_plan · gap.rs retirement) — ◐ MEASURED, not promoted
- **I4-A** — `demand_plan` dark step shipped (`SOVEREIGN_DEMAND_PLAN`,
  default off).
- **I4-B** — A/B verdict: **does NOT earn its keep** (2–3× retrieval p50,
  no recall gain; even ledger-only pays the ~4s planner call — a
  160-token structured generation, not the 1-token judge the cost model
  assumed). Fan-out decoupled (`SOVEREIGN_DEMAND_PLAN_FANOUT`), re-A/B
  still negative. A "gap-turn-only planner" (round 3) was **rejected**:
  it violates D1 (assembly is collation, no post-hoc LLM pass) + D2 (one
  demand model). Faithful state: the LLM plan stays dark; the
  deterministic demand set (P1a) is the default. **No promotion.**
- **I4-C** — gap-check fixture bank built (12 real triples). **Decision:
  retire gap DETECTION to the verdict** (12/12 on the bank; the gaps-
  residue only 11/12). `coverage_probe` **scoped to `enabled_corpora`**
  (floor was actually fine; the bug was arbitrary first-12 fan-out) —
  deterministic + ~10× faster. gap.rs itself **not yet deleted**.

## Outstanding — prioritized

### P0 — honesty correctness (root-cause, vision-central)
- **Gate should ABSTAIN, not release-as-`unverified`, a 0-holding
  provenance-flagged decline.** Discovered while measuring coverage:
  turns like "capital of Australia" where the model says "I don't have
  reliable information" get gate action `released` → verdict `unverified`
  (not `cannot_know_from_here`) → the coverage probe never runs → gap
  coverage defaults to `ClaimUncovered` (mis-routes a genuine knowledge
  gap). Fix is upstream in `runtime/grounding` (a 0-holding decline is an
  abstention). Has chaos-parity implications — must re-run the red-line
  parity after. This is the biggest single honesty gap left.

### P1 — finish the decided retirement + pin the invariants
- **Execute gap.rs retirement.** Route the `maybe_collaborate` hook
  (`runtime/collaboration.rs:190`) off `identify_gap` detection onto the
  ledger's `gaps` + a **phrasing-only** fast-slot pass (D4: may phrase,
  never invent). Re-run the fixture bank + the card's live behavior; then
  delete `gap.rs`. Decision is made (I4-C); this is execution.
- **Pin invariants I1–I6 as structural tests (§7).** Partly done (I2
  verdict truth-table; I6 ledger-wins in `EpistemicFooter.test`; I3
  memory-distinct render). Audit + complete — esp. **I1** "every answer
  surface produces a ledger" as a closed-surface test (KQ + streaming +
  simple + attached-doc + complex-task) and **I4** "no non-catalog route
  survives to the DTO."

### P2 — validate the rendering (measurement gates §8 not yet run)
- **Memory-provenance probe** — labeled set through the witness + factual
  paths asserting the rendered distinction (memory chip present, band
  correct, `FailOpen` marked). Extends the inner-chaos fixture pattern.
- **Ledger fidelity** — sampled turns, judge checks holdings ↔ prose
  correspondence (grounding-bench style).
- **Hard-gate the chaos acquisition_conjecture lane** — currently
  tracked; capture a baseline (now that coverage routing is scoped) and
  hard-gate per §8.

### P3 — cleanup + explicitly deferred
- **§6 deletions completion** — the GK caveat-prefix *protocol* role
  (the verdict is the SSOT now for answer-vs-abstain; the string-match
  path can shrink further). `parseSources` already legacy-only.
- **`demand_plan` disposition** — dark + measured-negative. Decide: keep
  the fan-out lever for future tuning vs. delete the dead default path.
- **Mobile rendering** — projection wired (I2-C); rendering deferred
  until the mobile toolchain pass.

## One-line take
The honesty machinery (I1–I3, the I2-C typed verdict) is shipped and
proven; the retrieval optimization (I4 demand_plan) is measured and
correctly kept dark. The highest-value remaining work is the **P0 gate
abstain-vs-release fix** — it's the one honesty defect the measurements
surfaced that still ships to users.
