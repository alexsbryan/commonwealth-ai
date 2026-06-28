// SPDX-License-Identifier: AGPL-3.0-or-later
//! `capability_posture` — freshness gate for the capability-reconciliation
//! artifact, sibling to [`super::drift_posture`].
//!
//! Reports whether `capability_findings` is current against the architecture
//! docs (the same SHA-256-of-narratives signal drift uses) plus the
//! corroborated / undocumented / drifted counts. Cheap: a few file reads +
//! hashing the small narrative docs. No LLM, no pipeline re-run.
//!
//! The reconcile verb (`sovereign enrich capability-reconcile`) writes the
//! `.fingerprint` sidecar via the shared `write_fingerprint`, so this tool is
//! a pure reader. It reuses [`compute_posture`] for the freshness verdict and
//! reads the JSON artifact only for the headline tallies.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use sovereign_core::error::Result;
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use super::drift_posture::{compute_posture, PostureStatus, DEFAULT_NARRATIVES};

/// Headline tallies pulled from the reconcile's JSON artifact — a permissive
/// subset of its `FindingSet` (extra fields ignored so the writer can evolve).
#[derive(Debug, Clone, Default, Deserialize)]
struct FindingCounts {
    #[serde(default)]
    corpus_id: String,
    #[serde(default)]
    corroborated: usize,
    #[serde(default)]
    undocumented: usize,
    #[serde(default)]
    drifted: usize,
}

fn status_str(s: PostureStatus) -> &'static str {
    match s {
        PostureStatus::Fresh => "fresh",
        PostureStatus::Stale => "stale",
        PostureStatus::Partial => "partial",
        PostureStatus::NeverRun => "never_run",
    }
}

/// `~/.sovereign/capabilities` — per-corpus artifacts live in subdirs.
fn capabilities_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".sovereign")
        .join("capabilities")
}

/// Resolve the corpus artifact dir from the `corpus` param, or fall back to the
/// single corpus subdir when unambiguous (the common one-repo case).
pub(crate) fn resolve_corpus_dir(
    root: &Path,
    params: &serde_json::Value,
) -> std::result::Result<PathBuf, String> {
    if let Some(c) = params
        .get("corpus")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        return Ok(root.join(c));
    }
    let mut subdirs: Vec<PathBuf> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    subdirs.sort();
    match subdirs.len() {
        0 => Err(format!(
            "no capability artifacts under {} — run `sovereign enrich capability-reconcile <corpus>`",
            root.display()
        )),
        1 => Ok(subdirs.remove(0)),
        _ => Err(format!(
            "{} corpora under {}; pass `corpus`",
            subdirs.len(),
            root.display()
        )),
    }
}

fn resolve_narratives(params: &serde_json::Value, workspace_root: Option<&Path>) -> Vec<PathBuf> {
    if let Some(arr) = params.get("narrative").and_then(|v| v.as_array()) {
        let v: Vec<PathBuf> = arr
            .iter()
            .filter_map(|v| v.as_str())
            .map(PathBuf::from)
            .collect();
        if !v.is_empty() {
            return v;
        }
    }
    let base = workspace_root.unwrap_or_else(|| Path::new("."));
    DEFAULT_NARRATIVES.iter().map(|r| base.join(r)).collect()
}

pub struct CapabilityPostureTool {
    root: PathBuf,
    workspace_root: Option<PathBuf>,
}

impl CapabilityPostureTool {
    pub fn new() -> Self {
        Self {
            root: capabilities_root(),
            workspace_root: None,
        }
    }
    pub fn with_workspace_root(mut self, root: PathBuf) -> Self {
        self.workspace_root = Some(root);
        self
    }
    /// Test seam: override the capabilities root.
    pub fn with_root(mut self, root: PathBuf) -> Self {
        self.root = root;
        self
    }
}

