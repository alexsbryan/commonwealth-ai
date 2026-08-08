# H4 gate — the verdict is COULD-NOT-JUDGE, and that is the finding

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

- **`retrieved_chunks` is empty** on every longform turn in a gv-shadow run — 6 of
  43 rows on `secret_agent`, including both turns that carry negative claims. The
  turn plainly had evidence (`present-maximal-statepower` released a 7,157-character
  answer and the ladder audited 9 claims against sealed evidence, then annotated it),
  but the transcript preserves none of it. With no evidence pool, there is nothing to
  resolve a span against and nothing to fold a margin over.
- **`violation_prob` is null** on every longform turn, because the Critic is only
  consulted on the short path (`chaos_monkey.rs:907`). A turn with no vp can never
  satisfy `|vp - tau| > 0.2`, so no longform claim can ever enter the high-margin set
  even if its evidence were restored.

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
