# Pre-registration — where a verifier earns its place

Written 2026-08-19, BEFORE any result is collected. Fixed at commit `b033aae2`.
Nothing below may be edited after the first result lands; changes append.

## Why this study exists

Seven candidate interventions were derived from one thesis: **the value of an
external verifier is proportional to how decorrelated its errors are from the
generator's, not to how sophisticated it is.** Three local measurements already
speak to it, in opposite directions:

| Prior result | Source | Direction |
|---|---|---|
| Harness equalises tiers: naked 4B 0.21, naked 35B 0.42, both harnessed 0.67 | situated-harness study, note `1652471e` | supports |
| Constrained decoding took a verifier from 0.05 (looked dead) to 64.97 BAcc, zero parse failures | note `b06399bf` | supports |
| Grounding-verifier crosses honesty (0.82) and collapses competence (0.12); **no threshold passes both gates** | note `446295e8` | refutes, for that instrument |

The third is the reason for this document. A verifier that cannot be gated on
is not a verifier, and the named cause was that the Critic reads *the same
retrieved chunks the generator read* — maximally correlated errors. Every
experiment below is a cheap test of one place where that correlation might be
broken, or one place where a verifier is being paid for and may not be earning it.

## The power statement — read this before reading any bar

**At the N available, every experiment here can KILL a line. None can CONFIRM
one.** Absence of an effect at these sample sizes is informative; presence is
not, because the confidence intervals will overlap almost anything.

This is deliberate. The purpose of a one-day pass is to delete candidates
cheaply, not to establish results. Any experiment that survives its kill bar is
promoted to a properly powered arm and **is not reported as a positive finding
in the meantime**. A surviving candidate's status is `not-yet-refuted`, which is
a third state and not a synonym for `supported`.

Where an experiment's input is not banked, it is marked so. An experiment with
no input has verdict `never-ran`, never `passed`.

## Shared discipline (applies to every experiment below)

1. **Two axes, never averaged.** Honesty and competence are reported separately
   throughout. One number makes the abstainer and the bluffer indistinguishable,
   which is the failure mode half of this document exists to detect.
2. **Wilson intervals, significance only when disjoint.** No point estimate is a
   result.
3. **The judge is part of the key.** `critic_model` currently resolves from the
   local `RoleProfile` (`chaos_monkey.rs:177`) and nothing in the record names
   it. Every artifact this study writes stamps the critic and the register
   fingerprint. Two arms whose judge differs are not two arms.
4. **could-not-judge is a legal answer**, not a non-response, and is counted in
   its own column.
5. **Absence is reported, never defaulted.** A missing input yields `never-ran`.
6. **A kill produces a `DEFAULTS_LEDGER.md` REJECTED row** naming the measurement
   and the bar, so the line is not re-litigated without new evidence.

---

# Tier 0 — runs today, zero inference, banked artifacts only

## X1 — Does the LLM judge earn its tokens where an executable witness exists?

**Context.** `ScoreSource` (`sovereign-agent-bench/src/scoring.rs`) already
encodes the correct ordering: `Hybrid` means "the auto-witness sets a floor; the
judge can lift but not lower." It stores both `auto_score` and `judge_score`, so
the judge's marginal contribution is a descriptive statistic over banked runs.

**H0.** In a domain with a perfect executable verifier, the LLM judge adds
nothing: it never lifts above the witness floor, or its lifts are unrelated to
any independent signal.

**Input (banked).** `sovereign/bench/agent-coding/.artifacts/` — 9 dated runs,
**24 `witness.json`** files and **49 `judge/dim_*-trial-*.json`** files.
`witness.json` carries `{verify_exit_ok, passed, failed, total, failed_names,
pass_fraction, bucketed_score}`; judge files carry the anchor and prompt.

**Instrument validation (must pass first).** Confirm `bucketed_score` and the
judge's anchor index are on the **same 0..=3 scale**. The judge is documented as
picking "a single anchor index 0..=3"; the witness bucketing is derived from
`pass_fraction`. If those two scales are not identical, the lift count is
meaningless and the experiment stops here with verdict `could-not-judge`.

