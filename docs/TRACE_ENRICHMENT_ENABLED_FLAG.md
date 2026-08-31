# Trace — does the enrichment-failure path write `enrichment_enabled: false`?

**Date:** 2026-08-07
**Occasion:** two `Unknown enrichment domain: literary` install failures
(`~/.sovereign/logs/daemon.err:37744` corpus=`brothers_karamazov` 17:38:45Z,
`:39185` corpus=`brothers-karamazov-book-1` 19:07:30Z).
**Question asked:** did the failure path WRITE `enrichment_enabled: false` into
`_corpus_meta.json` — a silent §18.3 substitution where the corpus reads
"complete" and enrichment is quietly dropped — or was the flag never set for
that install?

**Verdict: REFUTED**, and the refutation exposes a worse defect than the one
suspected. Method: code reading only; no repro ingest was needed.

---

## 1. The narrow answer — refuted

`enrichment_enabled` on `IndexMeta` is written in exactly **one** place in the
whole workspace, and it is written `false`:

- `corpus-engine/src/index/create.rs:231` — `enrichment_enabled: false,` in the
  fresh-index meta literal.

Every other mention in the repo is a **read** or an unrelated type:

- `corpus-engine/src/types.rs:347` — the `CorpusInfo` field declaration.
- `corpus-engine/src/index/mod.rs:411` — the `IndexMeta` field declaration.
- `corpus-engine/src/index/mod.rs:852` — `enrichment_enabled: meta.enrichment_enabled`
  (meta → `CorpusInfo` projection in `info()`).
- `corpus-engine/src/registry.rs:69` — a **different** field on the registry
  snapshot entry, derived from the recipe, not from the index meta.
- `sovereign/crates/sovereign-tools/src/enrichment_checker.rs:48` — read.
- desktop / CLI / studio sites — all reads of one of the two above.

There is **no setter**. `CorpusIndex` has `set_dedup_by_source`
(`index/mod.rs:982`), `set_display` (`index/mod.rs:1125`), `set_personal_scope`,
`set_grantable`, `set_mutable_merge` — and nothing for `enrichment_enabled`.

So the failure path did not write `false`. Nothing writes it but creation, and
creation always writes `false`.

## 2. The artifact examined is not the failed install's output

The order pointed at `~/.sovereign/indexes/brothers-karamazov-book-1/_corpus_meta.json`
whose `last_updated` (1786129667 = 2026-08-07T19:07:47Z) sits 17s after the
19:07:30Z failure WARN. That correlation is **not causal**:

- `daemon.err:39205-39209` — at 19:08:59Z the daemon downloaded a prebuilt
  snapshot for this corpus (sha256 `924bb447…`, *different* from the
  `e4d5e203…` it fetched at 19:06:04Z) and restored it at 19:09:00.708Z:
  `ingest: prebuilt snapshot restored — skipped full pipeline`.
- `corpus-engine/src/engine/ingest_prebuilt.rs:188-205` — the restore path
  unpacks the archive into `outcome.index_dir` and returns `IngestResult`
  **without rewriting `_corpus_meta.json`**. The meta on disk is the tarball's
  meta verbatim, and `tar` preserves the archive's stored mtimes.

The on-disk file's `created_at`/`last_updated`/mtime therefore describe the
machine and moment that *built the snapshot*, not the local failed ingest.
Its success shape (`ingestion_in_progress:false`, `indexes_built:true`,
`vector_index_built:true`) is the snapshot publisher's, correctly recorded.

## 3. What the failure path actually leaves behind — and it is honest

Order of operations inside `ingest_inner`
(`corpus-engine/src/engine/ingest.rs`):

1. `build_indexes(...)` — line 1679. Stamps `indexes_built` / `vector_index_built`
   / the FTS flags. This is what the log's `FTS title index done` line reports.
