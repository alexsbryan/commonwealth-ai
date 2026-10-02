// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-run output files with monotonic ordinals.
//!
//! Every phase run writes a timestamped output file to
//! `<root>/runs/<phase-id>-<mode>-<NNN>.json` regardless of whether
//! the cache was updated. This gives the developer a history of
//! raw outputs they can `diff` and `promote` from without losing
//! older runs to the cache's latest-only shape.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::types::PipelinePhase;
use crate::error::{Error, Result};

/// Writer for run output files under `<root>/runs/`.
#[derive(Debug, Clone)]
pub struct RunOutputWriter {
    runs_dir: PathBuf,
}

impl RunOutputWriter {
    pub fn new(runs_dir: impl AsRef<Path>) -> Self {
        Self {
            runs_dir: runs_dir.as_ref().to_path_buf(),
        }
    }

    pub fn runs_dir(&self) -> &Path {
        &self.runs_dir
    }

    /// Scan `runs/` and return the next ordinal for a given (phase, mode)
    /// prefix. `mode` is a free-form label the CLI chooses, like
    /// `"subset"` or `"full"`. Ordinal starts at 1.
    pub fn next_ordinal(&self, phase: PipelinePhase, mode: &str) -> Result<u32> {
        if !self.runs_dir.exists() {
            return Ok(1);
        }
        let prefix = format!("{}-{}-", phase.id(), mode);
        let mut max_seen: u32 = 0;
        for entry in fs::read_dir(&self.runs_dir)? {
            let entry = entry?;
            let name = match entry.file_name().into_string() {
                Ok(n) => n,
                Err(_) => continue,
            };
            let Some(rest) = name.strip_prefix(&prefix) else {
                continue;
            };
            // rest = "<NNN>.json" or "<NNN>.something.json"
            let Some(dot) = rest.find('.') else { continue };
            let numeric = &rest[..dot];
            if let Ok(n) = numeric.parse::<u32>() {
                if n > max_seen {
                    max_seen = n;
                }
            }
        }
        Ok(max_seen + 1)
    }

    /// Write the run, returning the absolute path of the file written.
    pub fn write<T: Serialize>(
        &self,
        phase: PipelinePhase,
        mode: &str,
        value: &T,
    ) -> Result<PathBuf> {
        fs::create_dir_all(&self.runs_dir)?;
        let ordinal = self.next_ordinal(phase, mode)?;
        let file_name = format!("{}-{}-{:03}.json", phase.id(), mode, ordinal);
        let path = self.runs_dir.join(file_name);
        let json =
            serde_json::to_string_pretty(value).map_err(|e| Error::Serialization(e.to_string()))?;
        fs::write(&path, json)?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;
    use tempfile::tempdir;

    #[derive(Serialize)]
    struct Dummy {
        a: u32,
    }

    #[test]
    fn first_ordinal_is_one_on_empty_dir() {
        let dir = tempdir().unwrap();
        let w = RunOutputWriter::new(dir.path().join("runs"));
        let n = w.next_ordinal(PipelinePhase::Questions, "subset").unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn write_creates_dir_and_returns_path() {
        let dir = tempdir().unwrap();
        let w = RunOutputWriter::new(dir.path().join("runs"));
        let path = w
            .write(PipelinePhase::Questions, "subset", &Dummy { a: 1 })
            .unwrap();
        assert!(path.exists());
        assert!(path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("questions-subset-001"));
    }

    #[test]
    fn ordinal_increments_across_writes() {
        let dir = tempdir().unwrap();
        let w = RunOutputWriter::new(dir.path().join("runs"));
        for i in 1..=3 {
            let path = w
                .write(PipelinePhase::Questions, "subset", &Dummy { a: i })
                .unwrap();
            let expected = format!("questions-subset-{:03}.json", i);
            assert_eq!(path.file_name().unwrap().to_string_lossy(), expected);
        }
    }

    #[test]
    fn ordinals_are_per_mode() {
        let dir = tempdir().unwrap();
        let w = RunOutputWriter::new(dir.path().join("runs"));
        w.write(PipelinePhase::Questions, "subset", &Dummy { a: 1 })
            .unwrap();
        w.write(PipelinePhase::Questions, "subset", &Dummy { a: 2 })
            .unwrap();
        let full_path = w
            .write(PipelinePhase::Questions, "full", &Dummy { a: 3 })
            .unwrap();
        assert!(full_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("questions-full-001"));
    }

    #[test]
    fn ordinals_are_per_phase() {
        let dir = tempdir().unwrap();
        let w = RunOutputWriter::new(dir.path().join("runs"));
        w.write(PipelinePhase::Questions, "subset", &Dummy { a: 1 })
            .unwrap();
        let p5 = w
            .write(PipelinePhase::Positions, "subset", &Dummy { a: 2 })
            .unwrap();
        assert!(p5
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("positions-subset-001"));
    }

    #[test]
    fn non_matching_files_do_not_affect_ordinal() {
        let dir = tempdir().unwrap();
        let runs = dir.path().join("runs");
        fs::create_dir_all(&runs).unwrap();
        fs::write(runs.join("random.json"), b"{}").unwrap();
        fs::write(runs.join("questions-other-999.json"), b"{}").unwrap();
        let w = RunOutputWriter::new(&runs);
        let n = w.next_ordinal(PipelinePhase::Questions, "subset").unwrap();
        assert_eq!(n, 1);
    }
}
