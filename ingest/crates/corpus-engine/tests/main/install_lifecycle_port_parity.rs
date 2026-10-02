// SPDX-License-Identifier: AGPL-3.0-or-later
//! The engine half of sovereign-daemon's corpus_lifecycle
//! (pb-ingest-dial-daemon-tests, phase-b-52).
//!
//! corpus_lifecycle drives `IngestPortDouble` and asserts the daemon's
//! install/pause/cancel/status bookkeeping and the port calls it makes.
//! What ingest does with those calls is proven here, through `&dyn
//! IngestPort` / `&dyn LocalCorpusPort` (never the inherent methods), over
//! the fixture that file seeded until the split: a local JSONL recipe of
//! paragraph docs, an 8-dimension mock embed, node id `node-test`.
//!
//! - a registry install runs to a finalised canonical and leaves no
//!   partition;
//! - a paused install keeps its partition, and the next install finalises
//!   from it;
//! - a cancelled install, wiped, leaves neither canonical nor partition;
//! - an unknown id is refused as not found, naming the corpus;
//! - an install whose embed fails errs and leaves no canonical;
//! - the article sampler estimates sections from the extracted JSONL,
//!   writes its sidecar, and reads it back.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use corpus_engine::CorpusEngine;
use corpus_index::corpus::Corpus;
use corpus_index::ingest_port::daemon::{IngestPort, InstallRefusal};
use corpus_index::ingest_port::LocalCorpusPort;
use corpus_index::types::EmbedFn;

/// Deterministic 8-dim vector derived from the input text, spread enough
/// that IVF-PQ training sees a non-degenerate distribution.
fn mock_embedding(text: &str) -> Vec<f32> {
    let mut bytes = [0u8; 32];
    for (i, b) in text.as_bytes().iter().enumerate() {
        bytes[i % 32] ^= *b;
    }
    (0..8)
        .map(|i| {
            let chunk = &bytes[i * 4..(i + 1) * 4];
            let as_u32 = u32::from_le_bytes(chunk.try_into().unwrap());
            (as_u32 as f32) / (u32::MAX as f32) * 2.0 - 1.0
        })
        .collect()
}

/// The embed the fixture's engine uses: 2 ms a call while `slow`, an
/// error while `fail`.
fn embed(slow: Arc<AtomicBool>, fail: Arc<AtomicBool>) -> EmbedFn {
    Arc::new(move |text: &str| {
        let (slow, fail) = (Arc::clone(&slow), Arc::clone(&fail));
        let v = mock_embedding(text);
        Box::pin(async move {
            if fail.load(Ordering::SeqCst) {
                return Err(corpus_index::Error::Embed(
                    "simulated mid-install embed failure".into(),
                ));
            }
            if slow.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            Ok(v)
        })
    })
}

/// corpus_lifecycle's recipe and JSONL source, `doc_count` docs.
fn seed_fixture(dir: &Path, corpus_id: &str, doc_count: usize) {
    let recipes = dir.join("recipes");
    std::fs::create_dir_all(&recipes).unwrap();
    std::fs::create_dir_all(dir.join("indexes")).unwrap();
    let source = dir.join(format!("{corpus_id}.jsonl"));
    let body: String = (0..doc_count)
        .map(|i| {
            serde_json::json!({
                "title": format!("Article {i}"),
                "text": format!(
                    "This is a paragraph of test content for article {i}. \
                     It is long enough to be kept by the chunker rather than \
                     filtered out as noise, and it carries a handful of stop \
                     words so the full-text index isn't trivially empty. \
                     Padding padding padding padding padding."
                ),
            })
            .to_string()
                + "\n"
        })
        .collect();
    std::fs::write(&source, body).unwrap();
    std::fs::write(
        recipes.join(format!("{corpus_id}.toml")),
        format!(
            r#"[corpus]
id = "{corpus_id}"
name = "Test Corpus"
description = "Integration-test fixture"
license = "MIT"
mesh_sharing = true
size_compressed_gb = 0.0
size_indexed_gb = 0.0

[acquire]
type = "local_file"
path = "{}"

[extract]
type = "jsonl"
content_field = "text"
title_field = "title"

[chunk]
type = "paragraph"
max_chars = 400
overlap_chars = 40

[index]
fts = true
vector = true
embedding_model = "mock-8d"
embedding_dimensions = 8
"#,
            source.display()
        ),
    )
    .unwrap();
}

struct Fixture {
    engine: Arc<CorpusEngine>,
    slow: Arc<AtomicBool>,
    fail: Arc<AtomicBool>,
    corpus: Corpus,
}

