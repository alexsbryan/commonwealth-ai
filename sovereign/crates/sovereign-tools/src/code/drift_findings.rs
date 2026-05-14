//! `drift_findings` — point-of-edit query against the latest drift report.
//!
//! Sibling to `drift_posture`: `posture` answers "is the report
//! current?", `findings` answers "what does the report say about
//! THIS symbol or THIS file?" The agent's pre-edit workflow
//! already runs `symbols(name)`, `callers(name)`, `blast(name)`;
//! `drift_findings(name)` is the matching narrative-side check —
//! "is this thing referenced by any normative claim in the
//! architecture docs?"
//!
//! Without this tool, the only way to surface a relevant finding
//! is to open the 200-line markdown report and scan it manually.
//! That's the wrong loop for an agent. This tool reads the JSON
//! sidecar that `sovereign drift detect` already produces and
//! filters in <10 ms.
//!
//! ## Data source
//!
//! Reads `~/.sovereign/drift/latest.md.json` by default — the
//! canonical mirror written by every `sovereign drift detect`
//! run since the orchestrator's mirror step landed. The tool
//! does NOT trigger a re-run; if the report is stale callers
//! should consult `drift_posture` first.
//!
//! ## Matching semantics
//!
//! Three match modes, controlled by the `kind` parameter:
//!
//! - `anchor` (default): substring-match the query against each
//!   finding's `narrative.canonical_name`. For Claim atoms this
//!   is `Claim.anchor` (the code symbol the LLM extracted as the
//!   anchor for the claim), or the prose first sentence as a
//!   fallback when no anchor was extracted. Anchors are
//!   case-insensitive against the query.
//!
//! - `path`: substring-match the query against
//!   `narrative.chunk_id`. Less precise than anchor matching but
//!   surfaces findings tied to a particular section of a
//!   narrative document.
//!
//! - `any` (broad): union of anchor + path + a substring match
//!   against the `quotable` excerpt. Useful when you're not sure
//!   how the model anchored the claim ("find anything that
//!   mentions LocalOnly even if the anchor is something else").
//!
//! ## Output shape
//!
//! Returns up to `limit` findings (default 20), sorted by
//! severity (Critical → Likely → Note). Each entry carries:
//! anchor (canonical_name), severity, headline, narrative
//! source (atlas_id + chunk_id), quotable excerpt, action,
//! atom_id. The atom_id lets a follow-up tool ("mark resolved")
//! reference the finding precisely without re-running drift.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

/// On-disk shape of one finding inside the drift report's JSON
/// sidecar. Mirrors `Finding` in `atlas_drift_report.rs`. We
/// deserialise permissively (extra fields ignored) so the
/// renderer can evolve without breaking this tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RawFinding {
    severity: String,
    kind: String,
    headline: String,
    #[serde(default)]
    narrative: Option<NarrativeRef>,
    #[serde(default)]
    action: String,
    #[serde(default)]
    rank_hint: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NarrativeRef {
    #[serde(default)]
    atlas_id: String,
    #[serde(default)]
    atom_id: String,
    #[serde(default)]
    atom_type: String,
    #[serde(default)]
    canonical_name: String,
    #[serde(default)]
    chunk_id: Option<String>,
    #[serde(default)]
    quotable: Option<String>,
}

/// Top-level shape of `latest.md.json`. Buckets line up with
/// the renderer's severity tiers.
#[derive(Debug, Clone, Deserialize)]
struct RawReport {
    #[serde(default)]
    critical: Vec<RawFinding>,
    #[serde(default)]
    likely: Vec<RawFinding>,
    #[serde(default)]
    notes: Vec<RawFinding>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchKind {
    Anchor,
    Path,
    Any,
}

impl MatchKind {
    fn parse(raw: Option<&str>) -> Self {
        match raw.unwrap_or("anchor") {
            "path" => Self::Path,
            "any" => Self::Any,
            _ => Self::Anchor,
        }
    }
}

/// Severity-ordered match. Lower index = higher priority for
/// display.
fn severity_order(s: &str) -> u8 {
    match s {
        "Critical" => 0,
        "Likely" => 1,
        "Note" => 2,
        _ => 3,
    }
}

pub struct DriftFindingsTool {
    drift_dir: PathBuf,
}

impl DriftFindingsTool {
    pub fn new() -> Self {
        let drift_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/"))
            .join(".sovereign")
            .join("drift");
        Self { drift_dir }
    }

