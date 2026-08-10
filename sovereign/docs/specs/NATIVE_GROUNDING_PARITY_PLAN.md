# Native Grounding — the parity plan

**Status:** plan of record for order `native-grounding-parity-plan`
(directive 0fcca5d3, operator-edited: first-principles mandate),
2026-08-10. This document decides nothing at runtime; it pre-registers
what the next execution orders must prove. `SOVEREIGN_NATIVE_GROUNDING`
stays OFF until the bars in §5 are met. The program design it extends
lives on branch `skunkworks/native-grounding` at
`sovereign/docs/specs/NATIVE_GROUNDING.md` (cited below as "the spec";
that file is not on main — every other citation here resolves on main).

**One sentence:** parity is taken structurally (the flag stops making
decisions), the baseline both arms share is repaired where the failures
actually live (retrieval, routing), and any "better" is earned only at
a decision point where a committed measurement shows headroom — which
today is claim-level honesty inside long answers, not turn-level
admission.

---

## 0. What the end user gets at flip-on

At `SOVEREIGN_NATIVE_GROUNDING=1` under this plan, a user asking their
corpus a question gets:

- **The same answers.** Turn-level honesty and competence are the
  incumbent's, by construction (§4 P1): the native path renders, it does
  not decide. 0.91 honesty-when-absent and 0.74 competence-when-present
  on the dev bank are the floor, not the target.
- **Provenance on every answer.** Each released answer arrives tiled
  into typed segments: sentences found verbatim in the user's sources
  carry a real chunk address ("in your sources, here"); sentences that
  are the model's own carry no badge and say so. This is the one
  committed asset that delivers the goal's "citable either way" clause,
  and it is display, never a verdict — the measured reason it must not
  be a verdict is resolver precision 0.7429 against a 0.98 bar
  (`sovereign/bench/calibration/resolver-precision/FINDINGS.md`).
- **Refusals that are refusals.** The current native abstention reroutes
  dropped evidence into a parametric general-knowledge turn that asserts
  false specifics behind a "Not in your sources" disclaimer the honesty
  classifier accepts (note 0ee9fc42; disclaimer present on 16 of 17 A/B
  failure cases, `sovereign/bench/calibration/step3/FAILURE_DECOMPOSITION.md`
  D3). Under this plan that surface is deleted: an abstain, wherever it
  ever fires, renders as an honest decline.

What flip-on does NOT buy at P1, said plainly: latency. The judge path
is unchanged, because the only measured judge-skip candidate was refused
at 0.7429 vs 0.98. Any latency claim waits for P3's instrument.

## 1. Asset and evidence inventory

### 1.1 Evidence (every number, by path)

