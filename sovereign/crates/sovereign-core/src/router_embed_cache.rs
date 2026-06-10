// SPDX-License-Identifier: AGPL-3.0-or-later
//! Disk cache for the router classifier exemplar embeddings.
//!
//! The four boot classifiers (embed router, scope, effort,
//! current-info) embed ~310 static exemplar strings at every process
//! start — measured at ~5.7s of the desktop splash screen
//! (2026-06-10, Qwen3-Embedding-0.6B at ~19ms/call, sequential).
//! Those embeddings are a pure function of (exemplar text, embed
//! model, embed method), so they're cached here keyed by text hash
//! and rehydrated in microseconds on every boot after the first.
//!
//! ## Validity — the sentinel probe
//!
//! The cache file carries no model identity (providers don't expose a
//! stable embed-model id across mesh wrappers and BYOM swaps).
//! Instead, validity is established the way the prebuilt-snapshot
//! installer checks embedding-space compatibility: re-embed one
//! sentinel string at open time and cosine-compare against the stored
//! sentinel embedding. Same model → cosine ≈ 1.0; swapped model →
//! cosine far below [`PROBE_MIN_COSINE`] (or a hard dims mismatch)
//! and the whole cache is discarded. Cost: one embed call per boot.
//!
//! ## Method asymmetry
//!
//! `embed_query` applies an instruction prefix on asymmetric models;
//! `embed` does not — and the effort classifier *deliberately* uses
//! the unprefixed form (see `effort_classifier.rs`). The two spaces
//! are not interchangeable, so keys carry a `q:`/`d:` discriminator
//! and the sentinel is probed through `embed_query` only (one shared
//! model serves both methods; a model swap invalidates both spaces).
//!
//! Env override: `SOVEREIGN_ROUTER_EMBED_CACHE=<path>` relocates the
//! file; `SOVEREIGN_ROUTER_EMBED_CACHE=0` disables caching entirely
//! (every call passes through to the provider, nothing is written).

use std::collections::HashMap;
use std::path::PathBuf;
use sha2::{Digest, Sha256};

use crate::traits::InferenceProvider;
use crate::Result;

/// Bump when the file layout changes incompatibly.
const SCHEMA_VERSION: u32 = 1;

/// Fixed probe string. Never routed, never user-visible — its only
/// job is to detect that the embedding space changed under the cache.
const PROBE_TEXT: &str = "sovereign router embed cache — sentinel probe v1";

/// Same-model re-embeds of the sentinel land at cosine ≈ 1.0 (modulo
/// nondeterministic GPU reduction order); a different model or
/// quantisation lands far lower. 0.98 splits those populations with
/// wide margin on both sides (the snapshot-installer probe uses 0.92
/// across *re-quantised* variants of the same model; this cache only
/// needs to accept the *identical* model).
const PROBE_MIN_COSINE: f32 = 0.98;

#[derive(serde::Serialize, serde::Deserialize)]
struct CacheFile {
    schema_version: u32,
    /// `embed_query(PROBE_TEXT)` at write time.
    probe: Vec<f32>,
    /// `"q:<sha256>"` / `"d:<sha256>"` → embedding.
    entries: HashMap<String, Vec<f32>>,
}

/// Boot-time embedding cache. Open once in `build_llm_router`, thread
/// through the classifier constructors, [`flush`](Self::flush) once
/// after assembly. Not intended for runtime query embedding — entries
/// accumulate only between open and flush.
pub struct BootEmbedCache {
    /// `None` → disabled (env opt-out or unresolvable path): every
    /// lookup misses and nothing is written.
    path: Option<PathBuf>,
    entries: HashMap<String, Vec<f32>>,
    probe: Vec<f32>,
    hits: usize,
    misses: usize,
    dirty: bool,
}

fn cache_path() -> Option<PathBuf> {
    match std::env::var("SOVEREIGN_ROUTER_EMBED_CACHE") {
        Ok(v) if v == "0" => None,
        Ok(v) if !v.trim().is_empty() => Some(PathBuf::from(v)),
        _ => dirs::home_dir().map(|h| h.join(".sovereign").join("router-embed-cache.json")),
    }
}

