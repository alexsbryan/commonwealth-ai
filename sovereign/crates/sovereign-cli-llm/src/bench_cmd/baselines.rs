//! Baseline storage convention for `sovereign bench all`.
//!
//! ```text
//! sovereign/bench/<group>/
//!     <bench>.toml                       # the bench definition
//!     baselines/<bench>/                 # one dir per bench
//!         latest.json -> 2026-MM-DD.json # symlink → most recent
//!         2026-MM-DD.json                # dated snapshots
//!         ...
//! ```
//!
//! Two readers, one writer:
//! - `read_latest` deserialises `latest.json` into the surface-typed
//!   shape (`EvalReport` for Enrichment, `EvalRun` for
//!   RetrievalJudge). Returns `Ok(None)` when no baseline exists
//!   yet — first-run case.
//! - `write_dated_and_update_latest` writes a fresh dated snapshot,
//!   atomically updates the `latest.json` symlink.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::de::DeserializeOwned;
use serde::Serialize;

use super::discover::DiscoveredBench;

/// `<bench_root>/<group>/baselines/<bench_id>/`. Created on demand by
/// `write_dated_and_update_latest`.
pub fn baseline_dir_for(bench_root: &Path, bench: &DiscoveredBench) -> PathBuf {
    bench_root
        .join(&bench.group)
        .join("baselines")
        .join(&bench.id)
}

/// Path to the canonical `latest.json` symlink inside the baseline
/// dir. May not exist yet.
pub fn latest_symlink_path(bench_root: &Path, bench: &DiscoveredBench) -> PathBuf {
    baseline_dir_for(bench_root, bench).join("latest.json")
}

/// Path for a fresh dated snapshot — `<dir>/<YYYY-MM-DD>.json`. If a
/// snapshot for today already exists, callers may decide to append
/// `-<NN>` suffix; v1 just overwrites.
pub fn dated_snapshot_path(bench_root: &Path, bench: &DiscoveredBench) -> PathBuf {
    let today = Utc::now().format("%Y-%m-%d").to_string();
    baseline_dir_for(bench_root, bench).join(format!("{today}.json"))
}

/// Read the bench's `latest.json` (following symlink if present),
/// deserialise into `T`. Returns `Ok(None)` when the file doesn't
/// exist — caller treats this as "first run, no baseline yet".
pub fn read_latest<T: DeserializeOwned>(
    bench_root: &Path,
    bench: &DiscoveredBench,
) -> Result<Option<T>, String> {
    let path = latest_symlink_path(bench_root, bench);
    // `path.exists()` follows symlinks, so this catches both the
    // dangling-symlink case (returns false) and the no-baseline
    // case (returns false). Either way → None.
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let parsed: T =
        serde_json::from_str(&bytes).map_err(|e| format!("parse {}: {e}", path.display()))?;
    Ok(Some(parsed))
}

/// Persist a fresh dated snapshot + atomically retarget the
/// `latest.json` symlink. Creates the baseline dir if missing.
///
/// macOS `std::os::unix::fs::symlink` doesn't atomically replace an
/// existing symlink; we do a remove-then-symlink pair. Race with a
/// concurrent reader is acceptable here — bench all runs are
/// single-writer.
pub fn write_dated_and_update_latest<T: Serialize>(
    bench_root: &Path,
    bench: &DiscoveredBench,
    report: &T,
) -> io::Result<PathBuf> {
    let dir = baseline_dir_for(bench_root, bench);
    fs::create_dir_all(&dir)?;
    let dated = dated_snapshot_path(bench_root, bench);
    let bytes = serde_json::to_vec_pretty(report)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    fs::write(&dated, &bytes)?;

    let latest = dir.join("latest.json");
    // Best-effort remove of any prior symlink/file at the path; if
    // nothing's there, `remove_file` errors — ignore.
    let _ = fs::remove_file(&latest);
    let dated_filename = dated
        .file_name()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("latest.json"));
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&dated_filename, &latest)?;
    }
    #[cfg(not(unix))]
    {
        // Fallback for non-unix: copy the file. Loses the "latest
        // points to dated" semantic but keeps the file present.
        fs::copy(&dated, &latest)?;
    }
    Ok(dated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench_cmd::discover::{BenchSurface, CorpusIdSource, CorpusState, DiscoveredBench};
    use serde::{Deserialize, Serialize};
    use tempfile::TempDir;

    fn fixture_bench() -> DiscoveredBench {
        DiscoveredBench {
            id: "golden".into(),
            group: "obsidian".into(),
            surface: BenchSurface::Enrichment,
            bench_path: PathBuf::from("/dev/null"),
            corpus_id: "obsidian-vault".into(),
            corpus_id_source: CorpusIdSource::Explicit,
            corpus_state: CorpusState::Ready,
            levers: vec!["mechanism".into()],
        }
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct Sample {
        v: i32,
        msg: String,
    }

    #[test]
    fn baseline_dir_structure() {
        let tmp = TempDir::new().unwrap();
        let bench = fixture_bench();
        let dir = baseline_dir_for(tmp.path(), &bench);
        assert!(dir.ends_with("obsidian/baselines/golden"));
    }

    #[test]
    fn read_latest_returns_none_when_absent() {
        let tmp = TempDir::new().unwrap();
        let bench = fixture_bench();
        let got: Option<Sample> = read_latest(tmp.path(), &bench).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn write_then_read_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let bench = fixture_bench();
        let written = Sample {
            v: 42,
            msg: "fresh-passing".into(),
        };
        let dated_path = write_dated_and_update_latest(tmp.path(), &bench, &written).unwrap();
        assert!(dated_path.exists());

        // latest.json symlink should resolve to the dated snapshot.
        let latest = latest_symlink_path(tmp.path(), &bench);
        assert!(latest.exists());
        let target = fs::read_link(&latest).unwrap();
        assert_eq!(target, PathBuf::from(dated_path.file_name().unwrap()));

        let got: Option<Sample> = read_latest(tmp.path(), &bench).unwrap();
        assert_eq!(got.unwrap(), written);
    }

    #[test]
    fn second_write_retargets_symlink() {
        let tmp = TempDir::new().unwrap();
        let bench = fixture_bench();
        // First write
        let _ = write_dated_and_update_latest(
            tmp.path(),
            &bench,
            &Sample {
                v: 1,
                msg: "old".into(),
            },
        )
        .unwrap();
        // Second write — overwrites because dated path resolves to
        // today; verifies idempotence + symlink retargeting.
        let _ = write_dated_and_update_latest(
            tmp.path(),
            &bench,
            &Sample {
                v: 2,
                msg: "new".into(),
            },
        )
        .unwrap();
        let got: Option<Sample> = read_latest(tmp.path(), &bench).unwrap();
        assert_eq!(got.unwrap().v, 2);
    }
}