2. `'enrichment: { … }` — lines 1737-1970. The field-model construction at
   line 1818, `FieldModelEngine::from_recipe(...)?`, is where
   `Error::UnknownEnrichmentDomain` (`corpus-engine/src/error.rs:88-89`, raised at
   `corpus-engine/src/enrichment/field_engine.rs:74`) leaves the function via `?`.
3. `index.mark_ingestion_complete()` — line 1993, which is the only writer of
   `ingestion_in_progress = false` (`corpus-engine/src/index/create.rs:544-549`).

Because (2) short-circuits, (3) **never runs**. The preserved partial keeps
`ingestion_in_progress: true` — the honest partial shape, not a success shape.

The error then propagates loudly, twice:

- `corpus-engine/src/engine/ingest.rs:414-437` — the `Err(e)` arm logs
  `Corpus '…' install failed (…), but committed chunks exist — preserving for
  resume`, does not touch the meta, and returns `Err(e)`.
- `commonwealth/crates/commonwealth-api/src/routes_internal/corpus_ingest.rs:1150-1160`
  — `record_failure(...)` writes a terminal `IngestProgress::Failed` **before**
  the WARN. The comment there names the exact bug this replaced: "A log-only
  handler here is the bug that made every ingest failure render as a completed
  install." Guarded by `commonwealth/crates/commonwealth-api/tests/corpus_lifecycle.rs:867`
  (`failed_ingest_reports_failed_and_stays_visible`).

So there is no silent substitution on the ingest failure path. The failure is
recorded, surfaced, and the on-disk state stays marked incomplete.

## 4. The real defect this trace found — the flag is inert, and it kills a health check

`enrichment_enabled` is documented as a fact about what happened:

- `corpus-engine/src/index/mod.rs:409-411` — *"True if the enrichment pipeline has
  been run at least once."*
- `corpus-engine/src/types.rs:346-347` — *"True if the enrichment pipeline has ever
  been run for this corpus."*

No code makes that true. **Every corpus on this machine reports
`enrichment_enabled: false` regardless of whether it was enriched** — the field
is a permanently-false constant wearing a doc comment that claims otherwise.

The consequence is measurable, not theoretical. `EnrichmentChecker`
(`sovereign/crates/sovereign-tools/src/enrichment_checker.rs:47-49`) opens with:

```rust
for info in &indexes {
    if !info.enrichment_enabled {
        continue;
    }
```

Because the flag is never true, this `continue` fires for **every corpus, always**.
The loop body — which detects missing field-model tables
(`HealthIssue::LowEnrichmentCoverage`) and interrupted enrichment
(`HealthIssue::StaleEnrichment`) — is unreachable in production. The health
check that exists specifically to say *"enrichment was requested here and did
not complete"* is structurally incapable of reporting a single issue.

That is the reason the `literary`-domain failure had no second surface. The
ingest-time report was correct and loud; the standing health surface that
should have kept saying "this corpus was supposed to be enriched and isn't"
returns clean for every corpus in the fleet.

## 5. Recommendation — **BUILT 2026-08-07** (branch `fix/enrichment-requested`)

**Status.** Both changes below landed as written. What shipped:

- `CorpusIndex::set_enrichment_requested(bool)` (`index/mod.rs`, beside
  `set_dedup_by_source`), called at the entry of the `'enrichment:` block
  (`engine/ingest.rs`) with the value of `install_time_enrichment_expected`
  — `false` for the `investigation` / `atlas` types that are enriched by a
  separate explicit command, `true` for everything else. That predicate is
  one decider (§10.6) and is also what the no-`InferenceFn` arm consults
  (gap 1 below). It replaced a hard-coded `true` at entry plus a
  `set_enrichment_requested(false)` un-stamp inside each early-out, whose
  second write could fail on its own and strand a permanent false positive.
- `IndexMeta.enrichment_enabled` → `enrichment_requested`, with
  `#[serde(alias = "enrichment_enabled")]`; same rename on its projection
  `IndexInfo`. `RegistryEntry.enrichment_enabled` (`registry.rs`) is
  untouched — disambiguating those two is the point of the rename.
