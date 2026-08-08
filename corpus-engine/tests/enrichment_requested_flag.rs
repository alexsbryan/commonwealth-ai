// SPDX-License-Identifier: AGPL-3.0-or-later
//! `IndexMeta.enrichment_requested` — the flag that makes the enrichment
//! health check reachable.
//!
//! **The thing being fixed.** Until 2026-08-07 this field was called
//! `enrichment_enabled`, was written in exactly one place
//! (`index/create.rs`, always `false`), and had no setter. Every corpus on
//! every machine therefore reported `false`, which made
//! `EnrichmentChecker`'s opening `if !info.enrichment_enabled { continue; }`
//! fire for every corpus, always — so `LowEnrichmentCoverage` and
//! `StaleEnrichment` were unreachable for all inputs. Full trace:
//! `docs/TRACE_ENRICHMENT_ENABLED_FLAG.md` §4.
//!
//! **What these tests pin.** The stamp happens at the ENTRY of ingest's
//! `'enrichment:` block, not at its exit. That distinction is the entire
//! design: a corpus whose enrichment *failed* must still report
//! `enrichment_requested == true`, because "was supposed to be enriched" is
//! the question the standing health surface needs answered. A success-only
//! stamp would leave exactly the failure case invisible, which is the defect.
//!
//! The two `break 'enrichment` early-outs (`investigation`, `atlas`) are the
//! other direction: those recipes are deliberately not field-model-enriched
//! at install time, so they must un-stamp rather than leave a standing
//! "unfinished enrichment" complaint against every such corpus.
//!
//! Delete `index.set_enrichment_requested(true)` from `engine/ingest.rs` and
//! `ingest_stamps_enrichment_requested_when_enrichment_produces_nothing`
//! fails.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use corpus_engine::{CorpusEngine, CorpusSpec, EmbedFn, Error};

// ─── Fixtures ────────────────────────────────────────────────

fn write_source(dir: &Path) -> PathBuf {
    let path = dir.join("source.txt");
    // Several paragraphs so the paragraph chunker produces >1 chunk and the
    // ingest reaches `build_indexes` (a zero-chunk ingest fails earlier and
    // never enters the enrichment block at all).
    let text = "The ledger opened on a Tuesday, which is the sort of detail \
                nobody records unless it matters later.\n\n\
                By Thursday the second column had been ruled twice, once in \
                pencil and once in something darker.\n\n\
                The auditor's note in the margin read: reconcile before the \
                quarter closes, and no later.\n\n\
                Nobody reconciled anything. The quarter closed on its own, \
                the way quarters do.\n";
    std::fs::write(&path, text).unwrap();
    path
}

/// A recipe with an optional `[enrichment]` block appended verbatim, so each
/// test states exactly the enrichment shape it is exercising.
fn write_recipe(recipes_dir: &Path, source: &Path, enrichment_block: &str) -> PathBuf {
    let recipe_path = recipes_dir.join("test_corpus.toml");
    let source_str = source.to_string_lossy();
    let toml = format!(
        r#"
[corpus]
id = "test_corpus"
name = "Test Corpus"
description = "enrichment_requested flag fixture"
license = "CC0"
mesh_sharing = false

[acquire]
type = "local_file"
path = "{source_str}"

[extract]
type = "plaintext"

[chunk]
type = "paragraph"
max_chars = 512
overlap_chars = 64

[index]
embedding_model = "test-mock"
embedding_dimensions = 8
{enrichment_block}
"#
    );
    std::fs::write(&recipe_path, toml).unwrap();
    recipe_path
}

fn working_embed_fn() -> EmbedFn {
    Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.1_f32; 8]) }))
}

/// Inference that always errors — the stand-in for the real-world enrichment
/// failures this flag exists to keep visible (an unregistered domain, a dead
/// model slot, a mid-phase kill). The block is entered; nothing completes.
fn always_failing_inference_fn() -> corpus_engine::types::InferenceFn {
    Arc::new(|_prompt: &str, _schema: Option<&serde_json::Value>| {
        // `corpus_engine::Error` has no dedicated inference variant, so this
        // borrows the nearest one; only the message text is load-bearing.
        Box::pin(async {
            Err(Error::InvalidInput(
                "simulated enrichment-time inference outage".to_string(),
            ))
        })
    })
}

