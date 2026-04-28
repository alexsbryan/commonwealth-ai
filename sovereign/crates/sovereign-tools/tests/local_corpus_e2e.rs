//! End-to-end test for `LocalCorpusManager`:
//!     register → pre_scan → ingest → search.
//!
//! Uses a mock embedding function (fixed non-zero vector per string)
//! so the test doesn't need a running llama.cpp / sovereign-inference
//! process.

use std::path::Path;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use corpus_engine::{CorpusEngine, EmbedFn};
use sovereign_core::error::Result as SovResult;
use sovereign_store::memory::InMemoryStateStore;
use tempfile::TempDir;

use sovereign_tools::local_corpus::{LocalCorpusConfig, LocalCorpusManager};

// ─── Mock embedder ───────────────────────────────────────────────────

/// Deterministic 32-dim embedding: byte histogram (`b as usize % 32`)
/// normalised to unit length. Good enough to produce non-zero vectors
/// that differ per input, which lets LanceDB's FTS+vector paths both
/// execute. No external service required.
fn mock_embed_fn(call_count: Arc<AtomicUsize>) -> EmbedFn {
    Arc::new(move |text: &str| {
        call_count.fetch_add(1, Ordering::Relaxed);
        let mut v = vec![0f32; 32];
        for b in text.as_bytes() {
            v[(*b as usize) % 32] += 1.0;
        }
        // Normalise.
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
        for x in &mut v {
            *x /= norm;
        }
        Box::pin(async move { Ok(v) })
    })
}

// ─── Test harness ────────────────────────────────────────────────────

struct Harness {
    _tmp: TempDir,
    data_dir: std::path::PathBuf,
    manager: LocalCorpusManager,
    _store: Arc<InMemoryStateStore>,
}

async fn harness() -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_path_buf();
    std::fs::create_dir_all(data_dir.join("indexes")).unwrap();
    std::fs::create_dir_all(data_dir.join("recipes")).unwrap();

    let store: Arc<InMemoryStateStore> = Arc::new(InMemoryStateStore::new());

    let embed_calls = Arc::new(AtomicUsize::new(0));
    let engine = Arc::new(CorpusEngine::new(
        data_dir.join("recipes"),
        data_dir.join("indexes"),
        mock_embed_fn(embed_calls),
    ));

    let manager = LocalCorpusManager::init(
        engine,
        store.clone() as Arc<dyn sovereign_core::traits::StateStore>,
        None,
        data_dir.clone(),
        data_dir.join("vault-snapshots"),
    )
    .await
    .expect("manager init");

    Harness {
        _tmp: tmp,
        data_dir,
        manager,
        _store: store,
    }
}

fn write_text_file(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, body).unwrap();
}

// ─── The test ────────────────────────────────────────────────────────

#[tokio::test]
async fn folder_register_prescan_ingest_search_roundtrip() -> SovResult<()> {
    let h = harness().await;

    // Build a synthetic folder of 3 readable text files + 1 hidden
    // file + 1 file with an unsupported extension.
    let folder = h.data_dir.join("source");
    std::fs::create_dir_all(&folder).unwrap();
    write_text_file(
        &folder,
        "alpha.txt",
        "Alpha document. The FOIA response covers budget allocations for 2023.",
    );
    write_text_file(
        &folder,
        "beta.txt",
        "Beta document discusses climate policy and carbon pricing mechanisms.",
    );
    write_text_file(
        &folder,
        "gamma.txt",
        "Gamma document contains meeting minutes and action items.",
    );
    write_text_file(&folder, ".hidden.txt", "should be ignored");
    write_text_file(&folder, "ignored.odt", "unsupported extension");

    // Register the folder.
    let cfg = LocalCorpusConfig::document_folder(folder.clone(), "City council 2024".into());
    let id = h.manager.register(cfg.clone()).await?;
    assert_eq!(id, cfg.id);
    let listed = h.manager.list().await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, id);

    // Pre-scan.
    let scan = h.manager.pre_scan(&id, None).await?;
    assert_eq!(
        scan.readable.len(),
        3,
        "three .txt files should be readable"
    );
    assert_eq!(scan.ignored_types, 1, ".odt should be counted as ignored");
    // Hidden file is silently skipped, not counted as ignored.
    // total_visited equals candidates + ignored = 3 + 1 = 4.
    assert_eq!(scan.total_visited, 4);
    assert!(scan.scanned_pdfs.is_empty());
    assert!(scan.protected_pdfs.is_empty());
    assert!(scan.corrupt_files.is_empty());

    // Ingest.
    let stats = h.manager.ingest(&id, None, None).await?;
    assert_eq!(stats.corpus_id, id);
    assert_eq!(stats.files_indexed, 3);
    assert!(
        stats.chunks_written >= 3,
        "should have at least one chunk per file, got {}",
        stats.chunks_written
    );
    assert!(
        stats.runtime_failures.is_empty(),
        "no runtime failures expected: {:?}",
        stats.runtime_failures
    );

    // Search — embeddings are deterministic so we know content from
    // "beta.txt" should rank highly for "climate policy".
    let results = h.manager.search(&id, "climate policy carbon", 5).await?;
    assert!(
        !results.is_empty(),
        "search should return at least one hit from the ingested corpus"
    );
    let top = &results[0];
    assert_eq!(top.corpus_id, id);
    assert!(
        top.content.to_ascii_lowercase().contains("climate")
            || top.content.to_ascii_lowercase().contains("carbon"),
        "top hit should reference climate/carbon; got: {}",
        top.content
    );

    Ok(())
}

