# notes_tiered bench

Measures whether the T1 semantic-blend tier in `NoteStore`
(`read_notes_scoped_semantic`) closes the FTS5-recall gap the
2026-05-25 audit (see
`sovereign/docs/specs/NOTES_TIERED.md`) surfaced — synonym /
stem / paraphrase misses where FTS5 alone returns nothing.

This bench has its own runner (a cargo example in
`corpus-engine-notes`), not the standard `sovereign bench all`
discovery surface. The discovery surface assumes a chunked
corpus with `chunks.lance`; notes_tiered scores directly against
a NoteStore SQLite DB, which is a different shape.

## What the bench does

1. Opens a fresh `NoteStore` in a temp dir.
2. Writes the 14 fixture notes from
   [`fixtures/notes.toml`](fixtures/notes.toml). These are
   committed proxies for the 38 notes in the 2026-05-25 audit
   session — same FTS5 failure patterns, but reproducible across
   machines.
3. For each of the 8 audit queries in
   [`fixtures/queries.toml`](fixtures/queries.toml), runs two
   paths:
   - **baseline**: `read_notes_scoped` (FTS5 BM25 only).
   - **semantic**: `read_notes_scoped_semantic` with the
     daemon's embed slot (when reachable; skipped under
     `--no-daemon`).
4. Computes `hit@k` (default `k=5`) against the fixture's
   `expected_hits` — the note ids each query should retrieve.
5. Aggregates per failure class:
   - **synonym**: query uses a word the note doesn't (e.g.
     "wikipedia" vs "wiki" in the content).
   - **stem**: query uses one stem, note uses another (e.g.
     "tokenize" vs "tokenizer").
   - **paraphrase**: same concept, different surface form
     ("EOS bypass" vs "force_continue").
   - **tokenization**: hyphen / underscore / case mismatches.
   - **exact_token**: regression-guard class — both paths
     should hit.

## Running

```sh
# Baseline-only (no daemon needed; runs in <100ms total):
cargo run --release -p corpus-engine-notes --example notes_tiered_bench -- --no-daemon

# Full T1 surface (daemon running with embed slot loaded):
cargo run --release -p corpus-engine-notes --example notes_tiered_bench

# Custom blend weight (0.0 = FTS5-only, 1.0 = cosine-only):
cargo run --release -p corpus-engine-notes --example notes_tiered_bench -- \
    --embed-weight 0.7

# Write a fresh baseline JSON:
cargo run --release -p corpus-engine-notes --example notes_tiered_bench -- \
    --no-daemon \
    --out sovereign/bench/notes_tiered/baselines/notes-tiered/latest.json
```

## Targets (per spec §T1)

| Path | Total hits expected | Source |
|---|---|---|
| audit (2026-05-25) | 5 / 16 | NOTES_TIERED.md problem table |
| baseline (this bench) | ~6 / 16 | reproducible FTS5 regression guard |
| T1 (semantic blend) | ≥ 11 / 16 | spec target ≥6/8 queries hit |
| T1+T2 (entities) | ≥ 13 / 16 | spec target ≥7/8 queries hit |

Latency budget: blend ≤ 50ms p95 on the 14-note fixture (with
the daemon's local embed slot warm; larger fixtures span
multiple gossip rounds for catch-up, not query latency).

## Output

The runner prints a per-failure-class table to stdout plus a
per-query breakdown. With `--out PATH`, the full report (every
query's actual vs expected, latencies, deltas vs audit) lands as
JSON for diffing across iterations. `baselines/notes-tiered/`
holds the committed regression-guard baseline.

## Why not a `[bank]/[[questions]]` TOML?

The existing retrieval-bench discovery
(`sovereign-cli-llm::bench_cmd::discover`) expects a chunked
corpus indexed at `~/.svrnmesh/indexes/<corpus_id>/`. Notes
live at `~/.svrnmesh/notes.db` (a single SQLite file) — the
scoring shape is `Note` containment by id, not chunk-section
containment. Rather than shoehorn notes into the chunk-bench
runner, we ship a focused example binary that owns its
fixtures + scorer.

## Extending

- **New query**: add a `[[queries]]` block to `queries.toml`.
  Include `expected_hits` (fixture note ids), `audit_baseline`
  (the hit count from the original audit, or 0 for new
  patterns), and `failure_class` (one of the existing classes
  or a new one).
- **New failure class**: add it to the catalog in this README,
  add at least one query that exercises it, then write fixture
  notes that produce the FTS5 miss the class describes.
- **T2 entity-graph extension**: when `read_notes_related`
  lands (Phase 2), extend the runner with a third path
  comparing PPR-seeded retrieval against baseline + T1.

## Files

- `fixtures/notes.toml` — 14 notes proxying the audit corpus
- `fixtures/queries.toml` — 8 audit queries
- `baselines/notes-tiered/latest.json` — committed
  regression-guard run output (FTS5 baseline-only mode)
- `../../../corpus-engine-notes/examples/notes_tiered_bench.rs` —
  runner
