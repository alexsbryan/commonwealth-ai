//! Described-asset dispatcher (AD-3).
//!
//! The architectural primitive every non-prose-data vertical inherits:
//! a folder walker that hashes each file, picks a sub-extractor by
//! magic-bytes / extension, and emits an
//! [`ExtractedDoc`](super::ExtractedDoc) whose `content` is the best
//! available description of the bytes plus metadata pointing at the
//! [`crate::asset_store`] entry (raw + optional typed parsed cache).
//!
//! The dispatcher is **extractor-kind, not pipeline-kind**: it runs at
//! the same layer as `Plaintext` / `Csv` / `Html`, not as an enrichment
//! phase. Every asset gets some atlas-visible representation
//! (prose-shaped, opaque-fallback at worst — `binary, 2.1MB,
//! magic=outlook-pst`); every asset worth structurally preserving gets
//! a typed parsed cache linked from its ledger entry.
//!
//! Sub-extractors are registered on the [`crate::engine::CorpusEngine`]
//! (parallel to the per-file [`CustomExtractorFn`](crate::engine::CustomExtractorFn)
//! registry) so heavy deps (`pdf-extract`, future LibreOffice) live in
//! `sovereign-tools` and the engine itself stays lean. Defaults shipped
//! in-tree: `xlsx` (calamine + parquet parsed-form), `docx` (zip+xml),
//! `plaintext` (UTF-8 inference), `opaque` (always-true fallback).
//!
//! The dispatcher writes a sidecar `<corpus_index>/atlas/asset_atoms.jsonl`
//! of pre-formed [`Asset`](crate::enrichment::atlas::atoms::Asset)
//! atoms during extraction. The next atlas write unions them into
//! `atoms.json`. The `Attaches` edge to the carrier doc is written by
//! the caller that supplied the bytes (the email extractor in Phase 2;
//! for folder-walk ingest the dispatcher itself writes a self-edge —
//! `Document(filename) → Attaches → Asset(sha256)` — once the
//! description atom lands during enrichment).

use std::collections::VecDeque;
use std::fs;
use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

use super::{slug, ExtractedDoc, Extractor};
use crate::asset_store::{AssetStore, AssetStoreHandle};
use crate::enrichment::atlas::atoms::{Asset, AtomEnvelope, AtomId};
use crate::enrichment::pipeline::atlas::EnrichmentDepth;
use crate::error::{Error, Result};

// ── AssetSubExtractor + registry ─────────────────────────────

/// Outcome of an [`AssetSubExtractor`] run. The dispatcher consumes
/// this and builds the final [`ExtractedDoc`] + asset atom.
#[derive(Debug, Clone)]
pub struct AssetExtraction {
    /// Prose-shaped description that becomes
    /// [`ExtractedDoc::content`]. The atlas pipeline picks this up
    /// the same way it picks up any other plaintext document.
    pub description: String,
    /// `asset_kind` tag the sub-extractor self-identifies as
    /// (`"xlsx"`, `"docx"`, `"pdf"`, …). Mirrored onto the Asset
    /// atom + carried in the ExtractedDoc metadata.
    pub asset_kind: String,
    /// Tier of structural fidelity the description carries. See
    /// [`ExtractionTier`].
    pub tier: ExtractionTier,
    /// MIME type the sub-extractor detected. `None` to fall through
    /// to the dispatcher's magic-bytes guess.
    pub mime: Option<String>,
    /// Optional path the sub-extractor wrote a typed parsed cache to
    /// via [`AssetStore::put_parsed`]. Recorded into the ledger by
    /// the dispatcher so Phase 4's column-aware extractor can read
    /// directly without re-resolving the asset store.
    pub parsed_form: Option<PathBuf>,
}