impl Default for CapabilityPostureTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for CapabilityPostureTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "capability_posture".to_string(),
            name: "Capability Posture".to_string(),
            description:
                "Return the freshness state of the capability-reconciliation artifact \
                 (`sovereign enrich capability-reconcile` output) without re-running the \
                 LLM pipeline. Sibling to `drift_posture`: cheap, idempotent, no model \
                 calls. Use to decide whether the corroborated/undocumented/drifted \
                 findings you're about to cite are current against the architecture docs \
                 (SYSTEM_OVERVIEW.md + ARCH_PRINCIPLES.md by default). Status: `fresh` \
                 (every narrative hash matches the recorded fingerprint), `stale` (a \
                 narrative was edited since the last reconcile — re-run it), `partial` \
                 (fingerprint missing a requested narrative), `never_run` (no artifact \
                 yet). When present, the response carries the \
                 corroborated/undocumented/drifted counts."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "corpus": {
                        "type": "string",
                        "description": "Corpus id (subdir under ~/.sovereign/capabilities). Defaults to the only corpus when unambiguous."
                    },
                    "narrative": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Override the narrative-doc set. Defaults to sovereign/SYSTEM_OVERVIEW.md + sovereign/ARCH_PRINCIPLES.md relative to the workspace root."
                    }
                },
                "required": []
            }),
            examples: vec![ToolExample {
                situation: "Decide whether to cite the capability findings or warn they're stale against the docs.".into(),
                call: json!({}),
            }],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Instant,
            scope: Scope::Session,
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "status": { "type": "string", "enum": ["fresh","stale","partial","never_run"] },
                    "corpus_id": { "type": "string" },
                    "last_run_at_unix": { "type": ["integer","null"] },
                    "age_seconds": { "type": ["integer","null"] },
                    "corroborated": { "type": ["integer","null"] },
                    "undocumented": { "type": ["integer","null"] },
                    "drifted": { "type": ["integer","null"] },
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
        let caps_dir = match resolve_corpus_dir(&self.root, params) {
            Ok(d) => d,
            Err(e) => {
                return Ok(StepOutput::Json(json!({ "status": "never_run", "hint": e })));
            }
        };
        let narrative_paths = resolve_narratives(params, self.workspace_root.as_deref());
        let posture = compute_posture(&caps_dir, &narrative_paths);
        let counts: FindingCounts = std::fs::read_to_string(caps_dir.join("capability_findings.json"))
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        Ok(StepOutput::Json(json!({
            "status": status_str(posture.status),
            "corpus_id": counts.corpus_id,
            "last_run_at_unix": posture.last_run_at_unix,
            "age_seconds": posture.age_seconds,
            "corroborated": counts.corroborated,
            "undocumented": counts.undocumented,
            "drifted": counts.drifted,
            "stale_paths": posture.stale_paths,
            "output_path": posture.output_path,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_ctx() -> ToolContext {
        ToolContext {
            conversation_id: Default::default(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
        }
    }

    #[test]
    fn resolves_single_corpus_and_errors_when_empty() {
        let root = TempDir::new().unwrap();
        // empty root → error
        assert!(resolve_corpus_dir(root.path(), &json!({})).is_err());
        // one subdir → resolves to it without a `corpus` param
        let only = root.path().join("commonwealth-ai");
        std::fs::create_dir_all(&only).unwrap();
        assert_eq!(resolve_corpus_dir(root.path(), &json!({})).unwrap(), only);
        // explicit corpus param wins
        assert_eq!(
            resolve_corpus_dir(root.path(), &json!({"corpus": "other"})).unwrap(),
            root.path().join("other")
        );
    }

    #[tokio::test]
    async fn never_run_when_no_artifact_then_reads_counts() {
        let root = TempDir::new().unwrap();
        let caps = root.path().join("c");
        std::fs::create_dir_all(&caps).unwrap();
        let ctx = test_ctx();
        let tool = CapabilityPostureTool::new().with_root(root.path().to_path_buf());

        // No fingerprint/findings yet → never_run with zero counts.
        let out = tool.execute(&json!({"corpus": "c"}), &ctx).await.unwrap();
        let v = match out {
            StepOutput::Json(v) => v,
            _ => panic!("expected json"),
        };
        assert_eq!(v["status"], "never_run");
        assert_eq!(v["corroborated"], 0);

        // With a findings artifact, the counts surface even before a fingerprint.
        std::fs::write(
            caps.join("capability_findings.json"),
            json!({"corpus_id":"c","corroborated":114,"undocumented":112,"drifted":0,"findings":[]}).to_string(),
        )
        .unwrap();
        let out = tool.execute(&json!({"corpus": "c"}), &ctx).await.unwrap();
        let v = match out {
            StepOutput::Json(v) => v,
            _ => panic!("expected json"),
        };
        assert_eq!(v["corroborated"], 114);
        assert_eq!(v["undocumented"], 112);
        assert_eq!(v["drifted"], 0);
        assert_eq!(v["corpus_id"], "c");
    }
}
