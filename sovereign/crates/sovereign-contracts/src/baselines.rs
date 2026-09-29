// SPDX-License-Identifier: AGPL-3.0-or-later
//! The dated-baseline file convention: `<dir>/YYYY-MM-DD.json` snapshots
//! and a `latest.json` symlink to the newest. Moved from sovereign-cli-llm's
//! `bench_cmd::baselines` (phase-b pb-cli-llm-bench-move) because two
//! programs store baselines this way — bench's lanes and svrn's
//! `svrn router fit --save-baseline` — and this is the one leaf both admit.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::de::DeserializeOwned;
use serde::Serialize;

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
/// remove-then-symlink as bench's `DiscoveredBench`-keyed writer.
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
}
