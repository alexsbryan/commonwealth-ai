# Step 3 — failure decomposition, component attribution, and the weak-link ranking

Order native-grounding-step3-tuning, deliverables D2-D4. Everything here is
computed from committed artifacts by two scripts in this directory:

- `build_failure_corpus.py` -> `failure_corpus.jsonl` (D2, 31 cases)
- `attribute_failures.py` -> `attribution.json` (D3, counts + mechanisms)

Re-running both regenerates the corpus and the attribution from the repo
alone. No number below is hand-typed from memory; each is the script output.

## D2 — the failure corpus (31 cases)

| family | n | source |
|---|---|---|
| comp_loss | 15 | Step 2 A/B: off-arm pass -> on-arm fail (the 15 competence-loss refusals) |
| comp_loss_r2_only | 1 | passed on r1, failed on r2, admission identical — the parametric coin |
| absent_uncaptured | 1 | ood-css-center: uncaveated GK answer in every arm (the 1 of 11) |
| retrieval_fact_loss | 8 | HARD lanes, committed baseline 2026-07-16/17 vs 2026-08-10 |
| retrieval_source_loss | 3 | same |
| routing_misroute | 3 | HARD routing lane, same snapshot pair |

Every A/B row carries the full stage trace: retrieval pool (with
gold-keyword presence), H1 admission (decision, margin, answerability, taus,
chunks dropped), resolver (never ran — recorded with why), judge
(gate_action null — early decline skips the gate), abstention action
(evidence withheld -> parametric fallback, disclaimer presence), and the
synthesis outcome in all three arms. Stages a case never reached say
`{"ran": false, "why": ...}` — absence reported, never defaulted.

## D3 — component attribution (counts over the 31)

Counterfactual basis, not simulation: the committed flag-off arm IS the
admission-forced-open replay (only `SOVEREIGN_NATIVE_GROUNDING` differs);
presence-of-answer is the bank's grep-verified gold keyword matched
verbatim against the retrieved pool; H1 is deterministic (r1 vs r2: 33/33
identical decisions AND margins).

| component | cases | share | mechanism (one sentence) |
|---|---|---|---|
| **H1 admission calibration** | **15** | 48% | tau_abstain (margin 5.885 / p 0.348) was fitted on 99.5% SEP-family pairs; on chaos-saltgrass the answerable turns' margins sit below it (failing turns span m 0.42-5.36), so H1 abstains on turns whose pool verifiably holds the answer — the flag-off counterfactual answers all 15 correctly |
| **retrieval** | **11** | 35% | the composed evidence pool lost bank-declared facts/sources vs the 2026-07-16/17 pools — drift from landed retrieval commits (atom-enum reorder d04a1100/f40f8e72 and siblings), adjudicated in D1_remint_adjudication.md |
| **routing** | **4** | 13% | probes classified out of the path carrying the failing capability: ood-css-center exits to CodeQuery where no admission/judge/caveat discipline exists; 3 HARD-lane probes fell from the embed layer to the coarse-LLM after 4d589963 and misroute there |
| **abstention action** | **1** | 3% | admission identical across runs (Abstain, m 4.24) yet pass r1 / fail r2 — the parametric general-knowledge fallback is a coin, and the failure is what the abstention DID, not whether it should have fired |
| incumbent judge | 0 | — | gate_action is null on every failing A/B case; the early decline skips the gate, so the judge never released a wrong verdict here |
| span resolver | 0 | — | no failing case exercised span resolution (citation_located=0 on all); its measured weakness (precision 0.7429 vs bar 0.98; Verbatim tier 4/130, all four wrong) lives in `../resolver-precision/` and enters D4 on that evidence, not this corpus's |
| synthesis | 0 | — | no case produced wrong prose with correct evidence admitted; css-center's caveat failure is charged to routing because the CodeQuery path has no caveat surface to fail |