/// Fidelity tier of an extracted description. The dispatcher does not
/// use this beyond recording it in metadata — downstream consumers
/// (atlas inspection, Phase 5 measurement) read it to qualify how the
/// description was produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractionTier {
    /// Prose-shaped extraction the sub-extractor recovered verbatim
    /// (text-PDF, DOCX body, plaintext) — the description IS the
    /// document's body text.
    Prose,
    /// Structural description the sub-extractor synthesised because
    /// the bytes are not prose (XLSX: "14 sheets, sheet Q3 has 847
    /// rows × 12 cols"; future calendar: "iCal feed: 47 events,
    /// attendees include …"). The parsed form, when present, is the
    /// "real" data behind this description.
    Structural,
    /// Opaque fallback. The dispatcher could not identify the bytes;
    /// the description is `binary, NNN bytes, magic=…`. Asset is
    /// still atlas-visible and reconciliation-traversable.
    Opaque,
}

impl ExtractionTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExtractionTier::Prose => "prose",
            ExtractionTier::Structural => "structural",
            ExtractionTier::Opaque => "opaque",
        }
    }
}

/// Pluggable per-asset extractor. Inspects the first ~512 bytes +
/// the file's extension and decides whether it owns the asset. The
/// dispatcher walks the registry in registration order; first
/// `detect()` win takes the asset.
///
/// **The opaque fallback always wins last** so every asset gets some
/// description.
pub trait AssetSubExtractor: Send + Sync {
    /// True when this sub-extractor wants to handle the asset. Cheap;
    /// the dispatcher reads `head_bytes` once per file and shares the
    /// slice across all detection calls.
    fn detect(&self, path: &Path, head_bytes: &[u8]) -> bool;

    /// Produce the description (and optionally write a typed parsed
    /// form via `store.put_parsed`). Receives the asset's full raw
    /// bytes pre-loaded so the sub-extractor does not have to
    /// re-read from disk.
    fn extract(
        &self,
        path: &Path,
        bytes: &[u8],
        sha256: &str,
        store: &dyn AssetStore,
    ) -> Result<AssetExtraction>;

    /// Self-identifying name (used in registration error messages
    /// and in the dispatcher's tracing events).
    fn name(&self) -> &'static str;
}

/// Registry of sub-extractors. Cheap-clonable (`Arc`) so the engine
/// can hand a snapshot to each dispatcher built per ingest.
#[derive(Clone, Default)]
pub struct AssetSubExtractorRegistry {
    inner: Arc<RwLock<Vec<Arc<dyn AssetSubExtractor>>>>,
}

impl AssetSubExtractorRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a sub-extractor. Ordering is meaningful: the dispatcher
    /// asks each in turn. Register more-specific detectors before
    /// less-specific ones.
    pub fn register(&self, ext: Arc<dyn AssetSubExtractor>) {
        let mut guard = self
            .inner
            .write()
            .expect("AssetSubExtractorRegistry poisoned");
        guard.push(ext);
    }

    pub fn snapshot(&self) -> Vec<Arc<dyn AssetSubExtractor>> {
        self.inner
            .read()
            .expect("AssetSubExtractorRegistry poisoned")
            .clone()
    }

    /// Default registry seeded with the corpus-engine in-tree
    /// sub-extractors (`xlsx`, `docx`, `plaintext`, `opaque`). Callers
    /// that ship additional kinds (sovereign-tools' `pdf` for example)
    /// build a registry, `register` their own, and pass it through to
    /// the engine via [`crate::engine::CorpusEngine::set_asset_sub_extractors`].
    pub fn defaults() -> Self {
        let r = Self::new();
        r.register(Arc::new(super::xlsx::XlsxSubExtractor));
        r.register(Arc::new(super::docx::DocxSubExtractor));
        r.register(Arc::new(PlaintextSubExtractor));
        r.register(Arc::new(OpaqueFallback));
        r
    }
}

// ── Plaintext sub-extractor (UTF-8 / ASCII inference) ─────────

/// Catches files whose first KiB is valid UTF-8 + has no NUL byte.
/// Returns the full text as the description (prose tier). Comes
/// before `OpaqueFallback` in the default registry so we don't
/// describe a perfectly readable `.csv` or `.eml` as opaque bytes.
pub struct PlaintextSubExtractor;

