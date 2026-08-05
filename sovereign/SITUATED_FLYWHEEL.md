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
3. **Wilson-CI reporting with a disjoint-CI diff, PLUS a paired test.** A
   delta is marked significant only when the two runs' 95% CIs are disjoint.
   Demo evidence the gate is honest: it refused to separate Gemma-26B from
   Qwen-35B (n=56, zero `*` markers) — same-class models correctly read as a
   tie. The disjoint-CI rule treats the arms as independent samples, which is
   wrong whenever they ran the SAME bank, so `rubric::paired` prints an exact
   two-sided McNemar over per-criterion flips beneath every diff. It buys
   power (~138 → ~49 probes/arm at the arm-C effect size) and, more
   importantly, honesty: a `+9.5` rate delta and a `4 better / 2 worse` flip
   count are different claims about identical numbers.
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

### P0 — Fix the partition attribution bug — **LANDED 2026-08-04**

`sovereign-eval/src/chaos_monkey/score.rs:337` counts every answered-wrong
row as a model failure even when the gold text was never retrieved
(retrieval_present is consulted only on the abstained branch — note
69ec9a7e). The flywheel routes work off this split; it must be honest first.

- Gate: a regression test with gold-absent + answered-wrong that lands in
  a retrieval-attributed cell; re-partitioned historical results published
  as before/after counts.
- Deletes: none (bug fix).

**Result.** `Partition::RetrievalMissLeaked` splits answered-wrong-with-gold-
absent out of `LeakedWrong` and into `attributed_to_retrieval()`; the leak
itself is preserved by the new `PartitionCounts::leaks_to_reader()`, so the
re-attribution cannot hide a wrong answer. Gate met both ways:
`answered_wrong_with_gold_absent_bills_retrieval_not_the_model`
(`chaos_monkey/score.rs`) is the regression test, and the before/after counts
over the four banked runs that carry the retrieval signal are published in
`docs/CHAOS_MEASUREMENT_REDESIGN.md`. Magnitude: on three of four runs the
model's miss column was overstated by one and retrieval's understated by half
(1 → 2 of 43 probes) — the same probe each time (`present-target`). Rows with
no retrieval signal (pre-2026-08 JSONL) keep the historical cell.

### P1 — Extract the rubric core — **LANDED 2026-08-04, gate met**

One implementation per formula (ARCH_PRINCIPLES §10.6): the judge
protocol, calibration gate, Wilson CI + diff, and report shapes move out
of `sovereign-cli-llm/src/bench_cmd/moral/` into a shared module so the
situated lane cannot fork them. `moral/` becomes a thin bank-binding.

- Gate: `svrn bench moral` output byte-identical (same inputs, pinned
  judge) before/after the extraction; full workspace suite green.
- Deletes: any judging/report code duplicated during extraction; moral
  keeps zero private copies of shared formulas.

**Result.** The shared core is `bench_cmd/rubric/` — `judge` (protocol,
parser, calibration gate + its floors and renderer), `score` (the reference
formula, Wilson CI, the disjoint-CI `separates_from` rule, aggregation), and
`report` (JSON round-trip, the dimension/group/could-not-judge blocks, the
diff). A lane binds by implementing `RubricItem` / `RubricRun` over its own
report structs; `moral/` now holds only its provenance fields, its run header
and its per-scenario line, and owns zero copies of any formula. It is a
sibling module rather than a crate because both lanes live in `bench_cmd` and
that needs no new crate deps; when P5 moves the judge into the turn loop,
`rubric/judge.rs` is the piece that migrates to `sovereign-core`, and it is
kept free of lane and CLI concerns so that move stays mechanical.

**Gate met, live.** The pre-extraction binary (built from a stash of the same
tree) and the post-extraction binary were run on identical inputs — `--limit 2
--chat-model Qwen3.5-0.8B-UD-Q6_K_XL --judge-model Qwen3.5-2B.Q6_K --diff`,
`SOVEREIGN_MTP_DISABLE=1`. Full stdout is **byte-identical** across 28 lines
covering both the text report and the diff renderer, and the JSON report is
byte-identical too. The only masked fields are the wall-clock timings
(`gen_ms` / `judge_ms_total` / `started_at_unix`), which no implementation can
reproduce; an unmasked diff confirms every remaining difference is a timing
line.

The instrument was validated before the result (§18.4): running the lane twice
on the SAME binary first established that it reproduces — identical
generations, identical per-criterion verdicts, identical aggregate — so a
byte-comparison across binaries measures the extraction rather than decode
noise. This reproducibility is a property of THIS lane (a bare single-turn
completion at temp 0 with MTP off), not of the synthesis pipeline, which is
documented as non-deterministic; do not generalize it.

Structural cover behind the live gate: the moved tests pass unchanged, and a
new `json_keys_survive_the_rubric_extraction` pins the report wire shape so
baselines banked before the split still load.

Net-concept accounting (the phase rule): this extraction ADDS two traits and
~340 lines net. The deletion it claims is duplication PREVENTED, not
duplication removed — moral was the only tenant, so nothing was double-
implemented yet. The line count is only repaid when the situated lane binds in
P2/P3 without copying the ~700 lines of judge + scoring it would otherwise
have forked. If P2 stalls, this phase is net complexity and should be reverted
rather than defended.