fn key(method: &str, text: &str) -> String {
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    format!("{method}:{:x}", h.finalize())
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

impl BootEmbedCache {
    /// Load the cache and validate it against the live provider via
    /// the sentinel probe. Always returns a usable cache — probe or
    /// read failures degrade to an empty (or disabled) cache and the
    /// classifiers embed exactly as they did before this existed.
    pub async fn open(inference: &dyn InferenceProvider) -> Self {
        let path = cache_path();
        let Some(ref p) = path else {
            tracing::info!(target: "router.bootstrap", "exemplar embed cache disabled via env");
            return Self::empty(None, Vec::new());
        };

        // The probe is also the freshly-computed value stored on
        // flush, so a failed probe embed disables writing too (we
        // couldn't stamp a valid sentinel).
        let probe = match inference.embed_query(PROBE_TEXT).await {
            Ok(e) if !e.is_empty() => e,
            Ok(_) | Err(_) => {
                tracing::warn!(
                    target: "router.bootstrap",
                    "exemplar embed cache: sentinel probe embed failed; caching disabled this boot"
                );
                return Self::empty(None, Vec::new());
            }
        };

        let parsed: Option<CacheFile> = std::fs::read(p)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        match parsed {
            Some(f) if f.schema_version == SCHEMA_VERSION => {
                let sim = cosine(&probe, &f.probe);
                if sim >= PROBE_MIN_COSINE {
                    tracing::info!(
                        target: "router.bootstrap",
                        entries = f.entries.len(),
                        probe_cosine = sim,
                        "exemplar embed cache validated"
                    );
                    Self {
                        path,
                        entries: f.entries,
                        probe,
                        hits: 0,
                        misses: 0,
                        dirty: false,
                    }
                } else {
                    tracing::info!(
                        target: "router.bootstrap",
                        probe_cosine = sim,
                        "exemplar embed cache: embed model changed; discarding stale cache"
                    );
                    Self::empty(path, probe)
                }
            }
            Some(_) => {
                tracing::info!(
                    target: "router.bootstrap",
                    "exemplar embed cache: schema changed; discarding"
                );
                Self::empty(path, probe)
            }
            None => Self::empty(path, probe),
        }
    }

    fn empty(path: Option<PathBuf>, probe: Vec<f32>) -> Self {
        Self {
            path,
            entries: HashMap::new(),
            probe,
            hits: 0,
            misses: 0,
            dirty: false,
        }
    }

    /// Cached `embed_query` (instruction-prefixed space).
    pub async fn embed_query_cached(
        &mut self,
        inference: &dyn InferenceProvider,
        text: &str,
    ) -> Result<Vec<f32>> {
        self.cached("q", text, || inference.embed_query(text)).await
    }

    /// Cached `embed` (unprefixed document space — the effort
    /// classifier's deliberate choice).
    pub async fn embed_cached(
        &mut self,
        inference: &dyn InferenceProvider,
        text: &str,
    ) -> Result<Vec<f32>> {
        self.cached("d", text, || inference.embed(text)).await
    }

    async fn cached<'a, F, Fut>(&mut self, method: &str, text: &str, embed: F) -> Result<Vec<f32>>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<Vec<f32>>> + 'a,
    {
        let k = key(method, text);
        if let Some(e) = self.entries.get(&k) {
            self.hits += 1;
            return Ok(e.clone());
        }
        let e = embed().await?;
        self.misses += 1;
        if self.path.is_some() {
            self.entries.insert(k, e.clone());
            self.dirty = true;
        }
        Ok(e)
    }

    /// Persist (atomic temp + rename) when anything was added. Logs
    /// the hit/miss split either way — the glassbox view of whether
    /// this boot paid for embeds or read them back.
    pub fn flush(&mut self) {
        tracing::info!(
            target: "router.bootstrap",
            hits = self.hits,
            misses = self.misses,
            "exemplar embed cache: boot embed accounting"
        );
        if !self.dirty {
            return;
        }
        let Some(ref p) = self.path else { return };
        let file = CacheFile {
            schema_version: SCHEMA_VERSION,
            probe: self.probe.clone(),
            entries: self.entries.clone(),
        };
        let write = || -> std::io::Result<()> {
            if let Some(dir) = p.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = p.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_vec(&file).map_err(std::io::Error::other)?)?;
            std::fs::rename(&tmp, p)
        };
        match write() {
            Ok(()) => {
                tracing::info!(
                    target: "router.bootstrap",
                    path = %p.display(),
                    entries = file.entries.len(),
                    "exemplar embed cache written"
                );
                self.dirty = false;
            }
            Err(e) => tracing::warn!(
                target: "router.bootstrap",
                error = %e,
                "exemplar embed cache write failed (boot unaffected)"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_separate_query_and_document_spaces() {
        assert_ne!(key("q", "same text"), key("d", "same text"));
        assert_eq!(key("q", "same text"), key("q", "same text"));
    }

    #[test]
    fn cosine_identical_is_one_and_orthogonal_is_zero() {
        let a = vec![1.0, 0.0];
        assert!((cosine(&a, &a) - 1.0).abs() < 1e-6);
        assert!(cosine(&a, &[0.0, 1.0]).abs() < 1e-6);
        assert_eq!(cosine(&a, &[1.0]), 0.0, "dims mismatch must reject");
    }

    #[test]
    fn cache_file_roundtrips() {
        let mut entries = HashMap::new();
        entries.insert(key("q", "hello"), vec![0.5f32, 0.25]);
        let f = CacheFile {
            schema_version: SCHEMA_VERSION,
            probe: vec![1.0, 0.0],
            entries,
        };
        let bytes = serde_json::to_vec(&f).unwrap();
        let back: CacheFile = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.schema_version, SCHEMA_VERSION);
        assert_eq!(back.entries.len(), 1);
        assert_eq!(back.probe, vec![1.0, 0.0]);
    }
}