impl AssetSubExtractor for PlaintextSubExtractor {
    fn detect(&self, _path: &Path, head_bytes: &[u8]) -> bool {
        if head_bytes.is_empty() {
            return false;
        }
        if head_bytes.contains(&0) {
            return false;
        }
        std::str::from_utf8(head_bytes).is_ok()
    }

    fn extract(
        &self,
        _path: &Path,
        bytes: &[u8],
        _sha256: &str,
        _store: &dyn AssetStore,
    ) -> Result<AssetExtraction> {
        let text = std::str::from_utf8(bytes)
            .map_err(|e| Error::Extraction(format!("plaintext sub-extractor: {e}")))?
            .to_string();
        Ok(AssetExtraction {
            description: text,
            asset_kind: "plaintext".into(),
            tier: ExtractionTier::Prose,
            mime: Some("text/plain".into()),
            parsed_form: None,
        })
    }

    fn name(&self) -> &'static str {
        "plaintext"
    }
}

// ── Opaque fallback ───────────────────────────────────────────

/// Always matches; produces a one-line structural description naming
/// size + the first 16 bytes as a hex "magic" preview. Last in the
/// default registry so it only runs when nothing more specific
/// matched.
pub struct OpaqueFallback;

impl AssetSubExtractor for OpaqueFallback {
    fn detect(&self, _path: &Path, _head_bytes: &[u8]) -> bool {
        true
    }

    fn extract(
        &self,
        path: &Path,
        bytes: &[u8],
        _sha256: &str,
        _store: &dyn AssetStore,
    ) -> Result<AssetExtraction> {
        let magic = bytes
            .iter()
            .take(16)
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
        let description = format!(
            "binary, {} bytes, ext={ext}, magic={magic}",
            bytes.len()
        );
        Ok(AssetExtraction {
            description,
            asset_kind: "opaque".into(),
            tier: ExtractionTier::Opaque,
            mime: Some("application/octet-stream".into()),
            parsed_form: None,
        })
    }

    fn name(&self) -> &'static str {
        "opaque"
    }
}

// ── Dispatcher (impl Extractor) ───────────────────────────────

/// Folder walker that hashes every file, dispatches to a registered
/// sub-extractor, and emits one [`ExtractedDoc`] per asset.
///
/// Constructed by [`crate::engine::CorpusEngine`] for
/// [`crate::recipe::ExtractorConfig::DescribedAsset`] recipes. The
/// `AssetStore` + registry handles are populated from the engine's
/// runtime state.
pub struct DescribedAssetExtractor {
    pub store: AssetStoreHandle,
    pub registry: AssetSubExtractorRegistry,
    /// Where Asset atoms get written as a sidecar JSONL. Read by
    /// `sovereign atlas inspect` (Phase 1 demoable) and merged into
    /// `atoms.json` during the next atlas write.
    pub asset_atoms_sidecar: PathBuf,
    /// Per-file size ceiling. Files larger than this skip the
    /// sub-extractor pass and go straight to the opaque fallback —
    /// keeps the dispatcher from loading multi-GiB blobs into RAM
    /// when a recipe author drops a video on it.
    pub max_bytes_per_asset: u64,
}

impl DescribedAssetExtractor {
    /// 64 MiB — generous for spreadsheets / docx / typical
    /// attachments; small enough that the worst-case in-memory
    /// footprint per concurrent extractor is bounded.
    pub const DEFAULT_MAX_BYTES_PER_ASSET: u64 = 64 * 1024 * 1024;
}

impl Extractor for DescribedAssetExtractor {
    fn extract(
        &self,
        source_path: &Path,
    ) -> Result<Box<dyn Iterator<Item = Result<ExtractedDoc>> + Send>> {
        let mut files = Vec::new();
        if source_path.is_file() {
            files.push(source_path.to_path_buf());
        } else {
            walk(source_path, &mut files)?;
        }
        files.sort();
        Ok(Box::new(DescribedAssetIterator {
            files: files.into(),
            store: Arc::clone(&self.store),
            sub_extractors: self.registry.snapshot(),
            asset_atoms_sidecar: self.asset_atoms_sidecar.clone(),
            max_bytes: self.max_bytes_per_asset,
        }))
    }
}

