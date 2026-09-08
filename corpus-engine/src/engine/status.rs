// SPDX-License-Identifier: AGPL-3.0-or-later
//! The corpus-status row — THE one decider for "what corpora exist under
//! this indexes dir, and what state is each in" (sv-surface rung 1).
//!
//! Ported from `sovereign-cli-llm/src/corpus_cmd/status.rs` (2026-09-08),
//! where it was born private to the CLI: the desktop had
//! `CorpusEngine::installed_indexes` for the same question and the two
//! walked the same directories with different rules — the §10.6 twin this
//! campaign exists to delete. One decider, three consumers now: the CLI's
//! `corpus status` prints these rows; the daemon serves them at
//! `GET /internal/corpus/status` (sovereign-mesh `reading_http`) for the
//! wire; the desktop's attach-mode future consumes the route.
//!
//! Pure function of the filesystem — no daemon, no async, no LanceDB opens;
//! it reads one small JSON per candidate directory.

use std::path::Path;

use serde::Serialize;

use crate::enrichment::tokens::read_token_snapshot;
use crate::Corpus;

/// Whether a corpus is usable on disk — THE one decider for that question
/// (§10.6), so `corpus status`, `corpus install --wait` and the daemon's
/// status route cannot drift apart.
///
/// It delegates the actual judgement to
/// [`crate::index::CorpusIndex::is_ingest_finished`] — NOT to
/// `is_ingestion_complete`, which answers the narrower "is a writer active
/// right now". That predicate is true for an ingest that stopped without
/// ever building its indexes, so a readiness that used it printed `ready`
/// for corpora no retrieval path would touch (`corpus_unavailability`
/// refuses `NotBuilt` before it looks at the query). One of them,
/// `wikipedia-newsworthy`, cost a chaos-soak triage two wrong conclusions:
/// the app's honest "I cannot search this corpus" was read as a fabricated
/// system status, because the status surface contradicted it about the same
/// corpus. Before that decider existed, the same question was answered by
/// asking whether a DIRECTORY existed — a second, wrong, implementation of
/// "is it installed", and the one that reported an ingest 0 seconds old as
/// an installed corpus.
///
/// Four states, not a boolean, deliberately: `Building` is the state a
/// naive surface has no name for and therefore renders as success, and
/// `Unsearchable` is the one it has no name for AFTER that. Same shape as
/// `sovereign-ci-bench.sh`'s `PASS(warn:setup)` — when the thing you would
/// judge is not there yet, say THAT rather than pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CorpusReadiness {
    /// The canonical index exists and its ingestion is fully committed.
    /// This is the only state in which `enrich init`, `chat --corpus` and
    /// search can open it.
    Ready,
    /// Bytes are landing: a partition directory exists, or the canonical
    /// directory is present but still flagged `ingestion_in_progress`.
    Building,
    /// On disk, no writer running — and the indexes were never built. The
    /// ingest STOPPED rather than finished, so every retrieval path refuses
    /// this corpus (`UnavailabilityReason::NotBuilt`) even though nothing is
    /// in progress and the directory looks complete. Distinct from
    /// `Building`, which will resolve on its own; this one will not, and
    /// wants a rebuild.
    Unsearchable,
    /// Nothing on disk for this corpus id.
    Absent,
}

impl CorpusReadiness {
    /// Lowercase, single-word, greppable — the CLI-contract journey
    /// asserts on these exact strings, so they are API, not decoration.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Building => "building",
            Self::Unsearchable => "unsearchable",
            Self::Absent => "absent",
        }
    }
}

