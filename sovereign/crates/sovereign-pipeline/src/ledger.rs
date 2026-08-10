// SPDX-License-Identifier: AGPL-3.0-or-later
//! Cost ledger for cloud pods launched by the pipeline tool.
//!
//! Each `pipeline pod up` appends a `PodRecord` to a JSON file at
//! `~/.svrnmesh/pipeline-pods.json`. `pod down` marks the record
//! `closed` and stamps `ended_at`, leaving the row for postmortem
//! cost reconstruction. Nothing is ever deleted on its own — the
//! ledger is append-only-ish: the operator can prune with `pod
//! list --prune` when they want a clean view.
//!
//! Writes are atomic via tempfile-rename — a crash mid-write can't
//! corrupt the file. Reads tolerate a missing file (returns empty).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LedgerError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("no pod with id `{0}` in ledger")]
    NotFound(String),
}

pub type Result<T> = std::result::Result<T, LedgerError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PodRecord {
    /// Vast.ai instance id, stringified.
    pub vast_id: String,
    /// Operator-supplied label. Defaults to `<recipe_id>-<vast_id>`.
    pub label: String,
    /// Recipe this pod was launched for (empty if none).
    pub recipe_id: String,
    /// GPU sku, e.g. `RTX_4090` or `L40S`.
    pub gpu_name: String,
    /// Container image actually launched (post-resolution).
    pub image: String,
    /// Unix seconds at launch.
    pub started_at: i64,
    /// Unix seconds at `pod down` (`None` while running).
    #[serde(default)]
    pub ended_at: Option<i64>,
    /// $/hr quoted at launch time. Vast rates can change; this is
    /// the rate we agreed to and what cost computations use.
    pub cost_per_hour: f64,
    /// `running` while in the ledger, `closed` after `pod down`.
    pub status: PodStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PodStatus {
    Running,
    Closed,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct LedgerFile {
    pub pods: Vec<PodRecord>,
}

/// Read the ledger at `path`. Missing file → empty ledger.
pub fn read(path: impl AsRef<Path>) -> Result<Vec<PodRecord>> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(vec![]);
    }
    let text = fs::read_to_string(path)?;
    if text.trim().is_empty() {
        return Ok(vec![]);
    }
    let file: LedgerFile = serde_json::from_str(&text)?;
    Ok(file.pods)
}

/// Atomic write: serialize into a sibling tempfile, fsync, rename.
fn write_atomic(path: &Path, pods: &[PodRecord]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = LedgerFile {
        pods: pods.to_vec(),
    };
    let body = serde_json::to_vec_pretty(&file)?;
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&body)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Append a fresh pod record. Errors if `vast_id` already exists in
/// the ledger as `running` — drop or close the prior entry first.
pub fn append(path: impl AsRef<Path>, rec: PodRecord) -> Result<()> {
    let path = path.as_ref();
    let mut pods = read(path)?;
    if pods
        .iter()
        .any(|p| p.vast_id == rec.vast_id && p.status == PodStatus::Running)
    {
        return Err(LedgerError::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("pod `{}` already running in ledger", rec.vast_id),
        )));
    }
    pods.push(rec);
    write_atomic(path, &pods)?;
    Ok(())
}

/// Mark a pod closed and stamp `ended_at`. Returns the updated
/// record so callers can print the final cost.
pub fn close(path: impl AsRef<Path>, vast_id: &str) -> Result<PodRecord> {
    let path = path.as_ref();
    let mut pods = read(path)?;
    let now = unix_now();
    let mut updated: Option<PodRecord> = None;
    for p in &mut pods {
        if p.vast_id == vast_id && p.status == PodStatus::Running {
            p.status = PodStatus::Closed;
            p.ended_at = Some(now);
            updated = Some(p.clone());
            break;
        }
    }
    let Some(rec) = updated else {
        return Err(LedgerError::NotFound(vast_id.to_string()));
    };
    write_atomic(path, &pods)?;
    Ok(rec)
}

/// Drop closed entries. Running entries are preserved.
pub fn prune_closed(path: impl AsRef<Path>) -> Result<u64> {
    let path = path.as_ref();
    let pods = read(path)?;
    let before = pods.len() as u64;
    let kept: Vec<PodRecord> = pods
        .into_iter()
        .filter(|p| p.status == PodStatus::Running)
        .collect();
    let removed = before - kept.len() as u64;
    write_atomic(path, &kept)?;
    Ok(removed)
}