| # | Finding | Number | Committed artifact |
|---|---|---|---|
| E1 | H1 offline kill gate: reranker margin beats top_cosine | AUROC 0.8990 vs 0.7994, delta +0.0995, 95% CI [+0.0889, +0.1092], 4,207 pairs; honesty-recall at FA 5%: 0.665 vs 0.235 | `sovereign/bench/calibration/h1-port/FINDINGS.md`, `h1_verdict.json`, `h1_scores.jsonl` |
| E2 | Step 2 A/B: honesty parity, competence collapse | honesty 0.91 / 0.91 / 0.91 (off / on r1 / on r2); competence 0.74 / 0.26 / 0.23; H1 abstained 31 of 33 admitted turns; saltgrass median margin 4.49 vs tau 5.885; ~50x FA-axis shift | `sovereign/bench/calibration/ab/FINDINGS.md`, `ab_verdict.json`; ledger row `sovereign/DEFAULTS_LEDGER.md` §"Native grounding, H1 admission" |
| E3 | Honesty headroom on the dev bank | 1 probe of 11 (incumbent catches 10/11); the 1 miss (`ood-css-center`) routes to CodeQuery and never reaches any admission/judge/caveat surface | `ab/FINDINGS.md`; `step3/attribution.json` routing cases; `step3/failure_corpus.jsonl` case `ab:ood-css-center` |
| E4 | Failure decomposition, 31/31 attributed | admission 15 (48%), retrieval 11 (35%), routing 4 (13%), abstention action 1 (3%); judge 0, resolver 0, synthesis 0, base model 0; flag-off arm answers all 15 admission cases correctly | `step3/FAILURE_DECOMPOSITION.md` D3, `attribution.json`, `failure_corpus.jsonl` |
| E5 | D5 tau recalibration: failed against registered bars | competence 0.65/0.65 vs bar 0.71 (off 0.74); answerable abstains 3/3 vs bar 2; honesty 0.91 every arm; admission deterministic 33/33 | `step3/FAILURE_DECOMPOSITION.md` D5, `d5_verdict.json`; note d6911acb |
| E6 | The margin-interleave mechanism | present wreck-name m=1.19 < absent Widow-Hetch m=1.31; no threshold on the current reranker margin buys more than honesty-parity while risking competence | `step3/FAILURE_DECOMPOSITION.md` §"Why no threshold can win here"; note d6911acb |
| E7 | Resolver precision: judge-skip refused | P(verified given certified) 0.7429, bar 0.98; Verbatim tier 4/130 claims, all 4 incumbent-failed (tier precision 0.000); all 26 true certifications are Fuzzy; Fuzzy = vocabulary overlap, not support | `resolver-precision/FINDINGS.md`, `resolver_precision_verdict.json` |
| E8 | Claim-level negative class exists in longform | 27 of 130 claims judged not-supported by the incumbent across the three frozen longneg/gv-shadow transcripts | `resolver-precision/FINDINGS.md` (inputs table + confusion matrix) |
| E9 | The abstention action is a confabulation surface | evidence drop -> parametric fallback behind an accepted disclaimer; 16/17 A/B cases carry the disclaimer; 1 case (`ab:longneg-fabspec-fraud-figures`) is a literal coin: identical Abstain + margin, pass r1 / fail r2 | note 0ee9fc42; `step3/FAILURE_DECOMPOSITION.md` D3 secondary mechanism |
| E10 | Incumbent-side HARD failures, both arms | retrieval 11 cases (8 fact-loss + 3 source-loss) vs 2026-07-16/17 baselines, byte-equal deltas across three captures; routing 3 HARD cases, lanes at 25/27 and 9/10 in all three captures | `step3/FAILURE_DECOMPOSITION.md` D2 + repeat counts; adjudication `step3/D1_remint_adjudication.md` |
| E11 | Semantic entropy (temperature variant): non-viable | 1 distinct value in 5 samples on every turn, both label classes; garbage above T 1.5 | the spec, Appendix A (skunkworks branch) |
| E12 | Judge cost of the incumbent | ~35 judge calls per gated longform turn — cited, NOT measured (no per-turn counter exists); A/B wall time 21m12s off vs 11m35s on over 42 probes is the abstention skip, not an independent win | `sovereign/DEFAULTS_LEDGER.md:937`; `ab/FINDINGS.md` bars (c),(d) |

### 1.2 Built assets (all on main unless marked)

| Asset | Path | State |
|---|---|---|
| Admission instrument: single calibration accessor, tau-override knobs, deterministic decide | `sovereign/crates/sovereign-core/src/runtime/grounding/native_grounding/admission.rs` (`admit`, `decide_from_margin`, `effective_thresholds`) | dark; knobs ledgered to retire with Step 3 either way |
| Typed contract + legacy shim | `sovereign/crates/sovereign-contracts/src/types/grounding_verdict.rs` (`GroundingVerdict`, `GroundingDecision`, `SegmentKind`, `to_gate_action`) | landed |
| Segments display, wire + CLI render | `sovereign/crates/sovereign-core/src/runtime/grounding/native_grounding/segments.rs` (`segments_for_display`; tests pin tiling, address-or-no-badge, scattered-vocabulary-never-badged) | landed; never yet measured on normally-answered turns (E2 run had 31/33 abstains: 56 segments, 0 grounded) |
| Span resolver | `sovereign/crates/sovereign-core/src/runtime/grounding/native_grounding/span_resolver.rs` | landed; display-only per E7 |
| Early-decline seam (the abstain action) | `sovereign/crates/sovereign-core/src/runtime/handlers/knowledge_query.rs:627-704` (`DeclinedBy::NativeH1`) | the E9 surface lives here |
| A/B + verdict harnesses | `sovereign/bench/calibration/ab/run_ab.sh`, `ab_verdict.py`; `step3/d5_verdict.py`, `fit_percorpus_tau.py` | replayable |
| Failure-corpus replay | `step3/build_failure_corpus.py`, `attribute_failures.py` | regenerate corpus + attribution from the repo alone |
| Calibration artifacts | `h1-port/h1_admission_calibration.json`, operating curves | committed |
| 4B training lane | `research/verifier-v0/` (ORPO pipeline, calibrate/operating-curve/contamination scripts) | research, reusable |
| Frozen longneg transcripts | branch `skunkworks/native-grounding` (sha256 pinned in `resolver_precision_verdict.json`) | replay inputs for P3a |

