K2 semantic queries (feature-fidelity campaign, Ontology: "New instrument, K2 semantic queries")

  python3 k2_bank.py [--audit]   -> bank-k2-dev.toml   (no network; reads ~/.svrnmesh/indexes/ft-ans-dev-b)
  python3 k2_t0.py               -> k2-t0.json         (one call to the local /v1/embeddings, model "embed")

The rule is in k2_bank.py's docstring; T0's in k2_t0.py's. --audit prints every mint mention per
hoard with its verdict (content / not_content / negated / implied), the trail behind each gold fact.

bank-k2-dev.toml: [[hoards]] and [[mints]] are the name tables a scorer resolves answers with.
Each [[questions]] row has the K1 fields (id, category, question, expected_facts, notes) plus
class, answer_type, answer (gold ids | int | one id), neutral (ids scored as neither found nor made
up), open_world_sensitive, implied_facts, and per gold item `witnesses`: alternative fact lists,
each fact with its attesting chunk ids. Counts and superlatives carry needed_facts instead.

Known limits, measured 2026-10-03:
- uncertainty has no rows: none of the truth-uncertain mints of the dev hoards present
  (Cyzicus, Thebes, Chios, Hierapolis Bambyce, Soli, Lyttus) is named anywhere in the fixture.
- 18 of 48 rows rest on one implied fact (Demanhur chunk 49); see implied_facts / notes.
- 51 chunks: k=80 is the whole corpus, so RAG@80 is 1.0 by construction.
- Gold is truth AND text. IGCH truth omits mints the text lists (IGCH 1444's table names
  Lampsacus, Sardes, Miletus, Side; truth has none of them); those are neutral, not gold.
