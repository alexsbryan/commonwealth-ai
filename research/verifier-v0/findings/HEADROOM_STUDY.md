# Headroom study — incumbent vs rung-1000 on held-out constructed corruptions (2026-08-07)

**The question:** does the shipped gate judge (Qwen3.6-35B-A3B, production
forced-choice register) have measurable blind spots a 4B specialist can beat —
judged by a referee neither model supplies? The gate's shipped 0.00 fabrication
rate is measured BY the incumbent, so it structurally cannot answer this
(§18.1). Labels-by-construction can, and do.

**The answer: yes, and the pre-RLVR checkpoint already beats it.**

## Protocol

- Bank: `data/heldout-sep/bank.clean.jsonl` — 2,494 constructed cases over 130
  fresh SEP windows. Disjoint from rung-1000's training at three levels:
  window ids (training used stride-104 windows; bank is stride-313), chunk ids
  (15 dropped), 13-gram content (116 evidence-overlap cases dropped; 126
  claim-overlap cases flagged in `bank.contamination.json`). ocr_garble
  excluded — referee bug (cosmetic garbles don't change truth value; note
  `7037f3b7`).
- Incumbent scored under its EXACT production procedure: `claim_chunk_support`
  prompt (`grounding/judge.rs:373-381`), passage cap 2,400, forced-choice A/B
  via the daemon's `x_forced_choice` wire path, support = A/(A+B), cosine
  ranking, cap 12, early-exit 0.95, tau 0.5, `oicp.privacy.sharding =
  local_only` (bare `model:"primary"` can be served by a peer — SLOT_POLICY
  §7). Ours: native protocol (no-think + grammar + margin, max-tokens 16),
  same chunk procedure. Each side its own best protocol (the cell-B rule,
  BASELINES.md).
- Scripts: `scripts/headroom_study.py` / `headroom_report.py` /
  `heldout_prepare.py`. Raw rows: `runs/headroom/scored.jsonl` (per-chunk
  support/margins, resumable, kill-tolerant supervisor). Report:
  `runs/headroom/report.txt`.

## Results (n=2,494: 1,364 grounded / 1,130 ungrounded)

| reading | incumbent | rung-1000 |
|---|---|---|
| AUC vs constructed labels | 0.8752 | **0.9622** |
| FA on grounded (excl. multi_hop, n=965) | 3.8% | matched at 3.8% |
| catch on ungrounded (n=1,130) @ that FA | 93.1% (misses 78) | **99.6% (misses 5)** |
| catch delta | — | **+6.5 pts [+5.0, +7.9] bootstrap 95%** |
| miss overlap | 74 incumbent-only | 1 ours-only (4 both) |

Per-kind incumbent miss rates (ours at matched FA in parens): entity_swap
**14.4%** (0.4%), negation_flip **12.2%** (4.1%), unsupported_addition
**7.8%** (0.0%), number_perturb 6.2% (1.2%), chimera/distractor ~0 (0).
Grounded-side FA parity: reframe 3.9% vs 4.1%, verbatim 3.7% vs 3.5%.
Consistent with the training-time teacher-discard rates (10.9/9.1/3.9 under
the HalluGuard register), now confirmed under the production register on
held-out data.

## The standalone finding: the gate structurally fails cross-chunk synthesis

multi_hop_conjunction (TRUE claims requiring both chunks of a window):
**99.7% incumbent FA, 99.5% ours** — both fail, structurally. The production
loop judges chunk-by-chunk and takes the max; no single chunk supports a
two-chunk synthesis. Proof of mechanism: the Stream-B teacher judged the
JOINED window (`teacher_label.py:88`) and false-alarmed only 8.5% on the same
kind. This is a timidity tax on every legitimately synthesized claim in
production, independent of judge model, and is excluded from the FA pool
above. Fix direction: judge top-k chunks JOINTLY (one joined passage) for
claims that fail all singles, or a dedicated multi-chunk pass.

## Caveats (report these with the numbers)

1. **Generator-shape familiarity.** rung-1000 trained on this generator's
   output (different windows, content-disjoint). Its ~0% corruption miss may
   partly reflect learning the generator's signature, not general BS-catching.
   Counterweight: AUC 0.9036 vs the incumbent on REAL production claims
   (substitution study). The kill condition stays armed against this: any
   RLVR/self-play gain must also show on instruments the generator did not
   produce (real-claim agreement audits, FaithBench non-regression).
2. **Corpus coverage.** Daemon churn (co-tenant M5 testing + one kernel OOM
   kill of the daemon) limited the harvest to the first ~43% of SEP
   chunk-space. One corpus, one domain.
3. **Incumbent errors are the daemon's serving of it** — 0 transport errors in
   the final data; verdicts are the production stack's own.

## Addendum: the generator-familiarity control (same day)

172 cases mined from chaos-monkey transcripts — claims extracted by the
production register from REAL model answers, labels mechanical
(value-in-evidence containment, judge-free): 69 real fabrications / 103
grounded. No corruption generator involved. Scripts: `control_mine.py`;
rows `runs/headroom/control_scored.jsonl`; report `control_report.txt`.

