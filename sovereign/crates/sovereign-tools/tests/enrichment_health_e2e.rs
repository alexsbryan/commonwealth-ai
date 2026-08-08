// SPDX-License-Identifier: AGPL-3.0-or-later
//! `EnrichmentChecker` — the first reachable firing.
//!
//! **Why this file exists.** From the day it was written until 2026-08-07 this
//! checker could not report a single issue for any input. Its opening guard
//! read `IndexMeta.enrichment_enabled`, a field written in exactly one place
//! (`corpus-engine/src/index/create.rs`) and always written `false`, with no
//! setter anywhere in the workspace. So `continue` fired for every corpus,
//! always, and `LowEnrichmentCoverage` / `StaleEnrichment` were dead code —
//! §18.1's "a check with no failing input you can name". Full trace:
//! `docs/TRACE_ENRICHMENT_ENABLED_FLAG.md` §4.
//!
//! **What this pins**, end to end through the real ingest pipeline — no
//! hand-written meta, no hand-set flag:
//!
//! - a recipe that asks for field-model enrichment, ingested against an
//!   inference function that fails every call, FIRES `LowEnrichmentCoverage`
//!   (the firing that was impossible for any input);
//! - the SAME recipe on an engine with no `InferenceFn` at all — the silent
//!   skip, a different arm of `engine/ingest.rs` that until 2026-08-07 wrote
//!   nothing to the meta — fires too;
//! - an installed corpus whose directory is an unpromoted
//!   `<id>-partition-<node>/` is OPENED at the path the listing reported, not
//!   at `index_dir/<corpus_id>` — the old resolution `Err`ed and the miss was
//!   swallowed;
//! - a failed ingest's partition, which no corpus listing on the machine can
//!   see, raises `IncompleteIngestPartition`;
//! - a run in which EVERY enrichment inference call failed says so at
//!   completion, in the log, naming the tally — not only on the standing
//!   surface;
//! - the same recipe without `[enrichment]` stays silent, and an interrupted
//!   ingest that never asked for enrichment stays out of this report — so the
//!   fix does not trade a check that never fires for one that always does.
//!
//! Delete `index.set_enrichment_requested(true)` from the entry of ingest's
//! `'enrichment:` block (`corpus-engine/src/engine/ingest.rs`) and the first
//! test fails: the corpus reports `enrichment_requested: false`, the checker's
//! guard `continue`s past it, and the report comes back clean — exactly the
//! dead-check state this replaced.
//!
//! Note what the fixture ingest reveals on its way past: every inference call
//! errors, and the ingest still returns `Ok` with "Ingestion complete". The
//! enrichment phases absorb a total outage, which is why BOTH surfaces are
//! pinned here — the completion WARN for the operator watching the install,
//! and the standing check for everyone who looks tomorrow.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use corpus_engine::{CorpusEngine, CorpusSpec, EmbedFn, Error};
use sovereign_core::health::{HealthCheckable, HealthIssue};
use sovereign_tools::enrichment_checker::EnrichmentChecker;
use tracing_subscriber::fmt::MakeWriter;

// ─── Fixture ─────────────────────────────────────────────────────────

/// Eight paragraphs, each comfortably over the philosophy domain's
/// `OVERVIEW_MIN_TOKEN_COUNT` of 80 words.
///
/// **That size is load-bearing, and it was learned the hard way.** The
/// original three-sentence fixture chunked to ONE chunk of ~40 words, which
/// `FieldModelEngine`'s overview filter dropped entirely
/// (`overview_chunks=0`, `field_engine.rs` word-count gate). Phase 1 then had
/// zero batches, clustering skipped itself at 1 < min_cluster_size, and the
/// run made **zero inference calls** — measured, `inference_calls=0
/// inference_failures=0`. So the "always-failing inference" every test here
/// passes was never actually failing anything: the corpus ended unenriched
/// because it was too small to enrich, not because inference was down.
///
/// The `LowEnrichmentCoverage` assertions were still true, but no test in
/// this file exercised an inference OUTAGE until this fixture grew. Shrink
/// it back and `a_total_inference_outage_says_so_at_completion_*` goes green-
/// for-the-wrong-reason: nothing fails, so nothing is reported.
fn write_source(dir: &Path) -> PathBuf {
    let path = dir.join("source.txt");
    let mut text = String::new();
    for i in 1..=8 {
        text.push_str(&format!(
            "Paragraph {i} exists so this corpus has a chunk the enrichment \
             pipeline will actually look at, which means it has to clear the \
             domain's minimum word count rather than merely exist. The \
             archivist noted that a ledger which records only its own \
             existence records nothing at all, and that the difference \
             between a catalogue and an inventory is the question each is \
             built to answer. A catalogue answers what is here; an inventory \
             answers what is missing, and only one of those can be checked \
             against the shelves without reading every spine. This paragraph \
             therefore carries more than eighty words on purpose, because a \
             shorter one would be filtered out before any prompt was built \
             and the outage under test would never happen.\n\n"
        ));
    }
    std::fs::write(&path, text).unwrap();
    path
}