#[tokio::test]
async fn empty_folder_returns_zero_indexed() -> SovResult<()> {
    let h = harness().await;
    let folder = h.data_dir.join("empty");
    std::fs::create_dir_all(&folder).unwrap();

    let cfg = LocalCorpusConfig::document_folder(folder.clone(), "Empty".into());
    let id = h.manager.register(cfg).await?;
    let stats = h.manager.ingest(&id, None, None).await?;
    assert_eq!(stats.files_indexed, 0);
    assert_eq!(stats.chunks_written, 0);
    Ok(())
}

#[tokio::test]
async fn obsidian_vault_ingest_reads_markdown_and_strips_frontmatter() -> SovResult<()> {
    let h = harness().await;
    let vault = h.data_dir.join("vault");
    std::fs::create_dir_all(&vault).unwrap();

    // Two notes with frontmatter, one without.
    write_text_file(
        &vault,
        "consciousness.md",
        "---\ntags:\n  - philosophy\n  - mind\n---\n# Consciousness\n\nQualia and the hard problem of consciousness.",
    );
    write_text_file(
        &vault,
        "qualia.md",
        "---\ntype: note\n---\n# Qualia\n\nPhenomenal experience and philosophical zombies.",
    );
    write_text_file(&vault, "plain.md", "# Plain\n\nNo frontmatter here.");

    // Hidden `.obsidian/` workspace — must be ignored.
    let dot_obs = vault.join(".obsidian");
    std::fs::create_dir_all(&dot_obs).unwrap();
    write_text_file(
        &dot_obs,
        "workspace.json",
        r#"{"this":"should be ignored"}"#,
    );
    // A non-markdown file at the root — extension filter should skip it.
    write_text_file(&vault, "diagram.svg", "<svg></svg>");

    let snap_root = h.data_dir.join("vault-snapshots");
    let cfg = LocalCorpusConfig::obsidian_vault(vault.clone(), snap_root);
    assert!(cfg.write_back.is_some(), "vault config carries write_back");
    assert!(cfg.watcher.enabled, "vault watcher default on");

    let id = h.manager.register(cfg).await?;

    let scan = h.manager.pre_scan(&id, None).await?;
    assert_eq!(scan.readable.len(), 3, "three .md notes should be readable");
    assert_eq!(scan.ignored_types, 1, ".svg should count as ignored");

    let stats = h.manager.ingest(&id, None, None).await?;
    assert_eq!(stats.files_indexed, 3);
    assert!(stats.chunks_written >= 3);
    assert!(stats.runtime_failures.is_empty());

    // Search should hit content *after* frontmatter stripping — the
    // body mentions "qualia" but the frontmatter key `tags:` does not,
    // and if we were indexing the raw `---` delimiters they'd show up
    // as noise.
    let results = h.manager.search(&id, "qualia phenomenal", 5).await?;
    assert!(!results.is_empty());
    let top_text = &results[0].content.to_ascii_lowercase();
    assert!(
        top_text.contains("qualia") || top_text.contains("phenomenal"),
        "top hit should contain a body term, got: {}",
        results[0].content
    );
    assert!(
        !top_text.contains("---"),
        "frontmatter delimiters should not be in indexed content"
    );

    Ok(())
}

