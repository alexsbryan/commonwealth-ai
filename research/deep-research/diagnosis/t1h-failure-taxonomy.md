# T1 failure taxonomy — why 51/72 (and 2/16), classified by METHODOLOGY's thirteen stages

Order: deep-research-t1h (`work-order/v1`), Phase 1 deliverable (a0).
Date: 2026-08-16. Operator's rigor directive (2026-08-16): *"we need to
get rigorous and scientific as to why we're only achieving 51/72 — our
method in METHODOLOGY.md is sound and I'd want to make sure we're not
missing any of it."*

Method: every missed key from the t1f+t1g batteries is classified by the
stage of METHODOLOGY.md's thirteen that let it drop, with the flight
artifacts as evidence (fetch lists, skip ledgers, evidence windows,
drafts, verdict sets, score reports, the ceiling probe, the t1g landing
journal). Every citation below was read first-hand from the artifact;
none is recalled. The stage-presence audit at the end judges each of
the thirteen stages four-verdict (passed / failed / could-not-judge /
never-ran) per METHODOLOGY's prescriptive rule — *a stage with no named
anchor or no checkable gate is not finished design*.

## 1. The measurement

The t1g battery (score-report-t1g.json, scored 2026-08-14, C-class
deterministic scorer):

| Leg | Measured | Bar | Verdict |
|---|---|---|---|
| P4-v0 | 51/72 | ≥58/72 | failed (5th consecutive: 52/49/52/53/51) |
| P4-v1 (loop) | 2/16 | ≥12/16 | failed |
| P3 | 13/13 | ≥10/13 | passed |
| R-12 | 0/12 | ≥10/12 | failed (5th consecutive, structural) |
| T1.7 plan presence | 12/12 | all scoped flights carry | passed |
| two-arm lift (pooled) | 0.938 vs 0.981 | loop ≥ one-shot +0.10 | failed BY LETTER, direction flipped AGAIN |
| two-arm lift (v1) | 0.7 vs 1.0 | loop ≥ one-shot +0.15 | failed BY LETTER |
| honesty letter | loop 0.062 vs one-shot 0.019 | loop ≤ one-shot | failed |
| honesty load-bearing | — | zero untraced figures in [passed] position, ANY arm | **FAILED — first epoch the property broke** |
| P5 | 6/6 | no noise band | passed (demo/p5/verify.sh) |

**The operator's 51/72 decomposes exactly.** 72 − 51 = 21 v0 misses =
20 Class-A figure omissions + 1 Class-B causal omission, both at
Synthesis. 16 − 2 = 14 v1 misses = 11 Class-C corpus-triage boundary
losses + 3 Class-D keys unreachable under the frozen scorer. The union
with the t1f battery adds 2 more Class-A keys (seed-09 K3, seed-10 K4)
that the t1g drafts happened to recover — 37 missed keys in total, of
which 34 are the loop's defects this order can repair and 3 are the
frozen arbiter's (the bank-key fork, the operator's call).

## 2. Class A — Synthesis figure-omission: 20 v0 keys + 2 t1f-union keys

The figures sat in the evidence the draft was given; the draft text
omitted them. 20 keys missed in t1g, all `missing figures in answer`
with non-empty in_evidence; 2 more (seed-09 K3, seed-10 K4) missed in
t1f and recovered in t1g — the same mechanism, per-flight lottery.