- `EnrichmentChecker` reads the new name and can now fire.

Tests: `corpus-engine/tests/enrichment_requested_flag.rs` (entry stamp survives
a failed enrichment; the early-outs never claim the request; a pre-rename meta
still parses) and `sovereign-tools/tests/enrichment_health_e2e.rs` (the
checker's first reachable `LowEnrichmentCoverage`, plus the silent case).

**Three gaps this fix did NOT close** — found while building it, all three
closed afterwards on `fix/enrichment-blind-arms`:

1. **CLOSED 2026-08-07.** The `None`-inference arm (`engine/ingest.rs`,
   "requests enrichment but no InferenceFn was provided … skipping") sits
   OUTSIDE the `'enrichment:` block, so it stamped nothing. A daemon with no
   inference model loaded installed an enrichment recipe, skipped enrichment,
   and the corpus still reported `enrichment_requested: false` — invisible to
   the checker. This is the same class of hole the fix just closed, one arm
   over. It now stamps `true` through the same
   `install_time_enrichment_expected` decider, so the silent skip is visible
   and `investigation`/`atlas` recipes stay exempt on this arm too.
   Watched-to-fail:
   `an_install_with_no_inference_fn_still_records_that_enrichment_was_requested`
   (corpus-engine) and
   `checker_fires_when_enrichment_was_requested_but_no_inference_fn_was_configured`
   (sovereign-tools); with the stamp deleted the meta reads `Some(false)` and
   the checker reports zero issues.
2. **CLOSED 2026-08-07.** `EnrichmentChecker` resolved each corpus with
   `open_index_for_corpus(corpus_id)`, which joins `index_dir/<corpus_id>` and
   therefore cannot open the `<corpus>-partition-<node>/` directory a FAILED
   ingest leaves behind (promotion runs only on `Ok`). The `if let Ok(index)`
   then swallowed the miss silently. So the exact scenario in §3 — enrichment
   dies mid-ingest — was not reported by this checker even with the flag
   fixed. `CorpusEngine::enriched_corpus_ids` already used the robust form,
   `CorpusIndex::open(&info.path)`.

   It turned out to be two defects wearing one coat, and both are now shut:

   - **Resolution.** The checker opens `info.path` — the path the listing
     actually reported — matching `enriched_corpus_ids` (one decider, §10.6).
     The `Err` arm is no longer swallowed: it WARNs that the corpus is
     "neither confirmed enriched nor reported unenriched" (§18.3).
   - **Visibility.** Resolution alone could never have reached the §3 case,
     because `installed_indexes()` drops any directory still flagged
     `ingestion_in_progress` (`engine/mod.rs`, the `is_ingestion_complete`
     gate) — a failed ingest's partition is not on the list to be resolved at
     all. `CorpusEngine::incomplete_ingests()` is the other half of that walk,
     built on the same predicate so the two cannot disagree about which
     installs finished; the checker maps its enrichment-requesting entries to
     the new `HealthIssue::IncompleteIngestPartition` (closed set, new
     variant, `sovereign-contracts/src/health.rs`). Scoped to
     `enrichment_requested` so the enrichment report does not become the
     machine's general ingest-failure log.

   Watched-to-fail, `sovereign-tools --test main enrichment_health_e2e`: with the
   resolution reverted and the partition scan deleted,
   `checker_opens_the_path_the_listing_reported_not_the_canonical_name` and
   `checker_reports_a_failed_ingest_partition_no_listing_can_see` both fail
   `left: 0 right: 1`. Both fixtures are built by re-shaping a REAL ingest,
   not by hand-writing a meta, and both assert first that the old surfaces are
   blind (`installed_indexes()` omits it, `open_index_for_corpus` errors)
   before asserting the new one fires.

3. **CLOSED 2026-08-07.** With an inference function that errors on every
   call, field-model enrichment ran to "Ingestion complete" and returned `Ok`
   with zero field-model tables. The pipeline absorbs per-call errors by
   design — a few unparseable cluster labels must not kill an ingest — and the
   emergent result is that a TOTAL outage is success-shaped (§18.3). Nothing
   at the ingest call site, and nothing in the log after the fact, said that
   the enrichment the recipe asked for had produced nothing.

   Deliberately NOT turned into an `Err`: the chunks are real and the ingest
   did succeed, so failing it would be its own lie and would discard usable
   work. Instead `engine/ingest.rs` wraps the `InferenceFn` it hands to
   `FieldModelEngine` in a counting decorator — local to that call site, so no
   enrichment signature changes — and at completion emits `enrichment
   requested and produced nothing: N/N inference calls failed` when every call
   failed. A `debug!` tally is emitted unconditionally. The guard is
   `calls > 0 && failed == calls`: `0/0` is an absence of work, not an outage.

   Watched-to-fail, `sovereign-tools --test main enrichment_health_e2e`,
   `a_total_inference_outage_says_so_at_completion_and_stays_reportable`: with
   the completion WARN deleted the test fails with an empty match against a
   buffer that contains only the pipeline's own per-call warnings; with it the
   captured line reads

       WARN corpus_engine::engine::ingest: enrichment requested and produced
       nothing: 2/2 inference calls failed corpus=health_corpus
       inference_calls=2 inference_failures=2

   **Instrument finding, recorded because it invalidated an earlier claim.**
   The `sovereign-tools` fixture used by every test in that file was three
   sentences long, chunked to ONE chunk of ~40 words, and the philosophy
   domain's overview filter (`OVERVIEW_MIN_TOKEN_COUNT = 80`) dropped it —
   `overview_chunks=0`. Phase 1 therefore had zero batches, clustering skipped
   itself at 1 < min_cluster_size, and the run made **zero inference calls**
   (measured: `inference_calls=0 inference_failures=0`). So the
   "always-failing inference" those tests pass was never failing anything: the
   corpus ended unenriched because it was too small to enrich. Their
   `LowEnrichmentCoverage` assertions were still true, but no test in the file
   exercised an inference outage until the fixture grew to eight 80+-word
   paragraphs. §18.4 — validate the instrument before the result.

The original recommendation, for the record:

Two changes, both small, in this order:

1. **Make the flag true when it is true.** Add
   `CorpusIndex::set_enrichment_enabled(bool)` alongside `set_dedup_by_source`
   (`index/mod.rs:982`), and call it with `true` at the *entry* to the
   `'enrichment:` block in `engine/ingest.rs:1737` — at the point the recipe
   declares enrichment, before the engine is constructed — not at its exit.
   Setting it on entry is what makes §4's checker useful: the flag then means
   "this corpus was supposed to be enriched", and the checker's job is to
   answer whether it was. Setting it only on success would leave the failure
   case invisible again, which is the defect being fixed.
   The `break 'enrichment` early-outs for `investigation` and `atlas` types
   (lines 1786, 1808) must stamp `false` / not stamp — those recipes are
   deliberately not field-model-enriched at install.

2. **One decider for the meaning.** `IndexMeta.enrichment_enabled` and
   `RegistryEntry.enrichment_enabled` (`registry.rs:69`) are two different
   facts sharing one name — "the recipe asks for enrichment" vs. "this index
   was enriched" (§10.6, §7.5). Rename the index-meta one to
   `enrichment_requested` (with a `#[serde(alias = "enrichment_enabled")]` so
   existing metas keep parsing) and leave the registry field alone.

A watched-to-fail test for (1): install a fixture recipe with a valid
enrichment domain against a stub inference that errors, assert
`EnrichmentChecker::check()` returns a non-empty `issues` — a check that today
returns empty for every possible input, which is §18.1's "a check with no
failing input you can name."

## 6. Disambiguation not needed

The code answer is unambiguous — a single write site, no setter, whole-repo
grep — so the controlled repro ingest the order permitted was not run and the
daemon was not touched.
