//! End-to-end test for the described-asset substrate (Phase 1).
//!
//! Mirrors the plan's "Demoable check": `sovereign corpus ingest
//! <mixed-binary-folder>` completes; the asset store holds raw bytes
//! and (where applicable) typed parsed caches; an asset_atoms.jsonl
//! sidecar lands per corpus with one Asset envelope per file.

use std::path::PathBuf;
use std::sync::Arc;

use corpus_engine::asset_store::{AssetStore, FilesystemAssetStore};
use corpus_engine::enrichment::atlas::atoms::AtomEnvelope;
use corpus_engine::extractors::described_asset::{
    AssetSubExtractorRegistry, DescribedAssetExtractor,
};
use corpus_engine::extractors::Extractor;

#[test]
fn dispatcher_walks_mixed_folder_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let docs_dir = dir.path().join("inbox");
    std::fs::create_dir_all(&docs_dir).unwrap();

    // Two plaintext docs (different bytes → two ledger entries).
    std::fs::write(docs_dir.join("note1.txt"), b"first note text").unwrap();
    std::fs::write(docs_dir.join("note2.txt"), b"second note text").unwrap();
    // Two duplicate binaries — one ledger entry, two Asset atoms
    // (same content-hash id, so atoms.json dedup later collapses).
    std::fs::write(docs_dir.join("a.bin"), &[0u8, 1, 2, 3, 4]).unwrap();
    std::fs::write(docs_dir.join("b.bin"), &[0u8, 1, 2, 3, 4]).unwrap();
    // Hidden + macOS junk are silently skipped.
    std::fs::write(docs_dir.join(".DS_Store"), b"junk").unwrap();

    let assets_root = dir.path().join("assets");
    let store: Arc<dyn AssetStore> =
        Arc::new(FilesystemAssetStore::new(&assets_root).unwrap());
    let sidecar: PathBuf = dir.path().join("atlas/asset_atoms.jsonl");
    let extractor = DescribedAssetExtractor {
        store: store.clone(),
        registry: AssetSubExtractorRegistry::defaults(),
        asset_atoms_sidecar: sidecar.clone(),
        max_bytes_per_asset: DescribedAssetExtractor::DEFAULT_MAX_BYTES_PER_ASSET,
    };

    let docs: Vec<_> = extractor
        .extract(&docs_dir)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(docs.len(), 4, "two text + two duplicate binaries");

    // Asset store has 3 unique entries (text × 2 + binary × 1).
    let entries = store.entries().unwrap();
    assert_eq!(entries.len(), 3);

    // Sidecar has 4 lines (one per observation; dedup at atoms.json
    // merge time).
    let sidecar_lines: Vec<_> = std::fs::read_to_string(&sidecar)
        .unwrap()
        .lines()
        .map(|l| l.to_string())
        .collect();
    assert_eq!(sidecar_lines.len(), 4);

    let mut kinds = std::collections::HashMap::<String, usize>::new();
    for line in &sidecar_lines {
        let env: AtomEnvelope = serde_json::from_str(line).expect("parse Asset envelope");
        match env {
            AtomEnvelope::Asset(a) => {
                *kinds.entry(a.asset_kind).or_default() += 1;
            }
            other => panic!("expected Asset envelope, got {other:?}"),
        }
    }
    assert_eq!(kinds.get("plaintext").copied().unwrap_or(0), 2);
    assert_eq!(kinds.get("opaque").copied().unwrap_or(0), 2);

    // ExtractedDoc metadata is well-shaped.
    for doc in &docs {
        let meta = doc.metadata.as_ref().expect("metadata populated");
        assert!(meta.get("asset_sha256").is_some());
        assert!(meta.get("asset_kind").is_some());
        assert!(meta.get("extraction_tier").is_some());
        assert!(meta.get("size").is_some());
    }
}

#[test]
fn dispatcher_records_parsed_form_when_sub_extractor_writes_one() {
    // Use a tiny custom sub-extractor that always writes a parsed
    // cache — verifies the dispatcher records parsed_form into both
    // the ledger entry AND the Asset atom.

    use corpus_engine::error::Result as CeResult;
    use corpus_engine::extractors::described_asset::{
        AssetExtraction, AssetSubExtractor, ExtractionTier,
    };

    struct StubParsedWriter;
    impl AssetSubExtractor for StubParsedWriter {
        fn detect(&self, p: &std::path::Path, _: &[u8]) -> bool {
            p.extension().and_then(|s| s.to_str()) == Some("stub")
        }
        fn extract(
            &self,
            _path: &std::path::Path,
            bytes: &[u8],
            sha256: &str,
            store: &dyn AssetStore,
        ) -> CeResult<AssetExtraction> {
            let parsed = store.put_parsed(sha256, "ndjson", b"{\"tagged\":true}\n")?;
            Ok(AssetExtraction {
                description: format!("stub asset of {} bytes", bytes.len()),
                asset_kind: "stub".into(),
                tier: ExtractionTier::Structural,
                mime: Some("application/x-stub".into()),
                parsed_form: Some(parsed),
            })
        }
        fn name(&self) -> &'static str {
            "stub"
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let docs_dir = dir.path().join("inbox");
    std::fs::create_dir_all(&docs_dir).unwrap();
    std::fs::write(docs_dir.join("payload.stub"), b"opaque-bytes").unwrap();

    let assets_root = dir.path().join("assets");
    let store: Arc<dyn AssetStore> =
        Arc::new(FilesystemAssetStore::new(&assets_root).unwrap());
    let registry = AssetSubExtractorRegistry::new();
    registry.register(Arc::new(StubParsedWriter));
    // Plaintext + opaque fallback added after the stub so the stub
    // wins on .stub extensions.
    registry.register(Arc::new(
        corpus_engine::extractors::described_asset::PlaintextSubExtractor,
    ));
    registry.register(Arc::new(
        corpus_engine::extractors::described_asset::OpaqueFallback,
    ));
    let extractor = DescribedAssetExtractor {
        store: store.clone(),
        registry,
        asset_atoms_sidecar: dir.path().join("atlas/asset_atoms.jsonl"),
        max_bytes_per_asset: DescribedAssetExtractor::DEFAULT_MAX_BYTES_PER_ASSET,
    };

    let docs: Vec<_> = extractor
        .extract(&docs_dir)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(docs.len(), 1);

    let entry = store
        .entries()
        .unwrap()
        .into_iter()
        .next()
        .expect("one ledger entry");
    assert!(
        entry.parsed_form.is_some(),
        "ledger entry must record parsed_form path"
    );

    let sidecar = std::fs::read_to_string(extractor.asset_atoms_sidecar).unwrap();
    let env: AtomEnvelope = serde_json::from_str(sidecar.lines().next().unwrap()).unwrap();
    match env {
        AtomEnvelope::Asset(a) => {
            assert_eq!(a.asset_kind, "stub");
            assert!(a.parsed_form.is_some());
        }
        other => panic!("expected Asset envelope, got {other:?}"),
    }
}