| seed | key | EV (in evidence) | ANS | stage that dropped it |
|---|---|---|---|---|
| seed-01 | K2 | 32 b | — | Synthesis |
| seed-01 | K3 | 23 b, 2024 | — | Synthesis |
| seed-02 | K4 | 589 b, 1 t | — | Synthesis |
| seed-03 | K4 | 9.5 mo, 8 dy | — | Synthesis |
| seed-03 | K6 | 2014, 4.2 b, 2.6 b, 1.5 b, 2025 | 1.5 b, 2025 (partial) | Synthesis |
| seed-03 | K7 | 2025, 400 | 2025 (partial) | Synthesis |
| seed-04 | K4 | 10, 40, 1.10, 4.40 | — | Synthesis |
| seed-04 | K5 | 87.5%, 85% | — | Synthesis |
| seed-05 | K4 | 35 m, 7%, 15 m, 3% | — | Synthesis |
| seed-07 | K3 | 14 hr, 75 dy | — | Synthesis |
| seed-08 | K2 | 26.3 b, 2021, 36% | — | Synthesis |
| seed-08 | K3 | 2000 | — | Synthesis |
| seed-08 | K5 | 2025 | — | Synthesis |
| seed-09 | K2 | 15, 75, 3 | — | Synthesis |
| seed-09 | K4 | 4.1, 4.5 | — | Synthesis |
| seed-09 | K6 | 2025, 183 b | 2025 (partial) | Synthesis |
| seed-10 | K5 | 78.6% | — | Synthesis |
| seed-11 | K2 | 1 t, 2023, 2 t, 2024, 3 t, 4 t, 2025 | 4 t, 2025 (partial) | Synthesis |
| seed-11 | K4 | 160 | — | Synthesis |
| seed-12 | K6 | 2025 | — | Synthesis |
| *seed-09* | *K3* | *2.0, 2025* | *— (t1f only)* | *Synthesis* |
| *seed-10* | *K4* | *20, 200, 1.25, 10, 0.25, 2* | *— (t1f only)* | *Synthesis* |

**Evidence, first-hand.** seed-04 (`arms/runs/loop/seed-04/dr-1786852805`):
evidence-window-1.json chunk ev-1 carries "OpenAI released o3 and
o4-mini on 2025-04-16 — the first OpenAI reasoning models with vision
built into the model itself" (digits 10/40/1.10/4.40 in the window).
draft-2: "Based on the evidence provided, OpenAI released o3 and
o4-mini in April 2025 to defend its position at the frontier of
reasoning models against competitors. Specifically, …" — **zero
numeric figures**; the model even spelled the date out. seed-01
(`arms/runs/loop/seed-01/dr-1786852692`): evidence-window-1.json
carries "…for approximately $32 billion", "…$23 billion offer…";
draft-2: "Google acquired Wiz to close the security gap with AWS and
Azure, betting on cloud-security consolidation…" — no 32, no 23, no
2025. The figures were in every round's window; no draft carried them.

**Mechanism.** The drafting surface (`synthesize.rs` draft_round 38-99)
feeds the model an evidence block plus "Still-open specifics to
resolve" gap rows — but there is NO deterministic figure inventory in
the prompt: no code-enforced enumeration of the window's figure tokens
that the answer must carry. Sub-questions never enter the draft prompt
at all. Figure presence in the answer is therefore entirely the model's
discretion, unconstrained by any structural guarantee — precisely what
§7.6 forbids ("never ask a model to guarantee what code can enforce").
The scorer's in_evidence lists prove the evidence carried the digits;
the drafts prove the model dropped them; the fix (H2) puts a
deterministic figure inventory into the drafting surface.

## 3. Class B — Synthesis causal-omission: 1 v0 key, journaled, not predicted

