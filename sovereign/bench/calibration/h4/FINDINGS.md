# H4 gate — the verdict is COULD-NOT-JUDGE, and that is the finding

> **SUPERSEDED 2026-08-09 (h4-gate-rerun order). The gate has since
> judged.** The blocker this document names — a single-class held-out
> label set — was a property of the banks, and the banks changed. Run a
> third time, still UNCHANGED, against the longform-negative harvest
> (merged at `e18653b0`), it returned **`Survives`**: agreement
> **0.7674** against the 0.90 bar (FAILED), audit p50 **1212 ms**
> against the 2 s bar (BEAT), and **0.7674 against the 0.7955 naive
> always-supported ceiling (FAILED — the mechanism is beaten by a
> scorer that looks at nothing)**. Disagreement 0.2326 stayed under the
> 0.25 kill bar by 0.0174. **The verdict, its numbers and why the
> margins overlap: [`rerun_20260809_longneg/VERDICT.md`](rerun_20260809_longneg/VERDICT.md).**
> Everything below remains accurate as the record of why the first two
> runs could not judge; it is no longer the current verdict.

> **UPDATE 2026-08-08 (longform-telemetry order).** Both telemetry defects named
> in "What would make this measurable" were built, and the gate was re-run
> UNCHANGED on a fresh harvest. **It returned could-not-judge again, for the
> same reason, with numerically identical results.** See
> [The re-run](#the-re-run-2026-08-08b--unchanged-gate-fresh-harvest) at the
> bottom, and read it together with this document: **two claims in "Where the
> negative class actually lives" are wrong, and are corrected in place there.**

**H4 was not proven and was not killed. The measurement §7.3 H4 specifies cannot be
made from the frozen transcripts that exist**, and the reason is structural rather
than a shortage of runs: the incumbent's negative-class claims live almost entirely
on longform turns, and longform turns are exactly the turns whose transcripts carry
no `retrieved_chunks` and no `violation_prob`. Of **17** negative-class claims in the
entire frozen corpus (5 transcripts, 157 turns), **5 are usable** for this gate — and
all 5 sit on a single turn.

The gate was allowed to fail, and it did, twice, in two different ways. Run once with
the calibration split single-class, it refused to invent a floor and exited 1. Run
with a two-class calibration split, it computed the floor, computed the agreement, and
then declined to call it a verdict because the held-out label set had one value in it.
Both refusals are in the code and both are tested.

**The order's stop condition is met verbatim:** *"frozen transcripts lack per-claim
fields the replay needs (name the missing fields — do not re-invoke judges to fill
them)."* The missing fields are named below. No judge was re-invoked.

## What was measured, and against which bar

Primary run — calibration and held-out split both on the dev banks (§7.1's
"Development / held-out" role), so the test bank is not in the verdict.

| §7.3 H4 asks for | Bar | Result |
|---|---|---|
| (a) verdict agreement on high-margin claims | >= 0.90 | **NOT JUDGEABLE** — 0.8696 (20/23) computed, but on a single-class label set |
| (b) per-turn audit wall-time p50 | <= 2000 ms | **877 ms** — met, measured, on 23 turns |
| (c) citation_fidelity / grounding_fidelity deltas | — | **NOT RUN** — needs the chaos-scorer cutover, which this order puts out of scope |

Second frozen source (the Phase-0 `secret_agent` gv-shadow transcript, read-only,
`second_source_secret_agent/`) reaches the same verdict independently: agreement
0.8095 (17/21), label balance 21 supported / 0 not, audit p50 **1930 ms**.

**Bar (b) is the one real result here, and it holds on both sources.** The mechanical
audit costs 877 ms p50 on the dev bank and 1930 ms on the test bank, against a
measured incumbent whole-gated-turn p50 of **24,558 ms** over the same 57 dev turns
(p90 70,477 ms, max 140,425 ms; the gv-shadow Critic call alone is 5,644 ms p50).
Those incumbent numbers are measured, from `saltgrass_gv_shadow_20260808.run.log`,
not cited — but read them as whole-turn times that include retrieval and synthesis,
because the chaos `ResultRow` carries no per-stage timing and the ladder's own share
cannot be isolated from it.

## Why (a) cannot be judged

*Agreement is measured against the incumbent's per-claim verdicts, read out of
`epistemic_state.holdings[].verification`. Both held-out sets contain only the
positive class.*

| Held-out set | high-margin claims | supported | NOT supported |
|---|---|---|---|
| saltgrass (dev) | 23 | 23 | **0** |
| secret_agent (second source) | 21 | 21 | **0** |

Against an all-positive set, "agreement" is a true-positive rate: a scorer that
answers *supported* unconditionally scores 1.000. §7.3 H4's two thresholds — beat at
>=0.90, kill above 25% disagreement — both presume the set can catch over-acceptance
*and* over-rejection. It cannot. Reporting 0.8696 as "agreement with the incumbent"
would be the §18.1 smell: a check with no failing input you can name. The gate says
`could_not_judge` instead, and the reason string is in `h4_verdict.json`.

### Where the negative class actually lives

Census over all five committed chaos transcripts:

| Transcript | turn | neg. claims | chunks | vp | usable? |
|---|---|---|---|---|---|
| secret_agent_gv_shadow | `present-maximal-statepower` | 4 | **0** | null | no |
| secret_agent_gv_shadow | `present-maximal-london` | 1 | **0** | null | no |
| saltgrass_ctl_r1 | `present-maximal-exposure` | 3 | 20 | **null** | no |
| saltgrass_ctl_r1 | `present-maximal-fraud` | 4 | 19 | **null** | no |
| saltgrass_compound_gv_shadow | `compound-killer-and-lugger` | **5** | 12 | 0.000 | **yes** |
| | **total 17** | | | | **5 usable** |

**Every unusable negative is a `present-maximal-*` longform turn.** That is not a
coincidence and it is the whole problem: longform is the class H4 exists to replace —
the ~1,400-LOC `gate_longform` ladder and the ~35 judge calls per gated turn (§2, §9)
are all on that path. The two fields the replay needs are missing there:

- ~~**`retrieved_chunks` is empty** on every longform turn in a gv-shadow run~~ —
  **CORRECTED 2026-08-08: wrong about "every".** A census over all 15 committed
  `*.transcripts.jsonl` finds 13 `rewrite_annotated` turns, of which **11 kept
  their evidence**. Exactly two lost it — `present-maximal-statepower` and
  `present-maximal-london`, both on the frozen `secret_agent` bank. On the DEV
  banks nothing was lost (`present-maximal-fraud` carries 19 chunks), which is
  why this run's own `unreplayable_turns_with_holdings` is `[]`. Length is not
  the predictor; **surface** is — see the re-run section for the actual cause
  (`Intent::ComplexTask`, whose evidence universe is a step-summary transcript
  that is never projected into `retrieved_chunks`).
- ~~**`violation_prob` is null** on every longform turn, because the Critic is only
  consulted on the short path (`chaos_monkey.rs:907`)~~ — **CORRECTED 2026-08-08:
  wrong about the mechanism.** That line has no longform branch at all. Its
  condition is `(grounding_verify || gv_shadow) && !naked && !chunk_texts.is_empty()`.
  `vp` is null exactly when the chunk list is empty, never because a turn was
  long. There is **one** root cause, not two, and the first implies the second:
  restore the evidence and the Critic runs on its own. (The consequence stated
  here — a turn with no vp can never satisfy `|vp - tau| > 0.2` — is correct.)

The gate emits this census itself, per turn, with the missing field named — see
`unreplayable_turns_with_holdings` in `second_source_secret_agent/h4_verdict.json`.

**And there is no split that fixes it.** All 5 usable negatives are on one turn. Any
split putting negatives on both the calibration and the held-out side would have to
divide one turn's claims — same evidence, same answer — across the two, which is
leakage of the most direct kind. The margin bar was not lowered and the band was not
widened; both remain at §7.3's values, guarded by a test.

## The floor, and its curve

The floor exists and is committed, per principle 2 — `h4_margin.calibration.curve.json`,
beside the code that reads it.

| | |
|---|---|
| signal | `h4_claim_margin` = `max_i margin(claim, chunk_i)`, k <= 8 |
| calibration set | `saltgrass_compound_gv_shadow_20260808`, 20 claims (15 supported / 5 not) |
| AUROC | **0.7867** |
| floor | **0.8009** |
| selection rule | the curve's best-balanced-accuracy threshold — by rule, not by hand |
| best balanced accuracy | 0.800 |
| recall at 20% false-alarm budget | 0.80 (realized false alarm 0.20) |
| recall at 5% and 10% budgets | 0.20 (realized false alarm 0.00) |

**Do not quote that AUROC as a result.** It is fitted on 20 claims whose entire
negative class is 5 claims from one turn. It is reported so the floor has a stated
provenance, not because 20 claims measure a cross-encoder. The recall column shows
the shape honestly: at a 5% false-alarm budget the floor catches one unsupported
claim in five.

## What the disagreements look like

Every disagreement in both runs is the same direction — the incumbent said supported,
the mechanism said not. Seven in total.

| Set | claim | margin |
|---|---|---|
| dev | "The salt-crusted hinges of the lid-hasp had been broken and…" | -7.431 |
| dev | "Lessa Pellow is the keeper of Wrack Point light." | -5.430 |
| dev | "What did the harbormaster's last entry in his own harbor boo…" | -0.941 |
| test | "Michaelis has an affair with Winnie but never takes her mone…" | -4.761 |
| test | "Stevie habitually draws with a compass and pencil." | -2.522 |
| test | "Type of charitable institution: almshouses" | -1.570 |
| test | "The intended target of the bomb plot is England." | -0.561 |

These are worth an operator's eye, because at least one of them looks like the
*incumbent* being wrong rather than the mechanism: in *The Secret Agent* it is Ossipon
who abandons Winnie and takes her money, and the probe id on that row is
`present-ossipon-abandons`. If the incumbent's positive labels carry errors of that
kind, then agreement against them is measuring the wrong thing in both directions, and
hand-adjudication is not a tiebreaker but the actual instrument. That is the operator's
call, not this worker's — §7.3 H4's kill criterion is a conjunction and its second
half was never in scope here.

## A second finding: span resolution is being fed the wrong unit

The span resolver returns `unverified_not_found` for **18 of 24** dev claims and
**23 of 26** test claims; nothing resolved verbatim at all, and the rest were `fuzzy`.

That is not a defect in the resolver — it is the input. The incumbent's holdings are
the ladder's *extracted paraphrases* ("Lessa Pellow is the keeper of Wrack Point
light."), and a paraphrase is by construction not a verbatim span of the evidence.
§5 H4's design does not ask it to be: the resolver is specified against **emitted
grounded segments carrying `{chunk_id, span}`**, produced by synthesis assembly —
which this order explicitly puts out of scope. Replaying it against extracted claims
measures the gap between the two units, not the resolver.

The practical consequence for the next order: **span resolution cannot be evaluated at
all until segment typing exists.** Only the margin fold could be exercised here.

## What this does and does not license

**Does:**

- Fund the instrument. `resolve_span`, the sentence sweep, `h4-sweep` and `h4-gate`
  are built, tested, deterministic, and validated on real transcripts. Re-running the
  verdict once better transcripts exist is one command.
- Establish bar (b) as met with room: 877 ms p50 against a 2,000 ms bar and a measured
  24.6 s incumbent turn. Whatever else is uncertain, the *cost* case for mechanical
  attribution is not.
- Name the two concrete transcript defects that block the measurement, both of which
  are small fixes on the chaos side: persist `retrieved_chunks` on longform turns, and
  consult the Critic on the longform path under `--gv-shadow`.

**Does not:**

1. Say H4 works. Agreement was never judged.
2. Say H4 fails. The kill criterion was never reached, and the kill path's
   adjudication sample was therefore not written.
3. License the cutover. Deleting the longform ladder on the strength of a
   could-not-judge would be exactly the failure mode this repo's §18 exists to
   prevent.
4. License lowering the margin band or pooling the splits to manufacture a
   two-class set.

## What would make this measurable

In rough order of cost:

1. **Persist `retrieved_chunks` on longform turns** in the chaos transcript writer.
   Without it, the class carrying most of the incumbent's negative verdicts is
   permanently unreplayable.
2. **Consult the Critic on the longform path under `--gv-shadow`**, so longform turns
   carry a `violation_prob` and can enter the high-margin set at all. §4's 2026-08-07
   correction already flagged that the gate is "structurally blind on exactly the turn
   class the §2 inventory says costs ~35 judge calls"; this order measured the
   consequence.
3. Re-run the dev banks with (1) and (2), then re-run `h4-gate` unchanged.
4. Only then, and only if the label set is two-class on both sides, is §7.3 H4's
   agreement number meaningful.

## Artifacts

| file | what |
|---|---|
| `h4_verdict.json` | the primary verdict — dev calibration, dev held-out |
| `h4_margin.calibration.curve.json` | the floor's operating curve (principle 2) |
| `h4_claim_scores.jsonl` | 44 scored claims, both splits, one row each |
| `second_source_secret_agent/` | the same three files for the Phase-0 frozen source, carrying the longform `unreplayable_turns_with_holdings` census |
| `../../chaos_monkey/results/saltgrass{,_compound}_gv_shadow_20260808.*` | deliverable 3's harvest |
| `../../chaos_monkey/results/saltgrass_gv_shadow_20260808.run.log` | the only home of the incumbent's per-turn timings |

Reproduce the verdict from frozen scores (no model, seconds):

```
svrn bench flywheel h4-gate \
  --calibrate sovereign/bench/chaos_monkey/results/saltgrass_compound_gv_shadow_20260808.transcripts.jsonl \
  --holdout sovereign/bench/chaos_monkey/results/saltgrass_gv_shadow_20260808.transcripts.jsonl \
  --from-scores sovereign/bench/calibration/h4/h4_claim_scores.jsonl \
  --out-dir sovereign/bench/calibration/h4
```

Reproduce the measurement (needs the reranker GGUF, ~2 min):

```
svrn bench flywheel h4-gate \
  --calibrate sovereign/bench/chaos_monkey/results/saltgrass_compound_gv_shadow_20260808.transcripts.jsonl \
  --holdout sovereign/bench/chaos_monkey/results/saltgrass_gv_shadow_20260808.transcripts.jsonl \
  --rerank-model <qwen3-reranker-0.6b-q8_0.gguf>
```

Run provenance: 2026-08-08, BeefyMac (macOS, 64 GB, Apple Metal unified);
reranker Qwen3-Reranker-0.6B-Q8_0; deliverable 3's harvest run on
FINAL-Bench_Darwin-36B-Opus-Q6_K as both primary and Critic, 57/57 probes,
41m13s wall.

---

# The re-run (2026-08-08b) — unchanged gate, fresh harvest

**The two telemetry defects were fixed, the harvest was redone, the gate was
re-run byte-unchanged, and the outcome is COULD-NOT-JUDGE again — with the same
numbers.** That is not a null result. It says the blocker was misdiagnosed: the
dev banks cannot supply a two-class longform label set, and no amount of
telemetry changes that.

## What the gate said

Primary (dev calibration, dev held-out), and the frozen `secret_agent` artifact
as a read-only second source. Both bars, both sources:

| | primary (dev) | second source (secret_agent) |
|---|---|---|
| outcome | **CouldNotJudge** | **CouldNotJudge** |
| (a) agreement, bar >= 0.90 | 0.8696 (20/23) — diagnostic only | 0.8095 (17/21) — diagnostic only |
| label balance | **23 supported / 0 not** | **21 supported / 0 not** |
| (b) audit p50, bar <= 2000 ms | **848 ms** over 23 turns — MET | **1920 ms** over 21 turns — MET |
| (c) fidelity deltas | NOT RUN (cutover, out of scope) | NOT RUN |
| floor / calibration AUROC | 0.8009 / 0.7867 over 20 claims | same calibration |
| unreplayable turns w/ holdings | **0** | 2, carrying 5 negative-class claims |

Against the prior run, **every gate quantity is identical** — agreement, 20/23,
the margin distribution, the floor, the AUROC. Only the audit timing moved
(p50 877 -> 848 ms, p90 2060 -> 2005 ms), which is host noise, not signal.

The kill path was not reached (disagreement 0.1304 against a 0.25 bar), so no
20-claim adjudication sample was prepared. The beat path was not reached either.

## The harvest is now fully replayable — and it did not help

`unreplayable_turns_with_holdings` is `[]` on both dev banks. The only three
evidence-free rows are `ood-canada-capital`, `ood-css-center`,
`ood-gold-symbol` — out-of-domain probes with no gate and no holdings. Both
longform probes carry evidence and a numeric turn vp
(`present-maximal-exposure` 20 chunks, `present-maximal-fraud` 19 chunks).

The negative class did not move: **5 `failed_once` claims, all on
`compound-killer-and-lugger`**, exactly as before. saltgrass returned 30
holdings and 30 `verified`, for the second harvest running.

## The real cause of the blind turns: a surface, not a length

The two turns that lose their evidence route to `Intent::ComplexTask`, which
`streaming.rs` runs inline through `handle_complex_task`. Its persisted metadata
has **no `retrieved_chunks` key at all**, because that surface's sealed universe
is the step-summary transcript — built for the gate and dropped after it
("Step summaries are synthesized prose, not retrieved chunks"). The released
answer says so itself: `present-maximal-statepower` opens *"Based on the
provided step results"*. `GateSurface::ComplexTask` also sets
`longform_chars = 0`, so every draft there takes the per-claim ladder. The one
surface whose evidence cannot be recovered is the one that always runs the
ladder H4 exists to replace.

The fix is `SOVEREIGN_GATE_AUDITED_EVIDENCE` (default off, set by
`chaos-monkey run` under `--gv-shadow`): the gate stamps its own audit inputs
and outputs onto its meta, so evidence is captured where the decision was made
and is independent of which handler ran. **It is unexercised by this harvest** —
it logged zero firings, because no dev-bank turn routed to ComplexTask. It is
proven by test, not by this run. The blind turns are on the frozen test bank.

## What the per-claim margins revealed — the one real new finding

The long-form ladder now retains the `violation_prob` it computes per claim and
previously discarded. On the single turn carrying the entire negative class:

| vp | claim |
|---|---|
| 0.9696 | Corwin Pellow was murdered by Severin Quenholt. |
| 0.9870 | The murder took place at The Cold Lantern inn on a summer evening. |
| null | The assistant's answer contains several unsupported or wrong statements: |
| null | Corwin Pellow was murdered by Severin Quenholt" - The evidence does not identi… |
| null | The killing took place at *The Cold Lantern* inn on a pleasant evening in summ… |

**Only two of the five negatives are per-claim judgements.** The other three are
not claims at all — they are specifics-scan / sentence-sweep JUDGE PROSE recorded
as claim rows: a critique preamble ending in a colon, and two fragments of the
judge's own commentary, quotation mark included. `longform_claims` appends
synthetic failures that never appeared in the extracted list, and these are that
path.

The ledger renders all five identically as `failed_once`, so the replay reads
five negative labels where the incumbent made two judgements. **The negative
class is 60% judge-commentary artifact.** This was invisible before the margins
were retained, it is a measurement-validity problem for any agreement number
computed against these labels, and it is NOT fixed here — the gate was frozen
this order, and correcting it changes what the ladder records.

## The instrument is reproducible — measured, not assumed

Two independent live `--gv-shadow` harvests, 3.5 hours apart, on **different
binaries** (the first without the audit-record telemetry, the second with it):

| bank | answers byte-identical | gate_action identical | turn vp identical |
|---|---|---|---|
| saltgrass | **36/37** | 37/37 | 37/37 |
| saltgrass_compound | **20/20** | 20/20 | 20/20 |

The single divergence (`present-maximal-exposure`) is two phrasings of the same
decline, both `abstained_decline`, both contributing zero holdings.

Two things follow. First, **the pipeline is effectively deterministic run-to-run
at temperature 0 under a fixed config** — an earlier reading of the
`ctl_r1`-vs-`gv_shadow` spread as run-to-run variance was wrong; those two
differ in configuration *and* code version, not just in the run. Second, this is
an empirical confirmation of the shadow-never-steers invariant at whole-system
scale, on top of the unit test that pins it structurally: turning the capture on
moved neither the released answers nor the gate's decisions.

## What would make this measurable — sharpened

The previous list is now spent: items 1-3 were done and item 4's precondition
(a two-class held-out set) still fails. The binding constraint is the **banks**,
and it is arithmetic:

| bank | longform (`present-maximal-*`) probes | role |
|---|---|---|
| `saltgrass` | **2** | dev / held-out |
| `saltgrass_compound` | **0** | dev / calibration |
| `secret_agent` | **6** | test — FROZEN, holds every blind turn |

The dev banks carry two longform probes between them, and in two consecutive
harvests neither produced a single `failed_once` holding. The negative class the
gate needs lives on the test bank, which the measurement may not use as a
development surface without spending its holdout value.

So, concretely, in order:

1. **A dev bank with longform probes that fail.** Not more runs of these two —
   the runs are reproducible, so re-running is measuring the same thing again.
   The bank must contain essay-shaped questions whose answers the incumbent
   ladder actually rejects claims from, and enough of them that negatives land
   on both sides of a leakage-free split.
2. **Decide what a longform negative IS, before counting one.** Three of the
   five negatives on the only turn that has any are judge prose. Any agreement
   number computed before that is settled is measuring the extractor's output
   format, not the mechanism.
3. **A dev bank that exercises `GateSurface::ComplexTask`,** since that is where
   the incumbent's unreplayable negatives concentrated on the test bank and the
   only surface whose evidence needs the new capture. Nothing on the dev banks
   routes there today, so the capture is currently proven only by test.

Only after 1 and 2 is §7.3 H4's agreement number meaningful.

Re-run provenance: 2026-08-08, BeefyMac (macOS, 64 GB, Apple Metal unified, M2
Max); reranker Qwen3-Reranker-0.6B-Q8_0; harvest on
FINAL-Bench_Darwin-36B-Opus-Q6_K as primary and Critic, 57/57 probes, 28m16s
wall (saltgrass exit 0 VERDICT PASS; saltgrass_compound exit 1 VERDICT FAIL —
the known 0-absent-probes NaN, not a harness failure). Artifacts in
`rerun_20260808b/`; the 20260808 artifacts this document's body cites are
untouched.

---

## Discernment: what a naive strategy would score (operator steer, 2026-08-08)

The frozen gate's bars say nothing about whether the *set* can tell a mechanism
from a coin. So, reported alongside them and never as a new bar: **what would a
scorer that never looks at anything achieve?**

| set | n | incumbent labels | naive ALWAYS-SUPPORTED | naive ALWAYS-NOT | mechanism |
|---|---|---|---|---|---|
| holdout, dev (high-margin) | 23 | 23 sup / 0 not | **1.0000** | 0.0000 | 0.8696 |
| holdout, secret_agent (high-margin) | 21 | 21 sup / 0 not | **1.0000** | 0.0000 | 0.8095 |
| calibration (all, two-class) | 20 | 15 sup / 5 not | 0.7500 | 0.2500 | 0.8000 *(in-sample)* |

**On both held-out sets a scorer answering "supported" unconditionally scores
1.0000 — it clears the 0.90 beat bar outright, and it beats the mechanism by
+0.1304 and +0.1905.** That is the quantitative form of the could-not-judge:
the bar is winnable by a null strategy, so passing it would have demonstrated
nothing. This is not a criticism of the bar — §7.3 H4's thresholds presume a set
that risks both error directions — it is the measurement of how far these sets
are from that presumption.

The only two-class set is the calibration set, and its number is **in-sample**:
the floor (0.8009) was fitted on those same 20 claims by the curve's
best-balanced-accuracy rule. Read as a ceiling on what an out-of-sample number
could look like, not as a result. Its margin over naive is **+0.0500** — 16/20
against 15/20 — i.e. one claim. And that margin is not even honestly earned:
of the 5 negatives the floor catches 4, but only **two of the five are real
per-claim judgements** (see the artifact finding above); two of the four
"catches" are judge-prose rows.

**Whenever a future held-out set is two-class, report the margin over naive
beside the agreement.** An agreement number without its naive ceiling cannot be
read.

## Representativeness: the set is degenerate, and the verdict is qualified

Two-class is necessary and not sufficient. Judged explicitly:

| criterion | status |
|---|---|
| negatives present at all in held-out | **NO** — 0 of 23 (dev), 0 of 21 (secret_agent) |
| negatives spread across turns | **NO** — all 5 on `compound-killer-and-lugger` |
| negatives spread across probes | **NO** — one probe |
| negatives spread across qtypes | **NO** — one, `partially_present` |
| negatives are actually claims | **NO** — 2 of 5; the other 3 are judge prose |
| more than one failure class | **NO** — both genuine negatives are fabricated-specific (wrong perpetrator; wrong place/time) |
| negatives on a LONGFORM turn | **NO** — the negatives are on a SHORT compound probe |

**Every criterion fails.** Even if a future split made this set nominally
two-class, a verdict computed on it would be qualified to the point of being
uninformative: it would measure one mechanism against one failure shape on one
turn, 60% of whose negative rows are not claims.

Note the last row especially. The negative class this initiative has been
reasoning about as "longform" is not longform at all — it is a short
`partially_present` compound probe. **No longform turn in either dev bank has
ever produced a `failed_once` holding**, across four harvests.

## Bank-side spec for the follow-on order

The dev banks structurally cannot supply the above, and the shortfall is
arithmetic, not stochastic. Every longform probe in every bank is
`qtype = "present"`:

| bank | probes | qtypes | longform probes | longform qtypes |
|---|---|---|---|---|
| `saltgrass` | 37 | present 19, absent_adjacent 6, absent_ood 5, provenance_trap 4, distractor 3 | 2 | present, present |
| `saltgrass_compound` | 20 | partially_present 20 | **0** | — |
| `secret_agent` (FROZEN) | 43 | present 25, absent_adjacent 6, absent_ood 5, provenance_trap 4, distractor 3 | 6 | all present |

So the hard cells exist in the banks (`distractor`, `provenance_trap`,
`partially_present`, `absent_adjacent`) but **not one of them is ever asked in
longform**. The follow-on order needs a dev bank built to this shape:

**1. Volume and spread.** At least **8 longform probes that induce at least one
`failed_once` holding**, on **>= 6 distinct turns**, split so that **>= 2
negative-carrying turns land on each side** of the calibration/holdout split.
Two per side is the floor for a leakage-free split (the current corpus has one
negative-carrying turn in total, which is why no split exists). Target >= 20
negative claims total so the naive ceiling on the holdout side sits meaningfully
below 1.0 — with 23 held-out claims, even 4 negatives moves it to 0.83, below
the beat bar, which is the point.

**2. Failure classes — at least three, in longform.** Take the existing qtype
vocabulary and ask it in essay form: `distractor` (does the essay take up the
plausible-but-wrong passage?), `provenance_trap` (does it attribute to the wrong
source?), `partially_present` (does it complete the half-supported claim from
imagination?), plus the fabricated-specific shape the two genuine negatives
already exhibit. One class is not a measurement.

**3. Both surfaces, deliberately.** The blind class and the recoverable class
must both be represented, because they exercise different code:
`GateSurface::ComplexTask` (evidence recoverable only via
`grounding_gate.audited_evidence`, `longform_chars = 0` so always the per-claim
ladder) versus KnowledgeQuery (evidence in `retrieved_chunks`). **How to induce
each is NOT settled and must not be guessed.** All six `secret_agent` longform
probes are phrased alike — "write an exhaustive, multi-section essay…" — yet
only `present-maximal-statepower` and `present-maximal-london` routed to the
blind class. Phrasing is not the lever, and this document does not know what is.

**4. Therefore, one cheap telemetry ask first: record the routed intent per
turn in the chaos transcript.** The blind class had to be diagnosed this order
by reading answer prose ("Based on the provided step results") and matching it
against handler source. A `routed_intent` key on the transcript row — the
router already decides it — turns "which surface did this probe take?" from an
inference into a fact, and makes requirement 3 authorable by observation
instead of by guess. Until it exists, a probe qualifies as blind-class only
once it has been *observed* to route there.

**5. Settle what a longform negative IS before counting one.** Three of the five
negatives in the only negative-carrying turn are specifics-scan / sentence-sweep
judge prose appended by `longform_claims` as synthetic failures. Any agreement
number computed before this is decided measures the extractor's output format,
not the mechanism. This is a decision for the seat, not a bank change.

Requirements 1, 2 and 5 gate the H4 measurement. Requirements 3 and 4 gate
whether the evidence capture built this order is ever exercised live — today it
is proven by test only, with zero firings across the harvest.