**No collapse — the edge replicates directionally, so generator-signature
memorization is ruled out as the explanation of the headroom result.**

- At the SHIPPED operating points (tau 0.5 both): ours misses 13.0% of real
  fabrications, incumbent 26.1% — half the miss rate.
- At matched FA (29.1%): +4.3 pts catch [-1.4, +11.6] — positive, not
  significant at n=69. AUC 0.847 vs 0.829 (near-parity, both far below the
  constructed-bank numbers — real prose claims are harder for everyone).
- The in-domain fabrication class (absent_adjacent, THE production failure
  mode): incumbent misses 50%, ours 12.5% (n=8). Distractor fabrications:
  20% vs 0%. Parametric-leak bait (out-of-domain true facts): both 0% —
  the incumbent does not leak there.
- **Ours' real weakness surfaced: timidity on hard grounded prose** —
  distractor-turn grounded claims 100% flagged (n=7, incumbent 57%),
  provenance-trap grounded 54.5% vs 36.4%. The RLVR target is confident
  support of true prose claims, not more suspicion.

Caveats: n=69 fabrications (5 runs of one 43-question bank; more chaos runs
manufacture more labeled cases cheaply). real_grounded labels verify VALUES
in evidence, not relations — some relation-errors may hide there, inflating
both models' apparent FA equally; and real prose claims include cross-chunk
synthesis, which the per-chunk procedure structurally rejects for both
judges (same mechanism as the multi_hop finding).

## Addendum 2: the bank grown with a contamination-free corpus (same day)

Two more chaos runs (saltgrass r1 + saltgrass_compound r1 — a corpus the
generator has never seen in pretraining, plus compound question shapes)
added 50 mined cases, 0 duplicates: 9 real_fab / 41 real_grounded. Combined
bank n=222 (78 fab / 144 grounded); rows
`runs/controlgrow/combined_scored.jsonl`, report `combined_report.txt`.

**The edge held and tightened: +5.1 pts catch at matched FA (32.6%),
bootstrap 95% CI [+0.0, +11.5] — the lower bound now sits at zero instead
of below it** (was +4.3 [-1.4, +11.6] at n=69). At shipped tau 0.5: ours
misses 12.8% of real fabrications vs incumbent 25.6% — the half-the-miss-
rate pattern replicates on the grown bank. Miss overlap: both 15,
incumbent-only 5, ours-only 1. AUC on real prose: 0.845 vs 0.824.

The grounded-prose timidity also replicates (ours@0.5 FA 38.9% vs 32.6%)
— unchanged as the RLVR target.

**Fab-yield reality check:** saltgrass runs yielded ~4.5 fabs/run against
the ~30/run the n≈300 plan assumed — the ungated saltgrass answers just
fabricate less than secret_agent's. Growing the fab side to
matched-FA significance via chaos runs alone is a many-run grind;
fab volume accumulates free off future chaos runs, but the arc does not
wait on it (the slot decision rests on complementarity, note 700bbe09).

## Addendum 3: the journal-grounded labels are 3/4 vacuous — filter before use (same day)

Journal mining completed over 2,490 gate turns: 1,167 `jrnl_grounded` /
2,072 undecided. A stratified 400-row sample (316 conversations, 60 corpus
groups, ≤2 rows/message, seed 17: `runs/controlgrow/journal/
jrnl_sample400.jsonl`) was dual-judge scored
(`runs/headroom/jrnl_timidity_scored.jsonl`).

Headline FA looked catastrophic and IDENTICAL for both judges — incumbent
54.2%, ours 55.0% at tau 0.5 (both-flag overlap 194 of ~220). That
identity was the tell that the instrument, not the judges, was under test
(§18.4). The decomposition:

| slice | n | inc FA | ours FA |
|---|---|---|---|
| weak labels (entity-only asserted_values) | 303 | 62.0% | 62.0% |
| strong labels (a number or ≥4-word value) | 97 | 29.9% | 33.0% |

