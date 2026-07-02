// SPDX-License-Identifier: AGPL-3.0-or-later
//! Baseline storage convention for `svrn bench all`.
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

/// `<bench_root>/<group>/baselines/<id>/` — the storage convention,
/// expressed over a raw `(group, id)` pair. This is the primitive both the
/// `DiscoveredBench`-keyed `bench all` path and the lane-baseline gate
/// (`bench gate`, which has no `DiscoveredBench`) build on, so the on-disk
/// layout is identical no matter which surface wrote it.
pub fn baseline_dir(bench_root: &Path, group: &str, id: &str) -> PathBuf {
    bench_root.join(group).join("baselines").join(id)
}

/// Read `<dir>/latest.json` (following the symlink), deserialise into `T`.
/// `Ok(None)` when the file doesn't exist — the first-run case.
pub fn read_latest_at<T: DeserializeOwned>(dir: &Path) -> Result<Option<T>, String> {
    let path = dir.join("latest.json");
    // `exists()` follows symlinks, so a dangling symlink and a missing
    // file both read as None (first run).
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let parsed: T =
        serde_json::from_str(&bytes).map_err(|e| format!("parse {}: {e}", path.display()))?;
    Ok(Some(parsed))
}

/// Persist a fresh dated snapshot into `dir` + atomically retarget the
/// `latest.json` symlink. Creates `dir` if missing. Same single-writer
/// remove-then-symlink as the `DiscoveredBench` writer below.
pub fn write_dated_and_update_latest_at<T: Serialize>(
    dir: &Path,
    report: &T,
) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let dated = dir.join(format!("{today}.json"));
    let bytes = serde_json::to_vec_pretty(report)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    fs::write(&dated, &bytes)?;

    let latest = dir.join("latest.json");
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
        fs::copy(&dated, &latest)?;
    }
    Ok(dated)
}

/// `<bench_root>/<group>/baselines/<bench_id>/`. Created on demand by
/// `write_dated_and_update_latest`.
pub fn baseline_dir_for(bench_root: &Path, bench: &DiscoveredBench) -> PathBuf {
    baseline_dir(bench_root, &bench.group, &bench.id)
}

/// Read the bench's `latest.json` (following symlink if present),
/// deserialise into `T`. Returns `Ok(None)` when the file doesn't
/// exist — caller treats this as "first run, no baseline yet".
pub fn read_latest<T: DeserializeOwned>(
    bench_root: &Path,
    bench: &DiscoveredBench,
) -> Result<Option<T>, String> {
    read_latest_at(&baseline_dir_for(bench_root, bench))
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
    write_dated_and_update_latest_at(&baseline_dir_for(bench_root, bench), report)
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
        let latest = baseline_dir_for(tmp.path(), &bench).join("latest.json");
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

/// Capture date + age-in-days of `<dir>/latest.json`, for staleness
/// surfacing at the consumption site (the April-30-baseline incident:
/// a HARD lane silently diffed against a six-week-old snapshot).
///
/// Primary source: the dated filename the `latest.json` symlink points
/// at (`2026-MM-DD.json`). Fallback: the resolved file's mtime (covers
/// the non-unix copy branch above). `None` when no baseline exists.
/// Schema-independent by design — works identically for typed
/// (`EvalReport`/`EvalRun`), untyped (routing), and lane baselines.
pub fn baseline_age(dir: &Path) -> Option<(String, u64)> {
    let latest = dir.join("latest.json");
    let captured: chrono::NaiveDate = fs::read_link(&latest)
        .ok()
        .and_then(|target| parse_dated_filename(&target))
        .or_else(|| {
            let mtime = fs::metadata(&latest).ok()?.modified().ok()?;
            Some(chrono::DateTime::<Utc>::from(mtime).date_naive())
        })?;
    let age_days = (Utc::now().date_naive() - captured).num_days().max(0) as u64;
    Some((captured.format("%Y-%m-%d").to_string(), age_days))
}

fn parse_dated_filename(path: &Path) -> Option<chrono::NaiveDate> {
    let stem = path.file_stem()?.to_str()?;
    chrono::NaiveDate::parse_from_str(stem, "%Y-%m-%d").ok()
}

/// Warn threshold for baseline age, env-overridable via
/// `SOVEREIGN_BASELINE_MAX_AGE_DAYS`. Default 14. Staleness is
/// operator information, never a gate verdict — renderers warn, exit
/// codes don't change.
pub fn baseline_max_age_days() -> u64 {
    parse_max_age_days(
        std::env::var("SOVEREIGN_BASELINE_MAX_AGE_DAYS")
            .ok()
            .as_deref(),
    )
}

fn parse_max_age_days(raw: Option<&str>) -> u64 {
    const DEFAULT_DAYS: u64 = 14;
    raw.and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&d| d > 0)
        .unwrap_or(DEFAULT_DAYS)
}

#[cfg(test)]
mod age_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn parses_dated_symlink_target() {
        assert!(parse_dated_filename(Path::new("2026-04-30.json")).is_some());
        assert!(parse_dated_filename(Path::new("latest.json")).is_none());
        assert!(parse_dated_filename(Path::new("notes.txt")).is_none());
    }

    #[test]
    fn age_none_when_no_baseline_or_dangling_symlink() {
        let tmp = TempDir::new().unwrap();
        assert!(baseline_age(tmp.path()).is_none());
        #[cfg(unix)]
        {
            // Dangling symlink to a dated name: the filename still
            // carries the capture date — age is reportable even though
            // the snapshot is gone (read_latest_at treats it as first
            // run; age is advisory either way).
            std::os::unix::fs::symlink("2020-01-01.json", tmp.path().join("latest.json")).unwrap();
            let (captured, age) = baseline_age(tmp.path()).unwrap();
            assert_eq!(captured, "2020-01-01");
            assert!(age > 365);
        }
    }

    #[test]
    fn age_zero_for_baseline_written_today() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("baselines").join("golden");
        write_dated_and_update_latest_at(&dir, &serde_json::json!({"v": 1})).unwrap();
        let (_, age) = baseline_age(&dir).unwrap();
        assert_eq!(age, 0);
    }

    #[test]
    fn max_age_parse_policy() {
        assert_eq!(parse_max_age_days(None), 14);
        assert_eq!(parse_max_age_days(Some("not-a-number")), 14);
        assert_eq!(parse_max_age_days(Some("0")), 14);
        assert_eq!(parse_max_age_days(Some("30")), 30);
    }
}
