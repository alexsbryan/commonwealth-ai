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
//!   `~/.sovereign/drift/latest.md` is current.
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

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use sovereign_core::error::Result;
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

/// Default narrative docs the architectural drift detector tracks.
/// Resolved relative to the workspace root.
pub const DEFAULT_NARRATIVES: &[&str] = &[
    "sovereign/SYSTEM_OVERVIEW.md",
    "sovereign/ARCH_PRINCIPLES.md",
];

/// File name of the fingerprint sidecar. Lives alongside
/// `latest.md` / `latest.md.json` so all drift state co-locates.
pub const FINGERPRINT_FILE: &str = ".fingerprint";

/// Default markdown output of `sovereign drift detect`.
pub const DEFAULT_REPORT_NAME: &str = "latest.md";

/// On-disk shape of the fingerprint sidecar. Schema-versioned so
/// future readers can detect format drift cleanly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriftFingerprint {
    pub schema_version: u32,
    pub generated_at_unix: u64,
    /// Map from absolute narrative path → SHA-256 hex.
    pub narrative_hashes: std::collections::BTreeMap<String, String>,
    /// Where the markdown report landed.
    pub output_path: String,
}

impl DriftFingerprint {
    pub const SCHEMA_VERSION: u32 = 1;
}

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

/// Write the fingerprint sidecar. Called by `sovereign drift detect`
/// on successful render. Returns the path written.
pub fn write_fingerprint(
    drift_dir: &Path,
    narrative_paths: &[PathBuf],
    output_path: &Path,
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(drift_dir)?;
    let mut hashes = std::collections::BTreeMap::new();
    for path in narrative_paths {
        let h = hash_file(path)?;
        hashes.insert(path.to_string_lossy().into_owned(), h);
    }
    let generated_at_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let fp = DriftFingerprint {
        schema_version: DriftFingerprint::SCHEMA_VERSION,
        generated_at_unix,
        narrative_hashes: hashes,
        output_path: output_path.to_string_lossy().into_owned(),
    };
    let out = drift_dir.join(FINGERPRINT_FILE);
    let body = serde_json::to_string_pretty(&fp).map_err(std::io::Error::other)?;
    std::fs::write(&out, body)?;
    Ok(out)
}

/// SHA-256 of a file's bytes, hex-encoded.
///
/// Public because the drift orchestrator needs the SAME hash to decide
/// whether a cached narrative atlas was built from the document it is
/// about to be reported against. Two implementations of one key is the
/// §10.6 smell, and here it would be worse than untidy: the fingerprint
/// written at the end of a run and the staleness check made at the start
/// must agree on what "this document changed" means, or the report can
/// assert it analysed a document it skipped.
pub fn hash_file(path: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Ok(format!("{:x}", h.finalize()))
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

#[async_trait]
impl Tool for DriftPostureTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "drift_posture".to_string(),
            name: "Drift Posture".to_string(),
            description:
                "Return the freshness state of the architectural-drift report \
                 (`sovereign drift detect` output) without re-running the LLM \
                 pipeline. Sibling to `lint_status`: cheap, idempotent, no \
                 cargo lock contention. Use to decide whether the drift digest \
                 you're about to cite is current against the narrative docs \
                 (SYSTEM_OVERVIEW.md + ARCH_PRINCIPLES.md by default). \
                 Status: `fresh` (every narrative hash matches the recorded \
                 fingerprint), `stale` (one or more narratives edited since \
                 last run — re-run `sovereign drift detect`), `partial` \
                 (fingerprint missing a requested narrative — new doc added \
                 since last run), `never_run` (no fingerprint or report \
                 exists yet). When fresh, the response carries the Act-on \
                 count and top-3 critical findings extracted from the report's \
                 JSON sidecar."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "narrative": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Override the narrative-doc set. Defaults to sovereign/SYSTEM_OVERVIEW.md + sovereign/ARCH_PRINCIPLES.md resolved relative to the workspace root."
                    }
                },
                "required": []
            }),
            examples: vec![
                ToolExample {
                    situation: "Decide whether to cite the drift report in the session-start brief, or warn that it's stale.".into(),
                    call: serde_json::json!({}),
                },
            ],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Instant,
            scope: Scope::Session,
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "status": { "type": "string", "enum": ["fresh","stale","partial","never_run"] },
                    "last_run_at_unix": { "type": ["integer","null"] },
                    "age_seconds": { "type": ["integer","null"] },
                    "act_on_count": { "type": ["integer","null"] },
                    "top_critical": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "doc": { "type": "string" },
                                "section": { "type": ["string","null"] },
                                "claim": { "type": "string" }
                            }
                        }
                    },
                    "narrative_paths": { "type": "array", "items": { "type": "string" } },
                    "stale_paths": { "type": "array", "items": { "type": "string" } },
                    "output_path": { "type": ["string","null"] }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let narrative_paths = resolve_narratives(params, self.workspace_root.as_deref());
        let posture = compute_posture(&self.drift_dir, &narrative_paths);
        Ok(StepOutput::Json(
            serde_json::to_value(&posture).unwrap_or(json!({})),
        ))
    }

    async fn signal(&self) -> Option<String> {
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