/// See [`CorpusReadiness`]. Pure function of the filesystem.
pub fn corpus_readiness(indexes_dir: &Path, corpus_id: &str) -> CorpusReadiness {
    let canonical = indexes_dir.join(corpus_id);
    if canonical.is_dir() {
        if crate::index::CorpusIndex::is_ingest_finished(&canonical) {
            return CorpusReadiness::Ready;
        }
        if crate::index::CorpusIndex::is_ingestion_complete(&canonical) {
            // No writer running, yet the ingest never built its indexes.
            // Reported `ready` until 2026-08-28 — see the type's docs.
            return CorpusReadiness::Unsearchable;
        }
        // The canonical dir exists but its ingest never committed — a
        // process killed mid-embed. `installed_indexes()` skips it; so do
        // we, and we say why rather than listing it as installed.
        return CorpusReadiness::Building;
    }
    // No canonical dir. An ingest in flight writes
    // `<corpus_id>-partition-<node_id>` and the canonical directory is
    // materialised ONLY by the finalise/merge step (see
    // `CorpusEngine::partition_path`), so a partition is exactly the
    // "still building" signal.
    let Some(partition_prefix) =
        Corpus::named(indexes_dir, corpus_id).map(|c| c.partition_prefix())
    else {
        return CorpusReadiness::Absent;
    };
    if let Ok(read) = std::fs::read_dir(indexes_dir) {
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(&partition_prefix) && entry.path().is_dir() {
                return CorpusReadiness::Building;
            }
        }
    }
    CorpusReadiness::Absent
}

/// One row per CORPUS (never per directory) — the shape the CLI prints and
/// the daemon's `/internal/corpus/status` serves.
#[derive(Debug, Clone, Serialize)]
pub struct CorpusStatusRow {
    pub corpus_id: String,
    /// Whether this corpus can actually be opened — see [`corpus_readiness`].
    pub state: CorpusReadiness,
    /// The readiness decider's greppable spelling, served beside the typed
    /// state so a shell over the route (`curl … | grep ready`) and the CLI's
    /// own column read ONE spelling, not two renderings of one enum.
    pub state_label: &'static str,
    #[serde(skip)]
    pub from_canonical: bool,
    pub chunk_count: Option<usize>,
    pub atlas_entities: Option<usize>,
    pub atlas_extracted_entities: Option<usize>,
    /// Rows in `atlas/atoms_ann.lance` -- the atoms the walk can seed on.
    /// `None` means no seed table, which is a different fact from a table
    /// covering zero atoms and is never rendered as `0`.
    pub atlas_embedded_atoms: Option<u64>,
    pub atlas_embeddings_cached: bool,
    /// Cumulative tokens spent in the corpus's `<corpus>-tier2`
    /// workspace's most recent extract run (Phase D2). `None` when
    /// no `_tokens.json` sidecar exists yet — i.e. Tier-2 hasn't
    /// run for this corpus.
    pub tier2_total_tokens: Option<u64>,
}

/// Scan `indexes_dir` into one row per CORPUS (not per directory).
///
/// Two rules, both of which a by-directory-name version got wrong:
///
/// 1. **A row is a corpus, keyed by the `corpus_id` in its
///    `_corpus_meta.json`** — never by the directory name. An in-flight
///    ingest writes `<corpus>-partition-<node>/`, and naming the row after
///    the directory invented a corpus called
///    `journey-fixture-partition-node-3148a89c1ae48238` that no one can
///    install, remove, or query.
/// 2. **Readiness comes from [`corpus_readiness`]** — so a corpus whose
///    bytes are still landing reads `building`, not a row
///    indistinguishable from a finished install.
///
/// The canonical directory's numbers are preferred when it exists — the
/// one `enrich init`, `chat` and search actually open. Same preference
/// `installed_indexes()`' `dedupe_by_corpus_id` applies.
pub fn scan_corpus_rows(indexes_dir: &Path) -> std::io::Result<Vec<CorpusStatusRow>> {
    let mut by_id: std::collections::BTreeMap<String, CorpusStatusRow> =
        std::collections::BTreeMap::new();
    for entry in std::fs::read_dir(indexes_dir)?.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with('.') || name.starts_with('_') {
            continue;
        }
        // The corpus this directory belongs to — its meta's `corpus_id`,
        // which a partition dir carries verbatim (observed: the partition
        // `journey-fixture-partition-node-…` declares
        // `"corpus_id": "journey-fixture"`). Fall back to the directory
        // name only for a dir with no readable meta, which is also the
        // only case where the two can legitimately disagree.
        let corpus_id = read_meta_corpus_id(&path).unwrap_or_else(|| name.to_string());
        let state = corpus_readiness(indexes_dir, &corpus_id);
        let is_canonical = name == corpus_id;
        if let Some(existing) = by_id.get(&corpus_id) {
            if !is_canonical && existing.from_canonical {
                continue;
            }
        }
        let mut row = read_corpus_status_row(&corpus_id, &path);
        row.state = state;
        row.state_label = state.label();
        row.from_canonical = is_canonical;
        by_id.insert(corpus_id, row);
    }
    Ok(by_id.into_values().collect())
}