fn write_recipe(recipes_dir: &Path, source: &Path, enrichment_block: &str) -> PathBuf {
    let recipe_path = recipes_dir.join("health_corpus.toml");
    let source_str = source.to_string_lossy();
    std::fs::write(
        &recipe_path,
        format!(
            r#"
[corpus]
id = "health_corpus"
name = "Health Corpus"
description = "EnrichmentChecker reachability fixture"
license = "CC0"
mesh_sharing = false

[acquire]
type = "local_file"
path = "{source_str}"

[extract]
type = "plaintext"

[chunk]
type = "paragraph"
# Wide enough to hold ONE of `write_source`'s paragraphs and too narrow to
# pack two, so the chunk count equals the paragraph count and every chunk
# clears the domain's 80-word overview floor.
max_chars = 1200
overlap_chars = 0

[index]
embedding_model = "test-mock"
embedding_dimensions = 8
{enrichment_block}
"#
        ),
    )
    .unwrap();
    recipe_path
}

fn mock_embed_fn() -> EmbedFn {
    Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.1_f32; 8]) }))
}

/// Inference that fails every call — the stand-in for the real-world
/// enrichment failures this check exists to surface (an unregistered domain,
/// a dead model slot, a mid-phase kill).
fn always_failing_inference_fn() -> corpus_engine::types::InferenceFn {
    Arc::new(|_prompt: &str, _schema: Option<&serde_json::Value>| {
        Box::pin(async {
            Err(Error::InvalidInput(
                "simulated enrichment-time inference outage".to_string(),
            ))
        })
    })
}

/// Ingest a real corpus into a temp index dir. `enrichment_block` is appended
/// to the recipe verbatim, so each test states the enrichment shape it means.
async fn installed_corpus(
    enrichment_block: &str,
    inference: Inference,
) -> (tempfile::TempDir, Arc<CorpusEngine>) {
    let dir = tempfile::tempdir().unwrap();
    let recipes_dir = dir.path().join("recipes");
    let indexes_dir = dir.path().join("indexes");
    std::fs::create_dir_all(&recipes_dir).unwrap();
    let source = write_source(dir.path());
    let recipe_path = write_recipe(&recipes_dir, &source, enrichment_block);

    let engine = CorpusEngine::new(recipes_dir, indexes_dir, mock_embed_fn())
        .with_embedding_model("test-mock");
    let engine = match inference {
        Inference::AlwaysFails => engine.with_inference_fn(always_failing_inference_fn()),
        Inference::Absent => engine,
    };
    engine
        .ingest(&CorpusSpec::RecipePath(recipe_path), None)
        .await
        .expect("fixture ingest must succeed");

    (dir, Arc::new(engine))
}

/// Which inference source the fixture engine is built with. The two arms are
/// the two ways a recipe's enrichment ask goes unanswered, and they take
/// DIFFERENT paths through `engine/ingest.rs`:
///
/// - `AlwaysFails` enters the `'enrichment:` block and every call inside it
///   errors;
/// - `Absent` never enters the block at all — `match self.inference.as_ref()`
///   takes its `None` arm, logs "no InferenceFn was provided … skipping", and
///   returns. Until 2026-08-07 that arm stamped nothing, so the corpus was
///   on-disk indistinguishable from one that never asked, and the checker
///   could not see it.
#[derive(Clone, Copy)]
enum Inference {
    AlwaysFails,
    Absent,
}

