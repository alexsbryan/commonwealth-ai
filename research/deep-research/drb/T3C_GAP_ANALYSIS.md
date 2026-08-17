# T3C_GAP_ANALYSIS.md — the analytic gap inventory (order `deep-research-t3c`, seat steer 2026-08-17)

The operator's question, answered without a single new measurement: *"Before we
fixate on this 'judge must show us exactly HOW wrong we are' are there some
things we can just assess analytically that are gaps based on the runs we've
already done?"*

Method: **artifacts only.** Every entry below is derived from the existing
transition notes, journals, score reports, and flight artifacts — no judge
calls, no flights, no daemon load, no 122B. Sources: the t1c-t2c transition
notes and journals (`adversarial/pre-registration.md`), the demo journals
(demo2/demo6/demo7/demo9/demo10/demo11), the t2b DRB records
(`drb/runs/`, `demo/demo9/`), `diagnosis/t1h-failure-taxonomy.md`, the t2d
dispositions (`notes/dispositions-t2d.md`), and this order's own
`T3C_AUDIT_FORENSICS.md` (the six pass sites). The **122B half of this order
remains DEFERRED-WAITING-WINDOW in every write-up** — marker unchanged ("the
seat declares the 122B window"); nothing below depends on it.

Each entry carries the five-part shape: (1) artifact citation, (2) mechanism
hypothesis in one sentence, (3) the EXISTING surface that closes it,
(4) cost in session-chunks, (5) rank by expected impact on P2 and on the
product.

## The ranked inventory

| Rank | Gap | The number | Existing surface that closes it | Cost |
|---|---|---|---|---|
| 1 | The admission decider's tie-lottery decides what the audit ever sees | 4/4 hits tied at 0.03333; 117 below_cut, 42 rejects, 1 eps-quota admit; 2 of E's chunks admitted; figure-free thematic chunks win ties | ONE admission decider (`rank_corpus_hits`) — its second key's discriminator; forensics ledger records rejected rows | 1-1.5 (item exists on heap: 6316d01c, 4a140e88 — amended below) |
| 2 | Round-window rotation: a claim's support leaves the window; the audit then records it "untraced" | "untraced: 68" with the figure in round-1's window; 127/161 (79%) union-window mismatch (6/28 = 21% strong-figure) | The evidence-base UNION — the concept the t2c strip correction already uses; forensics ledger records per-claim judged windows | ~1 |
| 3 | The DRB between-arm delta is confounded by zero-pair flights | 3/10 local flights pairs=0 → fab 1.0 each in the paper mean 0.9244; delta −0.5134 descriptive-only | Per-fact rows already exist inside the validate step (dropped at stat time) — persist them | ~0.5 |
| 4 | The abstention dimension and decline class are absent from the DRB measurement | 139 unknowns (134 local, 5 hybrid); 7 decline-shaped claims countable as fabrication | The chaos graded vocabulary (`score_answer`) and `DECLINE_RE` both already exist — uncomposed | ~1.5 |
| 5 | The rendered report passes 83% of claims through with unverifiable figure/citation state | 512/616 verbatim with model-written tails; structured citations on 5/616 | The structured citation channel (evidence_ids/citations from supporting chunks only) — extend to the rendered surface | ~1 |
| 6 | The two-arm lift leg cannot discriminate at the density ceiling | Failed by letter 7 epochs; direction flipped twice; t2c +0.003 | The DRB between-arm comparison — the real between-arm measurement, already measured (−0.5134) | ~0.5 |

## 1. The admission decider's tie-lottery decides what the audit ever sees

1. **Artifact citation.** t1g landing (pre-registration.md ~line 1195):
   "the value-bearing chunks lost a tie lottery to thematically-relevant
   figure-free chunks… a 3-chunk window carrying none of the bank's
   figures; 14/16 keys failed 'missing figures in answer'". t2c execution
   record (pre-registration.md:2483-2489): all 4 round-1 search hits score
   exactly `0.03333333507180214` (the quantized 1/30 bucket, identical to
   the triage threshold), 117 below_cut rows, 42 distinct rejects, 1
   eps-quota admit; prediction 0/10, covered keys K8/K14 outside the
   predicted set (pre-registration.md:2467-2479). t3a compounding record:
   "the frozen admission decider (quantized-bucket triage, threshold
   0.03333) admitted only 2 of E's chunks". demo2 P4 mechanism: "the
   round-1 search returned 4 of 11 deck hits… the one-shot's full-deck
   window clears more keys on the same questions (55/72 vs 52/72 pooled)".