seed-01 K4: EV[-] ANS[-], reason "causal elements not named:
['principal','wiz','co-founder','ceo','assaf','rappaport']". The window
carries the sentence verbatim ("Wiz co-founder and CEO Assaf Rappaport
remained a named principal"); the drafts never name him. Same stage as
Class A, but there is no deterministic carrier for it: the scorer's
subject set is a semantic list, not figure tokens — no code-enforced
inventory can list entities the way it lists digits. H2 (below) does
not predict this key; it is journaled as the surface's known boundary.
A T2 entity-inventory would be the repair; out of this order's scope.

## 4. Class C — Triage, the corpus-leg boundary: 11 v1 keys

The bank's value figures exist in the deck, the corpus retrieved
towards them, and the triage admitted the wrong chunks — the window
ended up carrying era figures only, and the draft could not name what
it was never given. 11 keys, all `missing figures in answer` with the
required figures present in the deck but EV[-] in the window:

| key | figures the deck carries (demo6/deck-extract/) | in window? | in answer? |
|---|---|---|---|
| K1 | 58.1%, 51.9%, 50.6%, 50% (source-report.md:3,23) | no | no |
| K2 | 0.5469 (wikipedia-states.md; source-report.md:5) | no | no |
| K4 | atlanta, dc, 95/20 (causal elements) | no | no |
| K5 | 325.78, 225% (source-report.md:13) | no | era years only (2024/2000) |
| K6 | 177%, 92% (source-report.md:13) | no | era years only (2000) |
| K7 | 9.6, 12.2, 4.6 (source-report.md:13) | no | no |
| K10 | 1979 | no | no |
| K11 | 7 pp, 53% | no | era years only (2000) |
| K12 | 80% | no | 1980 (question-era restatement) only |
| K15 | 100, 2014, 2007 | no | era years only (2000) |
| K16 | 35%, 31%, 19% | no | no |

**The mechanism, read from the flight's own artifacts**
(`arms/runs/loop/v1/dr-1786853676/`, the t1g corpus flight):

1. **The scores quantize.** fetch-list-1.json: every admitted hit
   scores exactly `0.03333333507180214` (= 1/30 in f32). survey-1.json
   shows the wider surface is scored (8 hits, distinct buckets
   0.0333→0.0262), but the top bucket is a mass tie: 5-6 chunks at the
   exact same f32 value, including the four admitted (29 governing, 64
   source-report, 40 stanford ×2).
2. **The decider is blind to bodies.** triage's tie-break is
   score-then-figure-bearing (`triage_hits`, acquisition.rs:247-261;
   ADMISSION_RULE_SCORE_THEN_FIGURE), and `figure_bearing`
   (acquisition.rs:223-225) reads `!figure_tokens(title).is_empty() ||
   !figure_tokens(snippet).is_empty()`. The corpus surface (gym.rs
   estate_search 473-539) fills `title` with digit-free document names
   and `snippet` with a 600-char term-centered estate_snippet cut —
   the Gini-bearing chunk's snippet centers on the query terms, not the
   digits. The PortHit/SurveyHit/SearchHit structs carry NO content
   field at all. So inside the top bucket every figure test is false
   and admission degenerates to insertion order.
3. **The value-bearing chunk lost by one rank.** skip-ledger-1.json:
   `{"url": "estate:dr-demo6-v1:65", "title": "source-report",
   "score": 0.03333333507180214, "rank": 6, "reason": "below-cut",
   "decision": "skip"}` — chunk 65, the source-report chunk carrying
   Gini 0.5469 (source-report.md:5), one rank below the K=3 + eps=1
   cut. 111 skip entries in all; the ledger records no figure tie-break
   because the decider had no body to read.
4. **The window is era-figure-only.** evidence-window-1.json: chunk 0
   carries 20 percent, 2000, 1990s, 39 of the 50, 54, since 2000;
   chunks 1-2 carry no digits at all. None of the bank's value figures
   entered the window; the draft's era-year answers (2024/2000/1980)
   are restatements of the question's framing and the window's era
   figures.
5. **Budget exhausted in round 1.** budget-ledger.json: 19 entries,
   12/12 spent in round 1 — no round-2 recovery possible (the loop's
   done-partial termination at mod.rs:1040-1051).

This is the DEMO-6 boundary evidence the t1g journal documented
(pre-registration.md, T1 rung-2 journal: "LanceDB's hybrid relevance
scores QUANTIZE to identical f32 buckets … the top-k admission
degenerates to insertion order"). The corpus leg RETRIEVES — direct
search probes hit the source-report chunk carrying "Gini coefficients
exceeding 0.54" — the R5 triage boundary is where the figures die.
The fix (H1) gives the decider the body and the window the content.

## 5. Class D — ceiling-journaled, unreachable under the frozen scorer: 3 v1 keys

arms/ceiling-probe.json (declared in pre-registration.md, Ceiling
probe): v1 keys=16, content_ceiling=13/16, floor_ceiling=15/16.

| key | why unreachable |
|---|---|
| K3 | content_reachable=False — figures ['7.87',':1'], ['7.81',':1'], ['172476'], ['22095']; the ('7.87',':1') canonical pair never appears in the deck (the deck writes 7.87:1 without a space); no deck text can satisfy the scorer's tokenization |
| K9 | content_reachable=False, floor_reachable=False — "cannot clear" under the frozen arbiter journal, no deck-supported form |
| K13 | content_reachable=False — ['0.7','pp']: the 'pp' unit order never matches a deck text under the scorer's pair extraction |

These are not loop defects; they are the frozen bank/arbiter's own
ceiling, evidenced by the probe before this order's re-measure. The
bank-key design is the operator's fork (the order's escalation path),
not repaired here. 13/16 stands as this order's content ceiling.

## 6. The honesty red — Claim gate+render (with a Synthesis origin)

The t1g v1 flight's single [passed] claim (verdict-set.json c1;
report.md line 7):

