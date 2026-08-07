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
