# THE GLiNER2 SPEEDUP IS A PROPERTY OF THE CHUNK-LENGTH DISTRIBUTION, NOT OF THE MODEL. IT IS 2.52x ON SEP AND 1.0x ON THE OBSIDIAN VAULT —…

THE GLiNER2 SPEEDUP IS A PROPERTY OF THE CHUNK-LENGTH DISTRIBUTION, NOT OF THE MODEL. IT IS 2.52x ON SEP AND 1.0x ON THE OBSIDIAN VAULT — THE CORPUS P2.1 ACTUALLY TARGETS.

Measured 2026-08-03, M2 Max, release build, same harness (`sovereign-gliner/examples/typing_audit.rs`), both backends through the production `LabeledEntityExtractor` seam, batches of 8 (production's `corpus_extract_entities_cmd::BATCH_SIZE`).

  ALL 3,175 obsidian vault chunks (p50 1,808 chars)
    v1  881.9 s   3.60 chunks/s
    g2  893.2 s   3.55 chunks/s      <- NO SPEEDUP. g2 is marginally SLOWER.

  50 sep chunks (p50 761 chars), note abc4fb34
    v1  2.87 chunks/s
    g2  7.24 chunks/s                <- 2.52x

MECHANISM. v1's gline-rs stack takes N texts per `inference` call and amortises the fixed cost across the batch; GLiNER2's export is one graph call per text, so its cost tracks text length directly. Double the chunk length and the advantage evaporates. Vault chunks are 2.4x longer than sep chunks.

WHAT THIS VOIDS. The entire P2.1 vault-lane economics chain: "NER is 15m17s of the 29m32s ship candidate (51.7%) -> at 2.52x that is 6m04s -> saves 9m13s -> 1.45x on the ship candidate, 2.56x cumulative from 52m03s". Every step after the multiplier is void, because the multiplier on that workload is 1.0. 2.52x has propagated into ENRICHMENT_ROADMAP.md, ENRICHMENT_ROADMAP_SIZING.md:220 and the P2.1 plan step; all of them are quoting a sep-corpus number as if it were a model constant.

THE GENERAL RULE. A throughput ratio between two extractors is not transferable across corpora unless the chunk-length distributions match. Any future "N x faster" claim must state the p50 chunk length it was measured at, and must be re-measured on the target corpus before it is allowed into a time prediction. The 50-chunk sep fixture is a fine instrument for "does it run on rc.9"; it is not an instrument for "how long will the vault build take".

TAKEN WITH THE TYPING RESULT (note f42cf7ec) THE VERDICT ON P2.1(a) IS: DO NOT ADOPT. Vault typing is worse (mention-level 96.9% v1 vs 81.8% g2; `Ostrom`, the vault's anchor entity, is Person x6 / Organization x6 under g2), volume is 3.4x with 47% of it in a `Work` label acting as a catch-all for ordinary noun phrases (16,053 vs v1's 632), and there is no time to win. What survives is the residency finding (~9 GB lighter, note 3f47d12e) and the `LabeledEntityExtractor` seam (commit 86f83c1a), which is what made this measurable at all.