**Measurement.** Over every (run, problem, dimension) triple with both halves
present: lift rate = `count(judge_score > auto_score) / count(pairs)`; agree
rate; and the distribution of `pass_fraction` on lifted rows (a lift over a
`pass_fraction == 1.0` row is the judge overriding a green witness — a distinct
and more serious category than a lift over partial credit).

**Rejects H0 if:** lift rate ≥ 15% **and** lifts concentrate on rows where
`0 < pass_fraction < 1` (the partial-credit band the witness genuinely cannot
adjudicate).

**KILL BAR.** Lift rate < 15%, **or** lifts are distributed indifferently across
`pass_fraction` including green rows. Action: collapse `HybridAutoFloor` to
`Auto` in the coding path, delete the judge calls, REJECTED row.

**Cost.** A script over banked JSON. Minutes.

---

## X2a — Is the unconstrained judge silently failing to parse?

**Context.** The agent-bench judge is the only judge in the system sampled
**unconstrained**: its request body is `{"model", "temperature"}` and its own doc
comment describes "a permissive JSON parser that tolerates the model wrapping the
JSON in a code fence." Meanwhile `CompletionRequest` carries `json_schema`
(`completion.rs:26`) and `lark_grammar` (`:216`), and llguidance is the single
grammar engine at every other schema site.

**H0.** The permissive parser never fires — every banked judge output was
well-formed JSON, so constraining the decoder would change nothing.

**Input (banked).** The 49 judge trial files above.

**Measurement.** Count outputs that required fallback recovery: fence-wrapped,
prose-prefixed, or otherwise not parseable as bare JSON on first attempt.

**Rejects H0 if:** fallback rate > 0.

**KILL BAR.** Fallback rate == 0 across all 49. Action: leave the judge
unconstrained, REJECTED row, and note that the two prior grammar wins do not
generalise to this register.

**Cost.** Minutes. **Note:** X2b (grammar the judge and replay) is Tier 1 and
only runs if X1 says the judge survives at all — there is no point constraining a
judge we are about to delete.

---

## E4 — Is round non-growth a valid stopping rule?

**Context.** The essay's central quantity is effective depth = raw depth ×
survival. The R-12 leg already measures content-round non-growth (r2→r3, final
≤ r2). This asks whether the measurement can become a rule.

**H0.** A "stop when round N does not grow over round N−1" rule cannot save
rounds without costing verdict-set items — the later rounds are doing work the
non-growth metric cannot see.

**Input (banked).** `research/deep-research/drb/runs/{local,hybrid}/drb-*/dr-*/`
— **20 flights** with `verdict-set.json`.

**Measurement.** Replay the rule over each banked flight: rounds saved, and
verdict-set items that would have been lost.

**Rejects H0 if:** ≥ 25% of rounds saved at a cost of ≤ 1 verdict-set item across
the 20 flights.

**KILL BAR.** > 1 item lost. Action: the non-growth metric is wrong, not the
depth. REJECTED row against the stopping rule; the metric stays as a report.

**Cost.** Zero inference. Re-analysis of banked artifacts.

---

# Tier 1 — needs one script or one bank run

## E1a — Reproduce the overlap, and measure vp stability

**This is instrument validation for E1b, not a result.** It reproduces note
`446295e8`'s finding from banked data and, because three of the six files are
re-runs of the same banks on different dates, yields a **test-retest stability
estimate for `violation_prob` for free** — which nothing has ever measured.

**H0.** `violation_prob` is stable across re-runs of the same item, and the
present/absent-adjacent distributions overlap as previously reported.

**Input (banked, and the counts matter).** `sovereign/bench/chaos_monkey/results/`
holds **1,109 transcript rows**, of which **192 carry `violation_prob`**. Those
192 are **not 192 independent observations** — they are 6 files forming 3
near-duplicate pairs (`saltgrass_longneg` at 39 rows × three dates,
`saltgrass_compound_longneg` at 25 rows × three dates). Effective distinct items:
**~64**, each measured 3×.

vp rows by qtype: partially_present 84, present 60, absent_adjacent 18,
absent_out_of_domain 6, provenance_trap 15, distractor 9 — i.e. the decisive
contrast (present vs absent_adjacent) is **20 vs 6 distinct items.**

