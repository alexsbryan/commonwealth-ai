# The Situated Flywheel

Spec, 2026-08-04. Status: PROPOSED — no phase is funded until the operator
signs off. Companion surfaces: `bench/moral/` (the apparatus this reuses),
`bench/chaos_monkey/` (the bank it extends), `landing/situated-agent.html`
(the thesis it operationalizes).

## Objective

Turn the moral-reasoning apparatus (calibrated cheap judging, per-criterion
process rubrics, significance-gated comparison) into a closed improvement
loop for situated behavior — grounding, gap-naming, abstention quality —
so every model in the local zoo gets measurably better at being a situated
agent, through harness changes proven by CIs rather than vibes.

**Done when:** one full flywheel revolution is on record: a situatedness
criterion bank graded a zoo model, a harness change motivated by a failing
criterion moved that criterion's CI to disjoint-better on a fixed-model
A/B run through the production synthesis path, the change survived the
net-simplification test, and the winning configuration is the production
default — so there is no bench-only victory to declare.

**Not worth continuing if:** Phase 2 shows no affordable judge passes
calibration on situatedness criteria (the criteria are then not binary
enough to judge cheaply), or Phase 4's first three harness A/Bs all land
inside overlapping CIs (harness levers are too weak to measure at this
bank size, same verdict the moral demo returned for same-class models).

## The claim

The moral lane (2026-08-04, note d6c1f386 lineage) proved four instruments
that are generic, with moral reasoning as their first tenant:

1. **Per-criterion binary judging with signed weights and evidence
   quotes.** Grades the reasoning path, not just the outcome. A lucky
   ungrounded answer can score below a well-grounded abstention — the
   situated-agent value system as arithmetic.
2. **The judge calibration gate.** ~30 hand-labeled items, sens/spec
   >= 0.85 floors, could-not-judge first-class (never defaulted). Measured:
   Qwen3.5-2B.Q6_K passes at sens 1.000 / spec 0.933 on moral criteria,
   100% trial-unanimity, ~1s/criterion in the fast slot.
3. **Wilson-CI reporting with a disjoint-CI diff.** A delta is marked
   significant only when the two runs' 95% CIs are disjoint. Demo evidence
   the gate is honest: it refused to separate Gemma-26B from Qwen-35B
   (n=56, zero `*` markers) — same-class models correctly read as a tie.
4. **The deterministic bank format.** Content-hash ids, converter-owned
   criteria text, regenerable byte-for-byte.

The chaos banks grade situated *outcomes* (answered/abstained/leaked, with
a retrieval-vs-model partition). This spec adds the *process* layer: which
situated behavior failed, per model, per probe — which is the layer a
harness can act on.

## The flywheel

```
        situate (retrieve, frame, scope)
              |
              v
        generate (zoo model under harness)
              |
              v
        judge per criterion (calibrated 2B, evidence quotes)
              |
     failed criteria route three ways
     /            |               \
    v             v                v
runtime repair  scaffold A/B    training pairs
(this turn,     (same criterion  (pass/fail transcripts
 dark, P5)      fails broadly →   on same situation →
                CI-gated harness  DPO/SFT for the zoo,
                change, P4)       P6)
```

Failure routing is gated by the chaos partition: retrieval-absent misses
go to the retrieval backlog, not the scaffold. Pre-requisite: fix the
`partition_cell()` attribution bug (P0) before trusting that split.

## Production-path mandate

The known failure mode this spec must structurally prevent: build an
immaculate measurement framework, then localize all the tuning to the
bench tool while the production end-user system never changes.

The house already has the seam and the cautionary comment. `eval run
--prod-pipeline` drives the production `Runtime::retrieve_evidence`
in-process but skips synthesis; `--synth` runs the full production turn
(routing → retrieval → synthesis → grounding gate). And `ablate.rs`
warns that picking the wrong mode is silent — the arm completes and
reports a delta of ~0 because the knob's consumer never ran.

Every situatedness criterion is answering-side (gap-naming, abstention
quality, citation behavior all happen during synthesis). Therefore:

