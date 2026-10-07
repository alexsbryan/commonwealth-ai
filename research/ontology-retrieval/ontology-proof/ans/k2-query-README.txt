K2 query-layer probe (campaign.md, Ontology: "Query-layer probe")

  python3 k2_query.py                          # 48 questions x 2 runs on the local "primary" -> k2-query.json (~4 min)
  python3 k2_query.py --from-json k2-query.json  # rescore stored queries, no model calls
  python3 k2_query.py --limit 1 --runs 1 --json /tmp/x.json   # smoke test

The rule and the constraint path (file:line) are in k2_query.py's docstring; the exact schema, system
prompt and rendered ontology documentation are stored in k2-query.json (prompt_sha256 pins them).

Columns: parse = executed == k2_t0 program on the atlas records (the pre-registered metric);
non-vac = the same, excluding agreement on an empty / 0 / None program answer; goldrec = agreement on
gold-complete records (k2_bank's attested facts, same Records interface); correct = expressible and
agreeing on both record sets; coinc = agreement on a question the grammar cannot express.
Instrument checks run first: the reference query must reproduce the program, and the program over
gold-complete records must reproduce gold; both are reported at the top of every run.
