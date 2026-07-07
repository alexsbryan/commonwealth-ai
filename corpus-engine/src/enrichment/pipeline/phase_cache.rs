// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-phase cache with mtime-based staleness.
//!
//! The CLI admin harness treats every phase's output as a cached JSON
//! file at `<root>/cache/<phase-id>.json`. The runner reads upstream
//! caches as inputs and writes this phase's cache atomically on
//! success. Cross-phase staleness is derived from filesystem mtime:
//! this phase is stale if any upstream phase's cache was written more
//! recently than this one.
//!
//! Exemplar files and the DESIGN-like config.json can also be treated
//! as "upstream" for staleness — if the developer hand-edits phase 5
//! exemplars, every cached phase ≥ 5 becomes stale until the next
//! run. The `is_stale_against_files` helper covers that.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::types::PipelinePhase;
use crate::error::{Error, Result};

/// The model that produced a cached phase, stamped alongside its
/// output so a later run under a *different* model does not silently
/// reuse the old model's work (the stale-on-model-swap hazard —
/// OICP v0.4 §6, "Client SHOULD key model-dependent caches on
/// `(model, fingerprint)`"). `fingerprint` is the host's opaque
/// weight/quant/template fingerprint when the manifest advertises one
/// (`ProviderModel.fingerprint`); `None` on v0.3 hosts, where the
/// model id alone keys the cache. Two identities are compatible iff
/// every field matches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheModelIdentity {
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
}

/// Handle on the on-disk cache directory for one corpus.
#[derive(Debug, Clone)]
pub struct PhaseCache {
    root: PathBuf,
    /// When set, every phase write stamps a `<phase>.model.json`
    /// sidecar with this identity, and every read refuses to serve a
    /// phase whose stamp names a *different* model — turning a silent
    /// stale-serve after a model swap into a clean cache miss (the
    /// pipeline then recomputes). `None` disables the guard, which is
    /// exactly the pre-guard behaviour — used by display-only readers
    /// (`status`, `show`) that should render whatever is cached
    /// regardless of model, and by tests.
    identity: Option<CacheModelIdentity>,
}

impl PhaseCache {
    pub fn new(cache_dir: impl AsRef<Path>) -> Self {
        Self {
            root: cache_dir.as_ref().to_path_buf(),
            identity: None,
        }
    }

