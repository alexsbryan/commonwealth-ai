# Gap-check judge inputs must be head+tail windowed, never head-only — head-only truncation files info requests for an enumerated answer's…

Gap-check judge inputs must be head+tail windowed, never head-only — head-only truncation files info requests for an enumerated answer's last item

The post-answer gap check (`sovereign-core/src/gap.rs::identify_gap`) feeds a
Fast-slot judge budget-capped inputs (~1,500 bytes answer + evidence, kept
small because grammar-constrained 9B decoding once made the audit a 55s wait).
**The budget must be spent head+tail (900+600 around an ELISION_MARKER), never
head-only.**

Why: Einstein four-papers false positive (2026-07-15) — answer 2,884
bytes, "Mass–energy equivalence" (paper 4) started at byte 1,942, past the old
head-only 1,500 cut. The judge saw a four-papers question, three papers of
answer, and filed an InformationRequestCard for the fourth paper the user
could already read. Any enumerated answer in the 1,500–4,000 window (below
`ANSWER_SATURATION_CHARS`, above the cap) reproduced this: enumerations put
item N at the tail, and gaps live at tails.

How to apply: if the caps or the saturation guard change, keep the tail
window and the labeled seam (`ELISION_MARKER` says "elided for length" so the
cut can't read as the answer trailing off — an unlabeled seam invites a false
"incomplete answer" verdict). Regression:
`gap::tests::window_keeps_the_enumerations_last_item`. Related work this day:
[[project-verification-counter-2026-07-15]].

---