1. **The lane's default and only comparable mode is the production
   synthesis path** — the same in-process turn loop chat users hit,
   grounding gate included. There is no bench-local chat loop, so there
   is no bench-local place for a scaffold to live. If a fast
   retrieval-only mode is ever added for iteration, its reports are
   labeled non-comparable and the diff refuses to compare across modes
   (same posture as the lane gate's model_mismatch refusal).
2. **P4 harness levers are production knobs, not bench flags.** Every
   arm variable must be a config key or registered env flag consumed by
   runtime code (`quality/env-flags.toml` — a new env read fails
   `cargo xtask env-gate` undeclared). The A/B arms differ by prod
   config only; a winning arm is therefore already a prod change — the
   promotion is flipping the default plus its DEFAULTS_LEDGER row, not
   a port from bench code to runtime code.
3. **Every report header names the pipeline mode and the prod-config
   fingerprint of its arm** (the resolved values of the knobs under
   test), so a report can never silently be mistaken for a different
   mode or configuration.
4. **A knob-consumer liveness check**: before an A/B arm counts, the
   run must observe (via tracing) that the knob's consumer executed at
   least once. A delta of ~0 with a consumer that never ran is reported
   as `never-ran`, not as "no effect" — the four-verdict rule
   (ARCH_PRINCIPLES §18.1) applied to bench arms.

## Phases

Each phase has a falsifiable gate and a Deletes ledger. A phase that adds
concepts without retiring any must say why in its gate review.

### P0 — Fix the partition attribution bug

`sovereign-eval/src/chaos_monkey/score.rs:337` counts every answered-wrong
row as a model failure even when the gold text was never retrieved
(retrieval_present is consulted only on the abstained branch — note
69ec9a7e). The flywheel routes work off this split; it must be honest first.

- Gate: a regression test with gold-absent + answered-wrong that lands in
  a retrieval-attributed cell; re-partitioned historical results published
  as before/after counts.
- Deletes: none (bug fix).

### P1 — Extract the rubric core

One implementation per formula (ARCH_PRINCIPLES §10.6): the judge
protocol, calibration gate, Wilson CI + diff, and report shapes move out
of `sovereign-cli-llm/src/bench_cmd/moral/` into a shared module so the
situated lane cannot fork them. `moral/` becomes a thin bank-binding.

- Gate: `svrn bench moral` output byte-identical (same inputs, pinned
  judge) before/after the extraction; full workspace suite green.
- Deletes: any judging/report code duplicated during extraction; moral
  keeps zero private copies of shared formulas.

### P2 — The situatedness criterion bank + its calibration set

Author criteria over the existing 32-probe chaos answerable bank plus its
unanswerable arm — no new probe corpus. Per probe, ~5-10 criteria drawn
from a small closed vocabulary of situated behaviors (grounding citation,
gap-naming before abstention, no outside knowledge imported, actionable
abstention, harmless refusal framing), signed weights, converter-generated
with content-hash ids.

Calibration does NOT transfer across criterion families: the moral
30-item set certifies nothing here. Hand-label a fresh ~30-item set
(balanced yes/no, including deliberate near-misses) and re-gate the judge.

- Gate: a judge from the zoo passes sens/spec >= 0.85 on the situatedness
  calibration set. If the 2B fails, escalate through the zoo until one
  passes or the stop-condition fires.
- Deletes: any ad-hoc per-probe grading heuristics in chaos scoring that
  the criterion bank now covers (enumerate at implementation time; the
  book-report fabricated-quote regex overcounting, note 24ffcb96, is a
  known candidate for retirement into a judged criterion).

### P3 — Profile the zoo

One bank run per zoo model, judge pinned, reports banked. Output: a
per-model, per-dimension situatedness profile table (fulfillment + CI),
the situated analogue of the moral demo's advisor/agent gap finding.

- Gate: profiles for every resident-viable zoo model; at least one
  routing decision (which model holds which slot for which workload)
  changed or explicitly re-confirmed by the numbers.
- Deletes: any prior vibes-based model-role notes the table supersedes.

### P4 — The harness A/B loop

The tuner. Hold the model fixed (the lane gate's model_mismatch refusal,
note 69a355e7, already enforces this), change exactly one harness variable
per iteration (note ca6d0a16's change hierarchy: prompt-level first),
run the bank, read the diff. Disjoint-CI improvement → keep; overlap →
revert (net-simplification rule). All arms run the production synthesis
path and vary production knobs only (see Production-path mandate); the
winning arm is promoted by flipping the prod default, never by porting
bench code.

- Gate: the done-when revolution — one criterion moved to
  disjoint-better by a harness change, on record with both reports, with
  the winning configuration live in the production default (ledger row
  moved) — not merely demonstrated in the bench.
- Deletes: every harness lever that A/B'd to noise gets removed, not
  left behind a flag.

### P5 — Runtime criterion repair (dark)

The calibrated judge moves into the turn loop: after generation, judge
the response against the probe-independent criterion vocabulary; a failed
criterion becomes one targeted revision instruction ("assertion without
supporting quote — quote or retract"), one repair pass, re-judge.

Ships dark behind a flag with a DEFAULTS_LEDGER row in the same commit.
Flip condition: repair lifts bank fulfillment with disjoint CIs at a
measured TTFT cost the operator accepts (grounding-gate history — note
lineage of the 35-call/turn fan-out — says per-claim hot-path work is
guilty until measured). Review-by: 30 days after landing.

- Gate: the ledger row's flip condition, measured on the bank.
- Deletes: if repair subsumes an existing grounding-gate check, the older
  check retires; two gates asserting one invariant is a §10.6 smell.

### P6 — Training pairs (deferred, unfunded)

Criterion-graded transcripts are natural preference pairs (same situation,
passing vs failing response). Emit them as a byproduct of P3/P4 runs in a
documented format; do not build a training pipeline yet. Funding this
phase requires its own impact estimate.

## Risks and open questions

- **CI width vs bank size.** 32 probes x ~8 criteria ≈ 250-300 criteria
  per dimension-slice at best — thinner than the moral bank's weakest
  dimension after its hardening. If P4 deltas don't separate, grow the
  bank before concluding the lever is weak (the moral lane sized its
  thinnest dimension to 90 for exactly this reason).
- **Judge self-grading.** A zoo model judging its own generations is a
  §7.6 concern. Mitigation: the judge is pinned and never the model under
  test in A/B arms; calibration is against human labels, not model output.
- **Criterion vocabulary drift.** The closed vocabulary is an enum, not
  open text (§2); adding a behavior is a converter + calibration change,
  never an inline prompt edit.

## What the user gets (demo mapping)

- P3: "which local model should hold this role" answered with a table.
- P4: chaos competence moves for a named reason, with the receipt.
- P5 (if flipped): visibly better abstentions — the agent says what is
  missing and what to do next, instead of a bare refusal.
