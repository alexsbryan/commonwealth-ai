// SPDX-License-Identifier: AGPL-3.0-or-later
//! Frozen-sample capture and read-back (I3).
//!
//! `capture` runs the real acquirer exactly once, freezes the raw source
//! bytes content-addressed under `<harness_root>/assets`, and records a
//! `capture.json` sidecar describing the sample. `FrozenSample` reads that
//! back and reconstructs the source tree for re-extraction — the network
//! never runs again for the same sample.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::asset_store::{AssetStore, FilesystemAssetStore};
use crate::engine::CorpusEngine;
use crate::error::{Error, Result};
use crate::recipe::{AcquirerConfig, Recipe};

/// One frozen raw source file, content-addressed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapturedFile {
    pub sha256: String,
    /// Path relative to the acquired source root — used to reconstruct the
    /// tree the extractor expects.
    pub rel_path: String,
    pub bytes: u64,
    /// Lightweight content-type hint (the file extension); glassbox only.
    pub content_type: Option<String>,
}

/// Per-doc metadata from the one capture-time extraction pass. Feeds the
/// Acquire-integrity rung and the count check; the docs themselves are NOT
/// stored — the harness re-extracts from the frozen raw source each iteration
/// so an edit to the extract config actually re-runs (I2).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapturedDoc {
    pub doc_id: String,
    pub title: Option<String>,
    pub url: Option<String>,
    pub content_bytes: usize,
    pub empty: bool,
}

/// The `capture.json` sidecar: the durable description of a frozen sample.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureManifest {
    pub recipe_id: String,
    pub sample_id: String,
    pub recipe_hash: String,
    /// Unix seconds at capture. Lives ONLY here (the sidecar), never inside a
    /// `HarnessRun`, so verdicts stay byte-identical across re-runs — I1.
    pub captured_at: i64,
    /// Human display of the source, for glassbox.
    pub acquirer: String,
    /// Requested sample size N (the extraction pass took the first N docs).
    pub sample_size: usize,
    /// True when the acquired source was a directory (vs a single file).
    /// Controls how the frozen tree is handed back to the extractor.
    pub source_is_dir: bool,
    pub source_files: Vec<CapturedFile>,
    pub docs: Vec<CapturedDoc>,
}

/// Capture a frozen sample: run the REAL acquirer ONCE (I3), freeze the raw
/// source bytes content-addressed under `harness_root/assets`, then run one
/// bounded extraction pass to record per-doc metadata. The network never runs
/// again for this sample.
pub async fn capture(
    engine: &CorpusEngine,
    recipe: &Recipe,
    harness_root: &Path,
    sample_size: usize,
) -> Result<CaptureManifest> {
    std::fs::create_dir_all(harness_root)?;

    // I3: acquire once, via the production acquirer. For HuggingFace datasets,
    // bound the download to the first shard (the only behaviour the legacy
    // `acquire_for_test` had over `acquire_source`) by setting `file_indices`.
    let mut acq_recipe = recipe.clone();
    if let AcquirerConfig::HuggingFaceDataset { file_indices, .. } = &mut acq_recipe.acquire {
        if file_indices.is_none() {
            *file_indices = Some(vec![0]);
        }
    }
    let download_dir = std::env::temp_dir().join(format!("harness-capture-{}", recipe.corpus.id));
    let _ = std::fs::remove_dir_all(&download_dir);
    std::fs::create_dir_all(&download_dir)?;
    let source_path = engine.acquire_source(&acq_recipe, &download_dir, &None).await?;

    // Freeze the acquired source file(s), content-addressed.
    let source_is_dir = source_path.is_dir();
    let source_root = if source_is_dir {
        source_path.clone()
    } else {
        source_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    };
    let store = FilesystemAssetStore::new(harness_root.join("assets"))?;
    let mut source_files = Vec::new();
    for path in walk_files(&source_path)? {
        let bytes = std::fs::read(&path)?;
        let rel = path
            .strip_prefix(&source_root)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();
        let content_type = path.extension().map(|e| e.to_string_lossy().to_string());
        let receipt = store.put_raw(&bytes, Some(&rel), None, &recipe.corpus.id)?;
        source_files.push(CapturedFile {
            sha256: receipt.sha256,
            rel_path: rel,
            bytes: receipt.size,
            content_type,
        });
    }
    source_files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

    // One bounded extraction pass for per-doc metadata (I2: same extractor the
    // production ingest builds). Failures here are tolerated per-doc; the
    // Acquire/Extract verdicts (later increments) judge what this records.
    let extractor = engine.make_extractor(&recipe.extract, &recipe.corpus.id);
    let docs: Vec<CapturedDoc> = extractor
        .extract(&source_path)?
        .take(sample_size)
        .filter_map(std::result::Result::ok)
        .map(|d| CapturedDoc {
            doc_id: d
                .url
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| d.source_id.clone()),
            title: d.title.clone(),
            url: d.url.clone(),
            content_bytes: d.content.len(),
            empty: d.content.is_empty(),
        })
        .collect();

    let manifest = CaptureManifest {
        recipe_id: recipe.corpus.id.clone(),
        sample_id: super::sample_id(source_files.iter().map(|f| f.sha256.clone()).collect()),
        recipe_hash: super::recipe_hash(recipe),
        captured_at: chrono::Utc::now().timestamp(),
        acquirer: crate::testing::acquirer_source_url(recipe),
        sample_size,
        source_is_dir,
        source_files,
        docs,
    };

    let json = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| Error::Extraction(format!("capture.json serialise: {e}")))?;
    std::fs::write(harness_root.join("capture.json"), json)?;
    Ok(manifest)
}

