// SPDX-License-Identifier: AGPL-3.0-or-later
//! Build a per-run manifest from the daemon's persisted state.
//!
//! Inputs (read-only):
//! - `~/.svrnmesh/features.db` — atos_runs, atos_tool_events
//! - `~/.svrnmesh/notes.db` — decisions, invariants, deviations, reflections
//! - the experiment repo on disk — CHARTER + spec SHAs, git head
//! - the running daemon — model list (best-effort; tolerated absent)
//!
//! Output: a single `manifest.json` ready for the grader to read.

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub run: RunInfo,
    pub experiment_repo: ExperimentRepo,
    pub models: Vec<ModelInfo>,
    pub opencode_version: Option<String>,
    pub tool_calls: Vec<ToolCallEvent>,
    pub notes: NotesByKind,
    pub generated_at_unix: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunInfo {
    pub run_id: String,
    pub feature_id: String,
    pub milestone_id: String,
    pub driver: String,
    pub session_id: Option<String>,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub exit_code: Option<i64>,
    pub stop_passed: Option<bool>,
    pub mode: String,
    pub stop_stdout: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentRepo {
    pub root: PathBuf,
    pub charter_path: Option<PathBuf>,
    pub charter_sha256: Option<String>,
    pub spec_shas: Vec<SpecSha>,
    pub git_head: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecSha {
    pub feature_id: String,
    pub spec_path: PathBuf,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub owned_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallEvent {
    pub event_id: String,
    pub call_id: String,
    pub tool_name: String,
    pub phase: String,
    pub args_json: Option<String>,
    pub outcome: Option<String>,
    pub duration_ms: Option<i64>,
    pub fired_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NotesByKind {
    pub decisions: Vec<NoteRow>,
    pub invariants: Vec<NoteRow>,
    pub uncertainties: Vec<NoteRow>,
    pub attempts: Vec<NoteRow>,
    pub deviations: Vec<NoteRow>,
    pub reflections: Vec<NoteRow>,
    pub redteam_findings: Vec<NoteRow>,
    pub other: Vec<NoteRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteRow {
    pub id: String,
    pub kind: String,
    pub content: String,
    pub created_at: i64,
    pub tool_name: Option<String>,
    pub source: Option<String>,
    pub feature_id: Option<String>,
    pub scope: Option<String>,
}

pub struct BuildOpts<'a> {
    pub features_db: &'a Path,
    pub notes_db: &'a Path,
    pub run_id: &'a str,
    pub experiment_repo: Option<&'a Path>,
    pub daemon_url: Option<&'a str>,
}

pub fn build(opts: BuildOpts) -> Result<Manifest> {
    let run = load_run(opts.features_db, opts.run_id)?;
    let session_id = run.session_id.clone();

    let tool_calls = load_tool_events(opts.features_db, opts.run_id)?;

    let notes = match session_id.as_deref() {
        Some(sid) => load_notes(opts.notes_db, sid)?,
        None => NotesByKind::default(),
    };

    let experiment_repo = match opts.experiment_repo {
        Some(p) => describe_experiment_repo(p)?,
        None => ExperimentRepo {
            root: PathBuf::new(),
            charter_path: None,
            charter_sha256: None,
            spec_shas: vec![],
            git_head: None,
        },
    };

    let models = match opts.daemon_url {
        Some(url) => fetch_models(url).unwrap_or_default(),
        None => vec![],
    };

    let opencode_version = detect_opencode_version();

    Ok(Manifest {
        schema_version: 1,
        run,
        experiment_repo,
        models,
        opencode_version,
        tool_calls,
        notes,
        generated_at_unix: chrono::Utc::now().timestamp(),
    })
}

fn load_run(features_db: &Path, run_id: &str) -> Result<RunInfo> {
    if !features_db.exists() {
        bail!("features.db not found at {}", features_db.display());
    }
    let conn = Connection::open_with_flags(
        features_db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening {}", features_db.display()))?;

    let row = conn.query_row(
        "SELECT id, feature_id, milestone_id, driver, session_id,
                started_at, ended_at, exit_code, stop_passed, mode, stop_stdout
         FROM atos_runs WHERE id = ?1",
        params![run_id],
        |r| {
            let stop_passed_int: Option<i64> = r.get(8)?;
            Ok(RunInfo {
                run_id: r.get(0)?,
                feature_id: r.get(1)?,
                milestone_id: r.get(2)?,
                driver: r.get(3)?,
                session_id: r.get(4)?,
                started_at: r.get(5)?,
                ended_at: r.get(6)?,
                exit_code: r.get(7)?,
                stop_passed: stop_passed_int.map(|n| n != 0),
                mode: r.get(9)?,
                stop_stdout: r.get(10)?,
            })
        },
    );
    match row {
        Ok(run) => Ok(run),
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            bail!("run_id `{run_id}` not found")
        }
        Err(e) => Err(anyhow::Error::new(e).context("loading run")),
    }
}

fn load_tool_events(features_db: &Path, run_id: &str) -> Result<Vec<ToolCallEvent>> {
    let conn = Connection::open_with_flags(
        features_db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .context("opening features.db (events)")?;
    let mut stmt = conn
        .prepare(
            "SELECT id, call_id, tool_name, phase, args_json, outcome, duration_ms, fired_at
             FROM atos_tool_events WHERE run_id = ?1
             ORDER BY fired_at ASC, id ASC",
        )
        .context("preparing event query")?;
    let mapped = stmt
        .query_map(params![run_id], |r| {
            Ok(ToolCallEvent {
                event_id: r.get(0)?,
                call_id: r.get(1)?,
                tool_name: r.get(2)?,
                phase: r.get(3)?,
                args_json: r.get(4)?,
                outcome: r.get(5)?,
                duration_ms: r.get(6)?,
                fired_at: r.get(7)?,
            })
        })
        .context("querying events")?;
    let mut out = Vec::new();
    for row in mapped {
        out.push(row.context("reading event row")?);
    }
    Ok(out)
}

fn load_notes(notes_db: &Path, session_id: &str) -> Result<NotesByKind> {
    if !notes_db.exists() {
        return Ok(NotesByKind::default());
    }
    let conn = Connection::open_with_flags(
        notes_db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .context("opening notes.db")?;

    let mut stmt = conn
        .prepare(
            "SELECT id, kind, content, created_at, tool_name, source, feature_id, scope
             FROM notes
             WHERE session_id = ?1 AND retired_at IS NULL
             ORDER BY created_at ASC",
        )
        .context("preparing notes query")?;
    let mapped = stmt
        .query_map(params![session_id], |r| {
            Ok(NoteRow {
                id: r.get(0)?,
                kind: r.get(1)?,
                content: r.get(2)?,
                created_at: r.get(3)?,
                tool_name: r.get(4)?,
                source: r.get(5)?,
                feature_id: r.get(6)?,
                scope: r.get(7)?,
            })
        })
        .context("querying notes")?;

    let mut buckets = NotesByKind::default();
    for row in mapped {
        let n = row.context("reading note row")?;
        match n.kind.as_str() {
            "decision" => buckets.decisions.push(n),
            "invariant" => buckets.invariants.push(n),
            "uncertainty" => buckets.uncertainties.push(n),
            "attempt" => buckets.attempts.push(n),
            "deviation" => buckets.deviations.push(n),
            "reflection" => buckets.reflections.push(n),
            "redteam_finding" => buckets.redteam_findings.push(n),
            _ => buckets.other.push(n),
        }
    }
    Ok(buckets)
}

fn describe_experiment_repo(root: &Path) -> Result<ExperimentRepo> {
    if !root.exists() {
        bail!("experiment repo not found at {}", root.display());
    }

    let charter_path = root.join("CHARTER.md");
    let (charter_path_opt, charter_sha) = if charter_path.exists() {
        let sha = sha256_file(&charter_path)?;
        (Some(charter_path.clone()), Some(sha))
    } else {
        (None, None)
    };

    let mut spec_shas = Vec::new();
    let features_dir = root.join(".sovereign").join("features");
    if features_dir.exists() {
        for entry in std::fs::read_dir(&features_dir).context("reading features/")? {
            let entry = entry.context("features/ entry")?;
            if !entry.file_type().context("file_type")?.is_dir() {
                continue;
            }
            let spec = entry.path().join("spec.md");
            if spec.exists() {
                let sha = sha256_file(&spec)?;
                spec_shas.push(SpecSha {
                    feature_id: entry.file_name().to_string_lossy().into_owned(),
                    spec_path: spec,
                    sha256: sha,
                });
            }
        }
        spec_shas.sort_by(|a, b| a.feature_id.cmp(&b.feature_id));
    }

    let git_head = git_head(root);

    Ok(ExperimentRepo {
        root: root.to_path_buf(),
        charter_path: charter_path_opt,
        charter_sha256: charter_sha,
        spec_shas,
        git_head,
    })
}

fn sha256_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Ok(hex::encode(h.finalize()))
}

fn git_head(root: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["-C"])
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn fetch_models(daemon_url: &str) -> Result<Vec<ModelInfo>> {
    let url = format!("{}/v1/models", daemon_url.trim_end_matches('/'));
    let resp: serde_json::Value = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .context("building reqwest client")?
        .get(&url)
        .send()
        .context("GET /v1/models")?
        .json()
        .context("parsing /v1/models JSON")?;
    let mut out = Vec::new();
    let data = match resp.get("data").and_then(serde_json::Value::as_array) {
        Some(d) => d,
        None => return Ok(out),
    };
    for m in data {
        let id = m
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        let owned_by = m
            .get("owned_by")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        if !id.is_empty() {
            out.push(ModelInfo { id, owned_by });
        }
    }
    Ok(out)
}

fn detect_opencode_version() -> Option<String> {
    let out = std::process::Command::new("opencode")
        .arg("--version")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn sha256_file_works() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("a.txt");
        std::fs::write(&path, b"hello").unwrap();
        let sha = sha256_file(&path).unwrap();
        assert_eq!(
            sha,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn describe_experiment_repo_handles_missing_features_dir() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("CHARTER.md"), b"hello").unwrap();
        let er = describe_experiment_repo(tmp.path()).unwrap();
        assert!(er.charter_sha256.is_some());
        assert_eq!(er.spec_shas.len(), 0);
    }

    #[test]
    fn describe_experiment_repo_collects_spec_shas() {
        let tmp = TempDir::new().unwrap();
        let f = tmp.path().join(".sovereign/features");
        std::fs::create_dir_all(f.join("alpha")).unwrap();
        std::fs::create_dir_all(f.join("beta")).unwrap();
        std::fs::write(f.join("alpha/spec.md"), b"alpha").unwrap();
        std::fs::write(f.join("beta/spec.md"), b"beta").unwrap();
        let er = describe_experiment_repo(tmp.path()).unwrap();
        assert_eq!(er.spec_shas.len(), 2);
        assert_eq!(er.spec_shas[0].feature_id, "alpha");
        assert_eq!(er.spec_shas[1].feature_id, "beta");
    }
}