**Therefore E1a cannot measure discrimination.** It can only (a) confirm the
overlap qualitatively and (b) report vp test-retest stability. Any AUC computed
here is reported with its interval and explicitly labelled underpowered.

**Rejects H0 if:** vp varies by > 0.15 across re-runs of the same item at the
same configuration — in which case the instrument is too unstable to gate on and
**E1b is cancelled**, because a more decorrelated evidence window cannot fix a
noisy judge.

**Cost.** Script over banked JSONL. Minutes.

---

## E1b — Does an FTS-built evidence window decorrelate the Critic?

**Runs only if E1a shows vp is stable.**

**H0.** Building the Critic's evidence window from a targeted corpus FTS for each
claim separates present from absent/adjacent no better than the generator's own
retrieved chunks — i.e. the correlation was not the cause.

**Instrument validation (must pass first).** Note `446295e8` establishes the
facts are FTS-findable: Winnie appears 92× and Stevie 173× in source. For a claim
naming Winnie, the FTS window must surface a chunk containing it. If it does not,
the window builder is broken and every subsequent number is noise.

**Input.** Requires (a) an FTS window builder and (b) a fresh `--gv-shadow` run
over the full banks to get vp on more than 64 items. `--gv-shadow` persists
`violation_prob` on every row **without gating** (`chaos_monkey.rs:118`), so
there is no product risk in the observation. Rescoring is offline
(`chaos_monkey.rs:1108`) and transcripts carry `retrieved_chunk_texts`
verbatim (`:840`), so the paired comparison needs no synthesis re-run.

**Primary measurement.** AUC of vp separating present from absent/adjacent, FTS
window vs chunk window, on identical rows.

**Decisive secondary — the bar that failed before.** There exists a τ where
**honesty ≥ 0.70 AND competence ≥ 0.60** simultaneously (the gate thresholds
already carried in `reliability.json`).

**Rejects H0 if:** AUC_FTS exceeds AUC_chunk with disjoint intervals, **and** such
a τ exists.