/// Read `enrichment_requested` straight off whichever `_corpus_meta.json` the
/// run left behind — canonical `<corpus>/` on success, or the partition
/// `<corpus>-partition-<node>/` when the ingest died before promotion. Reading
/// the raw JSON (rather than only the typed projection) is what makes the
/// on-disk contract, not just the in-memory struct, the thing under test.
fn meta_flag_on_disk(indexes_dir: &Path) -> (PathBuf, serde_json::Value) {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(indexes_dir)
        .unwrap_or_else(|e| panic!("indexes dir {} unreadable: {e}", indexes_dir.display()))
        .flatten()
    {
        let meta = entry.path().join("_corpus_meta.json");
        if meta.exists() {
            found.push(meta);
        }
    }
    assert_eq!(
        found.len(),
        1,
        "expected exactly one index dir with a meta under {}, found {found:?}",
        indexes_dir.display()
    );
    let raw = std::fs::read_to_string(&found[0]).unwrap();
    let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
    (found[0].clone(), json)
}

// ─── Tests ───────────────────────────────────────────────────

/// **The watched-to-fail case.** A recipe asks for field-model enrichment and
/// every inference call during enrichment fails. The corpus must record that
/// enrichment was requested, because that is the only thing that lets a
/// standing health check say "this corpus was supposed to be enriched and
/// isn't".
///
/// Note what the run does, because it is the reason the standing check
/// matters: the enrichment phases absorb a total inference outage and the
/// ingest returns `Ok` ("Ingestion complete — 1 chunks"). Nothing downstream
/// of the ingest call learns that enrichment produced nothing. The
/// `enrichment_requested` stamp is the only surviving trace that it was ever
/// supposed to happen.
///
/// Remove `index.set_enrichment_requested(true)` from the entry of the
/// `'enrichment:` block in `engine/ingest.rs` and this test fails on the
/// `enrichment_requested` assertions — the corpus goes back to reporting
/// `false`, which is the state that made `EnrichmentChecker` dead code.
#[tokio::test]
async fn ingest_stamps_enrichment_requested_when_enrichment_produces_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let recipes_dir = dir.path().join("recipes");
    let indexes_dir = dir.path().join("indexes");
    std::fs::create_dir_all(&recipes_dir).unwrap();
    let source = write_source(dir.path());

    let recipe_path = write_recipe(
        &recipes_dir,
        &source,
        r#"
[enrichment]
enabled = true
type = "field_model"
domain = "philosophy"
"#,
    );

    let engine = CorpusEngine::new(recipes_dir, indexes_dir.clone(), working_embed_fn())
        .with_embedding_model("test-mock")
        .with_inference_fn(always_failing_inference_fn());

    engine
        .ingest(&CorpusSpec::RecipePath(recipe_path), None)
        .await
        .expect("ingest returns Ok — the enrichment phases swallow the outage");

    // Validate the instrument before the verdict (§18.4): the enrichment
    // block really was entered and really produced nothing. If a future
    // change made enrichment succeed here, the flag assertion below would
    // still pass while testing something else entirely.
    let index = engine
        .open_index_for_corpus("test_corpus")
        .await
        .expect("a completed ingest must be openable by corpus id");
    assert!(
        !index.has_field_model_tables().await,
        "the fixture must end UN-enriched — otherwise this is not the \
         requested-but-not-done state under test"
    );

    let (meta_path, meta) = meta_flag_on_disk(&indexes_dir);
    assert_eq!(
        meta.get("enrichment_requested").and_then(|v| v.as_bool()),
        Some(true),
        "the entry stamp must survive the failure — {} says {meta:#}",
        meta_path.display()
    );

    // And the typed projection agrees, which is the surface
    // `EnrichmentChecker` actually reads.
    let installed = engine.installed_indexes().await.unwrap();
    let info = installed
        .iter()
        .find(|i| i.corpus_id == "test_corpus")
        .expect("the corpus must be listed as installed");
    assert!(
        info.enrichment_requested,
        "IndexInfo::enrichment_requested must project the stamped meta"
    );
}

/// The control. No `[enrichment]` block means the block is never entered and
/// nothing is stamped — so a plain corpus never shows up in the enrichment
/// health check. Without this, a stamp accidentally moved outside the
/// enrichment block would make the checker complain about every corpus on the
/// machine, which is the opposite failure and just as useless.
#[tokio::test]
async fn a_recipe_without_enrichment_never_claims_it_was_requested() {
    let dir = tempfile::tempdir().unwrap();
    let recipes_dir = dir.path().join("recipes");
    let indexes_dir = dir.path().join("indexes");
    std::fs::create_dir_all(&recipes_dir).unwrap();
    let source = write_source(dir.path());
    let recipe_path = write_recipe(&recipes_dir, &source, "");

    let engine = CorpusEngine::new(recipes_dir, indexes_dir.clone(), working_embed_fn())
        .with_embedding_model("test-mock");

    engine
        .ingest(&CorpusSpec::RecipePath(recipe_path), None)
        .await
        .expect("a plain ingest must succeed");

    let (_, meta) = meta_flag_on_disk(&indexes_dir);
    assert_eq!(
        meta.get("enrichment_requested").and_then(|v| v.as_bool()),
        Some(false),
        "no [enrichment] block means nothing was requested"
    );
}