## 2. The first-principles derivation

### 2.1 The goal, decomposed

The goal: grounded inference the end user can trust — **honest when the
sources are absent, competent when they are present, citable either
way, at latency/cost the product bears.** Any mechanism whatsoever that
serves this goal must supply four things:

- **(A) a presence signal** — some measurement of whether the evidence
  determines the answer, with a calibration whose false-alarm axis
  describes the deployment corpus (E2's 50x shift is what its absence
  looks like);
- **(B) a decision point** — where the signal is consulted;
- **(C) an action** — what happens on "absent", which must not convert
  a decline into a confabulation surface (E9);
- **(D) a truthful rendering** — user-visible provenance that never
  overstates what was verified (E7 is the boundary: display yes,
  verdict no).

These four are exhaustive for this goal: honesty and competence are
decided by (A) consulted at (B) acting via (C); citability is (D);
latency/cost is a property of where (B) sits and how much (A) costs.

### 2.2 The decision-point space (closed set of four)

A decision can only be taken at four places in a turn. This is the
searched space, not a sample — each point is characterized by what
information it has, whether its errors are recoverable, and what the
committed evidence says headroom there is worth.

| Decision point | Information available | Error shape | Measured terrain |
|---|---|---|---|
| **Pre-generation** (admission) | question + retrieved chunks; no answer exists yet | a false abstain destroys a correct answer unrecoverably | E2: -0.48 competence; E5: -0.09 at the best per-corpus fit; E6: the signal's classes interleave |
| **Decode-time** (token selection) | partial answer + both logit streams | soft bias; degrades, never destroys | unmeasured (spec H3/CAD; deferred by design — the expensive experiment) |
| **Post-generation** (judge/verify) | the full asserted answer | recoverable: repair, caveat, or decline after the fact | the incumbent lives here at 0.91/0.74; ~35 calls cited-not-measured (E12) |
| **Display-time** (rendering) | the released answer + evidence | cannot destroy competence; can only fail by over-claiming | segments built + tested; E7 fixes the boundary (address or no badge) |

### 2.3 The headroom law — what any mechanism can still buy here

This is the derivation's load-bearing step, and it is arithmetic over
committed numbers, not judgment:

1. **Turn-level honesty headroom at every in-path decision point is
   zero.** The incumbent catches 10 of 11 absent probes (E2). The 11th
   (`ood-css-center`) exits to CodeQuery before any admission, judge, or
   caveat surface runs (E3) — it is unreachable by ANY mechanism at any
   of the three in-path decision points, with any signal, at any
   threshold. Only a routing repair can reach it.
2. **A gating mechanism can only lose turn-level competence.** All 15
   admission-attributed failures are answers the flag-off arm gets
   right (E4). Realized attempts priced this: -0.48 (E2), -0.09 at the
   strongest in-mechanism tuning (E5). This holds independently of
   signal quality: with zero honesty to buy (point 1), even a perfect
   admission signal nets at best 0.00.
3. **Therefore, on the measured bank, cost floor exceeds benefit
   ceiling for admission-time gating regardless of signal.** E6 (the
   interleave) says the current signal is inadequate; points 1-2 say a
   better signal would still have nothing to win at that seam. Two
   independent dooms — the second is the one that survives a signal
   upgrade.
4. **Where committed measurements show real headroom:**
   - *turn honesty:* 1 probe, via routing repair only (E3) — potential
     dev honesty 0.91 -> 1.00;
   - *turn competence:* retrieval (11 cases) + routing (3 cases), both
     arms, HARD lanes (E10) — repairs lift the shared baseline;
   - *claim-level honesty:* 27/130 not-supported claims inside longform
     answers (E8), plus the disclaimer-accepted confabulation shape (E9)
     that turn-level honesty provably cannot see — this is unpriced
     territory with a real negative class;
   - *citability:* the incumbent renders no per-span provenance; the
     built segments surface (1.2) is unpriced value at zero decision
     risk;
   - *latency/cost:* the incumbent's ~35 calls (E12) — but no
     instrument exists yet to measure per-turn decline latency
     (`ab/FINDINGS.md` bar (c)), so latency claims are unfundable until
     one does.

### 2.4 Candidates placed in the space

Every known candidate, plus the mechanisms the space itself generates,
placed against 2.2/2.3. Placement, not privilege:

| Candidate | (A) signal | (B) point | Placement verdict |
|---|---|---|---|
| **4B head** (train answerability via `research/verifier-v0/`) | trained head | pre-generation | signal upgrade at a seam with 0.00 buyable honesty (2.3.1-3). Not dead as a *signal* — dead as an *admission* signal on this instrument. Retained as P3b's fallback at judge-time, where the information and the headroom are |
| **Drop answerability routing** | none | none | right about the admission gate (2.3.3); overbroad about the program — it forfeits (C) and (D), the two obligations with committed assets and unpriced headroom. Taken as P1's pre-registered kill outcome, not as the opening move |
| **Shell-at-parity recomposition** | n/a (decision-transparent) | display-time only | serves (C)+(D) fully, defers (A)+(B); parity structural, citability delivered. This is P1 |
| **Incumbent-side repairs** (retrieval, routing, abstain action) | n/a | upstream of all points | the only candidates aimed where the measured failures live (E4: 48% of the corpus is native-only; the other 52% minus 3% is shared). This is P2 |
| **H2b evidence-counterfactual** (decode the value with and without evidence; disagreement = parametric) | evidence-dependence delta | post-generation / escalation | the one designed mechanism with a written kill bar and *no data yet* (spec §5 H2b, §7.3 H2 — its gate was never run; Step 2 went to the A/B, Step 3 to tau). Targets exactly E8/E9's failure class, with the answer in hand, at a recoverable point. This is P3b's first signal |
| **Sentence-margin sweep / span certification (H4)** | reranker margin per sentence | post-generation | measured for judge-skip: refused (E7). Measured for display: honest. Stays display |
| **Semantic entropy (temperature)** | sample divergence | post-generation | measured non-viable (E11). Closed |
| **Evidence-tilted decoding (H3/CAD)** | logit contrast | decode-time | unmeasured; the only mechanism that reduces fabrication *during* generation, so it competes for E8's headroom, not admission's zero. Expensive (2x decode FLOPs) with competence risk; stays behind P3's instrument — if P3a prices claim-level headroom high and H2b under-delivers, CAD is next, per the spec's own ordering |
| **A third threshold on the reranker margin** | rerank margin | pre-generation | forbidden by E5/E6 and by note d6911acb's explicit instruction. Closed |

The space was searched: four decision points exhaustively enumerated,
every signal family the program has named or measured placed, and the
two unmeasured mechanisms (H2b, CAD) placed with the committed reason
each waits. A candidate outside this table must name a fifth decision
point or a new signal family to exist.

## 3. Candidate paths

### Path 1 — Recompose at parity, repair the baseline, earn "better" where headroom is measured (recommended)

