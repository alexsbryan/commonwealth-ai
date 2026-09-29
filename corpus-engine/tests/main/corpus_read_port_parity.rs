// SPDX-License-Identifier: AGPL-3.0-or-later
//! The engine half of svrn's tests that drive the ingest ports' double as a
//! `CorpusReadPort` (phase-b-47): sovereign-tools' enrichment_health_e2e and
//! turn_foreground_lease. Each case drives `CorpusEngine` through the port
//! svrn holds (`&dyn CorpusReadPort`, never the same-named inherent methods)
//! and pins what those tests now program the double to answer.
//!
//! - A run in which EVERY enrichment inference call failed says so at
//!   completion, in the log, naming the tally, and leaves a corpus that asked
//!   and has no field model; a run that never asked says nothing.
//! - An unpromoted `<id>-partition-<node>/` is listed at that path, and the
//!   canonical-name open fails on it.
//! - A failed enrichment ingest's partition is absent from
//!   `installed_indexes` and present in `incomplete_ingests` with
//!   `indexes_built`; a plain interrupted ingest is there without the ask.
//! - `foreground_lease` is `None` until a signal is installed, then pairs
//!   `begin` with `end` on drop.
//!
//! Note what the fixture ingest reveals on its way past: every inference call
//! errors, and the ingest still returns `Ok` with "Ingestion complete". The
//! enrichment phases absorb a total outage, which is why the completion WARN
//! is pinned here and the standing checker issue in svrn.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};

use corpus_engine::enrichment::pipeline::ChatPrompt;
use corpus_engine::{CorpusEngine, CorpusSpec, Error, ForegroundSignal};
use corpus_index::source::CorpusReadPort;
use corpus_index::types::EmbedFn;
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
/// inference_failures=0`. So the "always-failing inference" was never
/// actually failing anything. Shrink it back and
/// `a_total_inference_outage_says_so_at_completion` goes green-for-the-
/// wrong-reason: nothing fails, so nothing is reported.
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

const FIELD_MODEL: &str = r#"
[enrichment]
enabled = true
type = "field_model"
domain = "philosophy"
"#;

fn mock_embed_fn() -> EmbedFn {
    Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.1_f32; 8]) }))
}

/// Inference that fails every call — the stand-in for the real-world
/// enrichment failures the checker exists to surface (an unregistered domain,
/// a dead model slot, a mid-phase kill).
fn always_failing_inference_fn() -> corpus_engine::types::InferenceFn {
    Arc::new(|_prompt: &ChatPrompt, _max_tokens: Option<u32>| {
        Box::pin(async {
            Err(Error::InvalidInput(
                "simulated enrichment-time inference outage".to_string(),
            ))
        })
    })
}

/// Ingest the fixture corpus against always-failing inference into a temp
/// index dir. `enrichment_block` is appended to the recipe verbatim.
async fn installed_corpus(enrichment_block: &str) -> (tempfile::TempDir, Arc<CorpusEngine>) {
    let dir = tempfile::tempdir().unwrap();
    let recipes_dir = dir.path().join("recipes");
    let indexes_dir = dir.path().join("indexes");
    std::fs::create_dir_all(&recipes_dir).unwrap();
    let source = write_source(dir.path());
    let recipe_path = write_recipe(&recipes_dir, &source, enrichment_block);

    let engine = CorpusEngine::new(recipes_dir, indexes_dir, mock_embed_fn())
        .with_embedding_model("test-mock")
        .with_inference_fn(always_failing_inference_fn());
    engine
        .ingest(&CorpusSpec::RecipePath(recipe_path), None)
        .await
        .expect("fixture ingest must succeed");
    (dir, Arc::new(engine))
}

/// A cold engine over `dir`'s index dir, so nothing is served from the
/// `IndexInfo` cache the fixture ingest populated — the state a daemon
/// restarting after a failed install reads.
fn cold_engine(dir: &Path) -> Arc<dyn CorpusReadPort> {
    Arc::new(
        CorpusEngine::new(dir.join("recipes"), dir.join("indexes"), mock_embed_fn())
            .with_embedding_model("test-mock"),
    )
}

/// Move the installed corpus to the partition name promotion renames FROM,
/// optionally flipping its meta back to mid-ingest. Returns the partition.
fn unpromote(dir: &Path, mid_ingest: bool) -> PathBuf {
    let indexes_dir = dir.join("indexes");
    let partition = indexes_dir.join("health_corpus-partition-node-aaaa");
    std::fs::rename(indexes_dir.join("health_corpus"), &partition).unwrap();
    if mid_ingest {
        let meta_path = corpus_engine::Corpus::meta_in(&partition);
        let mut meta: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&meta_path).unwrap()).unwrap();
        meta.as_object_mut().unwrap().insert(
            "ingestion_in_progress".into(),
            serde_json::Value::Bool(true),
        );
        std::fs::write(&meta_path, serde_json::to_string_pretty(&meta).unwrap()).unwrap();
    }
    partition
}

/// Thread-shared buffer that `tracing_subscriber::fmt` writes into, so a test
/// can assert on what the ingest actually logged.
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

