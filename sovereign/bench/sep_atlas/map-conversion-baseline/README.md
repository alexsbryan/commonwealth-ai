# map-conversion baseline (2026-09-08, session 5aece809)

The measurements order `epistemic-index-map-conversion` rests on, kept here
because a scratchpad is not durable. All under the pre-registered map and
the 0.34 / 0.05 gates unless the file name says otherwise.

- `kind-*.txt` — `svrn atlas kind` over each bank: sep 6/21 classified,
  wikipedia 4/20, literary thematic 3/6, five wessex probes, five off-topic
  controls; `kind-floor-0.28.txt` is the same banks at
  SOVEREIGN_QUESTION_KIND_MIN_SIM=0.28.
- `ei2-floor034.json` / `ei2-floor028.json` — the SEP lane pair, same
  binary: facts 152/159 vs 149/159, summaries_appended 120 vs 64.
- `wiki-default.json` — the wikipedia lane: facts 113/130; its ledger showed
  a classified `tension` walk seeding 0 (`seed_kinds_unseen=[Claim, Position]`).
- `ei2-lane.sh`, `score.py`, `yield.py` — the lane runner and the one scorer
  (copied from the seat's ei5c lanes). `ei2-lane.sh <tag> <min_sim>`; edit
  the `S=` root before use.