Three phases, detailed in §4. Predicted deltas: turn honesty 0.91 ->
0.91 (P1, structural) -> 1.00 on dev if P2's routing repair catches
`ood-css-center` (E3); turn competence 0.74 -> 0.74 (P1) -> lifted by
P2's retrieval/routing repairs on the HARD lanes (E10: 25/27 -> 27/27,
9/10 -> 10/10, retrieval lanes re-green); claim-level honesty priced by
P3a from committed transcripts (E8's 27/130 is the raw material), then
improved by P3b/c only if its gate clears. Citability delivered at P1.
Cost: P1 ~1-2 sessions + one A/B (wall-time scale: E2's three arms ran
21m + 2x12m) + HARD lanes; P2 rides already-filed backlog orders
(routing-exemplar-fallthrough 4f21fa5e; retrieval atom-enum family
4f78f9f1, 10a1b08d) — this plan adds re-A/B cost only (~1 session);
P3a ~1 session (offline replay), P3b ~1-2 (offline), P3c ~2-3
(runtime + A/B), each behind its own kill bar. Risk: low at P1
(decision-transparent by construction; kill = Path 3), moderate at P3
(both mechanisms may die — the plan survives that as parity-plus-
citability).

### Path 2 — Train the 4B head now, keep admission-time gating

What it is: replace the signal (E1's reranker margin) with a head
trained for answer-containment via `research/verifier-v0/`, refit, re-
A/B, flip on HARD bars. Predicted delta from committed measurements:
turn honesty +0.00 — the ceiling is 10/11 already caught plus 1
unreachable (E3); turn competence <= 0.74 — gating only subtracts
(2.3.2), and the two priced attempts lost 0.48 and 0.09. Transfer risk
is now measured twice (E2's 50x FA shift; E6's interleave), and the
only claim-mined training volume is the same SEP family the failed
calibration was fitted on (spec §7.1: 4,207 pairs, 1,346 articles,
99.5% SEP). Cost: data minting for answerability labels + training +
calibration + per-family validation + A/B — >= 6 sessions plus GPU
time, the most expensive path. Verdict: dominated — highest cost
against a measured benefit ceiling of zero at its decision point. The
head re-enters at P3b as a judge-time fallback signal, where the
decision point actually has headroom to pay for the training.

### Path 3 — Drop-routing exit

What it is: delete the admission decide path, the flag, and the tau
knobs; the incumbent stands alone. Predicted delta: honesty 0.91,
competence 0.74, HARD lanes untouched — exact parity at zero risk, and
the largest immediate delete. Cost: ~1 session. What it forfeits, with
numbers: the segments surface (1.2) — the only committed asset serving
the goal's "citable either way" clause — and P3's shot at the 27/130
claim-level headroom (E8). The E9 confabulation surface does die with
it (the reroute is deleted along with the path). This is the correct
outcome if P1's bars fail, and it is pre-registered as exactly that
(§4 P1 kill). It is not taken first because P1 purchases the
citability deliverable for 1-2 sessions at structurally-zero decision
risk — if that trade reads as not worth 1-2 sessions, Path 3 is the
honest exit and this plan endorses taking it.

## 4. The recommendation, phase by phase

### P1 — The shell at parity (flag-on becomes decision-transparent)

**Change.** Flag-on comes to mean: (i) admission runs as *telemetry* —
`admit` computes margin and answerability and traces them on every
turn, but no turn's synthesis path may differ from flag-off (the
decline branch at `knowledge_query.rs:682` is not taken); (ii) the
typed verdict + segments ride the wire and render in the CLI; (iii)
the abstain arm of the seam is rewired from the parametric fallback to
the incumbent's honest-decline template (E9's fix — dormant while
nothing abstains, structural so the confabulation surface cannot
return); (iv) `SOVEREIGN_NG_TAU_ABSTAIN`/`_TAU_ANSWER` retire per
their ledger row.

**Falsifiable predictions.** Decision-trace identity with flag-off on
all 42 dev probes, both on-runs; honesty exactly 0.91, competence
exactly 0.74 (identical decisions imply identical red-lines up to
generation noise — E5's off-revalidation reproduced 0.74, so drift
here is signal, not noise); segments now measured on normally-answered
turns for the first time (E2's run had 31/33 abstains and proved
nothing about this surface).

**Promotion bars (pre-registered; parity operationally).**
1. No HARD lane regression vs committed baselines.
2. Dev A/B, both on-runs: honesty >= 0.91 AND competence >= 0.74 (the
   Step 2 flag-off levels).
3. Decision-trace identity: zero flag-on turns whose admit/decline/
   synthesis path differs from the flag-off arm.
4. What the flag must ALSO beat to justify its complexity (the
   citability earn): every `Grounded`-rendered segment on the A/B
   transcripts resolves to a real chunk address (structural — the
   `segments.rs` tests pin it; this bar checks it end-to-end on the
   wire); segment tiling covers every released knowledge answer; and
   zero flag-on turns assert an unverifiable specific behind a source
   disclaimer (E9's blatant-confab audit, run over the A/B
   transcripts).

**Kill bars.** Any decision divergence (bar 3) — monitor leaked into a
decision; or bar 2 missed in either run. Kill outcome: Path 3
(drop-routing exit), executed with its Deletes row, no re-litigation.

### P2 — Repair the shared baseline (composes with, does not own)

**Change.** The already-filed incumbent-side orders: routing exemplars
for the 3 HARD fallthrough probes + caveat discipline for parametric
CodeQuery answers (backlog `routing-exemplar-fallthrough`, 4f21fa5e —
this is the piece that reaches `ood-css-center`, the only turn-honesty
headroom on the bank, E3); retrieval pool drift (atom-enum family,
notes 4f78f9f1 / 10a1b08d, adjudicated in
`step3/D1_remint_adjudication.md`). This plan does not re-own that
work; it binds the flag to it.

**Falsifiable predictions.** Routing lanes 27/27 and 10/10 (from
25/27, 9/10 — E10); retrieval HARD lanes re-green; dev honesty 1.00
(11/11) once CodeQuery answers carry caveat discipline; dev competence
>= 0.74 with upside from the repaired pools.

