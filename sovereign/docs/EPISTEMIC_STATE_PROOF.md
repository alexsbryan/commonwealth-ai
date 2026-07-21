<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# Epistemic State — the before/after case

**Date:** 2026-07-20 · companion to [`EPISTEMIC_STATE.md`](./EPISTEMIC_STATE.md)
(vision) and [`EPISTEMIC_STATE_STATUS.md`](./EPISTEMIC_STATE_STATUS.md)
(execution log).

This document exists for the skeptic of the architecture investment: the
claim that making the answer's epistemic state a **typed artifact** (one
producer path, many views) beats the string-based predecessor is not an
aesthetic preference — it is measurable, and this is the measurement.
Every number cites a run artifact in `bench/chaos_monkey/results/`.

## 1. Method

Two live conditions on the **same bank, same model, same machine, same
day**: `secret_agent.toml` (43 questions: 32 answerable, 11 absent;
sealed to the `chaos-secret-agent` corpus), primary slot
`gemma-4-E4B-it-Q6_K`, full production chat path
(`Runtime::handle_message_stream`).

- **BEFORE** — the epistemic machinery disabled via its kill switches
  (`SOVEREIGN_EPISTEMIC_STATE=0`, `SOVEREIGN_GK_RESCUE=0`, legacy
  string-sniffing scorer): no ledger, no coverage probe, no acquisition
  routes, no rescue. This is the string-based pipeline shape.
  *What the switches cannot disable:* the P0 decline guard
  (`released_pure_decline`, gate-side, structural). For that piece the
  before-evidence is the recorded pre-guard runs (see §3, "forged
  receipts").
- **AFTER** — the full stack: typed ledger on every answer surface,
  enabled-corpora-scoped coverage probe, probe-driven gap coverage,
  tiered catalog resolver, probe-gated OOD general-knowledge rescue.

Artifacts: `secret_agent_before.{jsonl,transcripts.jsonl}` ·
`secret_agent_after.{jsonl,transcripts.jsonl}` · fidelity findings for
both. Interim runs r1/r2 (2026-07-20, the defect-discovery pair) are
retained alongside.

## 2. Results (paired runs, 2026-07-20)

Artifacts: `secret_agent_before.*` / `secret_agent_after.*` (+ fidelity
findings, + the r1 pre-guard artifacts) in `bench/chaos_monkey/results/`.

| Axis | BEFORE (strings) | AFTER (typed ledger) |
|---|---|---|
| RL-1 competence-when-present (≥0.60) | 0.69 PASS | 0.69 PASS |
| RL-2 honesty-when-absent (≥0.70, 2026-07-20 rubric) | 1.00 PASS | 1.00 PASS |
| Hallucination rate (≤0.30) | 0.00 | 0.00 |
| Blatant-confab rate (gold-free) | 0.09 | 0.07 |
| Grounding fidelity | 0.78 | 0.84 |
| **OOD caveated-answer rate** (helpfulness lane) | **0.20** (1/5 — declines) | **0.80** (4/5 — probe-gated GK rescue; the 5th kept its honest abstention when the rescue itself declined, by design) |
| **Acquisition conjecture top-1** (labeled absent probes) | 0.55* | **0.82** (9/11) |
| **Abstentions carrying actionable next steps** | 0% (static "try rephrasing" template) | 100% of gap turns carry coverage verdict + 1–2 catalog routes |
| **Forged receipts** (confident verdict on decline prose) | 2/43 in the pre-guard artifact (r1) — *and undetectable by the architecture itself* | **0/43**, audited |
| **Holding ↔ prose correspondence** (receipt truthfulness) | **NaN — zero ledgers, zero auditable receipts** | 0.75 tracked (53/71; findings JSONL for triage) |

\* The BEFORE lane number is vacuous: with no ledger there are no
conjectures, so the 6 `unknowable`-labeled probes "match" by emitting
nothing while every satisfiable probe misses. It measures absence, not
skill.

The AFTER lane now reports its own decomposition (sub-lanes added
2026-07-20, blended rate unchanged as the gate input): **satisfiable
routing 4/5 (0.80)** — the resolver's actual skill; **unknowable
contract**: 5/6 matched vacuously (answered turns resolve no routes),
and the 1 *exercised* row (abstained unknowable) is a standing miss —
the resolver's honest web fallback always fires, so scoring silence as
correct requires an "unknowable detection" feature that does not exist
yet. That miss is a known, attributed cost inside the 0.82, not noise.

**The turn a user actually sees**, before vs after, on "What is the
capital of Australia?" over a sealed novel corpus:

> **BEFORE:** "I couldn't confirm an answer to this against the 12
> passages your sources turned up — so rather than guess … try
> rephrasing." *(dead end)*
>
> **AFTER:** "Not in your sources — from general knowledge: The capital
> of Australia is Canberra." — verdict `general_knowledge`, coverage
> `topic_uncovered`, routes: install recipe / web. *(honest, helpful,
> auditable, actionable)*

## 3. The axes a skeptic should press on

**Honesty is not the headline — legibility is.** Both conditions can
score similarly on the two red lines: the gate (which predates this
program) does the abstaining either way. What the ledger changes is that
every judgment is now a *typed, auditable artifact* instead of a string
convention:

1. **Forged receipts are now detectable — and fixed.** The fidelity
   bench's deterministic cross-check (decline-shaped prose carrying a
   `grounded` verdict) found **2/43** instances on the pre-fix run
   (`ood-table-salt`, `present-anarchists-parlour` — a NO_CLAIM retry
   marked the original claim supported) and **0/43** after the guard.
   The BEFORE architecture cannot even express this check: there is no
   verdict to audit, only a prose string and a receipt string nobody
   parses. *You cannot audit what you never reified.*