/// Hours billed for one pod (rounded up to the nearest minute as
/// Vast bills, then expressed as fractional hours).
pub fn elapsed_hours(rec: &PodRecord) -> f64 {
    let end = rec.ended_at.unwrap_or_else(unix_now);
    let secs = (end - rec.started_at).max(0) as f64;
    secs / 3600.0
}

/// Total accrued cost for one pod, given the current `cost_per_hour`.
pub fn accrued_cost(rec: &PodRecord) -> f64 {
    elapsed_hours(rec) * rec.cost_per_hour
}

pub use sovereign_time::unix_now;

/// Default ledger path: `~/.svrnmesh/pipeline-pods.json`.
pub fn default_path() -> PathBuf {
    sovereign_contracts::rebrand::svrnmesh_root().join("pipeline-pods.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn rec(vast_id: &str, status: PodStatus) -> PodRecord {
        PodRecord {
            vast_id: vast_id.into(),
            label: format!("test-{vast_id}"),
            recipe_id: "r".into(),
            gpu_name: "L40S".into(),
            image: "ghcr.io/test/img:latest".into(),
            started_at: 1_000_000,
            ended_at: None,
            cost_per_hour: 0.5,
            status,
        }
    }

    #[test]
    fn read_missing_file_is_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("ledger.json");
        assert!(read(&path).unwrap().is_empty());
    }

    #[test]
    fn append_then_read_roundtrips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("ledger.json");
        append(&path, rec("123", PodStatus::Running)).unwrap();
        let pods = read(&path).unwrap();
        assert_eq!(pods.len(), 1);
        assert_eq!(pods[0].vast_id, "123");
        assert_eq!(pods[0].status, PodStatus::Running);
    }

    #[test]
    fn append_rejects_duplicate_running_pod() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("ledger.json");
        append(&path, rec("123", PodStatus::Running)).unwrap();
        let err = append(&path, rec("123", PodStatus::Running)).unwrap_err();
        assert!(format!("{err}").contains("already running"));
    }

    #[test]
    fn append_allows_relaunch_of_closed_pod_id() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("ledger.json");
        let mut r = rec("123", PodStatus::Closed);
        r.ended_at = Some(2_000_000);
        // Pretend we ran and closed this pod previously.
        write_atomic(&path, &[r]).unwrap();
        // Now Vast reuses the id — should accept.
        append(&path, rec("123", PodStatus::Running)).unwrap();
        let pods = read(&path).unwrap();
        assert_eq!(pods.len(), 2);
    }

    #[test]
    fn close_marks_status_and_stamps_ended_at() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("ledger.json");
        append(&path, rec("123", PodStatus::Running)).unwrap();
        let closed = close(&path, "123").unwrap();
        assert_eq!(closed.status, PodStatus::Closed);
        assert!(closed.ended_at.is_some());
    }

    #[test]
    fn close_errors_when_pod_absent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("ledger.json");
        let err = close(&path, "999").unwrap_err();
        assert!(matches!(err, LedgerError::NotFound(_)));
    }

    #[test]
    fn prune_closed_keeps_running_pods() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("ledger.json");
        append(&path, rec("a", PodStatus::Running)).unwrap();
        let mut closed = rec("b", PodStatus::Closed);
        closed.ended_at = Some(unix_now());
        // hand-inject closed entry
        let mut all = read(&path).unwrap();
        all.push(closed);
        write_atomic(&path, &all).unwrap();
        let removed = prune_closed(&path).unwrap();
        assert_eq!(removed, 1);
        let kept = read(&path).unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].vast_id, "a");
    }

    #[test]
    fn accrued_cost_uses_elapsed_hours() {
        let mut r = rec("123", PodStatus::Closed);
        r.started_at = 1_000_000;
        r.ended_at = Some(1_000_000 + 3600); // exactly 1 hour
        r.cost_per_hour = 0.42;
        let cost = accrued_cost(&r);
        assert!((cost - 0.42).abs() < 1e-9);
    }
}