/// Thread-shared buffer that `tracing_subscriber::fmt` writes into, so a test
/// can assert on what the ingest actually logged. Same shape as
/// `sovereign-mesh/tests/injection_order.rs`.
#[derive(Clone)]
struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for CaptureWriter {
    type Writer = Self;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Scope a WARN-level capturing subscriber to the current thread. `#[tokio::test]`
/// runs a current-thread runtime, so the ingest future — and the completion
/// WARN it emits — run on this same thread and land in the buffer.
fn capture_warns(buf: Arc<Mutex<Vec<u8>>>) -> tracing::subscriber::DefaultGuard {
    let subscriber = tracing_subscriber::fmt()
        .with_writer(CaptureWriter(buf))
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false)
        .without_time()
        .finish();
    tracing::subscriber::set_default(subscriber)
}

// ─── Tests ───────────────────────────────────────────────────────────

/// **Total-outage honesty.** Every enrichment inference call errors. The
/// pipeline absorbs each one — correctly; a few bad cluster labels should not
/// kill an ingest — and the run reaches "Ingestion complete" and returns `Ok`
/// with zero field-model tables. Success-shaped for a substitution nobody
/// asked for (§18.3).
///
/// The fix is deliberately NOT an `Err`: the chunks are real and the ingest
/// did succeed, so failing it would be its own lie and would throw away work
/// the user can use. What it must not do is stay silent. So ingest counts its
/// enrichment inference calls and their failures, and when ALL of them failed
/// it names the substitution at completion.
///
/// This test pins both halves of that honesty in one place: the WARN in the
/// log at the moment it happens, and the standing checker issue that survives
/// the log rotating. Before the change the first was absent entirely — the
/// only enrichment lines in a total-outage run were per-phase progress.
///
/// Delete the completion WARN from `engine/ingest.rs` and this fails on the
/// log assertion; the corpus is still installed, still searchable, and still
/// silently unenriched.
#[tokio::test]
async fn a_total_inference_outage_says_so_at_completion_and_stays_reportable() {
    let logs = Arc::new(Mutex::new(Vec::new()));

    let (_dir, engine) = {
        let _guard = capture_warns(logs.clone());
        installed_corpus(
            r#"
[enrichment]
enabled = true
type = "field_model"
domain = "philosophy"
"#,
            Inference::AlwaysFails,
        )
        .await
    };

    let captured = String::from_utf8_lossy(&logs.lock().unwrap()).to_string();

    // Validate the instrument before the verdict (§18.4). If the capture were
    // simply not wired up, every substring assertion below would "pass" by
    // being vacuously absent — so require the buffer to be non-empty first,
    // and require the corpus to actually be unenriched.
    assert!(
        !captured.is_empty(),
        "the capturing subscriber caught nothing at all — the assertions below \
         would be measuring a broken instrument, not the ingest"
    );
    let index = engine
        .open_index_for_corpus("health_corpus")
        .await
        .expect("the fixture corpus must be openable");
    assert!(
        !index.has_field_model_tables().await,
        "fixture must be UN-enriched — otherwise there was no outage to report"
    );

    assert!(
        captured.contains("enrichment requested and produced nothing"),
        "a run where every enrichment inference call failed must name the \
         substitution at completion; captured WARNs were:\n{captured}"
    );
    // The counts are the evidence, not decoration: "N/N" is what tells the
    // operator this was a TOTAL outage rather than a few flaky calls.
    assert!(
        captured.contains("inference calls failed"),
        "the WARN must carry the N/N tally; captured WARNs were:\n{captured}"
    );

    // And the standing surface agrees, which is what still answers the
    // question tomorrow when this log line has scrolled away.
    let report = EnrichmentChecker::new(engine.clone())
        .check()
        .await
        .expect("check must not error");
    assert!(
        report.issues.iter().any(|i| matches!(
            i,
            HealthIssue::LowEnrichmentCoverage { corpus_id, .. } if corpus_id == "health_corpus"
        )),
        "the checker must still report the corpus; report was: {report:#?}"
    );
}