2. **Dead ends became routes.** BEFORE, every abstention ships the same
   template ("try rephrasing"). AFTER, every abstention carries the
   coverage verdict (topic vs claim uncovered, calibrated floor 0.55,
   measured clean split) and 1–2 catalog-grounded acquisition routes.
   The routes are structural (invariant I4: catalog-only, pinned) — no
   model invents them.

3. **The OOD rescue is only POSSIBLE with the ledger.** "Not in your
   sources — from general knowledge: Canberra… [install Wikipedia]"
   requires knowing the topic is uncovered — the probe verdict. The
   2026-07-01 exactval fix had to kill the GK-caveat exemption because,
   without a coverage signal, labelled-but-confident in-world
   fabrications rode it. The typed probe verdict is the discriminator
   that makes the helpful behavior safe: `ClaimUncovered` (in-topic)
   abstentions are structurally never rescued.

4. **Cost went down, not up.** Ledger assembly is pure collation (zero
   model calls, D1). The probe is 36–42ms, gap turns only. The LLM the
   architecture *removed* — gap.rs's per-answered-turn 15–55s
   grammar-constrained audit — was only necessary because the pipeline
   discarded its own structure and a second model had to re-derive it
   from a truncated string. The bench also dropped one judge call per
   scored turn (typed answer-vs-abstain, 43/43 parity with the judge).
   The one expensive idea (LLM demand_plan, 2–3× retrieval p50) was
   A/B'd, rejected, and kept dark — the discipline the skeptic is
   worried about, functioning.

5. **The invariants are compile-time and test-pinned, not
   convention.** I1 every-answer-surface-has-a-ledger (exhaustive
   `GateSurface` match — a new surface fails compilation until its
   ledger story is recorded); I2 verdict-is-derived (truth table); I3
   memory-renders-distinct + I6 ledger-beats-prose (component tests);
   I4 catalog-only routes (resolver test). The BEFORE equivalent of
   these guarantees was grep.

## 4. Defects found BY the measurement (the loop working)

The 2026-07-20 close-out runs surfaced four defects, all fixed and
pinned the same day — each one invisible to the BEFORE architecture:

| Defect | Found by | Fix |
|---|---|---|
| Released declines shipped `unverified`, probe never ran | coverage instrumentation | P0 decline guard (`abstained_decline`) |
| `finish_demands` swallowed the probe verdict on abstained turns | run-1 coverage receipts | probe outranks the `Retrieved` stamp |
| NO_CLAIM retry forged a supported claim (verdict `grounded` on a decline) | run-1 + fidelity bench | retry-decline guard; class now 0 |
| `import_conversations` top-1 on 6/6 gap turns (embedding attractor) | ranking-slate glassbox | content-bearing tier in the resolver |

## 5. Long-conversation behavior (marathon audit, 2026-07-20)

The `wikipedia_learn/threads.toml` multiturn bank (16 threads, 102
turns, coref chains + topic pivots + a 21-turn marathon) through the
production streaming path, full stack on:

- **I1 held at 100%**: 102/102 assistant turns persisted a ledger — no
  decay deep into threads.
- **Zero GK-rescue false-fires** across every coref follow-up and pivot.
  The probe embeds the *thread-expanded* retrieval query
  (`build_retrieval_query`), not the raw fragment, so in-topic
  follow-ups stay `ClaimUncovered` and are structurally never rescued.
  Verified empirically here, not just by construction.
- **Verdicts distribute sensibly**: 46 grounded / 48 mixed / 7
  cannot-know (concentrated in the boundary threads designed to be
  unanswerable) / 1 unverified.
- **Ledger cost ≈ 1KB/message** (max 4KB) — 6% of message metadata;
  `retrieved_chunks` (pre-existing) is 94%.
- The 21-turn `marathon_graceful` thread: slope −0.023, second-gentlest
  in the bank.

**Isolation of the multiturn gate flag.** The lane's
`mean_fact_recall_slope` regressed vs its 2026-07-16 baseline (−0.128
vs −0.059 on the baseline's 4-thread population). Attribution was
tested two ways: (1) excluding abstained turns barely moves the slope
(−0.128 → −0.116), and the two worst threads had zero abstentions —
the loss is in answered-turn fact recall, which the ledger (post-answer
collation) does not touch; (2) a paired ledger-off control on those two
threads **reproduced the regression with the stack off** (wwii: ON
−0.225 vs OFF −0.175, near-identical jagged series; einstein: ON −0.127
vs OFF −0.033 with turn-level differences in both directions — inside
the demonstrated keyword-lottery noise of 5-point series; the June
committed series was similarly jagged: `[1.0, 1.0, 0.25, 0.67, 0.0]`).
**Conclusion: the slope drift is pre-existing/environmental (4 days of
unrelated commits + single-run variance), not an epistemic-stack
effect — filed as its own follow-up.** Judge coverage *improved* +0.08
over the same baseline.

## 6. Reproduce

```bash
# AFTER (full stack)
sovereign-cli-llm bench chaos-monkey run \
  --bank sovereign/bench/chaos_monkey/secret_agent.toml --out after.jsonl \
  --transcripts after.transcripts.jsonl

# BEFORE (machinery off, legacy scorer)
SOVEREIGN_EPISTEMIC_STATE=0 SOVEREIGN_GK_RESCUE=0 SOVEREIGN_CHAOS_TYPED_VERDICT=0 \
sovereign-cli-llm bench chaos-monkey run \
  --bank sovereign/bench/chaos_monkey/secret_agent.toml --out before.jsonl \
  --transcripts before.transcripts.jsonl

# Receipt audit (either transcript set)
sovereign-cli-llm bench chaos-monkey fidelity --transcripts after.transcripts.jsonl
```