/// The `corpus_id` a directory's `_corpus_meta.json` declares, if any.
fn read_meta_corpus_id(dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(Corpus::meta_in(dir)).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.get("corpus_id")
        .and_then(|x| x.as_str())
        .map(str::to_string)
}

/// How many chunks a corpus holds, and WHICH field said so.
///
/// The source is returned with the number because the two fields do not
/// mean the same thing: `enriched_chunks` is a count the enrichment pass
/// wrote, `next_chunk_id` is the id allocator, and a corpus that was
/// deduped or resumed can make the second disagree with reality. A caller
/// that asserts on this number (`svrn quality lane chat-ask` does) must be
/// able to say which field it read; before 2026-09-04 there was no such
/// caller and `corpus status` simply printed `-` for every
/// workflow-ingested corpus, because only the first field was consulted and
/// the notebook workflow never writes it.
///
/// Lance is deliberately not opened — far too heavy for a status line.
pub fn corpus_chunk_count(dir: &Path) -> Option<(usize, &'static str)> {
    let meta: serde_json::Value = std::fs::read_to_string(Corpus::meta_in(dir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())?;
    if let Some(n) = meta.get("enriched_chunks").and_then(|n| n.as_u64()) {
        return Some((n as usize, "enriched_chunks"));
    }
    // `next_chunk_id` is the NEXT id to hand out, so the count is one less.
    meta.get("next_chunk_id")
        .and_then(|n| n.as_u64())
        .and_then(|n| n.checked_sub(1))
        .map(|n| (n as usize, "next_chunk_id - 1"))
}

fn read_corpus_status_row(corpus_id: &str, dir: &Path) -> CorpusStatusRow {
    // Chunks: one decider, see `corpus_chunk_count`.
    let chunk_count = corpus_chunk_count(dir).map(|(n, _)| n);

    // Atlas: use the cached summary helper so a) the count agrees
    // with what mesh gossip advertises (Phase C1) and b) repeat
    // status calls don't reparse atoms.json on every invocation.
    let atlas_dir = dir.join("atlas");
    let summary = crate::enrichment::atlas::read_or_compute_atlas_summary(&atlas_dir)
        .ok()
        .flatten();
    let (atlas_entities, atlas_extracted_entities, atlas_embedded_atoms) = match summary {
        Some(s) => (
            Some(s.atom_count as usize),
            Some(s.tier2_count as usize),
            // Through the summary, which is where the seed-table row count is
            // derived once (ARCH 10.6) -- not by opening the Lance table here.
            s.ann.as_ref().map(|a| a.embedded_atoms),
        ),
        None => (None, None, None),
    };
    let atlas_embeddings_cached = atlas_dir.join("atoms.embeddings.bin").exists();

    // Phase D2: read `<enrichment>/<corpus>-tier2/_tokens.json` if
    // the Tier-2 workspace has run at least one extract pass.
    // <enrichment> is sibling of <indexes> — derive from the
    // corpus dir's grandparent.
    let tier2_total_tokens = dir
        .parent()
        .and_then(|p| p.parent())
        .map(|data_dir| {
            data_dir
                .join("enrichment")
                .join(format!("{corpus_id}-tier2"))
                .join("_tokens.json")
        })
        .and_then(|p| read_token_snapshot(&p))
        .map(|r| r.total_tokens);

    CorpusStatusRow {
        corpus_id: corpus_id.to_string(),
        // Overwritten by `scan_corpus_rows` from the one decider. The
        // pessimistic default matters: a future caller that forgets to set
        // it under-claims rather than inventing a readiness it never checked.
        state: CorpusReadiness::Building,
        state_label: CorpusReadiness::Building.label(),
        from_canonical: false,
        chunk_count,
        atlas_entities,
        atlas_extracted_entities,
        atlas_embedded_atoms,
        atlas_embeddings_cached,
        tier2_total_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Write a `_corpus_meta.json` these tests can turn on.
    ///
    /// EVERY FIELD BELOW IS LOAD-BEARING. `IndexMeta` (`index/mod.rs`)
    /// makes eight of them mandatory — `corpus_id`, `corpus_name`,
    /// `embedding_model`, `embedding_dimensions`, `mesh_sharing`,
    /// `license`, `created_at`, `last_updated` — and `read_meta` returns
    /// `Err` for the whole file if one is missing. `is_ingestion_complete`
    /// then maps that `Err` to `false`, so an under-specified fixture reads
    /// as "not complete" and every readiness assertion in this module fails
    /// for a reason that has nothing to do with readiness. (It did, on the
    /// first run.)
    ///
    /// That failure direction is the correct one — an unparseable meta is
    /// not an installed corpus — but it makes the fixture's completeness
    /// part of what these tests assert, so do not trim this down.
    ///
    /// The ordinary two states. `indexes_built` is derived as
    /// `!ingestion_in_progress` here because that is what a HEALTHY ingest
    /// looks like — but the coupling is exactly the assumption that broke
    /// `corpus status` (a stopped-but-unfinished ingest has neither flag
    /// set), so the stalled case needs [`write_meta_with`].
    fn write_meta(dir: &Path, corpus_id: &str, ingestion_in_progress: bool) {
        write_meta_with(
            dir,
            corpus_id,
            ingestion_in_progress,
            !ingestion_in_progress,
        );
    }

    fn write_meta_with(
        dir: &Path,
        corpus_id: &str,
        ingestion_in_progress: bool,
        indexes_built: bool,
    ) {
        std::fs::create_dir_all(dir).unwrap();
        let meta = serde_json::json!({
            "corpus_id": corpus_id,
            "corpus_name": format!("{corpus_id} (test)"),
            "embedding_model": "qwen-embedding-0.6b",
            "embedding_dimensions": 1024,
            "mesh_sharing": false,
            "license": "private",
            "created_at": 1_786_548_248_u64,
            "last_updated": 1_786_548_248_u64,
            "schema_version": 3,
            "is_shard": false,
            "ingestion_in_progress": ingestion_in_progress,
            "indexes_built": indexes_built,
        });
        std::fs::write(
            Corpus::meta_in(dir),
            serde_json::to_string_pretty(&meta).unwrap(),
        )
        .unwrap();
    }

    /// THE REGRESSION. Reproduced live on 2026-08-12 against a fresh sandbox
    /// HOME: `corpus install journey-fixture` exits 0 immediately, the daemon
    /// writes `journey-fixture-partition-node-3148a89c1ae48238/`, and the
    /// canonical `journey-fixture/` does not exist for another ~20 seconds.
    ///
    /// A status walk listed that partition directory BY NAME, so its output
    /// contained the string `journey-fixture` at t+0 with zero chunks
    /// committed — which is how the CLI-contract `enrich-atlas` journey's
    /// `stdout_contains = "{corpus}"` barrier passed, twice, before
    /// `enrich init` failed with `Index not found`.
    #[test]
    fn in_flight_partition_is_not_reported_as_an_installed_corpus() {
        let tmp = tempfile::tempdir().unwrap();
        let indexes = tmp.path();
        write_meta(
            &indexes.join("journey-fixture-partition-node-3148a89c1ae48238"),
            "journey-fixture",
            true,
        );

        let rows = scan_corpus_rows(indexes).unwrap();

        assert_eq!(rows.len(), 1, "one corpus is being built, so one row");
        // The row names the CORPUS, never the partition directory. There is
        // no corpus called `journey-fixture-partition-node-…` — you cannot
        // install it, remove it, or query it.
        assert_eq!(rows[0].corpus_id, "journey-fixture");
        assert_eq!(
            rows[0].state,
            CorpusReadiness::Building,
            "a partition mid-ingest is `building`; reporting it as installed \
             is the bug this test exists for"
        );
        assert_ne!(rows[0].state, CorpusReadiness::Ready);
    }

    /// The order's install → remove → install sequence, pinned at the level
    /// the decider actually sees. Recorded live in the same order:
    /// ready (t+25s) → absent (after `corpus remove --yes`) → building
    /// (t+0 of the second install). The third state is the one that used to
    /// read as success.
    #[test]
    fn install_after_remove_is_building_not_ready() {
        let tmp = tempfile::tempdir().unwrap();
        let indexes = tmp.path();
        std::fs::create_dir_all(indexes).unwrap();

        // 1. First install completed: canonical dir, ingestion committed.
        write_meta(&indexes.join("journey-fixture"), "journey-fixture", false);
        assert_eq!(
            corpus_readiness(indexes, "journey-fixture"),
            CorpusReadiness::Ready
        );

        // 2. `corpus remove --yes` — observed to remove the canonical dir and
        //    leave nothing behind. No registry row, no cache marker: this is
        //    the evidence that eliminated "remove is the liar".
        std::fs::remove_dir_all(indexes.join("journey-fixture")).unwrap();
        assert_eq!(
            corpus_readiness(indexes, "journey-fixture"),
            CorpusReadiness::Absent,
            "remove leaves nothing — absence must be reported as absence"
        );

        // 3. Second install, t+0: the daemon spawned a REAL ingest
        //    (`spawned: true`) which writes a partition first. The canonical
        //    dir is materialised only by the finalise step.
        write_meta(
            &indexes.join("journey-fixture-partition-node-3148a89c1ae48238"),
            "journey-fixture",
            true,
        );
        assert_eq!(
            corpus_readiness(indexes, "journey-fixture"),
            CorpusReadiness::Building,
            "install exits 0 here; the index is NOT usable here"
        );
    }

    /// A canonical directory whose ingest never committed (process killed
    /// mid-embed) is not usable either — `installed_indexes()` skips it, and
    /// so must this. Same rule, other shape.
    #[test]
    fn interrupted_canonical_ingest_is_building_not_ready() {
        let tmp = tempfile::tempdir().unwrap();
        write_meta(&tmp.path().join("halfway"), "halfway", true);
        assert_eq!(
            corpus_readiness(tmp.path(), "halfway"),
            CorpusReadiness::Building
        );
    }

    /// The partition probe matches on `<corpus_id>-partition-`, so a
    /// DIFFERENT corpus that merely shares a name prefix cannot make this one
    /// look like it is building. `foo` and `foo-bar` are separate corpora.
    #[test]
    fn a_prefix_sharing_corpus_does_not_forge_readiness() {
        let tmp = tempfile::tempdir().unwrap();
        write_meta(&tmp.path().join("foo-bar"), "foo-bar", false);
        assert_eq!(
            corpus_readiness(tmp.path(), "foo"),
            CorpusReadiness::Absent,
            "`foo-bar` says nothing about `foo`"
        );
        assert_eq!(
            corpus_readiness(tmp.path(), "foo-bar"),
            CorpusReadiness::Ready
        );
    }

    /// When both a finished canonical dir and a leftover partition exist, the
    /// corpus is ready and appears ONCE — the canonical dir is what
    /// `enrich init` and search open.
    #[test]
    fn canonical_wins_over_a_leftover_partition_and_collapses_to_one_row() {
        let tmp = tempfile::tempdir().unwrap();
        let indexes = tmp.path();
        write_meta(&indexes.join("journey-fixture"), "journey-fixture", false);
        write_meta(
            &indexes.join("journey-fixture-partition-node-old"),
            "journey-fixture",
            true,
        );

        let rows = scan_corpus_rows(indexes).unwrap();
        assert_eq!(rows.len(), 1, "one corpus, not one row per directory");
        assert_eq!(rows[0].corpus_id, "journey-fixture");
        assert_eq!(rows[0].state, CorpusReadiness::Ready);
        assert!(rows[0].from_canonical);
    }

    /// covers: ST-7
    ///
    /// The labels are asserted on by `sovereign/docs/cli-contract.toml`
    /// (journey `enrich-atlas`), so they are API. Renaming one silently
    /// turns that journey's barrier back into a vacuous check. The route
    /// serves `state_label` from the same `label()` — one spelling, both
    /// surfaces.
    #[test]
    fn readiness_labels_are_stable_api() {
        assert_eq!(CorpusReadiness::Ready.label(), "ready");
        assert_eq!(CorpusReadiness::Building.label(), "building");
        assert_eq!(CorpusReadiness::Unsearchable.label(), "unsearchable");
        assert_eq!(CorpusReadiness::Absent.label(), "absent");
    }

    /// covers: ST-6
    ///
    /// The 2026-08-28 failing input, recorded from disk rather than imagined:
    /// `wikipedia-newsworthy` had `ingestion_in_progress: false` beside
    /// `indexes_built: false` — 26 data fragments, no vector index — and this
    /// decider called it `ready`. 7 of 355 local corpora were in that state.
    ///
    /// It is not `Building`: nothing is writing, so it will never resolve on
    /// its own. It is not `Ready`: every retrieval path refuses it with
    /// `UnavailabilityReason::NotBuilt`. Reporting it as either is the
    /// substitution ARCH 18.3 forbids, and it made a truthful "I cannot
    /// search this corpus" from the app look like a fabrication.
    #[test]
    fn a_stopped_ingest_that_never_built_indexes_is_not_ready() {
        let tmp = tempfile::tempdir().unwrap();
        let indexes = tmp.path();
        write_meta_with(&indexes.join("stalled"), "stalled", false, false);
        assert_eq!(
            corpus_readiness(indexes, "stalled"),
            CorpusReadiness::Unsearchable,
            "a stopped ingest with no indexes must not report ready"
        );
    }

    /// The other direction: the change must not reclassify healthy corpora.
    #[test]
    fn a_finished_ingest_is_still_ready() {
        let tmp = tempfile::tempdir().unwrap();
        let indexes = tmp.path();
        write_meta_with(&indexes.join("healthy"), "healthy", false, true);
        assert_eq!(corpus_readiness(indexes, "healthy"), CorpusReadiness::Ready);
        // And a live writer is still Building, not Unsearchable — indexes are
        // legitimately absent mid-ingest and that state resolves itself.
        write_meta_with(&indexes.join("live"), "live", true, false);
        assert_eq!(corpus_readiness(indexes, "live"), CorpusReadiness::Building);
    }

    /// sv-surface rung 1's own pin: the ROW the route serves and the row the
    /// CLI prints are one value — `state_label` is derived beside `state` in
    /// the one constructor, so the wire's greppable spelling and the typed
    /// enum can never disagree.
    #[test]
    fn the_served_row_carries_its_label_beside_its_state() {
        let tmp = tempfile::tempdir().unwrap();
        let indexes = tmp.path();
        write_meta(&indexes.join("ready-corpus"), "ready-corpus", false);
        let rows = scan_corpus_rows(indexes).unwrap();
        assert_eq!(rows[0].state_label, rows[0].state.label());
        assert_eq!(rows[0].state_label, "ready");
    }
}