/// The control for the WARN. A corpus whose recipe never asked for enrichment
/// makes zero enrichment inference calls, so the total-outage condition
/// (`calls > 0 && failed == calls`) must not fire — `0 == 0` is not an
/// outage, it is an absence of work.
#[tokio::test]
async fn a_corpus_that_never_asked_does_not_report_a_zero_of_zero_outage() {
    let logs = Arc::new(Mutex::new(Vec::new()));
    {
        let _guard = capture_warns(logs.clone());
        let _ = installed_corpus("", Inference::AlwaysFails).await;
        // Instrument check (§18.4). A silence assertion over a subscriber
        // that was never wired up passes for the wrong reason and would keep
        // passing after the WARN it is supposed to bound went haywire. This
        // canary proves the buffer is live and this thread's events reach it.
        tracing::warn!("capture canary — the subscriber is live");
    }

    let captured = String::from_utf8_lossy(&logs.lock().unwrap()).to_string();
    assert!(
        captured.contains("capture canary"),
        "the capturing subscriber caught nothing — the silence assertion below \
         would be vacuous; captured:\n{captured}"
    );
    assert!(
        !captured.contains("enrichment requested and produced nothing"),
        "no enrichment was requested, so nothing was substituted; captured:\n{captured}"
    );
}

/// **The firing that was impossible.** A corpus whose recipe asked for
/// field-model enrichment, whose enrichment produced no field-model tables,
/// must raise a `LowEnrichmentCoverage` naming it.
///
/// If this starts failing, either the entry stamp stopped happening or the
/// checker's guard reverted — either way the standing "enrichment was
/// requested here and never completed" surface is back to reporting clean for
/// every corpus in the fleet.
#[tokio::test]
async fn checker_fires_low_enrichment_coverage_for_a_requested_but_unenriched_corpus() {
    let (_dir, engine) = installed_corpus(
        r#"
[enrichment]
enabled = true
type = "field_model"
domain = "philosophy"
"#,
        Inference::AlwaysFails,
    )
    .await;

    // Validate the instrument before the verdict (§18.4): the corpus really
    // did record the request, and really has no field-model tables. Without
    // both, a green assertion below would be measuring something else.
    let installed = engine.installed_indexes().await.unwrap();
    let info = installed
        .iter()
        .find(|i| i.corpus_id == "health_corpus")
        .expect("the fixture corpus must be installed");
    assert!(
        info.enrichment_requested,
        "ingest must have stamped enrichment_requested at the entry of its \
         enrichment block — without it the checker cannot see this corpus"
    );
    let index = engine
        .open_index_for_corpus("health_corpus")
        .await
        .expect("the fixture corpus must be openable");
    assert!(
        !index.has_field_model_tables().await,
        "fixture must be UN-enriched or the issue under test is not the one firing"
    );

    let report = EnrichmentChecker::new(engine.clone())
        .check()
        .await
        .expect("check must not error");

    let fired: Vec<&HealthIssue> = report
        .issues
        .iter()
        .filter(|i| {
            matches!(
                i,
                HealthIssue::LowEnrichmentCoverage { corpus_id, .. } if corpus_id == "health_corpus"
            )
        })
        .collect();
    assert_eq!(
        fired.len(),
        1,
        "a corpus that asked for enrichment and has no field-model tables must \
         raise exactly one LowEnrichmentCoverage; report was: {report:#?}"
    );
}