    /// Test seam: override the directory `latest.md.json` lives in.
    pub fn with_drift_dir(mut self, dir: PathBuf) -> Self {
        self.drift_dir = dir;
        self
    }
}

impl Default for DriftFindingsTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for DriftFindingsTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "drift_findings".to_string(),
            name: "Drift Findings".to_string(),
            description:
                "Return drift-report findings that reference a code symbol, file path, \
                 or substring — without re-running `sovereign drift detect`. Sibling \
                 to `drift_posture` (freshness gate) and `symbols`/`callers`/`blast` \
                 (code-side intelligence). \
                 \
                 Use this BEFORE editing a function, struct, or HTTP route to learn \
                 whether the architecture docs make any normative claim about it — \
                 the same way you'd run `callers(name)` to learn the code-side blast \
                 radius. \
                 \
                 Match modes (set via `kind`): \
                 `anchor` (default) — substring against `narrative.canonical_name`, \
                 which is the model-extracted code anchor for Claim atoms. \
                 `path` — substring against the source chunk id (narrative section). \
                 `any` — union of anchor, path, and quotable-excerpt substring. \
                 \
                 Reads `~/.sovereign/drift/latest.md.json` (canonical mirror written \
                 by every drift detect run). Returns `never_run` if no report exists \
                 yet; check `drift_posture` to confirm freshness before acting on \
                 results."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Code symbol, file path, or substring to search for. Required."
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["anchor", "path", "any"],
                        "description": "Match strategy. Default `anchor` — substring match against the claim's extracted code anchor. Use `path` for chunk-id matches, `any` for the union."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of findings to return (default 20). Sorted by severity (Critical first)."
                    }
                },
                "required": ["query"]
            }),
            examples: vec![
                ToolExample {
                    situation: "About to refactor `open_index_for_corpus`. Check whether the architecture docs make any normative claim about it before changing the signature.".into(),
                    call: serde_json::json!({ "query": "open_index_for_corpus" }),
                },
                ToolExample {
                    situation: "Editing the file `corpus-engine/src/recipe.rs`. Find every drift finding tied to that file's narrative sections.".into(),
                    call: serde_json::json!({ "query": "recipe.rs", "kind": "any" }),
                },
                ToolExample {
                    situation: "Want to see whether the narrative mentions `LocalOnly` anywhere — even in quotes, even if the anchor is something else.".into(),
                    call: serde_json::json!({ "query": "LocalOnly", "kind": "any", "limit": 5 }),
                },
            ],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Instant,
            scope: Scope::Session,
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "status": {
                        "type": "string",
                        "enum": ["ok", "never_run", "no_matches"],
                        "description": "`ok` when matches found. `never_run` when no drift report on disk. `no_matches` when report exists but query found nothing."
                    },
                    "report_path": { "type": ["string", "null"] },
                    "match_count": { "type": "integer" },
                    "match_mode": { "type": "string" },
                    "findings": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "severity": { "type": "string" },
                                "kind": { "type": "string" },
                                "anchor": { "type": "string" },
                                "headline": { "type": "string" },
                                "atlas_id": { "type": "string" },
                                "atom_id": { "type": "string" },
                                "chunk_id": { "type": ["string", "null"] },
                                "quotable": { "type": ["string", "null"] },
                                "action": { "type": "string" }
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
        let query = params
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("`query` is required".into()))?
            .trim();
        if query.is_empty() {
            return Err(Error::InvalidInput("`query` must not be empty".into()));
        }
        let kind = MatchKind::parse(params.get("kind").and_then(|v| v.as_str()));
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(20);

        let json_path = self.drift_dir.join("latest.md.json");
        let payload = match read_findings(&json_path, query, kind, limit) {
            Ok(p) => p,
            Err(ReadFindingsErr::Missing) => json!({
                "status": "never_run",
                "report_path": null,
                "match_count": 0,
                "match_mode": match_kind_str(kind),
                "findings": [],
                "hint": format!(
                    "No drift report at {}. Run `sovereign drift detect --code <path> \
                     --narrative <doc>` to produce one.",
                    json_path.display()
                )
            }),
            Err(ReadFindingsErr::Parse(e)) => {
                return Err(Error::InvalidInput(format!(
                    "drift report at {} is not valid JSON: {e}",
                    json_path.display()
                )));
            }
        };

        Ok(StepOutput::Json(payload))
    }
}

