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

## Landed 2026-07-20 (the close-out session)

### P0 — gate abstains a 0-holding decline — ✅ SHIPPED
`released_pure_decline` guard in `grounding/mod.rs` (`gate_answer`
fall-through): a NO_CLAIM release whose text is a pure provenance-flagged
decline is reclassified **`abstained_decline`** (the model's own decline
prose ships unchanged). Verdict now derives `cannot_know_from_here`, the
coverage probe runs, and the gap routes on real coverage instead of
defaulting `ClaimUncovered`. Caveated parametric ANSWERS ("Not in your
sources — from general knowledge: …") are structurally excluded and keep
releasing — reclassifying those would make the typed verdict an
unfaithful answer-vs-abstain proxy. Both chaos scorer paths (typed +
legacy) read the same gate action, so their parity is structural.
Unit-pinned (both directions); red-line live re-run recorded below.

### P1 — gap.rs retirement EXECUTED + invariants pinned
- **`gap.rs` is deleted.** `run_collaboration` takes `abstained: bool`
  (the signal the verdict derives from, D3) as its entire detection;
  the card's ask is a phrasing-only fast-slot pass
  (`phrase_gap_question`, D4 — hard fallback: the raw question).
  Answered turns pass through instantly (removes the 15-55s
  grammar-constrained audit from every answered turn). Post-stream
  callers derive the signal from persisted `grounding_gate.action`;
  handlers thread it from their gate outcome; the doc-op attached-doc
  path runs no gate → never fires the card (its short-answer cases
  already fall through to the gated runtime). Full execution record:
  `bench/gap_check/DECISION.md` §EXECUTED.
- **Invariant pins:** I1 closed-surface pin (`epistemic.rs` —
  exhaustive `GateSurface` match; new variants fail compilation until
  their ledger story is recorded) + runtime witness
  (`functional.rs::simple_turn_persists_epistemic_ledger_with_verdict`).
  I2 truth-table, I3/I6 (`EpistemicFooter.test`), I4 resolver pin
  (`acquisition.rs::routes_come_only_from_the_catalog`) audited, present.

### P2 — partially landed
- **Witness surface now emits Memory holdings.** Audit found NO
  production path passed `recalled`/`recall_verification` into the
  assembler — the §5 memory-distinction UI could never fire from a real
  turn. The non-streaming witness (expressive.rs, Relational) now
  persists `epistemic_state` when the recall verifier attributed the
  reply to an entry (verdict `memory_recall`, banded holding, FailOpen
  visible). Un-attributed witness turns stay ledger-less by design.
- **Acquisition lane hard-gate wired** (`min_acquisition_conjecture` in
  `Gates`/`Verdict`/manifest loader; disarmed at 0.0 until a manifest
  baseline is set — see below).

## Outstanding — prioritized

### Live validation (two 43-question runs, 2026-07-20, gemma-4-E4B)
Run 1 (P0 + retirement) exposed two last-mile defects, both fixed +
unit-pinned before run 2: (1) `finish_demands`' `Retrieved if abstained`
arm swallowed the probe verdict (OOD gaps mis-routed ClaimUncovered);
(2) a NO_CLAIM retry-decline marked the original claim supported —
`ood-table-salt` shipped ledger verdict `grounded` on "I don't have
reliable information on this." Run 2 (fixes live): OOD gaps
`topic_uncovered` with `install_recipe` routes present; table-salt
abstains. **RL-1 0.69 PASS · RL-2 (rubric below) 1.00 PASS ·
hallucination 0.00 · zero absent-side fabrications both runs.**

**RL-2 rubric edit (owner-approved scientific event, recorded in the
manifest):** an abstained OOD probe counts honest — the old
answered-with-caveat-only rubric conflated honesty with the hybrid
helpfulness bar, and its historical pass rode on released declines
being credited as caveated answers (unmasked by the P0 guard). OOD
timidity now lives in the TRACKED `ood-caveated-answer` lane
(this model: 1/5; June opus runs: 5/5 — model-behavior target).

### P2 remainder
- **Acquisition resolver top-1 ranking.** Lane baseline 0.45 (5/11)
  post-fixes — do NOT arm the gate on it. The misses are precise:
  `import_conversations` ranks top-1 on **6/6 gap turns across
  unrelated topics** (a generic-description embedding attractor);
  the right `install_recipe` route now appears but in slot 2. Next
  loop: measure catalog sims, then bias/floor connectors vs recipes —
  its own A/B. (Also: `unknowable`-labeled abstained turns emit
  routes, which the lane counts as misses — decide whether the lane's
  `unknowable ⇒ no conjecture` contract should apply to gap turns.)
- **OOD caveated-answer rate** — prompt/model work to restore the
  caveat-answer behavior on the small slot (tracked lane target).
- **Streaming-witness recall verifier.** The streaming witness runs no
  verifier (`recall_verification: None`), so desktop *streaming* witness
  turns still can't carry memory holdings. Extending the verifier there
  is the remaining memory-provenance wiring; the live labeled probe
  (§8) then belongs with the inner-chaos harness (`bench/inner_work`,
  recall fixtures already exist).
- **Ledger fidelity** — sampled turns, judge checks holdings ↔ prose
  correspondence (grounding-bench style).

### P3 — cleanup + explicitly deferred
- **§6 deletions completion** — the GK caveat-prefix *protocol* role
  (the verdict is the SSOT now for answer-vs-abstain; the string-match
  path can shrink further). `parseSources` already legacy-only.
- **`demand_plan` disposition** — dark + measured-negative. Decide: keep
  the fan-out lever for future tuning vs. delete the dead default path.
- **Mobile rendering** — projection wired (I2-C); rendering deferred
  until the mobile toolchain pass.

## Landed 2026-07-20 (afternoon — the F-wave + the proof)
- **F1** Acquisition resolver: content-bearing tier (recipes outrank
  connectors; the `import_conversations` attractor is dead). Lane
  0.45 → **0.82**; gate ARMED at 0.70 in the manifest.
- **F2** Streaming witness: post-stream recall verification + banded
  Memory-holding ledger on attributed recalls — the §5 memory
  distinction now fires on the real desktop chat path.
- **F3** OOD GK rescue (`gk_rescue.rs`, probe-gated, kill switch):
  caveated parametric answer + routes instead of the dead-end decline.
  OOD caveated-answer lane 0.20 → **0.80** (the 5th kept its honest
  abstention when the rescue itself declined — by design).
- **F4** Ledger-fidelity bench (`bench chaos-monkey fidelity`):
  deterministic forged-receipt cross-check + judged holdings↔prose
  correspondence. Caught a second forged-receipt instance on the
  pre-guard artifact; **0 conflicts** post-guard; correspondence
  tracked at ~0.75–0.80.
- **F5** Paired before/after runs + [`EPISTEMIC_STATE_PROOF.md`](./EPISTEMIC_STATE_PROOF.md)
  — the skeptic-facing case, every number citing a committed artifact.

## One-line take
The vision behaviors are now live end to end and measured: honest per
statement (typed ledger, auditable receipts, forged-receipt class at
zero), never a dead end (probe-driven rescue + catalog routes, 0.80/0.82
on their lanes), memory visibly distinct on both witness paths — and the
before/after proof doc makes the architecture case with committed
artifacts. Remaining tail: holdings↔prose triage (0.75 tracked),
`unknowable` lane semantics, mobile rendering, §6 cleanup.