struct DescribedAssetIterator {
    files: VecDeque<PathBuf>,
    store: AssetStoreHandle,
    sub_extractors: Vec<Arc<dyn AssetSubExtractor>>,
    asset_atoms_sidecar: PathBuf,
    max_bytes: u64,
}

impl Iterator for DescribedAssetIterator {
    type Item = Result<ExtractedDoc>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let path = self.files.pop_front()?;
            let outcome = self.dispatch_one(&path);
            match outcome {
                Ok(Some(doc)) => return Some(Ok(doc)),
                // Skipped (zero bytes, hidden, etc.) — keep looking.
                Ok(None) => continue,
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

impl DescribedAssetIterator {
    fn dispatch_one(&self, path: &Path) -> Result<Option<ExtractedDoc>> {
        if !path.is_file() {
            return Ok(None);
        }
        // Hidden files: skip. Mirrors the convention of the other
        // file walkers (custom_file, code).
        if path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|n| n.starts_with('.'))
        {
            return Ok(None);
        }
        let bytes = fs::read(path).map_err(|e| {
            Error::Extraction(format!(
                "described_asset: read {} failed: {e}",
                path.display()
            ))
        })?;
        if bytes.is_empty() {
            return Ok(None);
        }

        let source_doc_id = source_doc_id_for(path);
        let receipt = self.store.put_raw(
            &bytes,
            path.file_name().and_then(|s| s.to_str()),
            None,
            &source_doc_id,
        )?;

        let extraction = if bytes.len() as u64 > self.max_bytes {
            tracing::debug!(
                path = %path.display(),
                size = bytes.len(),
                max = self.max_bytes,
                "described_asset: asset exceeds max_bytes — falling back to opaque",
            );
            OpaqueFallback.extract(path, &bytes, &receipt.sha256, self.store.as_ref())?
        } else {
            let head = &bytes[..512.min(bytes.len())];
            let mut picked: Option<&Arc<dyn AssetSubExtractor>> = None;
            for sub in &self.sub_extractors {
                if sub.detect(path, head) {
                    picked = Some(sub);
                    break;
                }
            }
            let sub = picked.ok_or_else(|| {
                Error::Extraction(format!(
                    "described_asset: no sub-extractor matched {} — register OpaqueFallback last",
                    path.display()
                ))
            })?;
            tracing::trace!(
                path = %path.display(),
                sub = sub.name(),
                "described_asset: dispatched",
            );
            sub.extract(path, &bytes, &receipt.sha256, self.store.as_ref())?
        };

        if let Some(parsed_path) = extraction.parsed_form.as_deref() {
            self.store.record_parsed_form(&receipt.sha256, parsed_path)?;
        }

        // Build and persist the Asset atom (sidecar JSONL — picked up
        // by the next atlas write).
        let atom = Asset {
            id: Asset::make_id(&receipt.sha256),
            sha256: receipt.sha256.clone(),
            mime: extraction
                .mime
                .clone()
                .unwrap_or_else(|| "application/octet-stream".into()),
            original_filename: path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string(),
            size: receipt.size,
            asset_kind: extraction.asset_kind.clone(),
            described_by: None,
            parsed_form: extraction.parsed_form.clone(),
            first_seen_source_doc_id: source_doc_id.clone(),
            enrichment_depth: EnrichmentDepth::Extracted,
        };
        append_asset_atom(&self.asset_atoms_sidecar, &atom)?;

        // Build the ExtractedDoc — content = description text, metadata
        // carries the asset descriptor.
        let title = path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string());
        let metadata = serde_json::json!({
            "asset_sha256": receipt.sha256,
            "asset_kind": extraction.asset_kind,
            "extraction_tier": extraction.tier.as_str(),
            "parsed_form": extraction.parsed_form.as_ref().map(|p| p.to_string_lossy().to_string()),
            "original_filename": title,
            "size": receipt.size,
        });
        Ok(Some(ExtractedDoc {
            title,
            content: extraction.description,
            url: None,
            source_id: source_doc_id,
            metadata: Some(metadata),
            source_file: path
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string()),
            embed_text: None,
        }))
    }
}

