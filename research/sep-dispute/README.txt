SEP dispute bank, T0 (feature-fidelity campaign, dispute half; campaign.md Decisions 2026-10-03:
fixture rule, autonomy (2)). Model-free throughout: no chat or embedding calls.

  python3 inventory.py        -> inventory.json  every SEP / dispute-bearing atlas: kinds, pipeline, candidate ids
  python3 score.py --selftest  known-bad answers, each watched failing its check (13 cases)
  python3 score.py --control   positive control: each item's own witnesses read as an answer (11/11 pass)
  python3 census.py            -> census.json     what sep-kant-transcendental-idealism already holds of the gold
  python3 score.py answers.json [-o scores.json]  score answers {"<item id>": "<text>"}

Files. bank.toml: 12 items (11 scored, 1 neutral) over "Kant's Transcendental Idealism" (Stang);
the rule and the five checks are in its header. Witness chunk ids are ~/.svrnmesh/indexes/sep/
chunks.lance `id` (105306-105560); census.py refuses to run if a quote is not in its chunk or a
pattern misses its own witness. audit.toml: the agent's provisional reading of each lexical census
hit; census reports lexical and read numbers side by side, and an unread hit is never counted.

Why this entry: the whole entry is the dispute, and it names each reading with its holders
(Feder-Garve, Strawson / Prauss, Allison, Bird; Langton, Allais; Adickes, Westphal / Aquila, Van
Cleve; Robinson, Ameriks, Adams). sep-kant (49 claims) names three holders in all (Allison 2,
Langton 1, Kitcher 1): too thin for a bank.

Census, 2026-10-03 (38 position-holder pairs, 23 positions, 13 opposing pairs):
  attributed_to is the right holder   lexical 14/38, read 12/38 (+2 credited to a concept named
                                      after the holders, "Feder-Garve interpretation")
  items with every position held      3/11
  Position atoms                      0 (kind absent from all 1,772 SEP atlases; only obsidian-vault has them)
  Tension edge pairs the two sides    3/13 (all one edge, epistemic vs metaphysical dual aspect)
  Tension edge between holders        0/13
  one entity merging both sides       1/13 ("non-identity reading", alias "identity reading")
  tension_candidates.json             COULD-NOT-JUDGE: 0/731 ids resolve (positional claim-0004 ids vs
                                      content-hash atoms). 1,300 of 1,302 non-empty SEP candidate files
                                      are in this state; each predates its atoms.json. The positional
                                      recovery (6/13 pairs) is a substitution; only 404/615 of its
                                      entity_overlap pairs share an attribution, so it is not a number.

Changes made after the first run, both directions reported:
- A position pattern ('deny(ing)? identity') added so every pattern covers ALL its own witnesses
  (the check had been "any"). No census number moved.
- holders_credited window widened from one sentence to two (pronoun follow-ons). Control 6/11 ->
  11/11 (with the control's quote joiner fixed); all known-bad cases still fail, but the d07 swap
  is now caught by no_misattribution only, not by holders_credited.
- A lexical misattribution check over atlas claims was WITHDRAWN: 7 flagged, 0 true on reading
  (each was the holder's own claim against the other side).

Known limits:
- Chunk text lacks the entry's footnotes, numbered lists and tables (e.g. 105438 "And the
  non-spatiality thesis as:" is followed by nothing); holders cited only there do not exist here.
  n01 is neutral for that reason.
- claim_position (21/23) is a loose lexical upper bound; claim-level reading is in audit.toml and
  is the agent's, provisional. The operator's reading replaces it (principle 7).
- The atlas types readings as persons ("dual aspect view", "qualified phenomenalist reading") and
  aliases "transcendental realism" onto "transcendental idealism".
- The per-entry SEP atlases were built by `enrich sep-ingest` (sovereign/bench/sep_atlas/
  run_batch.sh:99-100), pipeline philosophy_atlas (sovereign-pipeline/src/enrich_cmd/
  sep_ingest.rs:203); only 4 have a config.json on disk, so for the rest the pipeline is inferred.