/// End-to-end through the manager: ingest a vault → run write-back
/// directly against a synthetic preview → verify sovereign/* tags
/// appear, user tags survive → rollback → verify bytes restored →
/// clean → verify no sovereign/* tags remain.
///
/// This bypasses the clustering + LLM labelling path (which requires
/// an `InferenceProvider`), exercising the write-back wiring through
/// `LocalCorpusManager`'s thin orchestration layer with a
/// hand-constructed preview.
#[tokio::test]
async fn vault_write_rollback_clean_round_trip() -> SovResult<()> {
    use sovereign_tools::local_corpus::clusterer::LabeledCluster;
    use sovereign_tools::local_corpus::preview::{
        ClusterSummary, FileAssignment, VaultPreview,
    };
    use sovereign_tools::local_corpus::writeback::WriteBack;

    let h = harness().await;
    let vault = h.data_dir.join("vault");
    std::fs::create_dir_all(&vault).unwrap();

    let original_a = "---\ntags: [draft]\n---\n# Note A\n\nSome body.\n";
    let original_b = "# Note B (no frontmatter)\n\nJust body.\n";
    write_text_file(&vault, "a.md", original_a);
    write_text_file(&vault, "b.md", original_b);

    let snap_root = h.data_dir.join("vault-snapshots");
    let cfg = LocalCorpusConfig::obsidian_vault(vault.clone(), snap_root);
    let wb_cfg = cfg.write_back.clone().expect("vault has write_back");
    let corpus_id = cfg.id.clone();
    h.manager.register(cfg).await?;

    // Build a preview by hand: two notes, one cluster. The
    // manager-level `write_tags` reads its preview from the in-
    // memory cache produced by `cluster`, so we write via
    // `WriteBack::execute` directly rather than trying to stub the
    // LLM.
    let preview = VaultPreview {
        clusters: vec![ClusterSummary {
            cluster: LabeledCluster {
                id: 1,
                tag_path: "test/both".into(),
                display_name: "Both Notes".into(),
                description: "A tiny cluster of two notes.".into(),
                note_count: 2,
                centroid_chunk_ids: vec![],
            },
            assignments: vec![
                FileAssignment {
                    chunk_id: 1,
                    relative_path: "a.md".into(),
                    note_title: "a".into(),
                    primary_tag: "sovereign/test/both".into(),
                    additional_tags: vec![],
                    confidence: 0.92,
                    existing_tags: vec![],
                },
                FileAssignment {
                    chunk_id: 2,
                    relative_path: "b.md".into(),
                    note_title: "b".into(),
                    primary_tag: "sovereign/test/both".into(),
                    additional_tags: vec![],
                    confidence: 0.77,
                    existing_tags: vec![],
                },
            ],
        }],
        outliers: vec![],
        flagged: vec![],
        total_notes: 2,
        tagged_notes: 2,
        outlier_count: 0,
        open_questions: vec![],
        namespace: "sovereign".into(),
    };

    let wb = WriteBack::new(wb_cfg, vault.clone(), corpus_id.clone());
    let write_result = wb.execute(&preview, 1, None).await?;
    assert_eq!(write_result.files_tagged, 2);
    assert_eq!(write_result.index_notes_created, 1);
    assert!(write_result.files_skipped.is_empty());

    // User tag survived; sovereign tag present.
    let a_post = std::fs::read_to_string(vault.join("a.md")).unwrap();
    assert!(a_post.contains("draft"), "user tag must survive: {a_post}");
    assert!(a_post.contains("sovereign/test/both"));
    // Index note exists.
    assert!(vault.join("_sovereign-index/test/both.md").exists());

    // Snapshot visible via the manager.
    let snaps = h.manager.list_snapshots(&corpus_id).await?;
    assert_eq!(snaps.len(), 1);

    // Rollback via the manager — bytes of a.md restore exactly.
    let _rb = h
        .manager
        .rollback(&corpus_id, &snaps[0].snapshot_path)
        .await?;
    let a_restored = std::fs::read_to_string(vault.join("a.md")).unwrap();
    assert_eq!(a_restored, original_a, "rollback must restore bytes");
    let b_restored = std::fs::read_to_string(vault.join("b.md")).unwrap();
    assert_eq!(b_restored, original_b);
    assert!(!vault.join("_sovereign-index/test/both.md").exists());

    // Write again; this time test the `clean` path via manager.
    let _ = wb.execute(&preview, 2, None).await?;
    let clean_result = h.manager.clean(&corpus_id).await?;
    assert!(clean_result.tags_removed_from >= 2);
    assert!(clean_result.index_notes_deleted >= 1);

    let a_clean = std::fs::read_to_string(vault.join("a.md")).unwrap();
    assert!(a_clean.contains("draft"), "user tag preserved by clean");
    assert!(
        !a_clean.contains("sovereign/"),
        "sovereign tags gone: {a_clean}"
    );
    assert!(
        !a_clean.contains("sovereign_"),
        "sovereign keys gone: {a_clean}"
    );
    assert!(!vault.join("_sovereign-index").exists());

    Ok(())
}

#[tokio::test]
async fn register_persists_across_manager_reload() -> SovResult<()> {
    let h = harness().await;
    let folder = h.data_dir.join("reload-source");
    std::fs::create_dir_all(&folder).unwrap();
    write_text_file(&folder, "a.txt", "reload test content");

    let cfg = LocalCorpusConfig::document_folder(folder, "Reload Test".into());
    let id = h.manager.register(cfg.clone()).await?;
    assert_eq!(h.manager.list().await.len(), 1);

    // Simulate a relaunch by constructing a second manager over the
    // same data_dir.
    let store = h._store.clone();
    let engine = Arc::new(CorpusEngine::new(
        h.data_dir.join("recipes"),
        h.data_dir.join("indexes"),
        mock_embed_fn(Arc::new(AtomicUsize::new(0))),
    ));
    let manager2 = LocalCorpusManager::init(
        engine,
        store as Arc<dyn sovereign_core::traits::StateStore>,
        None,
        h.data_dir.clone(),
        h.data_dir.join("vault-snapshots"),
    )
    .await?;
    let listed = manager2.list().await;
    assert_eq!(listed.len(), 1, "persisted config should reload");
    assert_eq!(listed[0].id, id);

    Ok(())
}