fn source_doc_id_for(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    slug(stem)
}

fn append_asset_atom(sidecar: &Path, atom: &Asset) -> Result<()> {
    if let Some(parent) = sidecar.parent() {
        fs::create_dir_all(parent).map_err(Error::Io)?;
    }
    let envelope = AtomEnvelope::Asset(atom.clone());
    let line = serde_json::to_string(&envelope).map_err(|e| {
        Error::Extraction(format!("described_asset: serialise atom: {e}"))
    })?;
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(sidecar)
        .map_err(Error::Io)?;
    let mut w = BufWriter::new(&mut f);
    w.write_all(line.as_bytes()).map_err(Error::Io)?;
    w.write_all(b"\n").map_err(Error::Io)?;
    w.flush().map_err(Error::Io)?;
    Ok(())
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = fs::read_dir(dir)
        .map_err(|e| Error::Extraction(format!("described_asset: read_dir {}: {e}", dir.display())))?;
    for entry in entries {
        let entry = entry
            .map_err(|e| Error::Extraction(format!("described_asset: dir entry: {e}")))?;
        let path = entry.path();
        // Skip macOS resource forks + Windows thumb caches without
        // commentary; they're never useful as ingest inputs.
        if path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|n| n == ".DS_Store" || n.starts_with("._") || n == "Thumbs.db")
        {
            continue;
        }
        if path.is_dir() {
            walk(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

// Allow consumers (Phase 2 email extractor) to manufacture an Asset
// atom directly when they already have the bytes in hand. Keeps the
// atom-shape sealed inside this module.
pub fn build_asset_atom(
    sha256: &str,
    mime: &str,
    asset_kind: &str,
    original_filename: &str,
    size: u64,
    parsed_form: Option<PathBuf>,
    described_by: Option<AtomId>,
    source_doc_id: &str,
) -> Asset {
    Asset {
        id: Asset::make_id(sha256),
        sha256: sha256.to_string(),
        mime: mime.to_string(),
        original_filename: original_filename.to_string(),
        size,
        asset_kind: asset_kind.to_string(),
        described_by,
        parsed_form,
        first_seen_source_doc_id: source_doc_id.to_string(),
        enrichment_depth: EnrichmentDepth::Extracted,
    }
}

/// Append a pre-built Asset atom + an `Attaches` edge to the
/// sidecar. Used by callers that wrote bytes through the store
/// outside the folder-walk path (the email extractor in Phase 2).
pub fn append_asset_atom_with_edge(
    asset_atoms_sidecar: &Path,
    atom: &Asset,
) -> Result<()> {
    append_asset_atom(asset_atoms_sidecar, atom)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset_store::FilesystemAssetStore;

    fn tmp_dispatcher() -> (tempfile::TempDir, DescribedAssetExtractor) {
        let dir = tempfile::tempdir().unwrap();
        let assets = dir.path().join("assets");
        let store: AssetStoreHandle = Arc::new(FilesystemAssetStore::new(&assets).unwrap());
        let ext = DescribedAssetExtractor {
            store,
            registry: AssetSubExtractorRegistry::defaults(),
            asset_atoms_sidecar: dir.path().join("atlas/asset_atoms.jsonl"),
            max_bytes_per_asset: DescribedAssetExtractor::DEFAULT_MAX_BYTES_PER_ASSET,
        };
        (dir, ext)
    }

    #[test]
    fn plaintext_extracts_prose() {
        let (dir, ext) = tmp_dispatcher();
        let docs_dir = dir.path().join("docs");
        std::fs::create_dir_all(&docs_dir).unwrap();
        std::fs::write(docs_dir.join("hello.txt"), b"hello world").unwrap();

        let docs: Vec<_> = ext
            .extract(&docs_dir)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].content, "hello world");
        let meta = docs[0].metadata.as_ref().unwrap();
        assert_eq!(meta["asset_kind"], "plaintext");
        assert_eq!(meta["extraction_tier"], "prose");
    }

    #[test]
    fn opaque_fallback_describes_binary() {
        let (dir, ext) = tmp_dispatcher();
        let docs_dir = dir.path().join("docs");
        std::fs::create_dir_all(&docs_dir).unwrap();
        std::fs::write(docs_dir.join("mystery.bin"), &[0u8, 1, 2, 3, 4, 0xff, 0xfe])
            .unwrap();

        let docs: Vec<_> = ext
            .extract(&docs_dir)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(docs.len(), 1);
        assert!(docs[0].content.starts_with("binary, 7 bytes"));
        let meta = docs[0].metadata.as_ref().unwrap();
        assert_eq!(meta["asset_kind"], "opaque");
    }

    #[test]
    fn asset_atom_lands_in_sidecar() {
        let (dir, ext) = tmp_dispatcher();
        let docs_dir = dir.path().join("docs");
        std::fs::create_dir_all(&docs_dir).unwrap();
        std::fs::write(docs_dir.join("a.txt"), b"alpha").unwrap();

        let _: Vec<_> = ext
            .extract(&docs_dir)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let sidecar = std::fs::read_to_string(&ext.asset_atoms_sidecar).unwrap();
        // One Asset envelope per ingested file.
        assert_eq!(sidecar.lines().count(), 1);
        let env: AtomEnvelope = serde_json::from_str(sidecar.lines().next().unwrap()).unwrap();
        match env {
            AtomEnvelope::Asset(a) => {
                assert_eq!(a.asset_kind, "plaintext");
                assert_eq!(a.original_filename, "a.txt");
            }
            other => panic!("expected Asset envelope, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_files_dedup_in_store_but_emit_two_atoms() {
        // Two files with identical contents in the same folder. The
        // asset store de-dups the bytes (one ledger entry); the
        // dispatcher emits one Asset atom per *observation* — same
        // sha256, same content-hash atom id (so when atoms.json
        // merges them, the duplicate collapses naturally).
        let (dir, ext) = tmp_dispatcher();
        let docs_dir = dir.path().join("docs");
        std::fs::create_dir_all(&docs_dir).unwrap();
        std::fs::write(docs_dir.join("a.txt"), b"same bytes").unwrap();
        std::fs::write(docs_dir.join("b.txt"), b"same bytes").unwrap();

        let docs: Vec<_> = ext
            .extract(&docs_dir)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(docs.len(), 2);
        let entries = ext.store.entries().unwrap();
        assert_eq!(entries.len(), 1, "asset store de-dups");
    }

    #[test]
    fn hidden_files_skipped() {
        let (dir, ext) = tmp_dispatcher();
        let docs_dir = dir.path().join("docs");
        std::fs::create_dir_all(&docs_dir).unwrap();
        std::fs::write(docs_dir.join(".hidden"), b"secret").unwrap();
        std::fs::write(docs_dir.join("visible.txt"), b"hello").unwrap();

        let docs: Vec<_> = ext
            .extract(&docs_dir)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].content, "hello");
    }

    #[test]
    fn mac_metadata_files_skipped_silently() {
        let (dir, ext) = tmp_dispatcher();
        let docs_dir = dir.path().join("docs");
        std::fs::create_dir_all(&docs_dir).unwrap();
        std::fs::write(docs_dir.join(".DS_Store"), b"junk").unwrap();
        std::fs::write(docs_dir.join("._fork"), b"junk").unwrap();
        std::fs::write(docs_dir.join("real.txt"), b"data").unwrap();
        let docs: Vec<_> = ext
            .extract(&docs_dir)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(docs.len(), 1);
    }
}