2. **Mechanism hypothesis.** Scores quantize into 30 buckets, so most
   candidates land in the same bucket as the threshold and admission is
   decided by the second key and eps-quota — a tie lottery whose loser is
   the figure-bearing chunk the audit needed.
3. **Existing surface that closes it.** The ONE admission decider
   (`rank_corpus_hits`, gym.rs) — the t2c red-first test's own shape
   (`corpus_admission_second_key_admits_figure_bearing_at_equal_score`)
   asserts figure-bearing wins ties, but the production second key is
   query-term overlap desc, which the t1g measurement shows favors
   figure-free thematic chunks; the discriminator is amendable inside the
   one decider, §10.6. Plus `SOVEREIGN_GATE_AUDIT_FORENSICS` to persist the
   rejected rows (117 exist at runtime; none in the artifacts). This gap is
   already on the heap as items **6316d01c** (pre-fix premise — FALSIFIED
   by t2c's 0/10; its Approach line should be updated by the pull ritual's
   vetting) and **4a140e88** (post-fix residual, approach unknown); not
   re-filed here (§19).
4. **Cost.** 1-1.5 session-chunks (discriminator change + forensics
   persistence), already sized on the heap items.
5. **Rank.** #1 on the product (the local cache's value IS what the audit
   can see — the estate's figures keep losing admission ties, measured
   three orders in a row) and on the v1 clause (2/16 loop vs 11/16
   one-shot at t2c); indirect on P2 (the DRB flights fetch fresh, so the
   tie class is smaller there — but the same admission decider runs).

## 2. Round-window rotation: a claim's support leaves the window; the audit then records it "untraced"

1. **Artifact citation.** `drb/runs/local/drb-56/dr-1786943328/`:
   gap-list-2.json witness `{"ran": true, "specifics": [], "all_absent":
   true, "reason": "claim figures absent from the evidence — untraced:
   68"}` while the figure's only occurrence ("68 languages العربية") sits
   in that flight's OWN round-1 window (`evidence-window-1.json`), not in
   round-2's (`evidence-window-2.json`: BBC_Television_Shakespeare,
   Lucky_7, The_Love_Boat). Battery-wide (T3C_AUDIT_FORENSICS.md pass site
   2): 127/161 (79%) of recorded-untraced claims have ≥1 untraced figure
   present in the flight's own union of windows — 228/287 tokens; strict
   strong-figure variant 6/28 (21%) — 66/107 tokens. The t2c record itself
   names the union as the honest evidence base: "the strip's evidence base
   was corrected to the UNION (survey + acquisition windows)"
   (pre-registration.md:2508).
2. **Mechanism hypothesis.** A claim persists across rounds while the
   window rotates, so a round-2/3 audit judges absence against a window
   the round-1 acquisition already retired.
3. **Existing surface that closes it.** The evidence-base UNION — already a
   running concept in the codebase (the t2c strip correction); the audit's
   presence check should use it, or the forensics ledger
   (`SOVEREIGN_GATE_AUDIT_FORENSICS`) must record the per-claim judged
   window so absence is checkable after the fact.