/// **The silent-skip firing.** Same recipe, same ask — but the engine has no
/// `InferenceFn` at all, so ingest never enters the `'enrichment:` block. It
/// takes the `None` arm, writes a WARN into a log, and returns `Ok`.
///
/// That arm is the one an embed-only / headless engine construction takes for
/// every enrichment-requesting recipe on the machine. Before 2026-08-07 it
/// stamped nothing, so the corpus reported `enrichment_requested: false` and
/// the checker `continue`d past it — the whole install-honesty surface stayed
/// silent for the most common way enrichment goes undelivered.
///
/// Delete the `set_enrichment_requested(true)` call from the `None` arm of
/// `engine/ingest.rs` and this test fails: zero issues instead of one.
#[tokio::test]
async fn checker_fires_when_enrichment_was_requested_but_no_inference_fn_was_configured() {
    let (_dir, engine) = installed_corpus(
        r#"
[enrichment]
enabled = true
type = "field_model"
domain = "philosophy"
"#,
        Inference::Absent,
    )
    .await;

    // Validate the instrument (§18.4): nothing was enriched, and the ask was
    // recorded. Without both, a green verdict below measures something else.
    let index = engine
        .open_index_for_corpus("health_corpus")
        .await
        .expect("the fixture corpus must be openable");
    assert!(
        !index.has_field_model_tables().await,
        "fixture must be UN-enriched — an engine with no InferenceFn cannot \
         have run field-model enrichment"
    );

    let report = EnrichmentChecker::new(engine.clone())
        .check()
        .await
        .expect("check must not error");

    let fired: Vec<&HealthIssue> = report
        .issues
        .iter()
        .filter(|i| {
            matches!(
                i,
                HealthIssue::LowEnrichmentCoverage { corpus_id, .. } if corpus_id == "health_corpus"
            )
        })
        .collect();
    assert_eq!(
        fired.len(),
        1,
        "a corpus installed on an engine with no InferenceFn asked for \
         enrichment and got none — the checker must say so; report was: {report:#?}"
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
    let (dir, _engine) = installed_corpus(
        r#"
[enrichment]
enabled = true
type = "field_model"
domain = "philosophy"
"#,
        Inference::AlwaysFails,
    )
    .await;
    let indexes_dir = dir.path().join("indexes");

    // A COMPLETE install that simply never got promoted: rename only. The
    // meta is untouched, so `ingestion_in_progress` stays false and the
    // directory is still a first-class installed corpus.
    let partition = indexes_dir.join("health_corpus-partition-node-aaaa");
    std::fs::rename(indexes_dir.join("health_corpus"), &partition).unwrap();

    let engine = Arc::new(
        CorpusEngine::new(dir.path().join("recipes"), indexes_dir.clone(), mock_embed_fn())
            .with_embedding_model("test-mock"),
    );

    // Validate the instrument (§18.4): the corpus IS listed, its reported
    // path is the partition, and the canonical-name resolution fails on it.
    // All three are what make this the blind spot and not some other bug.
    let installed = engine.installed_indexes().await.unwrap();
    let info = installed
        .iter()
        .find(|i| i.corpus_id == "health_corpus")
        .expect("an unpromoted partition is still a complete, installed corpus");
    assert_eq!(info.path, partition, "the listing must report the real path");
    assert!(
        engine.open_index_for_corpus("health_corpus").await.is_err(),
        "index_dir/<corpus_id> does not exist — this is the miss the old \
         resolution swallowed"
    );

    let report = EnrichmentChecker::new(engine.clone())
        .check()
        .await
        .expect("check must not error");

    let fired: Vec<&HealthIssue> = report
        .issues
        .iter()
        .filter(|i| {
            matches!(
                i,
                HealthIssue::LowEnrichmentCoverage { corpus_id, .. } if corpus_id == "health_corpus"
            )
        })
        .collect();
    assert_eq!(
        fired.len(),
        1,
        "the checker must open what the listing found; report was: {report:#?}"
    );
}

/// **The failed-ingest partition.** An ingest that dies inside its enrichment
/// phase leaves `<corpus_id>-partition-<node>/` behind with
/// `ingestion_in_progress: true` beside `indexes_built: true` — the
/// fingerprint traced in `docs/TRACE_ENRICHMENT_ENABLED_FLAG.md` §3.
/// `build_indexes()` stamped the second flag; enrichment then threw, so
/// `mark_ingestion_complete()` never ran and promotion to the canonical
/// directory (which happens only on `Ok`) never happened.
///
/// That directory is invisible to every corpus listing on the machine —
/// `installed_indexes()` skips anything mid-ingest — so before this the
/// checker reported "All checks passed" for a corpus whose install had
/// blown up. This test builds the fingerprint from a REAL ingest rather
/// than a hand-written meta, and pins both halves: the old resolution path
/// genuinely cannot see it, and the new issue genuinely fires.
#[tokio::test]
async fn checker_reports_a_failed_ingest_partition_no_listing_can_see() {
    let (dir, _engine) = installed_corpus(
        r#"
[enrichment]
enabled = true
type = "field_model"
domain = "philosophy"
"#,
        Inference::AlwaysFails,
    )
    .await;
    let indexes_dir = dir.path().join("indexes");

    // Re-shape the finished install into the failed-ingest partition: move
    // the directory to the partition name promotion would have renamed FROM,
    // and flip the meta back to mid-ingest. `indexes_built` is already true
    // from the real `build_indexes()` run, which is the half of the
    // fingerprint that says the ingest died LATE.
    let canonical = indexes_dir.join("health_corpus");
    let partition = indexes_dir.join("health_corpus-partition-node-aaaa");
    std::fs::rename(&canonical, &partition).unwrap();
    let meta_path = partition.join("_corpus_meta.json");
    let mut meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&meta_path).unwrap()).unwrap();
    let obj = meta.as_object_mut().unwrap();
    assert_eq!(
        obj.get("indexes_built").and_then(|v| v.as_bool()),
        Some(true),
        "the fixture ingest must have built its search indexes — without that \
         this is not the late-failure fingerprint under test"
    );
    obj.insert("ingestion_in_progress".into(), serde_json::Value::Bool(true));
    std::fs::write(&meta_path, serde_json::to_string_pretty(&meta).unwrap()).unwrap();

    // Rebuild the engine so nothing is served from the IndexInfo cache the
    // first ingest populated — the checker must reach this state cold, the
    // way a daemon restarting after a failed install does.
    let engine = Arc::new(
        CorpusEngine::new(dir.path().join("recipes"), indexes_dir.clone(), mock_embed_fn())
            .with_embedding_model("test-mock"),
    );

    // Validate the instrument (§18.4), and pin the OLD behaviour in the same
    // breath: this corpus is invisible to both surfaces the checker used to
    // rely on. If either of these assertions ever fails, the test below has
    // stopped measuring the blind spot it was written for.
    let installed = engine.installed_indexes().await.unwrap();
    assert!(
        !installed.iter().any(|i| i.corpus_id == "health_corpus"),
        "a mid-ingest directory must not appear as an installed corpus — \
         that is exactly why the loop over installed_indexes() cannot report it"
    );
    assert!(
        engine.open_index_for_corpus("health_corpus").await.is_err(),
        "the old resolution joins index_dir/<corpus_id>, which no longer \
         exists — this is the miss the checker used to swallow"
    );

    let report = EnrichmentChecker::new(engine.clone())
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
                    if corpus_id == "health_corpus"
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
    let (dir, _engine) = installed_corpus("", Inference::AlwaysFails).await;
    let indexes_dir = dir.path().join("indexes");

    let partition = indexes_dir.join("health_corpus-partition-node-aaaa");
    std::fs::rename(indexes_dir.join("health_corpus"), &partition).unwrap();
    let meta_path = partition.join("_corpus_meta.json");
    let mut meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&meta_path).unwrap()).unwrap();
    meta.as_object_mut()
        .unwrap()
        .insert("ingestion_in_progress".into(), serde_json::Value::Bool(true));
    std::fs::write(&meta_path, serde_json::to_string_pretty(&meta).unwrap()).unwrap();

    let engine = Arc::new(
        CorpusEngine::new(dir.path().join("recipes"), indexes_dir.clone(), mock_embed_fn())
            .with_embedding_model("test-mock"),
    );

    // Validate the instrument: the engine DOES see the incomplete directory —
    // so a silent report below is the scoping rule working, not the scan
    // failing to find anything.
    let seen = engine.incomplete_ingests();
    assert_eq!(
        seen.len(),
        1,
        "the engine must find the incomplete directory; found {seen:#?}"
    );
    assert!(
        !seen[0].enrichment_requested,
        "this fixture's recipe has no [enrichment] block"
    );

    let report = EnrichmentChecker::new(engine.clone())
        .check()
        .await
        .expect("check must not error");

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
    // No `[enrichment]` block at all: this is the state every corpus on every
    // machine was stuck in before the fix, and it must stay quiet.
    let (_dir, engine) = installed_corpus("", Inference::AlwaysFails).await;

    let report = EnrichmentChecker::new(engine.clone())
        .check()
        .await
        .expect("check must not error");

    assert!(
        report.issues.is_empty(),
        "an un-requested corpus must raise nothing; got: {report:#?}"
    );
}