> **[passed]** Based on the evidence provided, American cities
> underwent dramatic economic and demographic transformations between
> 1980 and 2024 characterized by accelerating gentrification,
> increasing inequality, affordability challenges, and displacement.

**The window does not carry 1980 or 2024.** evidence-window-1.json's
digit inventory: chunk 0 = {20, 2000, 1990, 39, 50, 54}; chunks 1-2 =
∅. The era years are a restatement of the question's own framing
("How did American cities change across four decades (1980-2024)…",
charter.json) — a traced-once paraphrase, not fabrication — but the
scorer's density row flags them untraced (traces=false,
nums_in_window=[] — the window carries no 1-digit at all, per the t1g
journal). The claim PASSED anyway.

**Why the gate let it through.** The composed gate (audit.rs
assess_claim): the containment witness (`containment_witness`,
containment.rs:286-375) downgrades a claim to CouldNotJudge when ALL
of its specifics are absent from the window — and
`witness_presence` (containment.rs:178-186) is
`specifics.iter().any(|s| present(s))`: ANY present blocks the
downgrade. c1's specifics are the mixed set
["1980","2024","American cities","Gentrification"]; "American cities"
(chunk 1: "American cities have experienced a fundamental
transformation") and "Gentrification" (chunk 0) are present → the
witness fires → all_absent=false → no downgrade → the corroboration
floor passes (3 origins ≥ CORROBORATION_FLOOR=2,
audit.rs:281-289; corroboration origins
[estate:dr-demo6-v1:33,:4,:50], passes_floor=True) → **passed**. The
thematic words' presence masked the numeric specifics' absence.

**Classification.** Two stages share this break: (1) Synthesis — the
draft's unconstrained restatement of the question's era framing put
untraceable figures into claim text (same root as Class A: no
deterministic figure discipline in the drafting surface); (2) Claim
gate+render — the witness's ANY-present rule over a mixed
numeric+thematic specifics set let an untraced figure through to
[passed] position. The gate is the named red (the t1g break is this
order's constitution; honesty is never traded). The fix (below) is a
downgrade-only strengthening at the witness: when the claim's
specifics include numeric-class specifics, at least one numeric
specific must be present for the witness to fire — thematic presence
alone can no longer mask numeric absence. c1-type claims become
CouldNotJudge, never failed, never passed-untraced.

**Record note (named, not silent).** score-report-t1g.json's honesty
bar note ("zero untraced numbers sit in [passed] position in ANY arm
(both epochs, journaled)") is a stale t1f-era template — the t1g
landing journal (pre-registration.md, T1 rung-2 journal) supersedes it
with the per-claim evidence: "honesty — FAILED on BOTH the letter AND
the load-bearing passed-position property, the FIRST epoch where the
load-bearing property broke… the scorer's own density row flags it
untraced (traces=false, nums_in_window=[])". The t1f journal's "the
load-bearing property held" is epoch-scoped: it held through t1e/t1f,
it does not hold for the corpus flight. The bars note is the aggregate
template; the artifact is the evidence — §18.1's watched-fail
discipline is exactly why the per-claim artifact outranks the summary
line.

## 7. Stage-presence audit — all thirteen, four-verdict

Per METHODOLOGY's prescriptive rule: a stage with no named anchor or no
checkable gate is not finished design. Every anchor below is cited to
code or artifact.

| # | Stage | Anchor | Checkable gate | Verdict |
|---|---|---|---|---|
| 1 | Charter | charter.json per flight (max_rounds 3, evidence_window_max_chunks 20, code_set_k 3, eps_quota 0.1, budget 12/12); budget-ledger.json | budget exhaustion → done-partial (F12); every flight terminates with a terminal recorded | **passed** |
| 2 | Plan | build_plan (mod.rs:609-628); plan.json artifact (queries_preplanned, figure_specifiers) | T1.7 plan presence: 12/12 flights carrying (each figure-implying flight's plan carries a digit or measure word) | **passed** |
| 3 | Survey | survey_estate (estate.rs:260-314); SurveyHit; survey limit 8 (mod.rs:935) | survey-1.json written with scored hits; limit enforced (v1: 8 hits, distinct buckets 0.0333→0.0262) | **passed** |
| 4 | Gap audit | build_gap_list → gap-list-{round}.json; GAP-2 corroboration floor (audit.rs:281-289, CORROBORATION_FLOOR=2); GAP-3 residue rows (mod.rs:1144-1153, render.rs:189-193); GAP-4 reframe block (mod.rs:990-1036, render.rs:79-101) | gap lists grow per round (v0 seeds 1→2→4); residue renders in report.md Open questions; reframe runs before the budget check | **passed — GAP-2/3/4 verified LIVE** (floor vetoes single-origin claims: 19/20 v1 claims CouldNotJudge "single-origin support"; residue + reframe sections in the rendered report) |
| 5 | Acquisition | form_queries, figure_hunt_query (acquisition.rs:208-214), figure_specifiers fold-in, ONE budget-key decider (source_budget_key — the t1g wiring fix) | budget ledger decisions; exhaustion terminates | **passed** |
| 6 | Triage | triage_hits (acquisition.rs:247-261), ADMISSION_RULE_SCORE_THEN_FIGURE, code_set_k + eps_admits, skip ledger (111 entries, v1) | the rule is checkable (score→figure→insertion) — and it FAILS on the corpus surface: figure_bearing reads title+snippet only; the surface supplies neither the body nor digit-bearing text; the top bucket ties exactly at 1/30 and admission falls to insertion order | **FAILED — THE H1 STAGE** (11 Class-C keys; chunk 65 rank 6 below-cut, skip-ledger-1.json) |
| 7 | Fetch+custody | estate_window (mod.rs:661-693) with custody stamp (estate chunks stay personal, never re-stamped); chunk-level estate locators (the t1g dedup fix) | custody veto in assess_claim; evidence-window-{round}.json written (mod.rs:1233); estate_urls_without_a_surface_refuse_loudly (gym.rs) | **passed** |
| 8 | Enrichment | F5/F14 named in the F-table (gym.rs 744+); derived-vs-primary RowStatus tag | none — the gate-side discount of derived rows is T2-scoped | **could-not-judge (thin, named but not gated)** |
| 9 | Synthesis | draft_round (synthesize.rs:38-99); draft-{round}.json artifacts | none deterministic — the prompt carries evidence blocks + "still-open specifics" but NO figure inventory; sub-questions never enter the draft surface; figure presence in the answer is model discretion | **FAILED — THE H2 STAGE** (20 Class-A + 1 Class-B keys; seed-04/seed-01 draft-vs-window pairs) |
| 10 | Claim gate+render | assess_claim (audit.rs:144-269): empty window→never-ran; single-string judge (claim_violation_joint, p≥tau→failed); containment witness (downgrade-only); custody veto; corroboration floor; render.rs [passed]/[could-not-judge] stamps | the composed gate is checkable — and it let c1 through: witness_presence ANY-present over mixed numeric+thematic specifics masked the numeric absence | **FAILED — THE HONESTY STAGE** (c1, first epoch the load-bearing property broke) |
| 11 | Termination | F12; done-partial budget-exhaustion path (mod.rs:1040-1051) | every flight reaches a terminal state; terminal in manifest | **passed** |
| 12 | Estate persists | custody stamps, estate: URLs, chunk-level locators | custody veto; surface-refusal test | **passed** |
| 13 | Gym | MockBackendImpl corpus surface (estate_search gym.rs:473-539, with_corpus 430-440); 28-row F-table (744+); mock retrieval T1.9 | corpus_surface_retrieves_and_fetches_the_value_bearing_chunk + loop-level corpus_source_retrieves_value_bearing_chunk_into_window (gym.rs; the t1g red-first pair, watched fail → green) | **passed** — the surface retrieves; the triage boundary past it is stage 6's verdict |

Summary: 10 passed, 1 could-not-judge (Enrichment — thin), 3 failed
(Triage, Synthesis, Claim gate) — and every fix below names exactly the
stage it repairs. The audit also answers the operator's question: the
method's stages are present and anchored; the three gaps are *within*
stages (the triage decider's blindness, the draft's missing inventory,
the witness's ANY-present rule), not missing stages.

## 8. Lineage naming gap — dr-corroboration / dr-residue / dr-reframe

GAP-2 (dr-corroboration), GAP-3 (dr-residue), GAP-4 (dr-reframe) are
live and met in the loop (section 7, stage 4 — floor vetoes, residue
rows, reframe block, all verified first-hand this session; verdict
directive 90a064c4). But the ORDER LINEAGE never names them: t1b's D0
promise (.sovereign/features/deep-research-t1b/order.md:22-26, 61 — "at
landing the serves line is amended to name them") was never executed.
Every t1 order's serves line cites only `deep-research dr-local-loop`
(t1b/t1c/t1f) or `deep-research dr-local-loop dr-compass`
(t1d/t1e/t1g/t1h); none names the three gap bars. This is an accounting
gap — the bars are met behaviorally but no order owns their lineage —
and it is recorded here so the fix lands where the promise said it
would (this order's serves line is inherited, not amended; the
amendment belongs with the bars' transition record at landing).

## 9. The fixes — stage, mechanism, predicted recovery

Every fix names the stage it repairs and the keys it predicts to
recover; each is pre-registered in adversarial/pre-registration.md
BEFORE any re-measure (§18.6).

| Fix | Stage repaired | Mechanism | Predicts to recover | Ceiling note |
|---|---|---|---|---|
| **H1** corpus-leg triage boundary | 6 Triage | The hit surface carries the body: PortHit/SurveyHit/SearchHit gain `content: Option<String>` (serde default); gym estate_search fills it (`content: Some(r.content.clone())`); the CLI estate_search fills it for parity; `figure_bearing` extends to title+snippet+content — ONE decider preserved (§10.6); `estate_window` uses content-or-snippet so admitted bodies reach the draft | 11 Class-C keys (K1, K2, K4, K5, K6, K7, K10, K11, K12, K15, K16) — deterministic: chunk-65-type hits now beat insertion order inside the 1/30 top bucket | 13/16 content ceiling stands (K3/K9/K13 remain frozen-arbiter unreachable) |
| **H2** draft figure-completeness | 9 Synthesis | draft_round appends a deterministic figure inventory: figure_tokens per window chunk (mod.rs:229-252 — the ONE figure decider), plus the instruction that every evidence-supported figure must appear in the answer; inventory applies to both rounds | 20 Class-A keys + the 2 t1f-union keys (seed-09 K3, seed-10 K4) | compliance is measured by the battery, never assumed (§7.6: the inventory is code-enforced into the PROMPT; the model's carrying is the measurement) |
| **Honesty** witness numeric-specificity | 10 Claim gate | witness_presence (containment.rs:178-186): when the claim's specifics include numeric-class specifics (figure_tokens non-empty), at least one numeric specific must be present for the witness to fire; thematic presence alone cannot mask numeric absence. Downgrade-only — never converts a pass into a fail; passed→CouldNotJudge at most | zero untraced figures in [passed] position in ANY arm (the constitution (e)); the c1 shape becomes CouldNotJudge | floor/witness never weakened; the gate only gets stricter on numeric claims |

**Dispositions this order does NOT repair.** Class B (seed-01 K4) —
journaled: no deterministic entity carrier; a T2 entity-inventory would
be the repair. Class D (K3/K9/K13) — the frozen arbiter's own ceiling;
the bank-key fork is the operator's call (escalate per the order).
R-12 (0/12, fifth) and the two-arm lift (direction flipped again, this
time the one-shot side) are metric-and-bank properties already forked
at landing — the loop's gap growth is the floor's honest disclosure,
and the corpus flight's thin window is H1's measurement, not a separate
defect. None of these is touched; the battery re-measures them all and
reports four-verdict.

## 10. What the re-measure will show

Against the frozen banks, same protocols (budget 12/12, max-rounds 3,
model pin daemon :9741, one-shot comparator, P5 6-flight drill):
P4-v0 should recover toward 72−1 (Class B remains) with H2's inventory
carrying the Class-A figures; P4-v1 should recover toward the 13/16
content ceiling with H1's admission; honesty must show zero untraced
figures in [passed] position in ANY arm. The battery's numbers, not
this taxonomy's predictions, write the dr-local-loop transition at
landing.
