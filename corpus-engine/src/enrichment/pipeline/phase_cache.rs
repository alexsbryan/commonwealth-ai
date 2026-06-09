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
use serde::Serialize;

use super::types::PipelinePhase;
use crate::error::{Error, Result};

/// Handle on the on-disk cache directory for one corpus.
#[derive(Debug, Clone)]
pub struct PhaseCache {
    root: PathBuf,
}

impl PhaseCache {
    pub fn new(cache_dir: impl AsRef<Path>) -> Self {
        Self {
            root: cache_dir.as_ref().to_path_buf(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Absolute path for a phase's cache file.
    pub fn path(&self, phase: PipelinePhase) -> PathBuf {
        self.root.join(format!("{}.json", phase.id()))
    }

    /// Read and parse `phase`'s cached output. `Ok(None)` if the file
    /// doesn't exist. `Err` only on IO or JSON parse failure.
    pub fn read<T: DeserializeOwned>(&self, phase: PipelinePhase) -> Result<Option<T>> {
        let path = self.path(phase);
        if !path.exists() {
            return Ok(None);
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
        Ok(())
    }

    /// Delete `phase`'s cache. Idempotent.
    pub fn clear(&self, phase: PipelinePhase) -> Result<()> {
        let path = self.path(phase);
        if path.exists() {
            fs::remove_file(path)?;
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
