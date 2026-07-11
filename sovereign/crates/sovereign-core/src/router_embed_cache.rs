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

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;

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

/// The committed/baked exemplar embedding cache (`sovereign/router/
/// router-embed-cache.json`), vendored into the binary so a shipped `.app` —
/// or any first launch with an empty `~/.sovereign` — validates it against the
/// live embed model (the sentinel probe) and HITS instead of re-embedding
/// ~310 strings sequentially (minutes on a CPU-only embed slot). Regenerated
/// offline by `sovereign router-cache rebuild` and freshness-gated in CI. The
/// committed placeholder (empty `entries`, `built_for: null`) degrades
/// gracefully: the probe rejects it and the classifiers embed exactly as they
/// did before this existed.
pub const BAKED_ROUTER_EMBED_CACHE: &str = include_str!("../../../router/router-embed-cache.json");

#[derive(serde::Serialize, serde::Deserialize)]
struct CacheFile {
    schema_version: u32,
    /// Identity of the embed model these vectors were built for. Set ONLY by
    /// the offline `router-cache rebuild` when it stamps the committed/baked
    /// artifact — the freshness gate compares it against the prescribed model.
    /// The runtime never stamps it (the cache is model-agnostic by design; the
    /// sentinel probe owns runtime validity), so a user-written on-disk cache
    /// carries `None`.
    #[serde(default)]
    built_for: Option<String>,
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
    /// Keys read or written this boot — the live exemplar set.
    /// Flush persists only these, pruning entries whose texts were
    /// removed from the exemplar files by a later release.
    touched: std::collections::HashSet<String>,
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

/// Pick the first cache source whose stored sentinel probe agrees with the
/// live `probe` (same embed space, cosine ≥ [`PROBE_MIN_COSINE`]) and whose
/// schema matches. Candidates are tried in priority order — a user-written
/// on-disk cache first (it can carry exemplars from a newer release than the
/// binary), then the binary-baked artifact.
///
/// The load-bearing property: a candidate that *parses* but fails validation
/// (wrong schema, or a probe from a different / degenerate embed model — e.g.
/// a zero-vector cache left behind by a past broken embed slot) no longer
/// *shadows* a healthy fallback. We advance to the next candidate instead of
/// giving up. Before this, a parseable-but-stale disk cache short-circuited the
/// baked fallback and forced a full re-embed of ~300 exemplars on the CPU-only
/// embed slot every boot (minutes) — the "router is slow to launch" bug.
///
/// Pure: no I/O, no inference. Returns the chosen `(entries, source)` or `None`
/// when nothing validates (caller then re-embeds and flushes a fresh cache).
fn select_cache_source(
    candidates: [(Option<CacheFile>, &'static str); 2],
    probe: &[f32],
) -> Option<(HashMap<String, Vec<f32>>, &'static str)> {
    for (parsed, source) in candidates {
        let Some(f) = parsed else { continue };
        if f.schema_version != SCHEMA_VERSION {
            tracing::info!(
                target: "router.bootstrap",
                source,
                "exemplar embed cache: schema changed; trying next source"
            );
            continue;
        }
        let sim = cosine(probe, &f.probe);
        if sim >= PROBE_MIN_COSINE {
            tracing::info!(
                target: "router.bootstrap",
                entries = f.entries.len(),
                probe_cosine = sim,
                source,
                "exemplar embed cache validated"
            );
            return Some((f.entries, source));
        }
        tracing::info!(
            target: "router.bootstrap",
            probe_cosine = sim,
            source,
            "exemplar embed cache: probe mismatch (embed model changed or degenerate cache); trying next source"
        );
    }
    None
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

        // Validate candidate caches against the live sentinel probe, in
        // priority order: the user-written on-disk cache first (it can carry
        // exemplars from a newer release than the binary), then the binary-baked
        // artifact so a shipped `.app` — or a cleared ~/.sovereign — still hits.
        // Crucially, an on-disk cache that PARSES but fails validation (stale
        // probe, wrong schema, or a degenerate zero-vector write from a past
        // broken embed slot) falls through to the baked fallback rather than
        // shadowing it — see `select_cache_source`.
        let disk = std::fs::read(p)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<CacheFile>(&bytes).ok());
        let baked = serde_json::from_str::<CacheFile>(BAKED_ROUTER_EMBED_CACHE).ok();
        match select_cache_source([(disk, "disk"), (baked, "baked")], &probe) {
            Some((entries, _source)) => Self {
                path,
                entries,
                touched: std::collections::HashSet::new(),
                probe,
                hits: 0,
                misses: 0,
                dirty: false,
            },
            None => {
                tracing::info!(
                    target: "router.bootstrap",
                    "exemplar embed cache: no source validated; re-embedding exemplars this boot"
                );
                Self::empty(path, probe)
            }
        }
    }

    /// True if the cache loaded a validated, non-empty embedding set — i.e.
    /// this boot will HIT instead of re-embedding. The desktop checks this to
    /// surface the `RebuildingRouterEmbeddings` phase honestly when it's false.
    pub fn is_populated(&self) -> bool {
        !self.entries.is_empty()
    }

    fn empty(path: Option<PathBuf>, probe: Vec<f32>) -> Self {
        Self {
            path,
            entries: HashMap::new(),
            touched: std::collections::HashSet::new(),
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
            self.touched.insert(k);
            return Ok(e.clone());
        }
        let e = embed().await?;
        self.misses += 1;
        if self.path.is_some() {
            self.entries.insert(k.clone(), e.clone());
            self.touched.insert(k);
            self.dirty = true;
        }
        Ok(e)
    }

    /// Persist (atomic temp + rename) when anything was added. Logs
    /// the hit/miss split either way — the glassbox view of whether
    /// this boot paid for embeds or read them back.
    ///
    /// Writes carry only entries this boot actually used, so texts
    /// removed from an exemplar file in a later release don't accrete
    /// in the cache forever — the next rewrite drops them.
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
        // Never persist a degenerate embedding space. An empty or all-zero
        // probe means the embed slot returned garbage this boot (a failed or
        // stub load) — writing it poisons every future boot: the sentinel probe
        // can never validate zero-vectors, so the file is dead weight that (via
        // the read-side fallback) is skipped, or (before that fix) shadowed the
        // healthy baked cache. Refuse the write. Correctness is unaffected — the
        // classifiers already embedded live this boot; only the next boot's
        // cache-hit is forgone, which is exactly right for a broken embed slot.
        if self.probe.iter().all(|&x| x == 0.0) {
            tracing::warn!(
                target: "router.bootstrap",
                probe_dims = self.probe.len(),
                "exemplar embed cache: refusing to persist a degenerate (all-zero) embedding space"
            );
            return;
        }
        let Some(ref p) = self.path else { return };
        let file = CacheFile {
            schema_version: SCHEMA_VERSION,
            // Runtime caches are model-agnostic (the probe owns validity), so
            // they carry no fingerprint. `router-cache rebuild` stamps the
            // committed/baked artifact's `built_for` separately after this.
            built_for: None,
            probe: self.probe.clone(),
            entries: self
                .entries
                .iter()
                .filter(|(k, _)| self.touched.contains(*k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        };
        let write = || -> std::io::Result<()> {
            if let Some(dir) = p.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = p.with_extension("json.tmp");
            std::fs::write(
                &tmp,
                serde_json::to_vec(&file).map_err(std::io::Error::other)?,
            )?;
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

/// Why a committed/baked router-embed cache is stale — surfaced by the
/// freshness gate (`router-cache check`, the CI test, and the bump hook) with
/// an actionable message. Pure: never runs inference — text-key coverage plus
/// a model-identity fingerprint compare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheStaleReason {
    /// The artifact didn't parse, or declares a different `schema_version`.
    Unreadable,
    /// The cache wasn't built for the currently-prescribed embed model — or it
    /// is the empty committed placeholder (`built_for: null`).
    ModelMismatch {
        committed: Option<String>,
        expected: String,
    },
    /// Exemplars exist with no entry in the cache — it predates an exemplar
    /// edit. `example` is one missing text (truncated) for context.
    MissingCoverage {
        missing: usize,
        total: usize,
        example: String,
    },
}

impl std::fmt::Display for CacheStaleReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable => write!(
                f,
                "router-embed-cache.json is unreadable or has an unexpected schema_version"
            ),
            Self::ModelMismatch {
                committed,
                expected,
            } => write!(
                f,
                "router-embed cache was built for {} but the prescribed embed model is {expected}",
                committed.as_deref().unwrap_or("<none/placeholder>")
            ),
            Self::MissingCoverage {
                missing,
                total,
                example,
            } => write!(
                f,
                "router-embed cache is missing {missing}/{total} exemplar embeddings \
                 (e.g. {example:?}) — exemplars changed since it was generated"
            ),
        }
    }
}

/// Pure, no-inference freshness check of a committed/baked router-embed cache.
/// Verifies (1) it was stamped for `expected_fingerprint` and (2) it carries an
/// entry for every `(method, text)` the four boot classifiers embed. The CI
/// gate test, the `router-cache check` verb, and the bump hook ALL call this
/// one function, so the gate can never disagree across surfaces.
///
/// `specs` are `(method, text)` pairs where `method` is `"q"` (instruction-
/// prefixed) or `"d"` (unprefixed) — build them with
/// [`crate::router_bootstrap::exemplar_specs`].
pub fn check_cache_fresh(
    cache_json: &str,
    specs: &[(&str, String)],
    expected_fingerprint: &str,
) -> std::result::Result<(), CacheStaleReason> {
    let parsed: CacheFile =
        serde_json::from_str(cache_json).map_err(|_| CacheStaleReason::Unreadable)?;
    if parsed.schema_version != SCHEMA_VERSION {
        return Err(CacheStaleReason::Unreadable);
    }
    match parsed.built_for.as_deref() {
        Some(fp) if fp == expected_fingerprint => {}
        other => {
            return Err(CacheStaleReason::ModelMismatch {
                committed: other.map(str::to_string),
                expected: expected_fingerprint.to_string(),
            });
        }
    }
    let mut missing = 0usize;
    let mut example = String::new();
    for (method, text) in specs {
        if !parsed.entries.contains_key(&key(method, text)) {
            if missing == 0 {
                example = text.chars().take(60).collect();
            }
            missing += 1;
        }
    }
    if missing > 0 {
        return Err(CacheStaleReason::MissingCoverage {
            missing,
            total: specs.len(),
            example,
        });
    }
    Ok(())
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
    fn flush_prunes_entries_not_touched_this_boot() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("cache.json");
        let live = key("q", "kept exemplar");
        let dead = key("q", "exemplar removed in a later release");
        let mut entries = HashMap::new();
        entries.insert(live.clone(), vec![1.0f32]);
        entries.insert(dead, vec![2.0f32]);
        let mut cache = BootEmbedCache {
            path: Some(path.clone()),
            entries,
            touched: std::iter::once(live.clone()).collect(),
            probe: vec![1.0],
            hits: 1,
            misses: 0,
            dirty: true,
        };
        cache.flush();
        let back: CacheFile = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(back.entries.len(), 1, "untouched entry must be pruned");
        assert!(back.entries.contains_key(&live));
    }