**KILL BAR.** No τ passes both gates on the FTS window either. Action: the FTS
lever is dead; the surviving branch is the note's other named lever — fix
retrieval discrimination (facet #2). REJECTED row.

---

## E2 — Are numeric failures computation failures or retrieval failures?

**Context.** All 40 registered tools are reads. Not one computes. `numeric_audit`
value-matches figures in prose against tool outputs but nothing in the loop
*produces* a figure by execution. Before building an executor, price it.

**H0.** Numeric-audit failures are not derivable from figures already in the
evidence window — they are retrieval failures, and a calculator fixes none.

**Input — NOT BANKED.** No numeric-audit ledger artifact was found. Producing
one requires a `numeric_audit`-instrumented arm over the sec-filings frozen set
and/or `corp-sheets`. **Until that runs, this experiment's verdict is
`never-ran`.**

**Measurement.** Classify each numeric-audit failure: (a) derivable by arithmetic
over figures present in the evidence window, (b) the figure was never retrieved,
(c) neither.

**Rejects H0 if:** ≥ 20% fall in class (a).

**KILL BAR.** < 10% in class (a). Action: the failure is retrieval, not
computation. Drop the executable-compute item entirely, REJECTED row.

---

## E3 — Does a retraction cascade have anywhere to fire?

**Context.** `GovernanceView::dead_law_sections` drops retrieved chunks of
superseded sections and RL-3 gates the turn on no dead law. That is a working
truth-maintenance mechanism in exactly one domain.

**H0.** No non-governance corpus carries enough supersession relations in
existing metadata for the mechanism to fire — there is nothing to generalise.

**Input.** Existing corpus indexes. Candidate corpora with plausible supersession
metadata: `federal-register-presidential` (versions),
`us-code` (years), `sec-filings-company` (amendments, restatements).

**Measurement.** Fraction of retrieved chunks whose source has an available
supersession relation derivable from metadata already present.

**Rejects H0 if:** ≥ 5% on at least one non-governance corpus.

**KILL BAR.** < 5% everywhere. Action: the TMS item is a no-op here regardless of
its elegance. REJECTED row.

**Cost.** Pure count over existing indexes, no inference.

---

# Tier 2 — needs a real build, and is underpowered until it doesn't

## X3 — The intent↔test verifier

**The refinement.** In code the decision stage is already perfect: the compiler
decides "does it build" exactly, the test runner decides "do these tests pass"
exactly. Neither can see whether the tests express the intent. So the transfer of
the grounding verifier into code should *not* point at the code — it should point
at the formalisation: `(issue statement, candidate test) → does this test
discriminate this intent?` That is the identical predicate rung-1000 was trained
on, with the arguments substituted, and its errors **cannot** correlate with the
compiler's because it never sees the implementation.

The codebase already found this principle in one place: the TDD solver's
`GenerateOneFailing` polarity accepts only when ≥1 new test appeared *and all of
them fail* — an entry-gap audit proving the test discriminates before anything
trusts it. 92% PASS_AS_RED across N=25.

**H0.** The intent↔test predicate is at chance for predicting whether an arm's
patch resolves the instance.

**Input.** `sovereign/bench/external/swebench/` — `instances.jsonl` holds
**12 instances**; `preds/` holds 4 arms (comaintainer, flat, native,
mini-swe-agent) with runlogs; `gold/` holds the reference.

**POWER — stated up front.** N=12 cannot produce an AUC with a usable interval.
**At this N, X3 is a smoke test for the pipeline, not a measurement of the
predicate.** The measurement requires the full SWE-bench Verified 500. Reporting
an AUC from 12 instances as a finding would repeat the exact error this document
is structured to prevent.

**Rejects H0 if:** on the full 500 — not on 12 — AUC exceeds chance with disjoint
intervals.

**KILL BAR.** At chance on the full set. Action: the code domain does not have
the entry-gap problem the text domain has; record it and stop. REJECTED row.

---

# Suggested order for one day

| # | Experiment | Input | Cost | Can kill |
|---|---|---|---|---|
| 1 | X1 judge vs witness | banked, 24+49 files | minutes | `HybridAutoFloor` |
| 2 | X2a parse-fallback census | banked, 49 files | minutes | the grammar fix |
| 3 | E4 stopping rule | banked, 20 flights | minutes | the depth rule |
| 4 | E1a overlap + vp stability | banked, 192 rows / ~64 items | minutes | **E1b itself** |
| 5 | E3 supersession census | corpus indexes | ~1h | the TMS item |
| 6 | E2 numeric census | needs an arm first | ~1h + arm | the compute item |

X1, X2a, E4 and E1a are the day. Three of the four can delete something, and
E1a is the gate on whether the largest remaining build is worth starting.

## Appendix — what this document does not test

The essay's items 1 and 2 (grammar-constrained decoding; externalised working
memory) are already built here and were measured decisive at the time they
landed. `json_constraint.rs` was deleted (−5,623 LOC) when llguidance became the
single grammar engine; `deep_research/` is a 10,817-line explicit state machine
writing versioned ICD artifacts per step with a frozen charter. Neither needs an
experiment. Item 6 (best-of-N) is already exploited by the TDD solver against a
perfect verifier — median 20/20 on Green vs 0–3/9 for the role loop — and the
marginal return from more compiler-verified search is expected to be small for
that reason.

---

# AMENDMENT 1 — data preparation, splits, and two bars that were not adjudicable

Appended 2026-08-19, still BEFORE any result. Prompted by the question "can we
derive proper significance along with holdout for generalization?" The answer
differs by experiment and two bars above were wrong.

## Which experiments take a holdout, and which must not

**No holdout — X1, X2a, E2, E3.** These are censuses: they measure a property of
a population and fit nothing. There is no model selection, therefore nothing to
overfit, and splitting would only burn power. What they require instead is a
denominator fixed in advance (done above) and **stratification** (below).

**Holdout mandatory — E1b.** Its decisive bar is existential ("there exists a τ").
Selecting τ by sweep and reporting that τ on the same items is fitting on test.
This is not hypothetical: note `446295e8` records "threshold sweep (offline from
logged vp) confirms overlap" — the prior attempt selected on the evaluation set.