**A value-containment label whose asserted_values are bare entity names is
near-vacuous** — "Adam Smith was controversial in his own day" is labeled
grounded because "Adam Smith" appears in the Scottish-Enlightenment chunk.
On such rows the judges are plausibly right to flag, and 76% of journal
labels are of this kind (vs the chaos-mined control bank, whose claims
carry the answer's asserted values). The 12-chunk judging cap was checked
and exonerated: small pools flag MORE (70% vs 50%), not less.

Consequences:
- **The strong-label slice is the only usable timidity baseline**: ours
  33.0% vs incumbent 29.9% at tau 0.5 (n=97) — consistent with the
  control-bank picture, gap small on real prose.
- **RLVR substrate MUST be filtered to strong labels.** Rewarding
  confident support on entity-only rows trains the model to support
  unsupported prose — the exact inversion of the grounded-honest north
  star. Filtered set: `runs/controlgrow/journal/jrnl_grounded_strong.jsonl`
  (308 of 1,167, 26%).
- journal_mine.py needs a label-strength gate (value-bearing assertion
  required) before its output is used unfiltered anywhere.

## Addendum 4: joined-evidence judging — the "timidity" was the procedure, and fixing it doubles the headline (same day)

Hypothesis from the multi_hop mechanism: per-chunk judging structurally
rejects claims whose support spans chunks. Test: rebuild the case files
with `evidence_chunks` joined into ONE passage (no code change — the
scoring loop, prompts, taus and register are untouched, so flips are
attributable to joining alone). Scored: the 97 strong-label journal rows
and the full 222-row control bank, both judges.
Rows: `runs/headroom/jrnl_strong_joined_scored.jsonl`,
`runs/headroom/control_joined_scored.jsonl`.

| instrument | judge | per-chunk | joined |
|---|---|---|---|
| journal strong, FA | incumbent | 29.9% | 34.0% (8 fixed, 12 NEW) |
| journal strong, FA | ours | 33.0% | **18.6%** (14 fixed, 0 new) |
| control grounded, FA | incumbent | 32.6% | **88.9%** (collapse) |
| control grounded, FA | ours | 38.9% | 27.8% |
| control fab, catch @ matched FA 32.6% | incumbent | 74.4% | — (n/a, collapsed) |
| control fab, catch @ matched FA 32.6% | ours | 79.5% | **85.9%** |
| control AUC | incumbent | 0.824 | 0.640 |
| control AUC | ours | 0.845 | 0.848 |

Three findings:

1. **Ours integrates joined evidence; the incumbent cannot.** Joined
   judging flips 14 journal grounded rows to supported with ZERO new
   flags, and holds AUC exactly (0.845→0.848) on the control bank. The
   incumbent flags 89% of grounded claims on joined passages — its
   support score becomes uninformative. The production per-chunk
   procedure is load-bearing FOR THE INCUMBENT ONLY.
2. **The grounded-prose "timidity" was procedure-induced, not a model
   property.** At joined judging ours is 15 pts AHEAD on journal prose
   (18.6% vs 34.0%) where per-chunk had it 3 pts behind. The M3.5 RLVR
   round aimed at timidity is obsolete as scoped.
3. **The headline edge roughly doubles by procedure change alone:**
   catch at matched FA 79.5%→85.9%; bootstrap delta vs incumbent
   (thresholds re-derived per resample) +10.6 pts [+0.0, +20.3] joined
   vs +6.5 [-2.5, +16.2] per-chunk. n=78 fabs still limits tightness.

Slot consequence: the disagreement-triggered slot should run OUR verifier
with JOINED evidence (also a latency win — one call, not up to 12
sequential), while the incumbent keeps its per-chunk production
procedure. Caveat: joined-ours shifts conservative at fixed tau 0.5
(catch 87.2%→80.8%); the operating tau must be re-picked for the joined
score distribution, not inherited.

## Addendum 5: operating tau + what the slot buys over the fast slot (same day)

**Tau re-pick (joined-ours, on the joined score distribution): tau = 0.9.**
Sweep on control catch/FA with journal-strong FA as second instrument:
0.5 → 80.8% catch / 27.8% FA / 18.6% jrnl; 0.9 → 85.9% / 31.9% / 24.7%;
0.95 → 87.2% / 34.7% / 26.8%. At 0.9 ours strictly dominates the
incumbent (74.4% @ 32.6%, jrnl 29.9%). Rationale for the high-catch
point: in a disagreement-triggered slot an FA costs one escalation, a
miss can pass a fabrication the incumbent also missed. Fallback tau 0.5
documented if the escalation budget binds. Caveats: n=78 fabs; the score
distribution is coarse (0.85/0.9 identical); re-check against production
disagreement telemetry once the slot runs.

**Vanilla-4B arm — what owning the slot buys.** Comparator:
`Qwopus3.5-4B-v3-MTP-Q6_K` — the model the daemon's fast slot was
swapped to on this date — under the IDENTICAL joined protocol, prompt,
grammar and margin extraction. Rows
`runs/headroom/vanilla4b_{control_joined,jrnl_strong_joined}_scored.jsonl`.

| metric (joined, control bank) | rung-1000 | vanilla fast-slot 4B |
|---|---|---|
| AUC | 0.848 | 0.763 |
| catch @ matched FA 32.6% | 85.9% | 67.9% |
| @ tau 0.5 | 80.8% catch / 27.8% FA | 71.8% catch / 37.5% FA |
| journal-strong FA @ 0.5 | 18.6% | 20.6% |

The training buys **+18 pts catch at matched FA** and better-on-both-axes
at any fixed tau; it is the difference between BEATING the incumbent
(+11.5) and LOSING to it (−6.5 — the vanilla fast slot at 67.9% does not
reach the incumbent's 74.4%). "Point the fast slot at the judging prompt"
is not a viable slot; the checkpoint is what makes the slot worth its
memory. On easy grounded prose the two 4Bs are close (18.6% vs 20.6%) —
the delta is almost entirely fabrication *detection*.

Instrument checks: incumbent joined re-run reproduced 222/222 verdicts
(deterministic); vanilla produced 0 null/unparseable verdicts (grammar
complied — a fair arm, not a parse-failure artifact).
