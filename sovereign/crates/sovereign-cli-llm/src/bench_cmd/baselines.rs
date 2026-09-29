// SPDX-License-Identifier: AGPL-3.0-or-later
//! Baseline storage convention for `svrn bench all` — and, since
//! 2026-07-28, for `svrn router fit --save-baseline`, which stores a
//! `FitSnapshot` under `routing/baselines/<bank>-fit/` through the
//! same two functions.
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

use std::io;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;

pub use sovereign_contracts::baselines::{
    baseline_age, baseline_dir, read_latest_at, write_dated_and_update_latest_at,
};

use super::discover::DiscoveredBench;

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
    use crate::bench_cmd::discover::{BenchSurface, CorpusIdSource, DiscoveredBench};
    use serde::{Deserialize, Serialize};
    use sovereign_contracts::index_layout::CorpusIndexState;
    use std::fs;
    use tempfile::TempDir;

    fn fixture_bench() -> DiscoveredBench {
        DiscoveredBench {
            id: "golden".into(),
            group: "obsidian".into(),
            surface: BenchSurface::Enrichment,
            bench_path: PathBuf::from("/dev/null"),
            corpus_id: "obsidian-vault".into(),
            corpus_id_source: CorpusIdSource::Explicit,
            corpus_state: CorpusIndexState::Ready,
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

    #[test]
    fn max_age_parse_policy() {
        assert_eq!(parse_max_age_days(None), 14);
        assert_eq!(parse_max_age_days(Some("not-a-number")), 14);
        assert_eq!(parse_max_age_days(Some("0")), 14);
        assert_eq!(parse_max_age_days(Some("30")), 30);
    }
}
