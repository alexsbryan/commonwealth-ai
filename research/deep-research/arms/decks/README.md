# The v0 estate decks (arms fixtures)

One deck per bank-v0 seed (`seed-01` … `seed-12`), served to the loop via
`--backend mock --mock-deck research/deep-research/arms/decks/seed-NN`.

## Provenance (named, never silent)

Each deck carries ONE hit: the **estate exemplar** — a document authored at
this mint from the seed's own coverage keys (bank/seeds.md) restating the
key facts (names, dates, figures, causal links) as the estate's document for
that question. The exemplar is the seed content in document form — the same
NWCI-authored facts, not a fabricated citation to a nonexistent source. The
URLs are `https://estate.example/seed-NN` (the mock estate's document
identity). The deck README, this file, is the provenance record.

## The single-origin shape (pre-registered consequence)

The v0 decks are intentionally single-origin: one document per question.
The loop's corroboration floor (GAP-2) caps every single-origin claim at
could-not-judge, so the v0 flights' reports will carry their coverage in
Open questions, not Findings. That is a MEASURED honesty result of this
bank's shape, not a scoring bypass: P4 coverage is scored per the bank v0
README semantics (named + evidence-supported over the answer+evidence
artifacts — honesty and coverage scored separately, never blended,
DEEP_RESEARCH.md P2), and the floor's caps are reported per run. The v1
source deck (bank/v1/deck/) is the two-origin shape by contrast — the same
measurement on the report-class question with real multi-source support.

## The run surface

- Loop arm: `sovereign deep-research run --question "<seed question>" --backend mock --mock-deck research/deep-research/arms/decks/seed-NN --max-rounds 3` (drafts delegated to the local daemon).
- Match tokens per deck are the question's distinctive nouns (OR-matched, case-insensitive substring) so every gap query re-lands on the exemplar.