**Holdout mandatory, but contamination is the larger risk — X3.** See below.

## X1 — CORRECTED population and bars

The 9 banked runs span **four harness/model configurations**
(`pi-commonwealth-coder`, `search-commonwealth-coder`,
`search-commonwealth-primary`, `pi-commonwealth-primary`), 1–3 problems each,
**19 problem-runs over at most 10 distinct problems**. These are not one
population and must not be pooled.

**Correction to the 15% bar.** At N=19, three events is 15.8% with a Wilson
interval of roughly [0.06, 0.34]. A 15% rate bar cannot be adjudicated at this N
and is **withdrawn**. Replaced by two bars that are adjudicable here:

- **KILL (clean).** The judge lifts on **zero** rows across all 19 problem-runs.
  Action as before: collapse `HybridAutoFloor` to `Auto`, REJECTED row.
- **ALARM (any N).** The judge lifts on ≥1 row where `pass_fraction == 1.0` —
  overriding a green witness. This is qualitatively different from lifting within
  the partial-credit band and is reportable at any sample size.
- **Otherwise:** report the lift rate **per configuration, stratified, unpooled**,
  with intervals, and the verdict is `not-yet-refuted` — not a rate estimate.

## E1 — required data preparation, in order

1. **E1a runs first and gates the spend.** vp test-retest across the 3× repeats.
   **If vp varies by > 0.15 on the same item at the same configuration, stop:**
   an unstable judge cannot support a threshold at any sample size, and the
   shadow pass below is wasted money.
2. **If E1a passes: one `--gv-shadow` pass over all four banks.** It gates
   nothing, so there is no product risk. This takes vp from ~64 distinct items to
   the full population (banks hold 118 questions; full transcript qtype counts are
   present 450 / partially_present 272 / absent_adjacent 126 /
   absent_out_of_domain 105 / provenance_trap 93 / distractor 63).
3. **Split by ITEM, not by row** — the repeats mean row-level splitting leaks the
   same item into both halves.
4. **Stratify on qtype.** `absent_adjacent` is the scarce and decisive class.
5. **Seed fixed and the split file written BEFORE the sweep**, recorded with the
   data, same convention as `CEILING_STUDY_PREREG.md` (seed 20260818, order
   randomized once and recorded).
6. τ is selected on dev only. The reported honesty/competence pair is the test
   split's, at the dev-selected τ, once.

## X3 — the negative control is a precondition, not a robustness check

SWE-bench Verified is public and its `problem_statement` text is almost certainly
in Qwen3.5-4B's pretraining. "The verifier predicts resolution" may therefore be
recall rather than reasoning about the (issue, test) correspondence.

**Required before any real score is collected:** a provably-blind negative
control in the same pass — scrambled `(issue, test)` pairs drawn across
instances. If mismatched pairs score as confidently as true pairs, the predicate
is recalling and the experiment is void regardless of its AUC. This is the same
control shape mechanism-fidelity already uses.

The 12 banked instances span 8 repos and carry a `difficulty` field; the full
Verified set is 500. The dev/test split must be drawn from the 500 and fixed
before anything is tuned on it.

## E4 — pin the parameter rather than split

The stopping rule has one free parameter (what counts as "growth"). With 20
flights, splitting 10/10 is worse than pinning. **Use the growth definition
already fixed by the R-12 leg** — it exists, and it was not chosen for this
experiment, which is what makes it a legitimate prior commitment.

## The generalization holdout that already exists and was missing here

Every bank in this document is a house corpus — `saltgrass`, `secret_agent`, the
`agent-coding` problems. A verifier tuned on them generalizes to them.

The external ruler already exists: **rung-1000's 550-item held-out
llm-aggrefact bank** (BAcc 74.73 vs HalluGuard 62.69,
`research/verifier-v0/findings/BASELINES.md`). **Any change to the grounding
verifier is re-scored there before it is described as an improvement**, not only
on the house banks. `sovereign/bench/external/` also holds RewardBench2 with
banked rows for `Qwen3.8-27B-UD-Q6_K_XL`.

A result that moves on saltgrass and not on the held-out bank is a house-corpus
artifact and is reported as one.
