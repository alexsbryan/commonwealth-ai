// SPDX-License-Identifier: AGPL-3.0-or-later
//! `capability_findings` — point-of-query against the capability-reconciliation
//! findings, sibling to [`super::drift_findings`].
//!
//! `capability_posture` answers "is the artifact current?"; this answers "what
//! does the reconcile say about THIS capability / kind?" — without re-running
//! the pipeline. Reads `~/.svrnmesh/capabilities/<corpus>/capability_findings.json`
//! (the `FindingSet` the reconcile writes), filters by kind
//! (`drifted` / `undocumented` / `corroborated` / `any`) and an optional query
//! substring against the capability label + evidence, and returns the matches
//! sorted drifted → undocumented → corroborated so the high-value findings rise
//! first.

use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use super::capability_posture::resolve_corpus_dir;

/// One finding from the reconcile artifact (permissive subset — extra fields
/// ignored so the writer can evolve without breaking this reader).
#[derive(Debug, Clone, Deserialize)]
struct RawFinding {
    kind: String,
    label: String,
    #[serde(default)]
    n_entries: usize,
    #[serde(default)]
    n_core: usize,
    #[serde(default)]
    evidence: String,
    #[serde(default)]
    docs: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawFindingSet {
    #[serde(default)]
    corpus_id: String,
    #[serde(default)]
    findings: Vec<RawFinding>,
}

/// Display order: the dangerous, valuable findings first.
fn kind_rank(k: &str) -> u8 {
    match k {
        "drifted" => 0,
        "undocumented" => 1,
        "corroborated" => 2,
        _ => 3,
    }
}

fn capabilities_root() -> PathBuf {
    sovereign_contracts::rebrand::svrnmesh_root().join("capabilities")
}

pub struct CapabilityFindingsTool {
    root: PathBuf,
}

impl CapabilityFindingsTool {
    pub fn new() -> Self {
        Self {
            root: capabilities_root(),
        }
    }
    /// Test seam: override the capabilities root.
    pub fn with_root(mut self, root: PathBuf) -> Self {
        self.root = root;
        self
    }
}

impl Default for CapabilityFindingsTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for CapabilityFindingsTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "capability_findings".to_string(),
            name: "Capability Findings".to_string(),
            description: "Query the capability-reconciliation findings without re-running the \
                 pipeline. Sibling to `capability_posture` (freshness gate) and \
                 `drift_findings`. Filter by `kind` (`drifted` — docs contradict the \
                 code; `undocumented` — code does it, no doc describes it; \
                 `corroborated` — docs and code agree; `any`) and/or a `query` \
                 substring matched against the capability label + evidence. Returns \
                 matches sorted drifted → undocumented → corroborated. Reads \
                 `~/.svrnmesh/capabilities/<corpus>/capability_findings.json`; returns \
                 `never_run` if no artifact exists — check `capability_posture` for \
                 freshness before acting on results."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "enum": ["drifted", "undocumented", "corroborated", "any"],
                        "description": "Filter by finding kind. Default `any`."
                    },
                    "query": {
                        "type": "string",
                        "description": "Optional substring matched against the capability label + evidence."
                    },
                    "corpus": {
                        "type": "string",
                        "description": "Corpus id (subdir under ~/.svrnmesh/capabilities). Defaults to the only corpus when unambiguous."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max findings to return (default 30), sorted drifted→undocumented→corroborated."
                    }
                },
                "required": []
            }),
            examples: vec![
                ToolExample {
                    situation: "List the capabilities the architecture docs don't describe.".into(),
                    call: json!({ "kind": "undocumented" }),
                },
                ToolExample {
                    situation: "Does the reconcile say anything about corpus_search?".into(),
                    call: json!({ "query": "corpus_search", "kind": "any" }),
                },
            ],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Instant,
            scope: Scope::Session,
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "status": { "type": "string", "enum": ["ok", "never_run", "no_matches"] },
                    "corpus_id": { "type": "string" },
                    "report_path": { "type": ["string", "null"] },
                    "match_count": { "type": "integer" },
                    "findings": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "kind": { "type": "string" },
                                "label": { "type": "string" },
                                "entries": { "type": "integer" },
                                "core_fns": { "type": "integer" },
                                "evidence": { "type": "string" },
                                "docs": { "type": ["string", "null"] }
                            }
                        }
                    }
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
                return Ok(StepOutput::Json(json!({
                    "status": "never_run",
                    "match_count": 0,
                    "findings": [],
                    "hint": e,
                })));
            }
        };
        let json_path = caps_dir.join("capability_findings.json");
        let raw = match std::fs::read_to_string(&json_path) {
            Ok(s) => s,
            Err(_) => {
                return Ok(StepOutput::Json(json!({
                    "status": "never_run",
                    "report_path": null,
                    "match_count": 0,
                    "findings": [],
                    "hint": format!(
                        "no artifact at {} — run `sovereign enrich capability-reconcile`",
                        json_path.display()
                    ),
                })));
            }
        };
        let set: RawFindingSet = serde_json::from_str(&raw).map_err(|e| {
            Error::InvalidInput(format!(
                "capability_findings.json at {} is not valid JSON: {e}",
                json_path.display()
            ))
        })?;

        let kind_filter = params.get("kind").and_then(|v| v.as_str()).unwrap_or("any");
        let query = params
            .get("query")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_lowercase());
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(30);

        let mut hits: Vec<&RawFinding> = set
            .findings
            .iter()
            .filter(|f| kind_filter == "any" || f.kind == kind_filter)
            .filter(|f| match &query {
                Some(q) => {
                    f.label.to_lowercase().contains(q.as_str())
                        || f.evidence.to_lowercase().contains(q.as_str())
                }
                None => true,
            })
            .collect();
        hits.sort_by(|a, b| {
            kind_rank(&a.kind)
                .cmp(&kind_rank(&b.kind))
                .then_with(|| b.n_core.cmp(&a.n_core))
        });

        let match_count = hits.len();
        let findings: Vec<serde_json::Value> = hits
            .into_iter()
            .take(limit)
            .map(|f| {
                json!({
                    "kind": f.kind,
                    "label": f.label,
                    "entries": f.n_entries,
                    "core_fns": f.n_core,
                    "evidence": f.evidence,
                    "docs": f.docs,
                })
            })
            .collect();
        let status = if match_count == 0 { "no_matches" } else { "ok" };

        Ok(StepOutput::Json(json!({
            "status": status,
            "corpus_id": set.corpus_id,
            "report_path": json_path.to_string_lossy(),
            "match_count": match_count,
            "findings": findings,
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

    fn seed(root: &std::path::Path) {
        let caps = root.join("c");
        std::fs::create_dir_all(&caps).unwrap();
        std::fs::write(
            caps.join("capability_findings.json"),
            json!({
                "corpus_id": "c",
                "corroborated": 1,
                "undocumented": 2,
                "drifted": 0,
                "findings": [
                    {"kind":"undocumented","label":"sovereign-tools/corpus_search","n_entries":1,"n_core":6,"evidence":"no doc describes it"},
                    {"kind":"undocumented","label":"sovereign-tools/vector_mean","n_entries":1,"n_core":3,"evidence":"no doc describes the vector mean job"},
                    {"kind":"corroborated","label":"sovereign-tools/parcel_analytics","n_entries":1,"n_core":9,"evidence":"docs reference compute_aggregates"}
                ]
            })
            .to_string(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn filters_by_kind_and_query() {
        let root = TempDir::new().unwrap();
        seed(root.path());
        let ctx = test_ctx();
        let tool = CapabilityFindingsTool::new().with_root(root.path().to_path_buf());

        // kind=undocumented → the two undocumented findings, drifted-first order.
        let out = tool
            .execute(&json!({"corpus":"c","kind":"undocumented"}), &ctx)
            .await
            .unwrap();
        let v = match out {
            StepOutput::Json(v) => v,
            _ => panic!("json"),
        };
        assert_eq!(v["status"], "ok");
        assert_eq!(v["match_count"], 2);
        // higher n_core (corpus_search=6) sorts before vector_mean=3
        assert_eq!(v["findings"][0]["label"], "sovereign-tools/corpus_search");

        // query narrows to a single capability across kinds.
        let out = tool
            .execute(&json!({"corpus":"c","query":"vector_mean"}), &ctx)
            .await
            .unwrap();
        let v = match out {
            StepOutput::Json(v) => v,
            _ => panic!("json"),
        };
        assert_eq!(v["match_count"], 1);
        assert_eq!(v["findings"][0]["label"], "sovereign-tools/vector_mean");
    }

    #[tokio::test]
    async fn never_run_without_artifact() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir_all(root.path().join("c")).unwrap();
        let ctx = test_ctx();
        let tool = CapabilityFindingsTool::new().with_root(root.path().to_path_buf());
        let out = tool.execute(&json!({"corpus":"c"}), &ctx).await.unwrap();
        let v = match out {
            StepOutput::Json(v) => v,
            _ => panic!("json"),
        };
        assert_eq!(v["status"], "never_run");
    }
}