/// The `break 'enrichment` early-out direction. An `investigation` recipe is
/// enriched by an explicit later command (`sovereign enrich investigation
/// build <id>`), never at install — so install must take the request back.
/// Leaving `true` here would make the health check report a standing,
/// permanent "unfinished enrichment" against every investigation corpus,
/// which is a false positive, not a finding.
#[tokio::test]
async fn an_investigation_recipe_takes_the_enrichment_request_back() {
    let dir = tempfile::tempdir().unwrap();
    let recipes_dir = dir.path().join("recipes");
    let indexes_dir = dir.path().join("indexes");
    std::fs::create_dir_all(&recipes_dir).unwrap();
    let source = write_source(dir.path());

    let recipe_path = write_recipe(
        &recipes_dir,
        &source,
        r#"
[enrichment]
enabled = true
type = "investigation"
"#,
    );

    let engine = CorpusEngine::new(recipes_dir, indexes_dir.clone(), working_embed_fn())
        .with_embedding_model("test-mock")
        // Inference is present so the block is ENTERED (the `None` arm skips
        // it entirely); it must never be called on this path.
        .with_inference_fn(always_failing_inference_fn());

    engine
        .ingest(&CorpusSpec::RecipePath(recipe_path), None)
        .await
        .expect("an investigation recipe must install cleanly — it skips install-time enrichment");

    let (_, meta) = meta_flag_on_disk(&indexes_dir);
    assert_eq!(
        meta.get("enrichment_requested").and_then(|v| v.as_bool()),
        Some(false),
        "the investigation early-out must un-stamp the entry request"
    );
}

/// **Back-compat.** Every `_corpus_meta.json` already on disk spells this key
/// `enrichment_enabled`. The `#[serde(alias = "enrichment_enabled")]` on
/// `IndexMeta` is what keeps those parsing, and this test proves it against a
/// real meta rather than a hand-written stub: ingest normally, rewrite the one
/// key back to its pre-rename spelling, and re-read through the same
/// `installed_indexes()` path production uses.
///
/// It also pins the harder half — an old meta that said `true` must still read
/// as `true`, not be silently defaulted to `false` by an unmatched key (§18.3:
/// absence is reported, never defaulted; here the key is present and must be
/// honoured).
#[tokio::test]
async fn a_pre_rename_meta_still_deserializes() {
    let dir = tempfile::tempdir().unwrap();
    let recipes_dir = dir.path().join("recipes");
    let indexes_dir = dir.path().join("indexes");
    std::fs::create_dir_all(&recipes_dir).unwrap();
    let source = write_source(dir.path());
    let recipe_path = write_recipe(&recipes_dir, &source, "");

    let engine = CorpusEngine::new(recipes_dir, indexes_dir.clone(), working_embed_fn())
        .with_embedding_model("test-mock");
    engine
        .ingest(&CorpusSpec::RecipePath(recipe_path), None)
        .await
        .expect("plain ingest must succeed");

    // Rewrite the real meta into its pre-rename shape: the key spelled the old
    // way, and set to `true` so a silent default-to-false is detectable.
    let (meta_path, mut meta) = meta_flag_on_disk(&indexes_dir);
    let obj = meta.as_object_mut().unwrap();
    obj.remove("enrichment_requested")
        .expect("the fresh meta must carry the NEW key — otherwise this test proves nothing");
    obj.insert("enrichment_enabled".into(), serde_json::Value::Bool(true));
    std::fs::write(&meta_path, serde_json::to_string_pretty(&meta).unwrap()).unwrap();

    let installed = engine.installed_indexes().await.unwrap();
    let info = installed
        .iter()
        .find(|i| i.corpus_id == "test_corpus")
        .expect("a pre-rename meta must still list as an installed corpus");
    assert!(
        info.enrichment_requested,
        "the serde alias must carry an old `enrichment_enabled: true` through \
         to `enrichment_requested`; a legacy meta that silently reads `false` \
         would re-blind the enrichment health check for every corpus \
         installed before the rename"
    );
}