/// Scope a WARN-level capturing subscriber to the current thread.
/// `#[tokio::test]` runs a current-thread runtime, so the ingest future — and
/// the completion WARN it emits — run on this same thread and land in the
/// buffer.
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
/// did succeed. What it must not do is stay silent, so ingest counts its
/// enrichment inference calls and their failures, and when ALL of them failed
/// it names the substitution at completion. The corpus it leaves is what the
/// checker's `LowEnrichmentCoverage` reads: listed as having asked, with no
/// field model.
///
/// Delete the completion WARN from `engine/ingest.rs` and this fails on the
/// log assertion.
#[tokio::test]
async fn a_total_inference_outage_says_so_at_completion() {
    let logs = Arc::new(Mutex::new(Vec::new()));
    let (_dir, engine) = {
        let _guard = capture_warns(logs.clone());
        installed_corpus(FIELD_MODEL).await
    };
    let engine: Arc<dyn CorpusReadPort> = engine;
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
    let installed = engine.installed_indexes().await.unwrap();
    let info = installed
        .iter()
        .find(|i| i.corpus_id == "health_corpus")
        .expect("the fixture corpus must be installed");
    assert!(
        info.enrichment_requested,
        "the corpus must be listed as having asked for enrichment"
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
        let _ = installed_corpus("").await;
        // Instrument check (§18.4): this canary proves the buffer is live and
        // this thread's events reach it, so the silence below is not vacuous.
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

/// A COMPLETE install that simply never got promoted is still a first-class
/// installed corpus, listed at its real path — and `open_index_for_corpus`,
/// which joins the canonical name, cannot open it. That pair is the blind
/// spot svrn's checker is tested against.
#[tokio::test]
async fn an_unpromoted_partition_is_listed_at_its_real_path() {
    let (dir, _engine) = installed_corpus(FIELD_MODEL).await;
    let partition = unpromote(dir.path(), false);
    let engine = cold_engine(dir.path());

    let installed = engine.installed_indexes().await.unwrap();
    let info = installed
        .iter()
        .find(|i| i.corpus_id == "health_corpus")
        .expect("an unpromoted partition is still a complete, installed corpus");
    assert_eq!(
        info.path, partition,
        "the listing must report the real path"
    );
    assert!(info.enrichment_requested);
    assert!(
        engine.open_index_for_corpus("health_corpus").await.is_err(),
        "index_dir/<corpus_id> does not exist"
    );
}

/// An ingest that dies inside its enrichment phase leaves
/// `<corpus_id>-partition-<node>/` behind with `ingestion_in_progress: true`
/// beside `indexes_built: true` (`docs/TRACE_ENRICHMENT_ENABLED_FLAG.md` §3).
/// Built here from a REAL ingest, not a hand-written meta: no installed
/// listing can see it, the canonical open fails, and only
/// `incomplete_ingests` names it, with the ask and the late-failure flag.
#[tokio::test]
async fn a_failed_enrichment_ingest_is_listed_only_as_incomplete() {
    let (dir, _engine) = installed_corpus(FIELD_MODEL).await;
    let partition = unpromote(dir.path(), true);
    let engine = cold_engine(dir.path());

    let installed = engine.installed_indexes().await.unwrap();
    assert!(
        !installed.iter().any(|i| i.corpus_id == "health_corpus"),
        "a mid-ingest directory must not appear as an installed corpus"
    );
    assert!(engine.open_index_for_corpus("health_corpus").await.is_err());

    let seen = engine.incomplete_ingests();
    assert_eq!(seen.len(), 1, "found {seen:#?}");
    assert_eq!(seen[0].corpus_id, "health_corpus");
    assert_eq!(seen[0].path, partition);
    assert!(
        seen[0].indexes_built,
        "the fixture ingest built its search indexes — the late-failure half \
         of the fingerprint"
    );
    assert!(seen[0].enrichment_requested);
}

/// The control: a plain interrupted ingest is listed as incomplete too, and
/// carries no enrichment ask, which is what keeps it out of the checker's
/// report.
#[tokio::test]
async fn an_interrupted_plain_ingest_is_incomplete_without_the_ask() {
    let (dir, _engine) = installed_corpus("").await;
    unpromote(dir.path(), true);
    let engine = cold_engine(dir.path());

    let seen = engine.incomplete_ingests();
    assert_eq!(seen.len(), 1, "found {seen:#?}");
    assert!(
        !seen[0].enrichment_requested,
        "this fixture's recipe has no [enrichment] block"
    );
}

/// Counts the turn-level foreground contract (issue #57 rec 4).
#[derive(Default)]
struct CountingForeground {
    begun: AtomicUsize,
    ended: AtomicUsize,
}

impl ForegroundSignal for CountingForeground {
    fn begin(&self) {
        self.begun.fetch_add(1, SeqCst);
    }
    fn end(&self) {
        self.ended.fetch_add(1, SeqCst);
    }
}

/// The lease svrn's turn holds (sovereign-tools turn_foreground_lease) comes
/// from here: `None` with no signal installed, and once the daemon installs
/// one, a lease that begins on acquire and ends on drop, fresh each time.
#[test]
fn the_foreground_lease_is_the_installed_signal_paired() {
    let dir = tempfile::tempdir().unwrap();
    let engine = CorpusEngine::new(
        dir.path().join("recipes"),
        dir.path().join("indexes"),
        mock_embed_fn(),
    );
    assert!(
        CorpusReadPort::foreground_lease(&engine).is_none(),
        "no signal installed, no lease"
    );

    let signal = Arc::new(CountingForeground::default());
    engine.set_foreground_signal(signal.clone());
    let port: &dyn CorpusReadPort = &engine;
    let lease = port.foreground_lease().expect("a signal is installed");
    assert_eq!(signal.begun.load(SeqCst), 1, "begun on acquire");
    assert_eq!(signal.ended.load(SeqCst), 0, "held while alive");
    drop(lease);
    assert_eq!(signal.ended.load(SeqCst), 1, "ended on drop");

    drop(port.foreground_lease());
    assert_eq!(signal.begun.load(SeqCst), 2);
    assert_eq!(signal.ended.load(SeqCst), 2);
}