    /// Enable the model-identity guard (see [`CacheModelIdentity`]).
    /// Reads and writes of pipeline phase I/O should flow through a
    /// cache built with the *same* identity so they agree; a cache
    /// built without it grandfathers every phase (serves regardless).
    /// Partial adoption is monotone-safe: an unstamped write simply
    /// leaves the next read to grandfather, never corrupts.
    pub fn with_model_identity(mut self, model: impl Into<String>, fingerprint: Option<String>) -> Self {
        self.identity = Some(CacheModelIdentity {
            model: model.into(),
            fingerprint,
        });
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Absolute path for a phase's cache file.
    pub fn path(&self, phase: PipelinePhase) -> PathBuf {
        self.root.join(format!("{}.json", phase.id()))
    }

    /// Absolute path for a phase's model-identity sidecar.
    fn identity_path(&self, phase: PipelinePhase) -> PathBuf {
        self.root.join(format!("{}.model.json", phase.id()))
    }

    /// Read the model that produced `phase`'s cached output, if it was
    /// stamped. `None` = unstamped (legacy cache) or unreadable stamp.
    fn read_stamp(&self, phase: PipelinePhase) -> Option<CacheModelIdentity> {
        let raw = fs::read_to_string(self.identity_path(phase)).ok()?;
        serde_json::from_str(&raw).ok()
    }

    /// Phases whose on-disk model stamp names a *different* model than
    /// this cache's configured identity. Empty when the guard is
    /// disabled, when no stamp differs, or when a phase is unstamped
    /// (grandfathered). The CLI calls this before a run to warn the
    /// operator up front that a model swap will force recomputation,
    /// rather than surfacing it only as a per-phase `missing upstream`.
    pub fn mismatched_phases(&self) -> Vec<(PipelinePhase, CacheModelIdentity)> {
        let Some(want) = &self.identity else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for phase in PipelinePhase::ALL {
            if self.path(*phase).exists() {
                if let Some(have) = self.read_stamp(*phase) {
                    if &have != want {
                        out.push((*phase, have));
                    }
                }
            }
        }
        out
    }

    /// Read and parse `phase`'s cached output. `Ok(None)` if the file
    /// doesn't exist. `Err` only on IO or JSON parse failure.
    pub fn read<T: DeserializeOwned>(&self, phase: PipelinePhase) -> Result<Option<T>> {
        let path = self.path(phase);
        if !path.exists() {
            return Ok(None);
        }
        // Model-identity guard: refuse to serve a phase produced by a
        // different model, so the pipeline recomputes rather than
        // mixing model outputs (stale-on-model-swap, OICP v0.4 §6). A
        // mismatch returns `Ok(None)` — the same signal as a cold
        // cache — so the runner recomputes (full cascade) or raises
        // `missing upstream` (partial run), preceded here by a warning
        // that explains *why* the otherwise-present cache was skipped.
        if let Some(want) = &self.identity {
            match self.read_stamp(phase) {
                Some(have) if &have != want => {
                    tracing::warn!(
                        phase = phase.id(),
                        cached_model = %have.model,
                        current_model = %want.model,
                        "phase cache was produced by a different model — not reusing it (recompute required)"
                    );
                    return Ok(None);
                }
                None => {
                    // Legacy cache written before the guard existed:
                    // grandfather it (serve), preserving prior
                    // behaviour. The next write re-stamps it.
                    tracing::debug!(
                        phase = phase.id(),
                        "phase cache has no model stamp — grandfathering under current model"
                    );
                }
                _ => {} // stamp matches the current model
            }
        }
        let raw = fs::read_to_string(&path)?;
        let parsed: T = serde_json::from_str(&raw).map_err(|e| {
            Error::Serialization(format!("phase cache {} parse error: {}", path.display(), e))
        })?;
        Ok(Some(parsed))
    }

    /// Atomically write `phase`'s cache. Creates the cache directory
    /// if missing.
    pub fn write<T: Serialize>(&self, phase: PipelinePhase, value: &T) -> Result<()> {
        fs::create_dir_all(&self.root)?;
        let json =
            serde_json::to_string_pretty(value).map_err(|e| Error::Serialization(e.to_string()))?;
        let path = self.path(phase);
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, json)?;
        fs::rename(&tmp, &path)?;
        // Stamp the producing model so a later read under a different
        // model can decline to reuse this output. Best-effort: the
        // phase output above is already durably written, and a missing
        // stamp only weakens the guard (the next read grandfathers) —
        // it never corrupts data — so a stamp failure warns rather
        // than failing the write.
        if let Some(id) = &self.identity {
            if let Err(e) = self.write_stamp(phase, id) {
                tracing::warn!(
                    phase = phase.id(),
                    error = %e,
                    "failed to write phase model stamp — model-swap guard weakened for this phase"
                );
            }
        }
        Ok(())
    }

    /// Atomically write `phase`'s model-identity sidecar.
    fn write_stamp(&self, phase: PipelinePhase, id: &CacheModelIdentity) -> Result<()> {
        let json = serde_json::to_string(id).map_err(|e| Error::Serialization(e.to_string()))?;
        let path = self.identity_path(phase);
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, json)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Delete `phase`'s cache (and its model stamp, if any).
    /// Idempotent.
    pub fn clear(&self, phase: PipelinePhase) -> Result<()> {
        let path = self.path(phase);
        if path.exists() {
            fs::remove_file(path)?;
        }
        let stamp = self.identity_path(phase);
        if stamp.exists() {
            let _ = fs::remove_file(stamp);
        }
        Ok(())
    }