fn fixture(dir: &Path, corpus_id: &str, doc_count: usize) -> Fixture {
    seed_fixture(dir, corpus_id, doc_count);
    let (slow, fail) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let engine = CorpusEngine::new(
        dir.join("recipes"),
        dir.join("indexes"),
        embed(Arc::clone(&slow), Arc::clone(&fail)),
    )
    .with_embedding_model("mock-8d")
    .with_self_node_id("node-test");
    Fixture {
        engine: Arc::new(engine),
        slow,
        fail,
        corpus: Corpus::named(dir.join("indexes"), corpus_id).unwrap(),
    }
}

async fn install(engine: &Arc<CorpusEngine>, corpus_id: &str) -> corpus_index::Result<()> {
    let port: Arc<dyn IngestPort> = Arc::clone(engine) as _;
    let prepared = port
        .prepare_registry_install(corpus_id, &BTreeMap::new())
        .await
        .unwrap_or_else(|e| panic!("the local recipe resolves: {e:?}"));
    (prepared.run)(None).await.map(drop)
}

/// Start an install on its own task and return once its partition is on
/// disk, so a cancel lands mid-ingest.
async fn install_in_flight(
    f: &Fixture,
    corpus_id: &str,
) -> tokio::task::JoinHandle<corpus_index::Result<()>> {
    f.slow.store(true, Ordering::SeqCst);
    let (engine, id) = (Arc::clone(&f.engine), corpus_id.to_string());
    let task = tokio::spawn(async move { install(&engine, &id).await });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !IngestPort::corpus_disk_status(f.engine.as_ref(), corpus_id).partition_present {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the partition never appeared"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    task
}

fn read_meta(dir: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(Corpus::meta_in(dir)).unwrap()).unwrap()
}

fn assert_finalised(f: &Fixture, corpus_id: &str) {
    let disk = IngestPort::corpus_disk_status(f.engine.as_ref(), corpus_id);
    assert!(disk.canonical_present, "{disk:?}");
    assert!(!disk.canonical_in_progress, "{disk:?}");
    assert!(
        !disk.partition_present,
        "the partition was promoted: {disk:?}"
    );
    let meta = read_meta(&f.corpus.root());
    assert_eq!(meta["ingestion_in_progress"], false);
    assert_eq!(meta["is_shard"], false);
    assert!(
        meta["processed_shards"]
            .as_array()
            .is_some_and(|a| a.is_empty()),
        "{meta}"
    );
}

/// corpus_lifecycle's reinstall: a registry install through the port runs
/// to a finalised canonical, the partition-of-self promoted away.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_registry_install_runs_to_a_finalised_canonical() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixture(dir.path(), "testcorpus", 60);
    install(&f.engine, "testcorpus")
        .await
        .expect("the install completes");
    assert_finalised(&f, "testcorpus");
}

/// corpus_lifecycle's pause: the port's cancel stops an install in flight,
/// the partition and its meta survive, and the next install finalises.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_paused_install_keeps_its_partition_and_the_next_install_finalises() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixture(dir.path(), "pausetest", 600);
    let task = install_in_flight(&f, "pausetest").await;
    assert!(
        LocalCorpusPort::cancel_corpus_ingest(f.engine.as_ref(), "pausetest"),
        "the install in flight is registered for cancel"
    );
    let stopped = task.await.unwrap();
    assert!(
        matches!(stopped, Err(corpus_index::Error::Cancelled(_))),
        "{stopped:?}"
    );
    let partition = f.corpus.partition("node-test");
    assert!(
        Corpus::meta_in(&partition).is_file(),
        "a pause keeps the partition's meta for the resume"
    );

    f.slow.store(false, Ordering::SeqCst);
    install(&f.engine, "pausetest")
        .await
        .expect("the resume completes");
    assert_finalised(&f, "pausetest");
}

/// corpus_lifecycle's cancel: cancel, then the port's wipe, leaves neither
/// the canonical nor any partition.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_install_wiped_leaves_no_canonical_and_no_partition() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixture(dir.path(), "testcorpus", 600);
    let task = install_in_flight(&f, "testcorpus").await;
    LocalCorpusPort::cancel_corpus_ingest(f.engine.as_ref(), "testcorpus");
    let _ = task.await.unwrap();
    LocalCorpusPort::remove_corpus_everything(f.engine.as_ref(), "testcorpus")
        .expect("the wipe succeeds");
    assert!(!f.corpus.root().exists());
    assert!(!f.corpus.partition("node-test").exists());
    let disk = IngestPort::corpus_disk_status(f.engine.as_ref(), "testcorpus");
    assert!(
        !disk.canonical_present && !disk.partition_present,
        "{disk:?}"
    );
}