/// Reader over a previously-captured frozen sample.
pub struct FrozenSample {
    pub manifest: CaptureManifest,
    root: PathBuf,
}

impl FrozenSample {
    /// Load the `capture.json` sidecar from `harness_root`. Returns `None` if
    /// no sample has been captured yet.
    pub fn load(harness_root: &Path) -> Result<Option<Self>> {
        let path = harness_root.join("capture.json");
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read(&path)?;
        let manifest: CaptureManifest = serde_json::from_slice(&raw)
            .map_err(|e| Error::Extraction(format!("capture.json parse: {e}")))?;
        Ok(Some(Self {
            manifest,
            root: harness_root.to_path_buf(),
        }))
    }

    /// Reconstruct the acquired source tree from the frozen blobs into `dest`,
    /// returning the path to hand the extractor. Reads only content-addressed
    /// bytes — the network never runs (I3).
    pub fn materialize(&self, dest: &Path) -> Result<PathBuf> {
        let store = FilesystemAssetStore::new(self.root.join("assets"))?;
        for f in &self.manifest.source_files {
            let bytes = std::fs::read(store.raw_path(&f.sha256))?;
            let out = dest.join(&f.rel_path);
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&out, &bytes)?;
        }
        if self.manifest.source_is_dir {
            Ok(dest.to_path_buf())
        } else if let Some(f) = self.manifest.source_files.first() {
            Ok(dest.join(&f.rel_path))
        } else {
            Ok(dest.to_path_buf())
        }
    }
}

/// Recursively collect every file under `root` (or just `root` if it is a
/// file), in sorted order so the frozen set is deterministic. Avoids the
/// optional `walkdir` dependency (it is feature-gated in this crate).
fn walk_files(root: &Path) -> Result<Vec<PathBuf>> {
    if root.is_file() {
        return Ok(vec![root.to_path_buf()]);
    }
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if file_type.is_file() {
                out.push(entry.path());
            }
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub_embed() -> crate::types::EmbedFn {
        std::sync::Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.0_f32; 8]) }))
    }

    fn fixture_recipe(jsonl_path: &Path) -> Recipe {
        let toml = format!(
            r#"
[corpus]
id = "harness-fixture"
name = "Harness Fixture"

[acquire]
type = "local_file"
path = "{}"

[extract]
type = "jsonl"
content_field = "text"
title_field = "title"

[chunk]
type = "paragraph"
max_chars = 2048

[index]
embedding_model = "stub"
"#,
            jsonl_path.display()
        );
        Recipe::from_toml(&toml).expect("fixture recipe parses")
    }

    #[tokio::test]
    async fn capture_freezes_sample_and_is_reproducible() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl = dir.path().join("fixture.jsonl");
        std::fs::write(
            &jsonl,
            "{\"title\":\"A\",\"text\":\"alpha body\"}\n\
             {\"title\":\"B\",\"text\":\"bravo body\"}\n\
             {\"title\":\"C\",\"text\":\"charlie body\"}\n",
        )
        .unwrap();

        let engine = CorpusEngine::new(
            dir.path().join("recipes"),
            dir.path().join("indexes"),
            stub_embed(),
        );
        let recipe = fixture_recipe(&jsonl);

        let harness_root = dir.path().join("harness");
        let m1 = capture(&engine, &recipe, &harness_root, 50).await.unwrap();
        assert_eq!(m1.docs.len(), 3, "all three fixture docs captured");
        assert!(!m1.sample_id.is_empty());
        assert!(harness_root.join("capture.json").is_file());
        assert_eq!(m1.source_files.len(), 1, "single local file frozen");
        assert!(!m1.source_is_dir);

        // I1: re-capturing identical bytes yields an identical sample_id.
        let harness_root2 = dir.path().join("harness2");
        let m2 = capture(&engine, &recipe, &harness_root2, 50).await.unwrap();
        assert_eq!(
            m1.sample_id, m2.sample_id,
            "sample_id reproducible over frozen content"
        );
        assert_eq!(m1.recipe_hash, m2.recipe_hash);

        // Read-back: the frozen blob reconstructs the original source bytes.
        let frozen = FrozenSample::load(&harness_root).unwrap().unwrap();
        let recon_dir = dir.path().join("recon");
        std::fs::create_dir_all(&recon_dir).unwrap();
        let src = frozen.materialize(&recon_dir).unwrap();
        let recon = std::fs::read_to_string(&src).unwrap();
        assert!(recon.contains("charlie body"));
    }
}