    pub fn mtime(&self, phase: PipelinePhase) -> Result<Option<SystemTime>> {
        let path = self.path(phase);
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(path.metadata()?.modified()?))
    }

    /// True when `phase`'s cache is older than any of its declared
    /// upstream dependencies. Never-run counts as stale. No cache =
    /// stale.
    pub fn is_stale(&self, phase: PipelinePhase) -> Result<bool> {
        let Some(my_mtime) = self.mtime(phase)? else {
            return Ok(true);
        };
        for dep in phase.dependencies() {
            let Some(dep_mtime) = self.mtime(*dep)? else {
                return Ok(true); // upstream never ran → stale
            };
            if dep_mtime > my_mtime {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Extend the dep-based staleness check with arbitrary extra
    /// files (exemplars, config.json). If any of them is newer than
    /// this phase's cache, report stale.
    pub fn is_stale_against_files(&self, phase: PipelinePhase, extras: &[PathBuf]) -> Result<bool> {
        if self.is_stale(phase)? {
            return Ok(true);
        }
        let Some(my_mtime) = self.mtime(phase)? else {
            return Ok(true);
        };
        for p in extras {
            if !p.exists() {
                continue;
            }
            let m = p.metadata()?.modified()?;
            if m > my_mtime {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn list_all(&self) -> Result<HashMap<PipelinePhase, PhaseCacheMeta>> {
        let mut out = HashMap::new();
        for phase in PipelinePhase::ALL {
            let path = self.path(*phase);
            if !path.exists() {
                continue;
            }
            let meta = path.metadata()?;
            out.insert(
                *phase,
                PhaseCacheMeta {
                    phase: *phase,
                    path,
                    mtime: meta.modified()?,
                    size_bytes: meta.len(),
                },
            );
        }
        Ok(out)
    }
}

#[derive(Debug, Clone)]
pub struct PhaseCacheMeta {
    pub phase: PipelinePhase,
    pub path: PathBuf,
    pub mtime: SystemTime,
    pub size_bytes: u64,
}

/// The status a CLI `status` row should render for a given phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseCacheStatus {
    NeverRun,
    Fresh,
    Stale,
}

impl PhaseCache {
    pub fn status(&self, phase: PipelinePhase) -> Result<PhaseCacheStatus> {
        if self.mtime(phase)?.is_none() {
            return Ok(PhaseCacheStatus::NeverRun);
        }
        if self.is_stale(phase)? {
            return Ok(PhaseCacheStatus::Stale);
        }
        Ok(PhaseCacheStatus::Fresh)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::thread::sleep;
    use std::time::Duration;
    use tempfile::tempdir;

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Dummy {
        schema_version: u32,
        value: i32,
    }

    #[test]
    fn write_read_roundtrip() {
        let dir = tempdir().unwrap();
        let cache = PhaseCache::new(dir.path());
        let d = Dummy {
            schema_version: 1,
            value: 42,
        };
        cache.write(PipelinePhase::Questions, &d).unwrap();
        let got: Dummy = cache.read(PipelinePhase::Questions).unwrap().unwrap();
        assert_eq!(got, d);
    }

    #[test]
    fn read_missing_returns_none() {
        let dir = tempdir().unwrap();
        let cache = PhaseCache::new(dir.path());
        let got: Option<Dummy> = cache.read(PipelinePhase::Questions).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn same_model_reuses_cache() {
        let dir = tempdir().unwrap();
        let cache = PhaseCache::new(dir.path()).with_model_identity("model-a", None);
        let d = Dummy {
            schema_version: 1,
            value: 7,
        };
        cache.write(PipelinePhase::Questions, &d).unwrap();
        // A fresh handle with the *same* identity must reuse it.
        let same = PhaseCache::new(dir.path()).with_model_identity("model-a", None);
        let got: Dummy = same.read(PipelinePhase::Questions).unwrap().unwrap();
        assert_eq!(got, d);
    }

    #[test]
    fn different_model_is_cache_miss() {
        let dir = tempdir().unwrap();
        let a = PhaseCache::new(dir.path()).with_model_identity("model-a", None);
        a.write(
            PipelinePhase::Questions,
            &Dummy {
                schema_version: 1,
                value: 1,
            },
        )
        .unwrap();
        // Same corpus, different model → the guard declines to reuse
        // model-a's output; read returns None so the pipeline recomputes.
        let b = PhaseCache::new(dir.path()).with_model_identity("model-b", None);
        let got: Option<Dummy> = b.read(PipelinePhase::Questions).unwrap();
        assert!(got.is_none(), "model swap must invalidate the phase cache");
        // And it surfaces up front for the operator warning.
        let mismatches = b.mismatched_phases();
        assert_eq!(mismatches.len(), 1);
        assert_eq!(mismatches[0].0, PipelinePhase::Questions);
        assert_eq!(mismatches[0].1.model, "model-a");
    }

    #[test]
    fn fingerprint_distinguishes_same_model_name() {
        let dir = tempdir().unwrap();
        let a = PhaseCache::new(dir.path())
            .with_model_identity("qwen", Some("fp-v1".to_string()));
        a.write(
            PipelinePhase::Questions,
            &Dummy {
                schema_version: 1,
                value: 1,
            },
        )
        .unwrap();
        // Same model id, changed weights/quant/template → new fingerprint
        // → cache miss.
        let a2 = PhaseCache::new(dir.path())
            .with_model_identity("qwen", Some("fp-v2".to_string()));
        let got: Option<Dummy> = a2.read(PipelinePhase::Questions).unwrap();
        assert!(got.is_none(), "fingerprint change must invalidate the cache");
    }

    #[test]
    fn legacy_unstamped_cache_is_grandfathered() {
        let dir = tempdir().unwrap();
        // Written by the pre-guard code path (no identity → no stamp).
        let legacy = PhaseCache::new(dir.path());
        let d = Dummy {
            schema_version: 1,
            value: 9,
        };
        legacy.write(PipelinePhase::Questions, &d).unwrap();
        assert!(!legacy
            .identity_path(PipelinePhase::Questions)
            .exists());
        // A later run *with* a guard must still serve it (grandfathered),
        // not treat the unstamped cache as a foreign-model miss.
        let guarded = PhaseCache::new(dir.path()).with_model_identity("model-a", None);
        let got: Dummy = guarded.read(PipelinePhase::Questions).unwrap().unwrap();
        assert_eq!(got, d);
        assert!(guarded.mismatched_phases().is_empty());
    }

    #[test]
    fn clear_removes_model_stamp() {
        let dir = tempdir().unwrap();
        let cache = PhaseCache::new(dir.path()).with_model_identity("model-a", None);
        cache
            .write(
                PipelinePhase::Questions,
                &Dummy {
                    schema_version: 1,
                    value: 1,
                },
            )
            .unwrap();
        assert!(cache.identity_path(PipelinePhase::Questions).exists());
        cache.clear(PipelinePhase::Questions).unwrap();
        assert!(!cache.identity_path(PipelinePhase::Questions).exists());
        assert!(!cache.path(PipelinePhase::Questions).exists());
    }

    #[test]
    fn atomic_tmp_file_is_cleaned_up() {
        let dir = tempdir().unwrap();
        let cache = PhaseCache::new(dir.path());
        let d = Dummy {
            schema_version: 1,
            value: 1,
        };
        cache.write(PipelinePhase::Questions, &d).unwrap();
        // tmp should not remain on disk after rename.
        assert!(!cache
            .path(PipelinePhase::Questions)
            .with_extension("json.tmp")
            .exists());
    }

    #[test]
    fn is_stale_when_upstream_never_ran() {
        let dir = tempdir().unwrap();
        let cache = PhaseCache::new(dir.path());
        let d = Dummy {
            schema_version: 1,
            value: 1,
        };
        // Write downstream phase without writing upstream.
        cache.write(PipelinePhase::Questions, &d).unwrap();
        assert!(cache.is_stale(PipelinePhase::Questions).unwrap());
    }

    #[test]
    fn is_fresh_when_no_deps_and_cache_exists() {
        let dir = tempdir().unwrap();
        let cache = PhaseCache::new(dir.path());
        let d = Dummy {
            schema_version: 1,
            value: 1,
        };
        cache.write(PipelinePhase::Ingest, &d).unwrap();
        // Ingest has no dependencies.
        assert!(!cache.is_stale(PipelinePhase::Ingest).unwrap());
        assert_eq!(
            cache.status(PipelinePhase::Ingest).unwrap(),
            PhaseCacheStatus::Fresh
        );
    }

    #[test]
    fn is_stale_when_upstream_is_newer() {
        let dir = tempdir().unwrap();
        let cache = PhaseCache::new(dir.path());
        let d = Dummy {
            schema_version: 1,
            value: 1,
        };
        // Write downstream first.
        cache.write(PipelinePhase::Ingest, &d).unwrap();
        cache.write(PipelinePhase::Questions, &d).unwrap();
        assert_eq!(
            cache.status(PipelinePhase::Questions).unwrap(),
            PhaseCacheStatus::Fresh
        );
        // Wait enough for mtime resolution to tick (mac HFS is seconds).
        sleep(Duration::from_millis(1100));
        // Touch upstream by rewriting — new mtime.
        cache.write(PipelinePhase::Ingest, &d).unwrap();
        assert_eq!(
            cache.status(PipelinePhase::Questions).unwrap(),
            PhaseCacheStatus::Stale
        );
    }

    #[test]
    fn status_never_run_when_no_cache() {
        let dir = tempdir().unwrap();
        let cache = PhaseCache::new(dir.path());
        assert_eq!(
            cache.status(PipelinePhase::Ingest).unwrap(),
            PhaseCacheStatus::NeverRun
        );
    }

    #[test]
    fn is_stale_against_extra_file() {
        let dir = tempdir().unwrap();
        let cache = PhaseCache::new(dir.path());
        let d = Dummy {
            schema_version: 1,
            value: 1,
        };
        cache.write(PipelinePhase::Ingest, &d).unwrap();
        cache.write(PipelinePhase::Questions, &d).unwrap();
        assert!(!cache
            .is_stale_against_files(PipelinePhase::Questions, &[])
            .unwrap());
        // Touch an extra file with a newer mtime.
        sleep(Duration::from_millis(1100));
        let extra = dir.path().join("exemplars_phase1.json");
        fs::write(&extra, b"new").unwrap();
        assert!(cache
            .is_stale_against_files(PipelinePhase::Questions, &[extra])
            .unwrap());
    }

    #[test]
    fn list_all_reports_only_existing_caches() {
        let dir = tempdir().unwrap();
        let cache = PhaseCache::new(dir.path());
        let d = Dummy {
            schema_version: 1,
            value: 1,
        };
        cache.write(PipelinePhase::Ingest, &d).unwrap();
        cache.write(PipelinePhase::Questions, &d).unwrap();
        let listed = cache.list_all().unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed.contains_key(&PipelinePhase::Ingest));
        assert!(listed.contains_key(&PipelinePhase::Questions));
    }

    #[test]
    fn clear_removes_cache() {
        let dir = tempdir().unwrap();
        let cache = PhaseCache::new(dir.path());
        let d = Dummy {
            schema_version: 1,
            value: 1,
        };
        cache.write(PipelinePhase::Ingest, &d).unwrap();
        cache.clear(PipelinePhase::Ingest).unwrap();
        assert!(!cache.path(PipelinePhase::Ingest).exists());
        // Double-clear is idempotent.
        cache.clear(PipelinePhase::Ingest).unwrap();
    }
}