/// corpus_lifecycle's 404: an id with no local override and no registry
/// entry is refused as not found, and the refusal names it.
#[tokio::test]
async fn an_unresolvable_recipe_is_refused_as_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixture(dir.path(), "testcorpus", 1);
    let port: Arc<dyn IngestPort> = Arc::clone(&f.engine) as _;
    match port
        .prepare_registry_install("no-such-corpus-xyz", &BTreeMap::new())
        .await
    {
        Err(InstallRefusal::RecipeNotFound(reason)) => {
            assert!(reason.contains("no-such-corpus-xyz"), "{reason}")
        }
        Err(other) => panic!("refused, but not as not-found: {other:?}"),
        Ok(_) => panic!("an unknown recipe resolved"),
    }
}

/// corpus_lifecycle's failed install: an embed that fails mid-install errs
/// the run (not as a cancel) and leaves no canonical.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_install_errs_and_leaves_no_canonical() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixture(dir.path(), "failcorpus", 20);
    f.fail.store(true, Ordering::SeqCst);
    let failed = install(&f.engine, "failcorpus").await;
    match failed {
        Err(corpus_index::Error::Cancelled(_)) | Ok(()) => {
            panic!("a failing embed must fail the install: {failed:?}")
        }
        Err(e) => assert!(
            e.to_string()
                .contains("simulated mid-install embed failure"),
            "{e}"
        ),
    }
    let disk = IngestPort::corpus_disk_status(f.engine.as_ref(), "failcorpus");
    assert!(!disk.canonical_present, "{disk:?}");
}

/// corpus_lifecycle's sampler: over a canonical in progress at
/// committed_iter_pos 30 and an extracted JSONL of 50 two-section
/// articles, the disk status reads the position, nothing is cached, the
/// sample estimates about 150 sections (the daemon's 30/total lands near
/// 0.20), writes its sidecar, and reads back cached.
#[test]
fn the_article_sampler_estimates_sections_and_caches_its_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let corpus_id = "testcorpus";
    let index_dir = dir.path().join("indexes");
    let downloads = index_dir.join("_downloads");
    std::fs::create_dir_all(&downloads).unwrap();
    let canonical = index_dir.join(corpus_id);
    std::fs::create_dir_all(&canonical).unwrap();
    std::fs::write(
        Corpus::meta_in(&canonical),
        serde_json::json!({
            "corpus_id": corpus_id,
            "ingestion_in_progress": true,
            "committed_iter_pos": 30u64,
            "processed_shards": [],
        })
        .to_string(),
    )
    .unwrap();
    let jsonl: String = (0..50)
        .map(|i| {
            serde_json::json!({
                "name": format!("Article {i}"),
                "identifier": i,
                "abstract": "Stub abstract with enough padding to clear the extractor minimums.",
                "url": format!("https://en.wikipedia.org/wiki/A{i}"),
                "sections": [
                    { "name": "A", "type": "section", "has_parts": [] },
                    { "name": "B", "type": "section", "has_parts": [] }
                ]
            })
            .to_string()
                + "\n"
        })
        .collect();
    std::fs::write(
        downloads.join(format!("{corpus_id}.extracted.jsonl")),
        jsonl,
    )
    .unwrap();
    let engine = CorpusEngine::new(
        dir.path().join("recipes"),
        index_dir,
        embed(Arc::default(), Arc::default()),
    )
    .with_self_node_id("node-test");
    let port: &dyn IngestPort = &engine;

    let disk = port.corpus_disk_status(corpus_id);
    assert!(disk.canonical_in_progress, "{disk:?}");
    assert_eq!(disk.committed_iter_pos, 30);
    assert!(port.cached_article_stats(corpus_id).is_none());

    let stats = port
        .compute_article_stats(corpus_id)
        .expect("the sampler estimates the extracted JSONL");
    assert!(stats.total_articles > 0, "{stats:?}");
    let fraction = 30.0 / stats.total_sections_estimate as f32;
    assert!(
        (fraction - 0.20).abs() < 0.30,
        "expected 30/total near 0.20, got {fraction} ({stats:?})"
    );
    assert!(
        downloads
            .join(format!("{corpus_id}.extracted.jsonl.count"))
            .exists(),
        "the sidecar is written"
    );
    let cached = port
        .cached_article_stats(corpus_id)
        .expect("the sidecar reads back");
    assert_eq!(
        cached.total_sections_estimate,
        stats.total_sections_estimate
    );
}