    #[test]
    fn cache_file_roundtrips() {
        let mut entries = HashMap::new();
        entries.insert(key("q", "hello"), vec![0.5f32, 0.25]);
        let f = CacheFile {
            schema_version: SCHEMA_VERSION,
            built_for: Some("Qwen3Embedding|https://example/repo".into()),
            probe: vec![1.0, 0.0],
            entries,
        };
        let bytes = serde_json::to_vec(&f).unwrap();
        let back: CacheFile = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.schema_version, SCHEMA_VERSION);
        assert_eq!(
            back.built_for.as_deref(),
            Some("Qwen3Embedding|https://example/repo")
        );
        assert_eq!(back.entries.len(), 1);
        assert_eq!(back.probe, vec![1.0, 0.0]);
    }

    /// A cache carrying `probe`, `n` entries, and `schema` — entries reuse the
    /// probe vector; only `probe`/`schema`/`len` matter for source selection.
    fn cache_with(probe: Vec<f32>, n: usize, schema: u32) -> CacheFile {
        let entries = (0..n).map(|i| (format!("q:{i}"), probe.clone())).collect();
        CacheFile {
            schema_version: schema,
            built_for: None,
            probe,
            entries,
        }
    }

    #[test]
    fn poisoned_disk_cache_falls_back_to_baked() {
        // Regression for the "router slow to launch" bug: a parseable on-disk
        // cache written by a past degenerate embed slot (8-dim zero-vectors)
        // must NOT shadow the healthy baked cache. Before the fix, this returned
        // an empty cache and forced a full re-embed of ~300 exemplars on the
        // CPU-only slot (minutes) every boot.
        let live = vec![1.0f32, 0.0, 0.0];
        let poisoned = cache_with(vec![0.0; 8], 277, SCHEMA_VERSION);
        let baked = cache_with(live.clone(), 303, SCHEMA_VERSION);
        let (entries, source) =
            select_cache_source([(Some(poisoned), "disk"), (Some(baked), "baked")], &live)
                .expect("baked fallback must validate when disk is poisoned");
        assert_eq!(source, "baked");
        assert_eq!(entries.len(), 303, "baked entries, not the poisoned disk set");
    }

    #[test]
    fn fresh_disk_cache_wins_over_baked() {
        // Priority order: a valid on-disk cache (possibly newer exemplars than
        // the binary) is preferred over the baked artifact.
        let live = vec![1.0f32, 0.0, 0.0];
        let disk = cache_with(live.clone(), 300, SCHEMA_VERSION);
        let baked = cache_with(live.clone(), 303, SCHEMA_VERSION);
        let (entries, source) =
            select_cache_source([(Some(disk), "disk"), (Some(baked), "baked")], &live).unwrap();
        assert_eq!(source, "disk");
        assert_eq!(entries.len(), 300);
    }

    #[test]
    fn both_sources_stale_returns_none() {
        // A genuinely swapped embed model (orthogonal probe): neither source
        // validates, so the caller re-embeds and flushes a fresh cache.
        let live = vec![1.0f32, 0.0, 0.0];
        let stale = vec![0.0f32, 1.0, 0.0];
        let disk = cache_with(stale.clone(), 277, SCHEMA_VERSION);
        let baked = cache_with(stale, 303, SCHEMA_VERSION);
        assert!(
            select_cache_source([(Some(disk), "disk"), (Some(baked), "baked")], &live).is_none()
        );
    }

    #[test]
    fn disk_schema_mismatch_falls_back_to_baked() {
        let live = vec![1.0f32, 0.0];
        let disk = cache_with(live.clone(), 10, SCHEMA_VERSION + 1);
        let baked = cache_with(live.clone(), 303, SCHEMA_VERSION);
        let (_entries, source) =
            select_cache_source([(Some(disk), "disk"), (Some(baked), "baked")], &live).unwrap();
        assert_eq!(source, "baked");
    }

    #[test]
    fn absent_disk_uses_baked() {
        let live = vec![1.0f32, 0.0];
        let baked = cache_with(live.clone(), 303, SCHEMA_VERSION);
        let (_entries, source) =
            select_cache_source([(None, "disk"), (Some(baked), "baked")], &live).unwrap();
        assert_eq!(source, "baked");
    }

    #[test]
    fn flush_refuses_degenerate_all_zero_probe() {
        // A broken embed slot yields an all-zero probe; persisting it would
        // poison future boots (the write-side origin of the 8-dim zero-vector
        // cache). flush() must refuse and leave no file behind.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("cache.json");
        let k = key("q", "exemplar");
        let mut entries = HashMap::new();
        entries.insert(k.clone(), vec![0.0f32; 8]);
        let mut cache = BootEmbedCache {
            path: Some(path.clone()),
            entries,
            touched: std::iter::once(k).collect(),
            probe: vec![0.0f32; 8],
            hits: 0,
            misses: 1,
            dirty: true,
        };
        cache.flush();
        assert!(!path.exists(), "degenerate cache must not be persisted");
    }
}