fn match_kind_str(k: MatchKind) -> &'static str {
    match k {
        MatchKind::Anchor => "anchor",
        MatchKind::Path => "path",
        MatchKind::Any => "any",
    }
}

enum ReadFindingsErr {
    Missing,
    Parse(serde_json::Error),
}

fn read_findings(
    json_path: &Path,
    query: &str,
    kind: MatchKind,
    limit: usize,
) -> std::result::Result<serde_json::Value, ReadFindingsErr> {
    let raw = match std::fs::read_to_string(json_path) {
        Ok(s) => s,
        Err(_) => return Err(ReadFindingsErr::Missing),
    };
    let report: RawReport = serde_json::from_str(&raw).map_err(ReadFindingsErr::Parse)?;

    let lc_query = query.to_lowercase();
    let mut hits: Vec<RawFinding> = Vec::new();
    for bucket in [&report.critical, &report.likely, &report.notes] {
        for f in bucket {
            if matches_finding(f, &lc_query, kind) {
                hits.push(f.clone());
            }
        }
    }
    // Severity-first sort, ties broken by rank_hint descending so
    // higher-confidence findings rise first.
    hits.sort_by(|a, b| {
        severity_order(&a.severity)
            .cmp(&severity_order(&b.severity))
            .then_with(|| {
                b.rank_hint
                    .partial_cmp(&a.rank_hint)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    let match_count = hits.len();
    let truncated: Vec<serde_json::Value> = hits
        .into_iter()
        .take(limit)
        .map(|f| {
            let nref = f.narrative.as_ref();
            json!({
                "severity": f.severity,
                "kind": f.kind,
                "anchor": nref.map(|n| n.canonical_name.as_str()).unwrap_or(""),
                "headline": f.headline,
                "atlas_id": nref.map(|n| n.atlas_id.as_str()).unwrap_or(""),
                "atom_id": nref.map(|n| n.atom_id.as_str()).unwrap_or(""),
                "chunk_id": nref.and_then(|n| n.chunk_id.clone()),
                "quotable": nref.and_then(|n| n.quotable.clone()),
                "action": f.action,
            })
        })
        .collect();

    let status = if match_count == 0 { "no_matches" } else { "ok" };

    Ok(json!({
        "status": status,
        "report_path": json_path.to_string_lossy(),
        "match_count": match_count,
        "match_mode": match_kind_str(kind),
        "findings": truncated,
    }))
}

fn matches_finding(f: &RawFinding, lc_query: &str, kind: MatchKind) -> bool {
    let nref = match &f.narrative {
        Some(n) => n,
        None => return false,
    };
    let anchor_hit = nref.canonical_name.to_lowercase().contains(lc_query);
    let path_hit = nref
        .chunk_id
        .as_deref()
        .map(|s| s.to_lowercase().contains(lc_query))
        .unwrap_or(false);
    let quote_hit = nref
        .quotable
        .as_deref()
        .map(|s| s.to_lowercase().contains(lc_query))
        .unwrap_or(false);
    match kind {
        MatchKind::Anchor => anchor_hit,
        MatchKind::Path => path_hit,
        MatchKind::Any => anchor_hit || path_hit || quote_hit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_report(dir: &Path, body: &serde_json::Value) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("latest.md.json"),
            serde_json::to_string(body).unwrap(),
        )
        .unwrap();
    }

    fn sample_report() -> serde_json::Value {
        json!({
            "critical": [
                {
                    "severity": "Critical",
                    "kind": "NormativeClaimWithoutAnchor",
                    "headline": "Normative claim — anchor `open_index_for_corpus` not in atlas — `…`",
                    "narrative": {
                        "atlas_id": "commonwealth-ai-system-overview",
                        "atom_id": "claim-0042",
                        "atom_type": "Claim",
                        "canonical_name": "open_index_for_corpus",
                        "chunk_id": "sec_00010",
                        "quotable": "open_index_for_corpus(corpus_id) which always opens <index_dir>/<corpus_id>"
                    },
                    "structural": null,
                    "action": "Search the codebase for `open_index_for_corpus`",
                    "rank_hint": 1000.0
                }
            ],
            "likely": [],
            "notes": [
                {
                    "severity": "Note",
                    "kind": "EntityRealityOnly",
                    "headline": "Component named in narrative — `Recipe`",
                    "narrative": {
                        "atlas_id": "commonwealth-ai-system-overview",
                        "atom_id": "entity-0007",
                        "atom_type": "Entity",
                        "canonical_name": "Recipe",
                        "chunk_id": "sec_00009",
                        "quotable": "Recipe::resolve_parameters validates parameter shape"
                    },
                    "structural": null,
                    "action": "Confirm `Recipe` is current.",
                    "rank_hint": 100.0
                }
            ]
        })
    }

    #[tokio::test]
    async fn anchor_substring_match_returns_critical_first() {
        let tmp = TempDir::new().unwrap();
        write_report(tmp.path(), &sample_report());
        let tool = DriftFindingsTool::new().with_drift_dir(tmp.path().to_path_buf());
        let out = tool
            .execute(&json!({"query": "open_index"}), &ToolContext {
                conversation_id: "t".into(),
                task_id: None,
                working_directory: None,
                in_reasoning_loop: false,
                agent_session_token: None,
            })
            .await
            .unwrap();
        let payload = match &out { StepOutput::Json(v) => v, other => panic!("expected Json, got {:?}", other) };
        assert_eq!(payload["status"], "ok");
        assert_eq!(payload["match_count"], 1);
        let f0 = &payload["findings"][0];
        assert_eq!(f0["anchor"], "open_index_for_corpus");
        assert_eq!(f0["severity"], "Critical");
    }

    #[tokio::test]
    async fn never_run_when_report_absent() {
        let tmp = TempDir::new().unwrap();
        let tool = DriftFindingsTool::new().with_drift_dir(tmp.path().to_path_buf());
        let out = tool
            .execute(&json!({"query": "anything"}), &ToolContext {
                conversation_id: "t".into(),
                task_id: None,
                working_directory: None,
                in_reasoning_loop: false,
                agent_session_token: None,
            })
            .await
            .unwrap();
        assert_eq!(match &out { StepOutput::Json(v) => v, other => panic!("expected Json, got {:?}", other) }["status"], "never_run");
        assert_eq!(match &out { StepOutput::Json(v) => v, other => panic!("expected Json, got {:?}", other) }["match_count"], 0);
    }

    #[tokio::test]
    async fn any_mode_matches_quotable_when_anchor_misses() {
        let tmp = TempDir::new().unwrap();
        write_report(tmp.path(), &sample_report());
        let tool = DriftFindingsTool::new().with_drift_dir(tmp.path().to_path_buf());
        // "resolve_parameters" is only in the quotable text, not the anchor.
        let out = tool
            .execute(
                &json!({"query": "resolve_parameters", "kind": "any"}),
                &ToolContext {
                    conversation_id: "t".into(),
                    task_id: None,
                    working_directory: None,
                    in_reasoning_loop: false,
                    agent_session_token: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(match &out { StepOutput::Json(v) => v, other => panic!("expected Json, got {:?}", other) }["match_count"], 1);
        assert_eq!(match &out { StepOutput::Json(v) => v, other => panic!("expected Json, got {:?}", other) }["findings"][0]["anchor"], "Recipe");
    }

    #[tokio::test]
    async fn anchor_mode_misses_quotable_only_matches() {
        let tmp = TempDir::new().unwrap();
        write_report(tmp.path(), &sample_report());
        let tool = DriftFindingsTool::new().with_drift_dir(tmp.path().to_path_buf());
        let out = tool
            .execute(
                &json!({"query": "resolve_parameters"}),
                &ToolContext {
                    conversation_id: "t".into(),
                    task_id: None,
                    working_directory: None,
                    in_reasoning_loop: false,
                    agent_session_token: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(match &out { StepOutput::Json(v) => v, other => panic!("expected Json, got {:?}", other) }["status"], "no_matches");
        assert_eq!(match &out { StepOutput::Json(v) => v, other => panic!("expected Json, got {:?}", other) }["match_count"], 0);
    }

    #[tokio::test]
    async fn empty_query_rejected() {
        let tmp = TempDir::new().unwrap();
        let tool = DriftFindingsTool::new().with_drift_dir(tmp.path().to_path_buf());
        let err = tool
            .execute(&json!({"query": "   "}), &ToolContext {
                conversation_id: "t".into(),
                task_id: None,
                working_directory: None,
                in_reasoning_loop: false,
                agent_session_token: None,
            })
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("must not be empty"));
    }
}
