// SPDX-License-Identifier: AGPL-3.0-or-later
//! `drift_posture` — read the architectural-drift report's freshness
//! state without re-running the LLM pipeline.
//!
//! Sibling to `lint_status` (cargo) and `test_status` (cargo test):
//! same freshness-gate pattern. Replaces the launchd-cron approach
//! the audit pass first proposed — instead of running drift detect
//! on a schedule, we lazily check whether the report is current
//! against the narrative docs and surface staleness through the
//! session-start brief + pre-push gate.
//!
//! ## Status semantics
//!
//! - **`fresh`** — a fingerprint sidecar exists and every narrative
//!   doc's SHA-256 matches the recorded hash. The report at
//!   `~/.svrnmesh/drift/latest.md` is current.
//! - **`stale`** — at least one narrative doc has been edited since
//!   the last drift run. Re-run `sovereign drift detect`.
//! - **`partial`** — fingerprint exists but doesn't cover one of the
//!   requested narrative docs (a new doc was added after the last
//!   run). Re-run to widen coverage.
//! - **`never_run`** — no fingerprint or report exists. First run
//!   pending.
//!
//! ## Why hashes, not mtimes
//!
//! mtime flips on `touch`, `git checkout`, even `cp -p` in some
//! filesystems. The drift report is expensive (~25-30 min per
//! narrative); we don't want to invalidate it on a no-op mtime
//! change. SHA-256 of the file contents is the honest signal.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::json;

use sovereign_contracts::error::Result;
use sovereign_contracts::tool_manifest::DeclaredTool;
use sovereign_contracts::types::*;
use std::sync::Arc;

/// Default narrative docs the architectural drift detector tracks.
/// Resolved relative to the workspace root.
pub const DEFAULT_NARRATIVES: &[&str] = &[
    "sovereign/SYSTEM_OVERVIEW.md",
    "sovereign/ARCH_PRINCIPLES.md",
];

/// Default markdown output of `sovereign drift detect`.
pub const DEFAULT_REPORT_NAME: &str = "latest.md";

// shim: the fingerprint codec (`FINGERPRINT_FILE`, `DriftFingerprint`,
// `write_fingerprint`, `hash_file`) moved to `sovereign-contracts`
// (`sovereign_contracts::drift_fingerprint`); re-exported here so the
// historical `sovereign_code::drift_posture::*` / `sovereign_code::*` importers
// are unchanged.
pub use sovereign_contracts::drift_fingerprint::{
    hash_file, write_fingerprint, DriftFingerprint, FINGERPRINT_FILE,
};

/// Computed freshness state. Returned by [`compute_posture`] and
/// rendered by the MCP tool / the brief / the pre-push hook.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriftPosture {
    pub status: PostureStatus,
    pub last_run_at_unix: Option<u64>,
    pub age_seconds: Option<u64>,
    pub act_on_count: Option<usize>,
    pub top_critical: Vec<TopCritical>,
    pub narrative_paths: Vec<PathBuf>,
    /// Narrative paths whose content hash no longer matches the
    /// fingerprint. Empty when status is `fresh` or `never_run`.
    pub stale_paths: Vec<PathBuf>,
    pub output_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PostureStatus {
    Fresh,
    Stale,
    Partial,
    NeverRun,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopCritical {
    pub doc: String,
    pub section: Option<String>,
    pub claim: String,
}

/// Compute the drift posture. Cheap — reads two small files and
/// SHA-256s the narrative docs (which are small markdown files).
/// No network, no LLM.
pub fn compute_posture(drift_dir: &Path, narrative_paths: &[PathBuf]) -> DriftPosture {
    let fingerprint_path = drift_dir.join(FINGERPRINT_FILE);
    let fingerprint: Option<DriftFingerprint> = std::fs::read_to_string(&fingerprint_path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok());

    // Default report path: whatever the fingerprint says, or
    // `<drift_dir>/latest.md` as a fallback.
    let output_path = fingerprint
        .as_ref()
        .map(|f| PathBuf::from(&f.output_path))
        .or_else(|| {
            let p = drift_dir.join(DEFAULT_REPORT_NAME);
            p.exists().then_some(p)
        });

    let Some(fp) = fingerprint else {
        return DriftPosture {
            status: PostureStatus::NeverRun,
            last_run_at_unix: None,
            age_seconds: None,
            act_on_count: None,
            top_critical: Vec::new(),
            narrative_paths: narrative_paths.to_vec(),
            stale_paths: Vec::new(),
            output_path,
        };
    };

    let age_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs().saturating_sub(fp.generated_at_unix));

    // Compare current narrative hashes against the recorded set.
    let mut stale_paths: Vec<PathBuf> = Vec::new();
    let mut missing_in_fingerprint = false;
    for path in narrative_paths {
        let key = path.to_string_lossy().to_string();
        let current = hash_file(path).unwrap_or_default();
        match fp.narrative_hashes.get(&key) {
            Some(recorded) if recorded == &current && !current.is_empty() => {
                // Match — this path is fresh.
            }
            Some(_) => stale_paths.push(path.clone()),
            None => {
                missing_in_fingerprint = true;
            }
        }
    }

    let status = if !stale_paths.is_empty() {
        PostureStatus::Stale
    } else if missing_in_fingerprint {
        PostureStatus::Partial
    } else {
        PostureStatus::Fresh
    };

    // Pull Act-on count and top critical from the JSON sidecar that
    // the drift report renderer writes alongside the markdown. If
    // the sidecar is missing or malformed, fall back gracefully.
    let (act_on_count, top_critical) = output_path
        .as_ref()
        .and_then(|p| {
            let json_path = sidecar_for(p);
            std::fs::read_to_string(json_path).ok()
        })
        .and_then(|raw| read_act_on(&raw))
        .unwrap_or((None, Vec::new()));

    DriftPosture {
        status,
        last_run_at_unix: Some(fp.generated_at_unix),
        age_seconds,
        act_on_count,
        top_critical,
        narrative_paths: narrative_paths.to_vec(),
        stale_paths,
        output_path,
    }
}