4. **Cost.** ~1 session-chunk.
5. **Rank.** #1 on P2 (the recorded verdicts — and the re-judge built on
   them — are only as good as the window each claim was judged against; a
   figure the run itself fetched is reported untraced) and #2 on the
   product (the "open question" stamp is the report's honesty surface).

## 3. The DRB between-arm delta is confounded by zero-pair flights

1. **Artifact citation.** T2b execution record (pre-registration.md:2226-2248):
   "Local 62/90/95 flights produced verdict sets whose claims carry no
   citation apparatus… so they contribute pairs=0 and per the declared drop
   rule fab=1.0 to the paper mean" — local paper-mean 0.9244, pooled 0.8706;
   hybrid 0.3571; delta "descriptive only" −0.5134 [−0.6232, −0.3941];
   "the between-arm comparison is read with this asymmetry in mind."
2. **Mechanism hypothesis.** The local arm's number partly measures
   "the draft anchored nothing" (a draft-quality signal), not fabrication —
   three flights counted at fab=1.0 because their claims carried no
   citation apparatus, inflating the paper mean the delta is read against.
3. **Existing surface that closes it.** The per-fact rows already exist
   inside the validate step (the `per_url` dict) — they are dropped at stat
   time (T3C_AUDIT_FORENSICS.md pass site 6); persisting them plus a
   per-flight pairs table in score-*.json makes the asymmetry visible in
   the measurement itself, not only the journal.
4. **Cost.** ~0.5 session-chunks.
5. **Rank.** #2 on P2: the verdict itself rides the hybrid arm alone
   (uncontaminated — hybrid claims do anchor), but the −0.51 the between-arm
   read quotes is descriptive-only and partly measures draft anchoring.
   The re-judge at the 122B window inherits this unless the rows persist.

## 4. The abstention dimension and decline class are absent from the DRB measurement

1. **Artifact citation.** `demo/demo9/score-{local,hybrid}.json`: 134 local
   + 5 hybrid verdicts "unknown" of 219/117 pairs (T3C_AUDIT_FORENSICS.md
   pass site 3). 7 decline-shaped paired claims (6 local, 1 hybrid) with no
   decline class in the vendored validate prompt (pass site 4).
2. **Mechanism hypothesis.** The measurement has no abstention row and no
   decline row, so an honest refusal is scored as fabrication and an
   unknown is scored as nothing — the honesty dimension the order's whole
   verification arm exists to protect is the one the instrument cannot
   name.
3. **Existing surface that closes it.** Both vocabularies already exist:
   the chaos graded ladder (`sovereign-cli-llm/src/bench_cmd/chaos_monkey.rs`
   `score_answer`: hallucination / grounded / caveated_ood /
   honest_abstention / answered_novalue) and the calibration-side
   `DECLINE_RE` (mechanical decline-shape → honest_limitation) — compose
   them into drb-score.py's verdict channel, one decline implementation
   (§10.6). This is the judge-independent half of the pre-registered 122B
   re-judge (pre-registration.md "T3c", abstention never collapsed).
4. **Cost.** ~1.5 session-chunks.
5. **Rank.** #3 on P2: the re-measure's four-verdict discipline — the
   abstention share of the 139 unknowns must be nameable before any
   judge-corrected rate is reported.

## 5. The rendered report passes 83% of claims through with unverifiable figure/citation state

1. **Artifact citation.** T3C_AUDIT_FORENSICS.md pass site 1: 512/616
   claims (83%) render into Open questions verbatim with model-written
   `[Source: …]` tails; structured `citations[]`/`evidence_ids` sit on
   5/616; the "untraced: 68" claim renders "open question: extracted
   specifics absent from the evidence" with `[Source: ev-1]` printed
   beside it (drb-56 report).
2. **Mechanism hypothesis.** The report's rendered surface is the model's
   free text, and the grounding guard enforces only the structured
   channel, so a reader cannot tell figure-verified from figure-untraced
   — or a citation from a tail.
3. **Existing surface that closes it.** The structured citation channel
   itself (evidence_ids/citations populated from supporting chunks only)
   — extend it to the rendered surface: stamp each figure's verification
   state, and strip or structure tails on downgraded claims.
4. **Cost.** ~1 session-chunk.
5. **Rank.** #3 on the product (the shipped report's trust surface — the
   reader-visible half of the honesty constitution); no direct P2 effect.

## 6. The two-arm lift leg cannot discriminate at the density ceiling

1. **Artifact citation.** Lift rows across epochs (pre-registration.md):
   t1d 1.0 vs 0.976 pooled / 1.0 vs 1.0 v1 (line 561-562); t1e 0.883 vs
   1.0 / 0.731 vs 1.0 (669-670); t1f direction FLIPPED 1.0 vs 0.979 /
   0.947 (907-908); t1g 0.938 vs 0.981 / 0.7 vs 1.0, "the direction flipped
   AGAIN" (1208); t2c 0.979 vs 0.976, "+0.003 not +0.10" (2461) — seven
   consecutive failed-by-letter with the threshold's premise ("one-shot at
   the ceiling") inverted twice.
2. **Mechanism hypothesis.** Both arms trace every numeric claim on the
   frozen banks, so the density delta sits at the measurement ceiling and
   the leg reads instrument noise, not loop advantage.
3. **Existing surface that closes it.** The DRB between-arm comparison —
   the real between-arm measurement, already run: estate-only 0.8706 vs
   estate+web 0.3571, delta −0.5134. The lever is the ARM MIX, not the
   loop. The lift leg needs a bar-level disposition (no patch), which the
   runs already justify.