**Promotion bar for the flag.** After P2 lands, the parity ruler
moves: one re-A/B, and bars 1-4 of P1 re-apply **at the new flag-off
levels** (whatever the repaired incumbent scores). Parity means parity
with the incumbent as it then stands, not as it stood when weakest.

**Kill bars.** None for the flag (decision-transparent throughout);
the backlog orders carry their own bars.

### P3 — Earn "better" only where the instrument shows headroom

**P3a — validate the instrument before the result.** Define the
claim-level honesty metric (unsupported-assertion rate in released
longform answers, incumbent judge verdicts as labels) and measure the
incumbent's number offline from the committed frozen transcripts (E8's
inputs; no new bench runs). Also add the per-turn stage-timing field
the latency story has been missing since `ab/FINDINGS.md` bar (c)
declared it underivable. **Kill bar:** if the incumbent's claim-level
failure rate on those transcripts is < 3% (roughly: the negative class
was an artifact of the longneg harvest, not a live quality hole), the
program ends at P1+P2 — parity plus citability, with a written record
that no further headroom was found. That is a valid end state, not a
failure.

**P3b — the offline gate for the first new signal.** Run H2b's
evidence-counterfactual gate exactly as pre-registered in the spec
(§5 H2b mechanism, §7.3 H2 bars: within 0.05 AUROC of the incumbent
Critic at < 20% of its per-turn judge cost, or better at any cost;
kill if it cannot separate the fabrication cases the Critic catches).
Offline, frozen transcripts, no runtime wiring. **If H2b dies:** the
4B head (Path 2's asset, now at the right decision point) is the
registered fallback signal, with the same bars; if that also dies, CAD
(decode-time) is the remaining unmeasured mechanism, and it inherits
the spec's H3 gate. One signal judged per order; no confounded arms.

**P3c — wire the winner, judge it end-to-end.** Replace the incumbent
single-claim verify escalation with the P3b winner at the
post-generation seam. Bars: claim-level honesty >= the incumbent's
P3a number; turn honesty and competence >= flag-off (both runs); no
HARD regression; latency measured by the P3a instrument, reported as
measured (E12's "~35 calls" remains cited-not-measured until then).
Kill: any bar missed — the incumbent verify path stays, and P3c's
wiring is reverted; the program still stands at P1+P2 parity.

## 5. Parity, operationally (the pre-registered definition)

The flag flips ON as default when and only when, on the then-current
incumbent baseline:

1. **No HARD lane regression** (both build-gate scripts green is
   assumed; this is the bench contract).
2. **Dev-bank A/B at or above the incumbent on the Step 2 bars:**
   honesty >= 0.91 and competence >= 0.74 — raised to the flag-off
   levels current at judging time if P2 has moved them.
3. **The citability earn (what the flag must beat to justify its
   complexity):** P1 bar 4 in full — resolvable addresses on every
   Grounded badge, full tiling, zero disclaimer-confabulations.
4. An execution order that weakens any of these bars must say so to
   the operator in its own draft (order seam, restated).

## 6. Deletes ledger, per phase

| Phase | Deletes | Notes |
|---|---|---|
| P1 | `SOVEREIGN_NG_TAU_ABSTAIN` / `_TAU_ANSWER` + `apply_tau_overrides` + their `quality/env-flags.toml` rows and ledger row (retire clause already written); the parametric-fallback reroute on the Abstain arm (`knowledge_query.rs` E9 branch) replaced by the honest-decline template | net knob count down 2; the confabulation surface structurally gone |
| P2 | owned by the backlog orders' own ledgers | this plan claims none of their deletes |
| P3a (if killed) | the admission decide path (`decide_from_margin` decision arms + the decline branch) — Path 3's trim executed inside the program, keeping segments | the 17-phrase decline zoo does NOT delete here: it belongs to the incumbent path, which remains the decider |
| P3c (if promoted) | the incumbent single-claim verify path (~700 non-test LOC, the spec §9 row); the decline-recognition zoo's retirement becomes fundable only now, when abstention decisions flow typed end-to-end | the big deletes stay tied to an earned gate, never to hope |

## 7. What this plan does not claim

- No latency win is promised before P3a's instrument exists (E12).
- The literary-family H1 row (19 pairs) is not evidence and is not
  used anywhere above (`h1-port/FINDINGS.md`).
- The segments surface has never been measured on normally-answered
  turns; P1's A/B is that first measurement, and bar 4 is written so
  it can fail.
- Nothing here re-litigates the E5/E6 verdicts: no threshold on the
  existing reranker margin appears in any phase.