Secondary mechanism, counted separately and not added to the table: on all
15 comp-loss cases the abstention action rerouted to a parametric turn that
asserted specifics it cannot know (e.g. present-killer flag-on: "Percival in
*The Last of Us Part II*") behind a "Not in your sources" disclaimer the
honesty classifier accepts. Zero of the 15 competence losses would have been
saved by a better action (a refusal also fails competence) — but the action
converts a wrong abstention from an honest decline into a confabulation
surface. That risk is real on 16 of 17 A/B cases (disclaimer present on 16;
absent on css-center).

Repeat counts (§18.5): A/B admission identical across 2 runs; comp-loss
outcomes reproduced 15/15 in r2; HARD-lane signatures identical across the
step2-order run (2026-08-09), the seat control run (2026-08-09/10), and the
2026-08-10 re-mint captures (retrieval deltas byte-equal per question;
routing misroutes 25/27 + 9/10 in all three).

### The stop-condition check

The order's not-worth-continuing clause: stop if the dominant share is
base-model capability. It is not. 15/31 attribute to a fitted threshold
(pure calibration data), 11/31 to retrieval pool composition (landed,
adjudicated drift), 4/31 to routing classification, 1/31 to a fallback
design choice. The flag-off arm proves the base model answers 23/31
competence probes correctly when the evidence reaches it. Zero cases
attribute to the model being unable to do the task. Step 3 continues.

## D4 — weak-link ranking (attributed share x tunability)

| rank | component | share | knob | mechanism of fix | pre-registered bar (proposal — pinned via seat log before any judging run) | cost |
|---|---|---|---|---|---|---|
| 1 | H1 admission calibration | 15/31 | per-corpus operating point for tau_abstain/tau_answer: env-declared experimental override (`SOVEREIGN_NG_TAU_ABSTAIN`/`_TAU_ANSWER`, answerability units), read inside the single `calibration()` accessor, traced at admission | shift the operating point in margin space so the FA budget the curve promises (5%) holds on THIS corpus; fit on `saltgrass_compound` (25 probes, all answerable, zero absent — its labels touch only false alarms, never honesty), judge on `saltgrass` | ON-with-tau' vs committed OFF arm: (i) competence-when-present >= 0.71 on both on-runs; (ii) honesty-when-absent >= 0.91 on both; (iii) abstains on the 31 answerable probes <= 2 (the 5% FA anchor, rounded up); (iv) admission decisions identical across the 2 on-runs; kill: if FA<=5% on compound forces tau' below every compound margin, per-corpus thresholding is vacuous on this corpus — record failed | fit script + env knob + 1 compound harvest run + 2 on-runs + 1 off revalidation (~1.5-2h daemon time) |
| 2 | abstention action | 1/31 primary, 16/17 secondary risk | on `GroundingDecision::Abstain`, emit the incumbent's honest-decline template instead of rerouting to the parametric general-knowledge turn | removes the confabulation surface: a wrong abstention becomes a visible refusal, not fluent false prose behind a disclaimer | on the A/B: zero flag-on turns that assert an unverifiable specific behind a source disclaimer (blatant-confab audit over transcripts); competence unchanged vs arm-mate (the action never saves competence) | small code change at the early-decline seam + rides the same A/B as rank 1 if ordered so |
| 3 | routing | 4/31 | embed exemplars for the 3 fallthrough probes (backlog `routing-exemplar-fallthrough`, 4f21fa5e); caveat discipline for parametric CodeQuery answers | restore deterministic embed-layer verdicts where the classifier-embedding change dropped marginal probes to the flapping LLM layer | routing lane: the 3 probes route embed-layer with named exemplars, lanes 27/27 and 10/10, then baselines re-minted to passing | out of this order's scope block — filed, owned by the backlog |
| 4 | retrieval | 11/31 | atom-enum gating/cap-N dedup family (existing backlog notes 4f78f9f1, 10a1b08d) | pool composition re-tuning is quality-lane work with its own bench and its own trade to price | n/a in this order — the D1 adjudication accepted current levels as the known ruler | separately funded |
| 5 | span resolver | 0/31 | Verbatim-tier coverage (4/130 claims, precision 0.000 on that tier) | no failure share in this corpus and the judge-skip it would fund is already refused by the pre-pinned 0.98 bar | none proposed — do not tune a component with zero attributed failures | — |

**Top-ranked executable tuning: rank 1, per-corpus tau recalibration.**
Rank 2 rides the same A/B if the seat orders it bundled; it is listed
separately because one tuning is judged per run (no confounded arms).

The order pre-names all three known candidates: per-corpus tau_abstain
recalibration (rank 1, taken), per-corpus admission-curve re-fit (subsumed:
a full re-fit needs labeled per-corpus pairs that do not exist off the
judged bank — the operating-point shift is the version of this that does
not fit on the bank under test), resolver Verbatim-tier coverage (rank 5,
declined with reason).
