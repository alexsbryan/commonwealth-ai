# R-12 v0 — the fixture-vs-bar evidence package (order deep-research-t6c)

For the operator's disposition call on the R-12 v0 leg. Written from the
t6c forensics + the measured batteries; the re-measured row from
revolution 1's battery is filled in at the end (the battery re-flights
the frozen bank, zero API cost).

## What R-12 gates

The scorer's R-12 leg (arms/score-arms.py:744-752 — read-only for
this order EXCEPT the one leg the t6c steer re-cut by disposition,
directive 9bf1d984, option 2): the OLD instrument was
`strict = all(sets[i] < sets[i-1])` over the consecutive round
gap-TEXT sets — the open-question ledger must shrink STRICTLY every
round (subset, not just smaller); the NEW instrument (this landing)
is the non-growth premise `all(sets[i] <= sets[i-1])`, bar ≥10/12
v0 seeds. Old strict-shrink citations stay labeled ("R-12
strict-shrink, retired 2026-08-18").

## The mechanism — why a v0 gap can never close

The loop's closing path has one door: a gap's claim re-audited on the
next round's window, judged supported by ≥2 DISTINCT origins (the
corroboration floor, CORROBORATION_FLOOR=2, audit.rs) — a passed claim
leaves the ledger; everything else re-enters as a gap (verbatim). A
claim can therefore close ONLY when its evidence spans two origins.

The v0 estate decks are single-origin by construction: one source
article per fact (frozen bank/seeds.md; a mock deck holds one or few
documents per topic). The floor is on. A single-origin claim can never
collect two distinct source_urls, so every v0 gap claim caps at
could-not-judge for the whole run. The ledger can only stay equal or
grow; it can never shrink. The r1→r2 transition compounds it: round 1
with an empty window emits the abstention gap, whose text is not in the
round-2 content set — that pair can never be a strict subset either.

## The measured record

- t1c/t1h/t6b batteries: R-12 v0 = 0/12 every revolution (score-report-
  t6b.json: "R-12 failed 0/12 v0 (structural)").
- v0 seed-01 trajectory: 1 → 4 → 5 (grows), and the fold validation on
  the same data: round-3 stays 4 = round-2 — equal, never strict.
- The fold cannot manufacture a v0 pass: it only merges duplicates and
  keeps canonical first-seen texts; strict shrinking requires gaps to
  CLOSE, which requires two origins, which the fixture forbids.

## The reading: fixture vs bar

One of the two is unreachable given the other:

- FIXTURE (single-origin decks) → shrinking is impossible → R-12 v0
  fails by construction, whatever the loop does.
- BAR (strict-subset on ≥10/12) → demands a fixture where gaps can
  close — two-origin decks, or a floor-off configuration.

The disposition is the operator's call (the bank and the bars are
frozen for this order; the loop code is what t6c changes). Options,
without recommendation:

1. Re-cut the 12 v0 decks to two origins per fact (fixture change —
   frozen-bank exception) and re-measure R-12 v0 as a real leg.
2. Transition the v0 R-12 bar to "the gap set stops growing" (the
   trajectory gate this order applies to v1) — a bar change.
3. Gate R-12 on the v1 row only (journaled) and retire the v0 leg as a
   structural canary.
4. Keep as-is: the leg documents the floor's honest accounting on
   single-origin fixtures; R-12 remains 0/12 until a fixture re-cut.

## What this order's revolution-1 battery re-measures

The 12 v0 flights + v1 through the folded loop (runs-t6c root). The v0
R-12 row is REPORTED, never gated: expected 0/12 (structural). The v1
trajectory is the order's gate (final ≤ round-2, set stops growing).

## Operator decision (2026-08-18)

Option 2, verbatim: "option 2 for R-12". The v0 R-12 leg transitions to
the non-growth premise — the gap set never grows across rounds on the
single-origin estate (>=10/12 seeds); the strict-shrink premise is
closed by decision (descoped transition in quality/initiative-bars.toml,
dr-compass bar, directive 9bf1d984). The bar text is re-cut accordingly;
the scorer's R-12 leg is re-cut to the same premise (t6c steer,
pre-registered before re-scoring; old-instrument strict-shrink citations
stay labeled). The t2d freeze held the text pending exactly this call.

## Revolution-1 battery result (filled at landing)

(battery = 13 flights, runs-t6c root, 18:13→19:12 PDT on the
D2-fixed daemon, plus seed-01/02 re-flights after the daemon-503
start; the one-shot arm was reaper-killed in-flight and re-run
manually — all 13 pairs freshly written. Score report:
research/deep-research/arms/score-report-t6c.json, scorer the
re-cut R-12-nongrow leg.)

**v0 row under the re-cut premise:** R-12-nongrow 0/12 — the
literal all-pairs formula fails the r1→r2 pair on EVERY seed (the
round-1 empty-window abstention gap closes at r2 and is never in
the r2 content set — the same pre-registered artifact that made
strict-shrink unreachable). The fold's real effect is visible on
the content-rounds pair: r2→r3 NON-GROWING on 9/12 seeds (01, 02,
03, 04, 05, 08, 10, 11, 12 stable; 06 11→12 and 09 4→6 added
new-figure re-expressions of window evidence — 11%, May 8 2025,
a valuation — the r3-draft seam; 07 2→38 is a degenerate r3
draft's fragment-claims, draft corruption not fold misses).
The literal formula is implemented verbatim per the directive; the
intent-vs-formula question (the disposition's option-2 text reads
as the v1-style final-pair gate) is flagged for the operator.

**v1 trajectory (the order's gate):** 1 → 27 → 35 — NOT converged
(35 > 27). The pilot's 1→39→66 became 1→27→35 (r2 merged 38
content claims to 26; r3 = 27 verbatim prior + 8 new: 6
new-figure re-expressions of the round-2-acquired second origin,
2 figureless fragments). KEPT the fold — large compression with
honest accounting; the growth seam is the r3 DRAFT re-expressing
newly-acquired evidence, the pre-registered rev-2 target.

**No-regression legs vs score-report-t6b.json (bars unchanged):**
P3 12/13 → 12/13 clean; T1.7 12/12 → 12/12 clean; two-arm lift
failed in both batteries (pooled 1.0 vs 0.979 → 0.797 vs 0.807).
Two legs crossed: P4-v1 (loop) 13/16 → 11/16 (below the ≥12/16
bar) and honesty (loop 0.0 vs one-shot 0.021 → 0.203 vs 0.193).
Mechanism check on every dropped key: the figures WERE in the
round-1 evidence windows (100/2014/2007/0.7/6.7/53; 589b; 2.6b;
4.1/4.5) — all five drops are ANSWER-SIDE figure omissions in the
sampled draft, not acquisition failures (the fold's only query-
side channel), and the untouched one-shot arm moved in the same
battery with zero code change (density 0.979→0.807, coverage ±3
keys, ungrounded 0.021→0.193). The crossings sit inside the
battery's own noise band; no fold-attributable regression is
demonstrated, and no-regression is not demonstrated at the bar
level either. The rev-2 battery (draft-seam fix, pre-registered)
is the second sample that separates noise from trend (§18.5:
one run is not a measurement).