fn sidecar_for(md: &Path) -> PathBuf {
    let mut p = md.to_path_buf();
    let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
    if ext == "md" {
        p.set_extension("md.json");
    } else {
        // Whatever the path is, append `.json`.
        let mut s = p.into_os_string();
        s.push(".json");
        p = PathBuf::from(s);
    }
    p
}

/// Parse the drift report JSON sidecar's Act-on section. Returns
/// (count, top-3 critical findings). Best-effort: the orchestrator's
/// renderer is the source of truth for shape and may evolve; we keep
/// this lenient.
fn read_act_on(raw: &str) -> Option<(Option<usize>, Vec<TopCritical>)> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    // Common shapes the renderer has used:
    //   { "act_on": [ { doc, section, claim, severity }, ... ] }
    //   { "findings": [ { ..., "severity": "critical" } ] }
    let mut count: Option<usize> = None;
    let mut top: Vec<TopCritical> = Vec::new();
    if let Some(arr) = v.get("act_on").and_then(|x| x.as_array()) {
        count = Some(arr.len());
        for item in arr.iter().take(3) {
            top.push(item_to_top_critical(item));
        }
    } else if let Some(arr) = v.get("findings").and_then(|x| x.as_array()) {
        let critical: Vec<&serde_json::Value> = arr
            .iter()
            .filter(|x| {
                x.get("severity")
                    .and_then(|s| s.as_str())
                    .map(|s| s.eq_ignore_ascii_case("critical"))
                    .unwrap_or(false)
            })
            .collect();
        count = Some(critical.len());
        for item in critical.iter().take(3) {
            top.push(item_to_top_critical(item));
        }
    }
    Some((count, top))
}

fn item_to_top_critical(v: &serde_json::Value) -> TopCritical {
    let s = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
            .unwrap_or_default()
    };
    let claim = if !s("claim").is_empty() {
        s("claim")
    } else if !s("description").is_empty() {
        s("description")
    } else {
        s("title")
    };
    TopCritical {
        doc: s("doc"),
        section: v
            .get("section")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
        claim,
    }
}

// ── MCP tool ────────────────────────────────────────────────

pub struct DriftPostureTool {
    workspace_root: Option<PathBuf>,
    drift_dir: PathBuf,
}

impl DriftPostureTool {
    pub fn new() -> Self {
        let drift_dir = sovereign_contracts::rebrand::drift_dir();
        Self {
            workspace_root: None,
            drift_dir,
        }
    }

    pub fn with_workspace_root(mut self, root: PathBuf) -> Self {
        self.workspace_root = Some(root);
        self
    }

    pub fn with_drift_dir(mut self, dir: PathBuf) -> Self {
        self.drift_dir = dir;
        self
    }
}

impl Default for DriftPostureTool {
    fn default() -> Self {
        Self::new()
    }
}

