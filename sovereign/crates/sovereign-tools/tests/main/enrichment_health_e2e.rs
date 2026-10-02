// SPDX-License-Identifier: AGPL-3.0-or-later
//! `EnrichmentChecker` — the first reachable firing.
//!
//! **Why this file exists.** From the day it was written until 2026-08-07 this
//! checker could not report a single issue for any input. Its opening guard
//! read `IndexMeta.enrichment_enabled`, a field written in exactly one place
//! (`ingest/crates/corpus-engine/src/index/create.rs`) and always written `false`, with no
//! setter anywhere in the workspace. So `continue` fired for every corpus,
//! always, and `LowEnrichmentCoverage` / `StaleEnrichment` were dead code —
//! §18.1's "a check with no failing input you can name". Full trace:
//! `docs/internal/TRACE_ENRICHMENT_ENABLED_FLAG.md` §4.
//!
//! **What this pins** is the checker, svrn's code, over the ingest ports'
//! double listing leaf `CorpusIndex`es on disk (phase-b-47):
//!
//! - a corpus that asked for enrichment and has no field model FIRES
//!   `LowEnrichmentCoverage` (the firing that was impossible for any input);
//! - an installed corpus whose directory is an unpromoted
//!   `<id>-partition-<node>/` is OPENED at the path the listing reported, not
//!   at `index_dir/<corpus_id>` — the old resolution `Err`ed and the miss was
//!   swallowed;
//! - a failed ingest's partition, which only `incomplete_ingests` lists,
//!   raises `IncompleteIngestPartition`;
//! - a corpus that never asked stays silent, and an interrupted ingest that
//!   never asked for enrichment stays out of this report — so the fix does
//!   not trade a check that never fires for one that always does.
//!
//! What ingest writes for those inputs — the request stamp on both of its
//! arms, the completion WARN when every enrichment inference call failed, the
//! partition paths `installed_indexes` and `incomplete_ingests` report — is
//! the engine's, proven on `impl CorpusReadPort for CorpusEngine` in
//! corpus-engine's tests/main/enrichment_requested_flag.rs and
//! tests/main/corpus_read_port_parity.rs.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use corpus_index::index::CorpusIndex;
use corpus_index::ingest_port::double::IngestPortDouble;
use corpus_index::source::CorpusReadPort;
use corpus_index::types::{IncompleteIngest, IndexInfo};
use sovereign_core::health::{HealthCheckable, HealthIssue};
use sovereign_tools::enrichment_checker::EnrichmentChecker;

// ─── Fixture ─────────────────────────────────────────────────────────

const CORPUS: &str = "health_corpus";

/// A leaf `CorpusIndex` for `CORPUS` at `path`, with no field-model tables,
/// stamped with the recipe's enrichment ask; its listing as ingest reports
/// it.
async fn index_at(path: &Path, enrichment_requested: bool) -> IndexInfo {
    let index = CorpusIndex::create(path, CORPUS, "Health Corpus", "test-mock", 8, false, "CC0")
        .await
        .unwrap();
    index
        .set_enrichment_requested(enrichment_requested)
        .unwrap();
    let index = CorpusIndex::open(path).await.unwrap();
    // Validate the instrument before the verdict (§18.4): the corpus really
    // records the ask, and really has no field-model tables. Without both, a
    // verdict below would be measuring something else.
    assert!(
        !index.has_field_model_tables().await,
        "fixture must be UN-enriched or the issue under test is not the one firing"
    );
    let info = index.info().await.unwrap();
    assert_eq!(info.enrichment_requested, enrichment_requested);
    assert_eq!(info.path, path);
    info
}

/// The double over `index_dir`, listing `installed` and `incomplete`.
fn engine(
    index_dir: PathBuf,
    installed: Vec<IndexInfo>,
    incomplete: Vec<IncompleteIngest>,
) -> Arc<IngestPortDouble> {
    Arc::new(
        IngestPortDouble::new()
            .with_index_dir(index_dir)
            .opening_indexes_under_index_dir()
            .with_installed_indexes(installed)
            .with_incomplete_ingests(incomplete),
    )
}

fn low_coverage_for_corpus(issues: &[HealthIssue]) -> usize {
    issues
        .iter()
        .filter(|i| {
            matches!(
                i,
                HealthIssue::LowEnrichmentCoverage { corpus_id, .. } if corpus_id == CORPUS
            )
        })
        .count()
}

// ─── Tests ───────────────────────────────────────────────────────────

/// **The firing that was impossible.** A corpus whose recipe asked for
/// field-model enrichment, whose enrichment produced no field-model tables,
/// must raise a `LowEnrichmentCoverage` naming it.
///
/// If this starts failing, the checker's guard reverted — and the standing
/// "enrichment was requested here and never completed" surface is back to
/// reporting clean for every corpus in the fleet.
#[tokio::test]
async fn checker_fires_low_enrichment_coverage_for_a_requested_but_unenriched_corpus() {
    let dir = tempfile::tempdir().unwrap();
    let indexes_dir = dir.path().join("indexes");
    let info = index_at(&indexes_dir.join(CORPUS), true).await;

    let report = EnrichmentChecker::new(engine(indexes_dir, vec![info], vec![]))
        .check()
        .await
        .expect("check must not error");

    assert_eq!(
        low_coverage_for_corpus(&report.issues),
        1,
        "a corpus that asked for enrichment and has no field-model tables must \
         raise exactly one LowEnrichmentCoverage; report was: {report:#?}"
    );
}

