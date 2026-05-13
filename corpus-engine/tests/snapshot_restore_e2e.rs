//! End-to-end empirical validation of the prebuilt-snapshot restore
//! path against a real archive on disk.
//!
//! Marked `#[ignore]` so it only runs on demand — the test reads a
//! multi-GB local file. Invoke explicitly:
//!
//! ```bash
//! SNAPSHOT_PATH=~/.sovereign/snapshots/wikipedia-qwen-embedding-0.6b-2026-05-13.tar.zst \
//!   cargo test --release --package corpus-engine --test snapshot_restore_e2e \
//!     -- --ignored --nocapture restore_real_wikipedia_snapshot_as_sibling
//! ```
//!
//! What it proves:
//!   1. The on-disk .tar.zst archive parses end-to-end.
//!   2. The sha256 in the recipe's `[prebuilt]` block matches the
//!      bytes on disk.
//!   3. The restorer rewrites tar paths and patches
//!      `_corpus_meta.json::corpus_id` so the archive can be installed
//!      under a non-default corpus id without clobbering the original.
//!   4. The result has the expected on-disk layout (LanceDB chunks
//!      table, atlas subtree, meta file).

use corpus_engine::snapshot::restore_snapshot_archive;
use std::path::PathBuf;

const WIKIPEDIA_PREBUILT_SHA256: &str =
    "65fa045c95d6ffdaa92b7634ea95acb2d9d4bac6c9f6cd20bc831c098c10c6bd";
const TARGET_CORPUS_ID: &str = "wikipedia-prebuilt-test";

fn snapshot_path() -> Option<PathBuf> {
    std::env::var_os("SNAPSHOT_PATH").map(PathBuf::from)
}

#[test]
#[ignore]
fn restore_real_wikipedia_snapshot_as_sibling() {
    let Some(archive) = snapshot_path() else {
        panic!(
            "set SNAPSHOT_PATH to the local .tar.zst (e.g. \
             ~/.sovereign/snapshots/wikipedia-qwen-embedding-0.6b-2026-05-13.tar.zst)"
        );
    };
    assert!(archive.is_file(), "{} does not exist", archive.display());

    let restore_root = tempfile::tempdir().expect("create tmp dir for restore root");
    let restore_root = restore_root.path().to_path_buf();
    eprintln!("Restore root: {}", restore_root.display());

    let outcome = restore_snapshot_archive(
        &archive,
        &restore_root,
        TARGET_CORPUS_ID,
        Some(WIKIPEDIA_PREBUILT_SHA256),
        "qwen-embedding-0.6b",
        1024,
    )
    .expect("restore_snapshot_archive succeeded");

    // ── Manifest integrity ────────────────────────────────────
    assert_eq!(
        outcome.manifest.corpus_id, "wikipedia",
        "manifest preserves the archive's original id"
    );
    assert_eq!(outcome.manifest.embedding_model, "qwen-embedding-0.6b");
    assert_eq!(outcome.manifest.embedding_dimensions, 1024);
    assert_eq!(outcome.manifest.chunk_count, 1_847_442);

    // ── Path rewriting landed under target id ─────────────────
    let expected_index = restore_root.join("indexes").join(TARGET_CORPUS_ID);
    assert_eq!(outcome.index_dir, expected_index);
    assert!(outcome.index_dir.is_dir(), "index dir missing");
    assert!(
        !restore_root.join("indexes/wikipedia").exists(),
        "rename failed: archive corpus-id path was created instead of target's"
    );

    // ── Expected on-disk layout ───────────────────────────────
    assert!(outcome.index_dir.join("_corpus_meta.json").is_file());
    assert!(
        outcome.index_dir.join("chunks.lance").is_dir(),
        "LanceDB chunks table missing"
    );
    assert!(
        outcome.index_dir.join("atlas").is_dir(),
        "atlas subtree missing — published with atlas embedded under indexes/<id>/atlas/"
    );

    // ── _corpus_meta.json patched to target id ────────────────
    let meta: serde_json::Value = serde_json::from_slice(
        &std::fs::read(outcome.index_dir.join("_corpus_meta.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        meta.get("corpus_id").and_then(|v| v.as_str()),
        Some(TARGET_CORPUS_ID),
        "patch_meta_corpus_id did not run / produced wrong value"
    );
    assert_eq!(
        meta.get("embedding_model").and_then(|v| v.as_str()),
        Some("qwen-embedding-0.6b"),
        "embedding_model should be preserved across rename"
    );

    // ── No separate enrichment subtree expected for this snapshot ──
    // The wikipedia archive bundled the atlas under indexes/<id>/atlas/
    // rather than enrichment/<id>/, so atlas_included=false in the
    // manifest and enrichment_dir comes back None.
    assert!(outcome.enrichment_dir.is_none(),
        "this snapshot has atlas embedded in the index, not a separate enrichment subtree");
    assert!(
        !restore_root.join("enrichment").join(TARGET_CORPUS_ID).exists(),
        "no enrichment subtree should be created for this archive"
    );

    // ── Bytes match HF reported size ──────────────────────────
    assert_eq!(
        outcome.archive_size_bytes, 8_386_497_983,
        "archive_size_bytes should match the bytes we sha-verified"
    );

    eprintln!(
        "\n✓ End-to-end restore validated.\n  index: {}\n  meta.corpus_id: {}\n  chunks_lance/: present\n  atlas/: present",
        outcome.index_dir.display(),
        TARGET_CORPUS_ID,
    );
}