impl DriftPostureTool {
    /// Bind this tool's state to its `drift_posture` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        let run_state = Arc::clone(&state);
        sovereign_contracts::tool_manifest::declared("drift_posture", move |params, ctx| {
            let state = Arc::clone(&run_state);
            async move { state.run(&params, &ctx).await }
        })
        .with_signal({
            let state = Arc::clone(&state);
            Arc::new(move || {
                let state = Arc::clone(&state);
                Box::pin(async move { state.signal_now().await })
                    as std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send>>
            })
        })
    }

    /// The executable half of `drift_posture`.
    async fn run(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let narrative_paths = resolve_narratives(params, self.workspace_root.as_deref());
        let posture = compute_posture(&self.drift_dir, &narrative_paths);
        Ok(StepOutput::Json(
            serde_json::to_value(&posture).unwrap_or(json!({})),
        ))
    }

    async fn signal_now(&self) -> Option<String> {
        let narrative_paths = resolve_narratives(&json!({}), self.workspace_root.as_deref());
        let posture = compute_posture(&self.drift_dir, &narrative_paths);
        match posture.status {
            PostureStatus::Stale => Some(format!(
                "Drift report stale ({} narrative doc(s) changed since last run)",
                posture.stale_paths.len()
            )),
            PostureStatus::NeverRun => {
                Some("Drift report never run — `sovereign drift detect` to seed".into())
            }
            _ => None,
        }
    }
}

fn resolve_narratives(params: &serde_json::Value, workspace_root: Option<&Path>) -> Vec<PathBuf> {
    let explicit: Option<Vec<String>> =
        params
            .get("narrative")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            });
    let raw =
        explicit.unwrap_or_else(|| DEFAULT_NARRATIVES.iter().map(|s| s.to_string()).collect());
    raw.into_iter()
        .map(|p| canonicalize_or_join(&p, workspace_root))
        .collect()
}

fn canonicalize_or_join(p: &str, workspace_root: Option<&Path>) -> PathBuf {
    let raw = PathBuf::from(p);
    let joined = if raw.is_absolute() {
        raw
    } else if let Some(root) = workspace_root {
        root.join(&raw)
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(&raw))
            .unwrap_or(raw)
    };
    std::fs::canonicalize(&joined).unwrap_or(joined)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp_dir(label: &str) -> PathBuf {
        let p =
            std::env::temp_dir().join(format!("drift_posture_test_{label}_{}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn never_run_when_no_fingerprint_present() {
        let dir = tmp_dir("never");
        let narrative = dir.join("doc.md");
        std::fs::write(&narrative, b"hello").unwrap();
        let posture = compute_posture(&dir, std::slice::from_ref(&narrative));
        assert_eq!(posture.status, PostureStatus::NeverRun);
        assert!(posture.last_run_at_unix.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn fresh_when_hashes_match() {
        let dir = tmp_dir("fresh");
        let narrative = dir.join("doc.md");
        std::fs::write(&narrative, b"first version").unwrap();
        let output = dir.join("latest.md");
        std::fs::write(&output, b"# report").unwrap();
        write_fingerprint(&dir, std::slice::from_ref(&narrative), &output).unwrap();
        let posture = compute_posture(&dir, std::slice::from_ref(&narrative));
        assert_eq!(posture.status, PostureStatus::Fresh);
        assert!(posture.stale_paths.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn stale_when_narrative_changed() {
        let dir = tmp_dir("stale");
        let narrative = dir.join("doc.md");
        std::fs::write(&narrative, b"first version").unwrap();
        let output = dir.join("latest.md");
        std::fs::write(&output, b"# report").unwrap();
        write_fingerprint(&dir, std::slice::from_ref(&narrative), &output).unwrap();
        // Modify the narrative after the fingerprint is written.
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&narrative)
            .unwrap();
        f.write_all(b"changed version").unwrap();
        drop(f);
        let posture = compute_posture(&dir, std::slice::from_ref(&narrative));
        assert_eq!(posture.status, PostureStatus::Stale);
        assert_eq!(posture.stale_paths.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn partial_when_new_narrative_added() {
        let dir = tmp_dir("partial");
        let a = dir.join("a.md");
        let b = dir.join("b.md");
        std::fs::write(&a, b"alpha").unwrap();
        std::fs::write(&b, b"beta").unwrap();
        let output = dir.join("latest.md");
        std::fs::write(&output, b"# report").unwrap();
        // Fingerprint covers only a.md.
        write_fingerprint(&dir, std::slice::from_ref(&a), &output).unwrap();
        // Query asks about both — b.md is uncovered.
        let posture = compute_posture(&dir, &[a.clone(), b.clone()]);
        assert_eq!(posture.status, PostureStatus::Partial);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sidecar_for_md_appends_json() {
        let p = sidecar_for(&PathBuf::from("/tmp/latest.md"));
        assert_eq!(p, PathBuf::from("/tmp/latest.md.json"));
    }
}
