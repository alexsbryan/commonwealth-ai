<!-- ledger -->

**five-programs-25 · 2026-09-24 · REVIEW-mint-fp-core-residue (resolve goes to the reader leaf, not core) · director** — this commit
- Needed: the mint worker found that five-programs-24's fork (2), "move resolve to core", breaks a second consumer. corpus-mcp calls `resolve_evidence` and cannot depend on sovereign-core. It asked where resolve lives, whether the cap rises, and whether the ~12-file carve is one row.
- Chose: resolve moves to corpus-engine-atlas-reader, and corpus-engine re-exports it at the historical path. It stays one row, the cap stays 8, and the carve stays one row. I corrected the row text and reopened it (`[ ]`). No code changed. Boundary gate FAILED at 62 violations (reproduced at b553ba70d, EXIT=1).
- Because: five-programs-24 read too much into §12 D1. D1 names the svrn-side policy as `candidate_atlas_ids` plus walk choice, and `candidate_atlas_ids` is already in core. The walk itself is already in the reader, and it went there so corpus-mcp could reach it (ground/mod.rs:13-17). The reader's doc lists resolve as the walk's step 3. Putting resolve in core forces either a twin (principle 8) or a reversal of ei-5a-build-cut, while the existing leaf serves both callers (principle 11).

<!-- appendix -->

## five-programs-25 · 2026-09-24 — resolve joins the walk in the reader leaf; core-residue mint proceeds at 8 rows

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fpcoreres-20260924.md. Reproduced at b553ba70d:

- `cargo xtask boundary-gate` (toolbox, corpus-engine/) → FAILED (62), EXIT=1.
- `git grep -nE "resolve_evidence|EvidenceFetcher|ResolvedChunk" -- '*.rs'` outside resolve.rs finds three crates. corpus-engine's re-export is at enrichment/atlas/mod.rs:94. sovereign-core uses it at atlas_grounding.rs:388 and :500. corpus-mcp uses it at tools.rs:473 and at ask.rs:32 and :64 (`IndexEvidenceFetcher`). The reader's own ground/mod.rs:30-35 and report.rs:282 document it.
- corpus-mcp/Cargo.toml's `sovereign-enrichment-build` comment records ei-5a-build-cut, which cut the sites that dragged sovereign-core, sovereign-tools and sovereign-inference (closure 695 → 590).
- docs/FIVE_PROGRAMS.md:1032 says: "`ground`'s selection POLICY (`candidate_atlas_ids`, walk choice) is the consumer's decision and moves to the svrn side". `candidate_atlas_ids` is at sovereign-core atlas_grounding.rs:25, so that half has already landed.
- corpus-engine-atlas-reader/src/ground/mod.rs:13-17 says the walk "moves down here, where `corpus-mcp` can reach it without taking a dependency the boundary-gate forbids". Step 3 of the same doc is `resolve_evidence`.
- resolve.rs imports `kernel_types::CorpusId`, `crate::types::ScoredChunk` (a corpus-index re-export), `ChunkRequest` and `ChunkSelector`. All of these are inside the reader's allow-list (quality/ARCH_LAYERS.toml, `corpus-engine-atlas-reader`), so no budget changes.

The worker's options were (a) reader, (b) core plus a corpus-mcp dependency on core, (c) a twin, (d) a new leaf, and (e) leave it where it is. I chose (a). (b) reverses a measured cut, (c) violates principle 8, (d) is the operator's §12 3a call, and (e) leaves core's edge red forever. The EvidenceFetcher design already has resolve deciding once for both callers and taking only the fetch from each. That makes it shared walk mechanics, not one consumer's policy.

I kept the carve as one row: the three governance files are whole-file moves with no allow-list change, and a ninth row would break the cap for a mechanical step.

Falsified if moving resolve into the reader needs a dependency outside the reader's allow-list, or if either consumer turns out to need a resolve decision the other must not share (a per-caller budget or scope rule). In that case the policy is not shared and fork (2) reopens.

</details>
