//! Baseline persistence + `latest.json` symlink retargeting.
//!
//! Duplicated shape of `sovereign-cli/src/bench_cmd/baselines.rs` per
//! ARCH §10.3 — re-implementing 60 lines is cheaper than a workspace
//! shuffle for the MVS. PR 2 lifts both into a shared crate.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{de::DeserializeOwned, Serialize};
use thiserror::Error;

const LATEST_SYMLINK: &str = "latest.json";

#[derive(Debug, Error)]
pub enum BaselineError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("baseline root invalid: {0}")]
    Invalid(String),
}

pub fn baseline_dir(bench_root: &Path, group: &str) -> PathBuf {
    bench_root.join("baselines").join(group)
}

pub fn dated_snapshot_path(bench_root: &Path, group: &str, agent: &str, model: &str) -> PathBuf {
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let model_slug = slug(model);
    baseline_dir(bench_root, group).join(format!("{today}-{agent}-{model_slug}.json"))
}

pub fn latest_symlink_path(bench_root: &Path, group: &str) -> PathBuf {
    baseline_dir(bench_root, group).join(LATEST_SYMLINK)
}

/// Write `report` to the dated snapshot path and retarget `latest.json`
/// to point at it. Returns the snapshot path.
pub fn write_dated_and_update_latest<T: Serialize>(
    bench_root: &Path,
    group: &str,
    agent: &str,
    model: &str,
    report: &T,
) -> Result<PathBuf, BaselineError> {
    let dir = baseline_dir(bench_root, group);
    fs::create_dir_all(&dir)?;
    let snapshot = dated_snapshot_path(bench_root, group, agent, model);
    let bytes = serde_json::to_vec_pretty(report)?;
    fs::write(&snapshot, &bytes)?;
    retarget_latest(&dir, &snapshot)?;
    Ok(snapshot)
}

/// Read the report `latest.json` points at, if any.
pub fn read_latest<T: DeserializeOwned>(
    bench_root: &Path,
    group: &str,
) -> Result<Option<T>, BaselineError> {
    let link = latest_symlink_path(bench_root, group);
    if !link.exists() {
        return Ok(None);
    }
    // `latest.json` is either a symlink to a dated file or a plain
    // file (Windows fallback). Either way, `read` resolves it.
    let bytes = fs::read(&link)?;
    let parsed: T = serde_json::from_slice(&bytes)?;
    Ok(Some(parsed))
}

fn retarget_latest(dir: &Path, snapshot: &Path) -> Result<(), BaselineError> {
    let link = dir.join(LATEST_SYMLINK);
    if link.exists() || link.symlink_metadata().is_ok() {
        let _ = fs::remove_file(&link);
    }
    // Use the filename only — symlinks within the baselines dir stay
    // portable when the repo moves.
    let target = snapshot
        .file_name()
        .ok_or_else(|| BaselineError::Invalid("snapshot has no filename".into()))?;
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, &link)?;
    }
    #[cfg(not(unix))]
    {
        // Windows fallback: just copy.
        fs::copy(snapshot, &link)?;
    }
    Ok(())
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | ' ' | ':' | '\\' => '-',
            other
                if other.is_ascii_alphanumeric()
                    || other == '.'
                    || other == '-'
                    || other == '_' =>
            {
                other
            }
            _ => '-',
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
    struct Demo {
        n: u32,
        s: String,
    }

    #[test]
    fn write_then_read_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let d = Demo {
            n: 7,
            s: "hello".into(),
        };
        let snapshot = write_dated_and_update_latest(
            tmp.path(),
            "agent-coding",
            "pi",
            "commonwealth/coder",
            &d,
        )
        .unwrap();
        assert!(snapshot.exists());
        assert!(snapshot
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .contains("commonwealth-coder"));
        let back: Option<Demo> = read_latest(tmp.path(), "agent-coding").unwrap();
        assert_eq!(back, Some(d));
    }

    #[test]
    fn read_latest_returns_none_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let back: Option<Demo> = read_latest(tmp.path(), "nothing-here").unwrap();
        assert!(back.is_none());
    }

    #[test]
    fn slug_replaces_problem_characters() {
        assert_eq!(slug("commonwealth/coder"), "commonwealth-coder");
        assert_eq!(slug("a:b c"), "a-b-c");
        assert_eq!(slug("ok-1.2"), "ok-1.2");
    }
}