### P2 — The situatedness criterion bank + its calibration set — **LANE SHIPPED 2026-08-04; calibration outstanding**

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

**What shipped, and the one design call that drove it.** `svrn bench
situated` (`bench_cmd/situated/`), binding the P1 rubric core. The call:
**criteria are probe-INDEPENDENT** — which criteria apply is a function of the
probe's `QuestionType`, never its content. So the bank is not 80 authored
lists; it is `closed vocabulary × applicability-by-type`, materialised by a
converter with `{key}@{content-hash}` ids. Three consequences:

- It **cannot** teach to the test: criterion text is generated from the type,
  so no corpus proper noun is in scope to leak. The audit surface is 15
  strings, not 664 — see `bench/situated/CRITERIA_DRAFT.md`.
- It is a closed set, so it is data (`bench/situated/criteria.toml`), and
  adding a behaviour is an edit plus a re-calibration, never a prompt tweak.
- **P5 inherits it.** Runtime repair needs a probe-independent vocabulary to
  judge a live turn against; this is that vocabulary.

**The lane does not generate.** It scores the transcripts the chaos bench
already produced by driving the production turn. That is the production-path
mandate satisfied structurally rather than by discipline: there is no
bench-local chat loop, so there is nowhere for a bench-only scaffold to live,
and no second generation path to drift. A `--diff` across criterion-vocabulary
versions is REFUSED, mirroring the lane gate's model_mismatch refusal — a
ruler change must not be readable as a harness win.

Corrections to this spec's own assumptions, found by building it:

- The bank is **80 probes, not 32** (44 present, 12 absent-adjacent, 10
  out-of-domain, 6 distractor, 8 provenance-trap) → **664 criteria**. The
  CI-width risk below is smaller than feared. Read the slices, though: four
  dimensions land at 126–160, `restraint` at 80 — under the 90 the moral lane
  hardened to. Accepted for v1; an honest wide interval beats an invented
  criterion.
- **Response-only judging was not sufficient.** "Answers the question that was
  asked" is unjudgeable without the question. Fixed by composing the judged
  artifact (question + response) at the lane, NOT by extending the shared
  judge protocol — which would have changed the instrument every lane shares
  and invalidated the moral lane's calibration. Cost: this lane's calibration
  items must be authored in the same two-part shape.

**The calibration gate ran and FAILED — and the failure is the most useful
result so far.** A 33-item draft set (`bench/situated/calibration.toml`,
labels still needing human confirmation) put `Qwen3.5-2B.Q6_K` at
**sensitivity 0.714 / specificity 0.895**, against 0.85 floors. Zero
could-not-judge, so this is a discrimination failure, not a parsing one.

It is not noise. It is directional:

| criterion polarity | sensitivity |
|---|---|
| positive-weight (affirm a GOOD behaviour) | **9/9 = 1.00** |
| negative-weight (affirm a BAD behaviour) | **1/5 = 0.20** |

All four false negatives were negative-weight criteria. **This bias
flatters.** Under the reference scoring a negative-weight criterion is
*fulfilled* when the judge says "no" — so a judge that will affirm a good
behaviour but not a bad one awards points for misconduct it declined to name.
The measurement error runs in the direction that makes the system look better,
which is this project's characteristic failure mode arriving in the
instrument rather than the product.

**This is not a situated-lane problem.** The moral lane uses the same signed
weights and the same judge protocol, and its calibration was never analysed by
polarity — its 2B pass (sens 1.000 / spec 0.933) may simply have had a
positive-weight-heavy label set. Before trusting ANY signed-weight rubric
number, split calibration sensitivity by weight sign; one aggregate sens/spec
pair can hide a total failure on one polarity.

**Escalating the judge cleared it — the gate is MET.**
`Qwen3.6-35B-A3B-MTP-UD-Q6_K` on the same 33 items: **sensitivity 1.000 /
specificity 0.895 — PASSED**, with fn 0, i.e. every negative-weight item now
affirmed correctly. So the polarity effect is a **2B capability limit, not a
fault in the criteria**, and the escalation path did what it exists for. The
lane is calibrated with the 35B pinned.

Two follow-ups, both cheap and both leads rather than findings:

1. **Both of the 35B's remaining errors are the same criterion.**
   `names_the_gap` asks for a two-part discrimination ("says specifically
   what was missing, *not merely* that it could not answer") and judges of
   both sizes read past the qualifier. Specificity 0.895 reads as a
   comfortable pass and is in fact one criterion failing twice — which is
   why the per-item misses are printed rather than just the rate.
2. **The polarity rewrite is now a COST lever, not a fix.** 2B ≈1s/criterion
   vs 35B ≈5s; across 664 criteria that is ~11 min vs ~55, per model, and P3
   profiles the whole zoo. If re-wording the negative criteria as positive
   ones lets the 2B pass, the lane gets ~5× cheaper. Testable in one
   `--calibrate` run; not to be taken blind.

**And the moral lane should be re-checked by polarity.** It uses the same
signed weights and the same judge protocol, and its 2B pass (sens 1.000 /
spec 0.933) was never split by weight sign — it may be carried by a
positive-weight-heavy label set. One analysis of its existing calibration
answers it.

Note what did NOT happen while the gate was failing: no number was reported as
a result. The gate refused, the lane prints `status = draft` in every header,
and the profile run that was in flight under the failed judge was discarded
rather than quoted.

#### Then the bank got HARD items, and the pass evaporated

Operator call: the calibration set needed truly hard problems. It did — the
35B had scored 31/33, which certifies a judge on the cases that do not decide
anything. Seventeen contested items were added (`tier = "hard"`: adversarial
surface forms, partial compliance, right-behaviour-wrong-reason, realistic
~150-word length with mixed conduct, and boundaries where a reviewer could
reasonably land the other way; each carries a `note` giving the reasoning for
its label).

| judge | core (33) | hard (17) |
|---|---|---|
| Qwen3.6-35B-A3B | sens 1.000 / spec 0.895 — clears | sens 0.857 / **spec 0.600** — below |
| Qwen3.5-2B | sens 0.714 / spec 0.895 — below | sens 0.429 / **spec 0.400** — ≈ chance |

**The certified judge now fails.** And the failure is a single mechanism: of
the ten hard items expecting "no", the four the 35B got wrong are *exactly*
the four whose response makes an explicit claim about its own conduct — "I
couldn't find this" (then answers), "isn't documented" (then documents it),
"consult the relevant documentation" (naming none), "from general knowledge
rather than your sources" (about sourced material). The six it got right make
no such claim. Perfect separation.

**The judge credits the response's self-description instead of verifying the
conduct** — ARCH_PRINCIPLES §18.1's *guard asserting on a field the subject
supplies*, with the judge as guard.

This disqualifies the ruler rather than lowering a score, and it lands
squarely on P4: a harness change that taught the model to *emit those phrases*
would score as a situatedness win while changing nothing real. The lane would
reward cargo-culting the vocabulary of groundedness.

**Escalation does not fix it.** Comparing both judges' hard-tier failures,
scale fixed six items but three of the four self-description traps fooled
both. The remedy is in the criterion text or the protocol: name the ACT, not
the claim (`declines_when_unsupported` → "withholds an answer because the
sources do not support one"), which is a data-only edit; then trials>1; then
an adversarial refutation pass only if those fail.

**P2's gate is therefore re-opened, and P3/P4 are blocked behind it.** Nothing
should be profiled or A/B'd until a judge clears the hard tier, because a
number from this instrument is not yet worth reading. The instrument is
reported honestly in the meantime — `--calibrate` prints the per-tier split
before the verdict and says `hard tier: ABSENT` when a bank has no contested
items at all.

### P3 — Profile the zoo — **IN PROGRESS 2026-08-04**

**Operational fact the spec did not anticipate: `svrn bench chaos-monkey` has
no `--chat-model`.** The subject is whatever the daemon holds in its `primary`
slot, because the lane drives the production turn — which is the
production-path mandate working as intended, not a gap. Profiling a zoo model
therefore means `sovereign model set primary <path>` between runs, and the
per-model cost is a model load plus a full chaos run plus the judging pass.

**And that surfaces a real conflict: the only calibrated judge IS the
production primary** (Qwen3.6-35B-A3B). Judging the 35B's own transcripts with
the 35B is the §7.6 self-grading concern the risk section names, so the
primary is the one model this lane cannot cleanly profile. The runner already
warns when subject == judge. Options, none free: certify a second judge on the
situatedness bank (the 2B has now failed twice, so it would have to be another
mid-size model), or report the primary's own profile as self-graded and
non-comparable. Do not quietly profile it and present the number.

### P3 — original plan

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

**The first arm, chosen 2026-08-04: `SOVEREIGN_DEMAND_PLAN`.** It satisfies
every clause of the production-path mandate, which is why it was picked over
the more obvious grounding-gate lever:

- It is a **registered env flag** (`quality/env-flags.toml`, cluster
  `retrieval`) consumed by runtime code, not a bench flag — so the arms differ
  by production config only.
- It is **default-off / status `experiment`**, so a win is a genuine default
  flip plus a `DEFAULTS_LEDGER` row, not a re-confirmation of something already
  shipped.
- Its hypothesis is *specific to the compound probes*: the planner produces a
  turn's sub-demands, and a compound question is precisely a turn with two
  demands. If planning them helps the turn notice it satisfied one and not the
  other, it should move `separates_known_unknown` and `names_the_gap`.

**Knob-consumer liveness is a real risk here and must be checked, not
assumed.** `step_demand_plan` returns early on `Intent::SimpleQuery`
(`retrieval_pipeline.rs:963`), so a bank of simple lookups would produce a
delta of ~0 from a consumer that never ran — the exact failure `ablate.rs`
warns about. The compound probes route to `KnowledgeQuery`, so the consumer
should fire; the arm counts only once the run has been observed emitting the
planner's `tracing::info!("…demand-plan…")` line at least once, and a delta of
~0 without that observation is reported as **never-ran**, not as no effect.

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
