# GLiNER2 DOES NOT FIX TYPE-COLLAPSE — IT MISTYPES ~1 MENTION IN 3 WHERE v1 MISTYPES 1 IN 294. AND THE ENTITY-LEVEL SCORE HIDES IT COMPLETELY.

GLiNER2 DOES NOT FIX TYPE-COLLAPSE — IT MISTYPES ~1 MENTION IN 3 WHERE v1 MISTYPES 1 IN 294. AND THE ENTITY-LEVEL SCORE HIDES IT COMPLETELY.

Measured 2026-08-03, M2 Max, release build, through the PRODUCTION seam (`LabeledEntityExtractor`), so these are the labels `chunk_entities` would actually store. Harness: `sovereign-gliner/examples/typing_audit.rs` (committed). Oracle: `research/enrichment-spikes/data/typing_oracle_sep.json` — BonJour, Sosa + 17 philosopher surnames that must be `Person`. Fixture: the 269 sep chunks that actually mention BonJour or Sosa (targeted, not sampled).

                              v1        GLiNER2
  entity level (dominant)   17/17      17/17
  MENTION level (rows)   293/294     167/248
                          99.7%        67.3%

Sosa under GLiNER2: Person x75, Work x33, Organization x24, Location x1, Event x1. BonJour: Person x22, Work x17, Organization x3. Under v1 both are Person every single time.

THE METHOD TRAP, AND IT IS THE POINT. The first version of this audit scored only the DOMINANT label per surface form and reported 17/17 vs 17/17 — clean parity, adopt it. Adding a per-mention column inverted the verdict in the same run on the same data. `chunk_entities` is a MENTION table: a minority mistyping is a wrong row on disk, not a rounding error. ANY future extractor comparison must score mentions, not entities; an entity-level aggregate over a mention-level store is not a measurement of that store.

SECOND TRAP, ALSO LIVE. On a 20-chunk slice the same audit showed bare `bonjour` -> Work as the DOMINANT label — the 2026-08-02 eyeball defect. On 269 chunks Person dominates and the defect disappears from the dominant view while remaining in a third of the rows. A small fixture can make a systematic minority error look categorical; a large one can make it look like none. Neither is the number; the rate is.

WHAT IS NOT CLAIMED. Not a recall verdict: GLiNER2 produces MORE mentions overall on the same fixture (1511 vs 1226). Its extra volume is real, it just lands elsewhere while these named entities are both under-found (248 vs 294 mentions) and mistyped. The pattern in the head-to-head table is person-names-associated-with-works drifting to `Work` (bonjour, cezanne, el greco, matisse, renoir, sosa, kelp, recanati, soames).

CONSEQUENCE FOR P2.1. The roadmap's ordering — "(a) the conversation/vault path, replacing v1, fixing type-collapse by extracting types jointly" — is NOT supported by this stack. Speed (2.52x, note abc4fb34) and residency (~9 GB lighter, note 3f47d12e) stand; the drop-in QUALITY case does not. `SOVEREIGN_GLINER_MODEL_ID` therefore ships default-off with a DEFAULTS_LEDGER row whose flip condition includes "no per-label typing regression" — this run is the evidence that condition is currently unmet.