/// **The resolution swap.** A corpus can be fully installed and still not
/// live at `index_dir/<corpus_id>`: an unpromoted `<corpus_id>-partition-
/// <node>/` is listed by `installed_indexes()` with that path, and
/// `open_index_for_corpus(corpus_id)` — which joins the canonical name —
/// cannot open it. The checker's old `if let Ok(index) = …` then swallowed
/// the `Err` and produced no issue, so the corpus was reported clean without
/// anyone having looked at it.
///
/// Opening `info.path` (the resolution `CorpusEngine::enriched_corpus_ids`
/// already used) is the fix. Put `open_index_for_corpus(&corpus_id)` back and
/// this test fails: zero issues instead of one.
#[tokio::test]
async fn checker_opens_the_path_the_listing_reported_not_the_canonical_name() {
    let dir = tempfile::tempdir().unwrap();
    let indexes_dir = dir.path().join("indexes");
    let partition = indexes_dir.join("health_corpus-partition-node-aaaa");
    let info = index_at(&partition, true).await;
    let engine = engine(indexes_dir, vec![info], vec![]);

    // Validate the instrument (§18.4): the canonical-name resolution fails on
    // this corpus. That is what makes it the blind spot and not some other bug.
    assert!(
        engine.open_index_for_corpus(CORPUS).await.is_err(),
        "index_dir/<corpus_id> does not exist — this is the miss the old \
         resolution swallowed"
    );

    let report = EnrichmentChecker::new(engine)
        .check()
        .await
        .expect("check must not error");

    assert_eq!(
        low_coverage_for_corpus(&report.issues),
        1,
        "the checker must open what the listing found; report was: {report:#?}"
    );
}

/// **The failed-ingest partition.** An ingest that dies inside its enrichment
/// phase leaves `<corpus_id>-partition-<node>/` behind with
/// `ingestion_in_progress: true` beside `indexes_built: true` — the
/// fingerprint traced in `docs/internal/TRACE_ENRICHMENT_ENABLED_FLAG.md` §3.
///
/// That directory is invisible to every corpus listing on the machine —
/// `installed_indexes()` skips anything mid-ingest — so before this the
/// checker reported "All checks passed" for a corpus whose install had blown
/// up. `incomplete_ingests()` is the only listing that names it, and the
/// checker must carry its path and its `indexes_built` into the issue.
#[tokio::test]
async fn checker_reports_a_failed_ingest_partition_no_listing_can_see() {
    let dir = tempfile::tempdir().unwrap();
    let indexes_dir = dir.path().join("indexes");
    let partition = indexes_dir.join("health_corpus-partition-node-aaaa");
    let engine = engine(
        indexes_dir,
        vec![],
        vec![IncompleteIngest {
            corpus_id: CORPUS.into(),
            path: partition,
            indexes_built: true,
            enrichment_requested: true,
        }],
    );

    let report = EnrichmentChecker::new(engine)
        .check()
        .await
        .expect("check must not error");

    let fired: Vec<&HealthIssue> = report
        .issues
        .iter()
        .filter(|i| {
            matches!(
                i,
                HealthIssue::IncompleteIngestPartition { corpus_id, .. }
                    if corpus_id == CORPUS
            )
        })
        .collect();
    assert_eq!(
        fired.len(),
        1,
        "a failed enrichment ingest's partition must be reported, not \
         silently absent; report was: {report:#?}"
    );
    match fired[0] {
        HealthIssue::IncompleteIngestPartition {
            path,
            indexes_built,
            ..
        } => {
            assert!(
                path.ends_with("health_corpus-partition-node-aaaa"),
                "the issue must name the directory on disk so the operator can \
                 find it; got {path}"
            );
            assert!(
                *indexes_built,
                "indexes_built distinguishes a late failure (enrichment) from \
                 an early one (mid-embed) — it must survive into the issue"
            );
        }
        other => panic!("unreachable — filtered above: {other:?}"),
    }
}

/// The control for the partition scan. A plain interrupted ingest — one whose
/// recipe never asked for enrichment — is a real problem, but it is not this
/// component's to report. Without this bound the enrichment report becomes
/// the machine's general ingest-failure log and stops meaning anything.
#[tokio::test]
async fn checker_leaves_an_interrupted_plain_ingest_to_someone_else() {
    let dir = tempfile::tempdir().unwrap();
    let indexes_dir = dir.path().join("indexes");
    let partition = indexes_dir.join("health_corpus-partition-node-aaaa");
    let engine = engine(
        indexes_dir,
        vec![],
        vec![IncompleteIngest {
            corpus_id: CORPUS.into(),
            path: partition,
            indexes_built: true,
            enrichment_requested: false,
        }],
    );

    let report = EnrichmentChecker::new(engine.clone())
        .check()
        .await
        .expect("check must not error");

    // Validate the instrument: the checker DID read the incomplete listing —
    // so a silent report is the scoping rule working, not the scan never
    // happening.
    assert!(
        engine.calls().contains(&"incomplete_ingests"),
        "the checker must consult incomplete_ingests; calls were {:?}",
        engine.calls()
    );
    assert!(
        report.issues.is_empty(),
        "an interrupted ingest that never asked for enrichment is not an \
         enrichment issue; got: {report:#?}"
    );
}

/// The other verdict. A corpus that never asked for enrichment must not be
/// reported — otherwise the fix trades a check that can never fire for one
/// that always fires, and the operator learns nothing either way.
#[tokio::test]
async fn checker_stays_silent_for_a_corpus_that_never_asked_for_enrichment() {
    let dir = tempfile::tempdir().unwrap();
    let indexes_dir = dir.path().join("indexes");
    let info = index_at(&indexes_dir.join(CORPUS), false).await;

    let report = EnrichmentChecker::new(engine(indexes_dir, vec![info], vec![]))
        .check()
        .await
        .expect("check must not error");

    assert!(
        report.issues.is_empty(),
        "an un-requested corpus must raise nothing; got: {report:#?}"
    );
}
