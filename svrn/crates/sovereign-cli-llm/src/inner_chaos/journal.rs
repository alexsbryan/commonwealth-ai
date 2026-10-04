// SPDX-License-Identifier: AGPL-3.0-or-later
//! Journal discipline — same as the knowledge chaos harness:
//! `test-artifacts/inner-chaos-journal.jsonl`, wiped on start, one
//! JSON line per witness turn, plus a stamped copy per run so
//! history survives the next wipe.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::judge::WitnessVerdict;

/// One journaled witness turn — the receipt the fix loop runs on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnRecord {
    pub ts_unix_ms: u64,
    pub thread: usize,
    /// 1-based turn index within the thread.
    pub turn: usize,
    pub persona: String,
    pub conv_id: String,
    pub user: String,
    pub response: String,
    /// `None` when the turn was not judged (judge disabled, judge
    /// call failed, or output unparseable) — see `judge_failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<WitnessVerdict>,
    /// True iff a judge WAS requested but produced no usable
    /// verdict. Distinct from judge-disabled (verdict None,
    /// judge_failed false). Failed-judge turns are never counted
    /// as safe.
    #[serde(default)]
    pub judge_failed: bool,
    /// Setup / runtime error for this turn, when the thread aborted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub brain_ms: u64,
    #[serde(default)]
    pub runtime_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_ms: Option<u64>,
}

pub struct Journal {
    path: PathBuf,
    file: File,
}

impl Journal {
    /// Open the journal, WIPING any prior content (the stamped copy
    /// from the previous run is the archive; the live file is
    /// always this-run-only).
    pub fn create(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create journal dir {}: {e}", parent.display()))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .map_err(|e| format!("open journal {}: {e}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            file,
        })
    }

    /// Append any serializable record as one JSONL line. Generic so
    /// the recall extension (`super::recall::RecallTurnRecord`) can
    /// share the wipe-on-start + flush discipline without a second
    /// journal type; the core loop still passes `TurnRecord`.
    pub fn append<T: serde::Serialize>(&mut self, record: &T) -> Result<(), String> {
        let line = serde_json::to_string(record).map_err(|e| format!("serialize record: {e}"))?;
        writeln!(self.file, "{line}").map_err(|e| format!("write journal: {e}"))?;
        self.file.flush().map_err(|e| format!("flush journal: {e}"))
    }

    /// Copy the live journal to a stamped sibling
    /// (`inner-chaos-<stamp>.jsonl`) and return the copy's path.
    pub fn stamped_copy(&self, stamp: &str) -> Result<PathBuf, String> {
        let dir = self.path.parent().unwrap_or(Path::new("."));
        let dest = dir.join(format!("inner-chaos-{stamp}.jsonl"));
        std::fs::copy(&self.path, &dest)
            .map_err(|e| format!("stamp journal copy {}: {e}", dest.display()))?;
        Ok(dest)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inner_chaos::judge::WitnessCategory;

    fn record() -> TurnRecord {
        TurnRecord {
            ts_unix_ms: 1,
            thread: 0,
            turn: 1,
            persona: "reflective_control".into(),
            conv_id: "c".into(),
            user: "u".into(),
            response: "r".into(),
            verdict: Some(WitnessVerdict {
                red_lines: vec![],
                signals: vec![],
                category: WitnessCategory::Good,
                why: "w".into(),
            }),
            judge_failed: false,
            error: None,
            brain_ms: 10,
            runtime_ms: 20,
            judge_ms: Some(5),
        }
    }

    #[test]
    fn journal_wipes_appends_and_stamps() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artifacts/inner-chaos-journal.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "stale line from prior run\n").unwrap();

        let mut journal = Journal::create(&path).unwrap();
        journal.append(&record()).unwrap();
        journal.append(&record()).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            !content.contains("stale line"),
            "journal must wipe on start"
        );
        assert_eq!(content.lines().count(), 2);

        // Roundtrip.
        let parsed: TurnRecord = serde_json::from_str(content.lines().next().unwrap()).unwrap();
        assert_eq!(parsed.persona, "reflective_control");
        assert!(parsed.verdict.unwrap().is_safe());

        let stamped = journal.stamped_copy("test-stamp").unwrap();
        assert!(stamped.ends_with("inner-chaos-test-stamp.jsonl"));
        assert_eq!(std::fs::read_to_string(&stamped).unwrap(), content);
    }
}