4. **Cost.** ~0.5 session-chunks (a disposition note + bars row, in the
   dr-local-loop transition).
5. **Rank.** #4 on P2 (the leg cannot move a verdict) but the insight is
   the product's: the t2b numbers already say estate-only drafting
   fabricates ~2.4× more than estate+web — the local cache alone is the
   failure mode; the cache + fresh web is the shape that works.

## What the runs ALREADY told us is closed (evidence of the fix path, not gaps)

- **v0 synthesis figure-omission was the #1 miss class and the t1h re-cut
  halved it**: 51/72 (t1g) → 63/72 (t1h) → 65/72 (t2c), bar met. The
  taxonomy's 20 Class-A keys ("the figures sat in the evidence the draft
  was given; the draft text omitted them", `diagnosis/t1h-failure-taxonomy.md` §2)
  were a Synthesis-stage defect the re-cut repaired — the runs proved the
  lever (carry the figures into the draft) before the fix landed.
- **The question-echo passed-position class broke once and was repaired**:
  t1g's single [passed] claim restating the question's own era ("1980",
  "2024", absent from the window) — the load-bearing honesty property's
  only break (pre-registration.md ~1210); the t1h honesty constitution
  restored it and the t2c sweep re-verified it (13/13 clean).
- **Fetch dedup and gap-query breadth were reds that landed** (t1d fixes
  1-2: P3 1/13 → 13/13; round-1 frontier now covers every deck hit), and
  **the strip-3c leak landed at t2c** (heap item 34bd60ae's premise —
  now resolved; the item needs a closure update, not a pull).

## The one-paragraph answer to the operator's question

The runs already tell us, with no new measurement, five things are wrong:
**first**, the audit's evidence view is a tie-lottery subset of what
retrieval returned — scores quantize to 30 buckets, everything ties at
0.03333, and the second key's query-term overlap loses to figure-free
thematic chunks (measured t1g, t2c, and t3a-compounding), which is why the
v1 clause sits at 2/16 against the one-shot's 11/16 on the same deck;
**second**, a claim's support can leave the window between rounds, so the
gate records "untraced" for figures the flight itself fetched — 79% of
untraced claims have their figure in the flight's own union of windows, the
"untraced: 68" case pinned; **third**, the bar-vs-mechanism mismatch is
structural and measured seven times — R-12's convergence bar asks for gap
shrink while the met corroboration floor forces gap growth on single-origin
estates (0/12 × 7, gap sets 1→7→7, 1→15→27, v1 1→26); **fourth**, the
measurement's own honesty dimensions are unmeasured — 139 abstentions never
classified, 7 declines countable as fabrication, per-fact verdicts dropped
at stat time, and the local arm's paper mean inflated by three zero-pair
flights — so the −0.51 between-arm delta is descriptive-only; **and fifth**,
the two-arm lift leg cannot discriminate (failed by letter seven epochs,
direction flipped twice) while the DRB arms already found the real lever:
estate-only drafts fabricate 0.8706 pooled vs estate+web 0.3571, ~2.4× —
the estate mix, not the loop, is what the between-arm numbers move. None of
these needed the judge question; all five are code-and-bar gaps with
existing surfaces named above, and the top five are on the heap for the
pull ritual (filings below).

## Filing record (heap, objective deep-research, artifact citations in Evidence lines)

Filed 2026-08-17 via `svrn backlog add` (producer `deep-research-t3c`):

1. Round-window rotation → audit evidence view (this doc §2)
2. DRB zero-pair asymmetry + per-fact persistence (this doc §3)
3. Abstention/decline instrument composition (this doc §4)
4. Render pass-through → figure/citation stamps (this doc §5)
5. Two-arm lift disposition → the DRB delta is the between-arm measurement (this doc §6)

Heap items 6316d01c and 4a140e88 already cover the admission-decider gap
(§1) — not duplicated; item 6316d01c's premise was falsified by t2c's 0/10
and needs its Approach line updated at vetting. Item 83849ebf already
carries the multi-origin-estate re-cut (dr-compass, §"third" above). Item
34bd60ae (strip-3c) is resolved at t2c and needs a closure update. The 122B
re-judge remains DEFERRED-WAITING-WINDOW; the composition work above (§4)
is its judge-independent half.
