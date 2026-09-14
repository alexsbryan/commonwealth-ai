# THE OUTLINE A/B DID NOT ANSWER ITS QUESTION — because the arm varied TWO things (2026-08-24/25, task 69, replayed against…

THE OUTLINE A/B DID NOT ANSWER ITS QUESTION — because the arm varied TWO things (2026-08-24/25, task 69, replayed against dr-estate-dr-1787620475, zero Tavily, pinned greedy 27B).

RESULT:
  control-1 49.63 | control-2 45.41  -> mean 47.52, spread 4.22
  outline-1 41.27 | outline-2 46.29  -> mean 43.78, spread 5.02
  DELTA -3.74, WITHIN-ARM SPREAD 5.02 => NOT RESOLVED. Per-dim: compr -4.33, insight -5.37, instr -2.63, read -0.24.
The predicted direction (compr + insight UP, per note 0f5bce03) did NOT appear — both went nominally DOWN.

WHY THE ARM IS UNINTERPRETABLE. The per-section word budget was a FIXED "300-380 words" independent of section count, so section count silently set total length:
  control 23 H2 sections -> 9,084 / 9,354 words   (INSIDE the reference band 6,898-13,348)
  outline  9-10 sections -> 3,702 / 4,053 words   (about HALF the reference floor)
`overall = T/(T+R)` compares head to head, so the outline arm was handicapped ~58% on length while being tested for structure. The mechanism DID fire — the loop logged `report outline planned ... sections=7 frontier=20` on both outline runs — so this is not a plumbing failure, it is a confounded design. Mine.

FIXED IN CODE, not remembered: `synthesize::section_word_budget(sections) = TARGET_REPORT_WORDS / sections`, clamped [300, 1400]. A 7-section and a 20-section plan now target comparable totals. Watched red: `total_length_does_not_ride_on_section_count` fails if the budget is constant.

WHAT MUST BE RE-RUN: outline vs control at MATCHED length on the same cached estate. Until then the outline flag stays off and its reversal condition stays unmet, and NOTHING here says structure is a dead lever — the judge's per-criterion evidence for it (note 0f5bce03) is untouched by this arm.

SEPARATE AND WORTH KEEPING: the replay control is the BEST task-69 result we have ever produced — 47.52 mean, vs the live web flight that BUILT that estate at 43.35, vs Perplexity 44.67 (same judge). Replaying a cached estate scores HIGHER than the live flight that populated it, plausibly because corpus retrieval is embedding-ranked over all 98 sources while the live path is bounded by fetch order. That deserves its own measurement — it would mean acquisition and retrieval should be separated in the loop.
