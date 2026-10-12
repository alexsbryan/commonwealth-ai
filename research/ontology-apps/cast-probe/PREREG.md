# Cast probe: pre-registration, 2026-10-12, before any number is computed

Question. Identity across documents is decided by RESOLVE alone, offered candidates by two proposers (the declared
thread; TF-IDF over whole documents, `resolve_records/propose.rs`), and no stage keeps a register of particulars
(the cast: each type's records with the words that name them). Invariant 3 asks that a proposer's recall be
reported; none is. This probe measures, per declared type RESOLVE decides, on all six runs/about legs (ours and
blind, Ward, uv, GVC): whether the cast has the particular, whether the right record is put in front of the decider,
and whether the decider takes it; then replays generic proposers the cast would add, with no model call. It decides
which proposers the cast is built from and gives the party slice (ec, in build) its link-step instrument.

Units and gold, the ladders' own (`ladder.py` placements, one run each, runs/about): Ward, a tune message in exactly
one gold deal (49); uv, a tune gold document and its case; GVC, a gold event mention and its chain (statements
mapped to mentions by the GVC ladder's alignment of cited lines). Blind legs read gold through the evaluator's frozen
map (`blind-author/<tag>-mapping.json`). A type gold does not label (blind-gvc `involved_person`) is could-not-judge.

Order. RESOLVE's own document order, read from `resolve_decisions.jsonl`. A unit is *given* when an earlier unit of
the same gold particular already sits on a record; the *right records* are those holding an earlier unit of it.

Measures, per leg and type.
- R1 inventory: the share of gold particulars with a record (the ladder's one-to-one match).
- R2 proposal recall: of given units, the share whose document was offered a right record (the logged candidates).
- R3 decision given proposal: of given units offered a right record, the share whose statement went to one; beside
  it, the share where the model's argmax was a right record, so a weigher override is visible.
- Novelty: of units not given, the share that opened a record (R3's other half: a first naming must open).
- R2' replayed proposers, each a ranking of the records open before the unit's document, recall at 1, 3 and 10:
  N name: the words About and Mention pointed at in the unit's document, matched to the words pointed at in each
  record's documents (normalised exact, then shared-token share); E embedding: the production embedder
  (qwen-embedding-0.6b, cite-probe gates) over the unit's pointed words plus its statement's cited sentence, against
  each record's, max cosine; D declared reference: records holding a document the unit's document names by a
  declared field (uv `target_url`; Ward `base_subject` as the extractor's second thread; GVC declares none, so D is
  never-ran there); and the union with the logged candidates. Mean candidates shown, beside each.

Bars, fixed now.
1. A replayed proposer is worth building when, at recall at 10, its union with the logged candidates raises R2 by
   at least 10 points on at least two of the three systems (either author), and lowers it nowhere (it cannot: a
   union) while adding at most 10 candidates per unit on average.
2. The cast is the bottleneck where R2 is below .80 on a leg; the decider is where R2 is at least .80 and R3 below
   .80. Both are read per leg, so the slice that is built (proposers, decider, or both) is the one the leg shows.
Reported, no bar: R1, novelty, the weigher-override share, per type. One run each, so a difference of one or two
units is inside run-to-run churn (measured 2026-10-08 at about 10 items each way on Ward's stage).
