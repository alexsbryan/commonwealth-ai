//! `ResearchFindingTool` — durably commit a confirmed fact pulled
//! from the network so the next session (or a checkpoint restore)
//! doesn't have to re-discover it.
//!
//! Why this exists: across 15 recipe-author projects in the trial,
//! the agent called `web_search` and `web_fetch` heavily but wrote
//! zero `research_finding` notes — the kind landed in the v7 schema
//! during M1 with no tool wrapping it. Findings stayed in the
//! ephemeral chat context and evaporated as soon as the conversation
//! turned over. The first fix is just to give the agent the surface;
//! a stricter "auto-write on every web_fetch" pass can come later if
//! the passive form drifts.
//!
//! Storage: thin wrapper over [`NoteStore::write_note_full`]. Same
//! shape as `decision_log` — `kind = "research_finding"`, `scope =
//! "feature"`, `feature_id = <project>`, structured fields in the
//! `payload_json` column. The dashboard's research-log card reads
//! these without parsing free text.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use corpus_engine_notes::{NoteScope, NoteSource, NoteStore};
use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

/// How confident the agent is in the claim. The dashboard surfaces
/// this so the partner can spot a stack of `low` findings before
/// they propagate into the recipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingConfidence {
    High,
    Medium,
    Low,
}

impl FindingConfidence {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "high" => Some(Self::High),
            "medium" => Some(Self::Medium),
            "low" => Some(Self::Low),
            _ => None,
        }
    }
}

/// Coarse classifier so the dashboard / situated-context renderer
/// can separate "facts about the API I'm wiring" from "facts about
/// the partner's domain" — the two are usefully distinct in audit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingScope {
    /// Concrete facts about the data source: endpoint shape,
    /// pagination, auth, field names. The bulk of API research.
    ApiContract,
    /// Domain-side knowledge: how the partner's field tags entities,
    /// what counts as a "case" vs a "memorandum disposition", etc.
    Domain,
    /// Anything else the agent thinks is worth remembering.
    Other,
}

impl FindingScope {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "api_contract" => Some(Self::ApiContract),
            "domain" => Some(Self::Domain),
            "other" => Some(Self::Other),
            _ => None,
        }
    }
}

/// Structured payload stored in the note's `payload_json` column.
/// Public so the situated-context renderer / dashboard can deserialise
/// without redefining the schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchFindingPayload {
    pub source_url: String,
    pub confidence: FindingConfidence,
    pub scope: FindingScope,
}

#[derive(Default)]
pub struct ResearchFindingTool {
    notes: Option<Arc<NoteStore>>,
}

impl ResearchFindingTool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_notes(notes: Arc<NoteStore>) -> Self {
        Self {
            notes: Some(notes),
        }
    }
}

#[async_trait]
impl Tool for ResearchFindingTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "research_finding".into(),
            name: "ResearchFinding".into(),
            description:
                "Record a fact you confirmed from a network source — \
                 typically right after a `web_fetch` or `probe_url` \
                 that gave you ground truth about an API contract or \
                 domain detail. Each finding becomes a durable note \
                 the next session can re-use, so you don't repeat \
                 the same web research after a checkpoint restore. \
                 `claim` is the fact in your own words (one or two \
                 sentences). `source_url` is the page that backs it. \
                 `confidence` reflects how strongly the source \
                 supports the claim (`high` = direct quote from \
                 official docs; `medium` = inferred from a working \
                 example; `low` = best guess from indirect signal). \
                 `scope` separates `api_contract` facts (endpoint \
                 paths, pagination, auth) from `domain` facts (the \
                 partner's field-specific framing) so audit views \
                 stay legible. Call this whenever a web tool gives \
                 you a non-obvious fact you'll bake into the recipe."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "feature_id": {
                        "type": "string",
                        "description":
                            "Feature id of the recipe-author project this finding belongs to."
                    },
                    "claim": {
                        "type": "string",
                        "description":
                            "The fact, in your own words. One or two sentences."
                    },
                    "source_url": {
                        "type": "string",
                        "description":
                            "URL that backs the claim — usually the page \
                             you just web_fetch'd or probe_url'd."
                    },
                    "confidence": {
                        "type": "string",
                        "enum": ["high", "medium", "low"]
                    },
                    "scope": {
                        "type": "string",
                        "enum": ["api_contract", "domain", "other"]
                    }
                },
                "required": ["feature_id", "claim", "source_url", "confidence", "scope"]
            }),
            examples: vec![ToolExample {
                situation:
                    "Just probed the CourtListener v4 endpoint and saw the \
                     `next` field is a fully-qualified URL, not a token."
                        .into(),
                call: json!({
                    "feature_id": "<project-uuid>",
                    "claim":
                        "CourtListener v4 returns the `next` pagination field as a \
                         fully-qualified URL; recipes should use \
                         `[acquire.pagination] type = \"next_url\"` rather than \"cursor\".",
                    "source_url":
                        "https://www.courtlistener.com/api/rest/v4/opinions/?cluster__docket__court=ca9&page_size=2",
                    "confidence": "high",
                    "scope": "api_contract"
                }),
            }],
            effect: Effect::Write,
            idempotency: Idempotency::NonIdempotent,
            latency: Latency::Instant,
            scope: Scope::Persistent,
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "finding_id": {"type": "string"}
                },
                "required": ["finding_id"]
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![Permission::RecipeAuthoring]
    }

    async fn execute(
        &self,
        params: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<StepOutput> {
        let notes = self.notes.as_ref().ok_or_else(|| {
            Error::InvalidInput(
                "ResearchFindingTool was constructed without a NoteStore; \
                 cannot write finding."
                    .into(),
            )
        })?;
        let feature_id = required_str(params, "feature_id")?;
        let claim = required_str(params, "claim")?;
        let source_url = required_str(params, "source_url")?;
        let confidence_str = required_str(params, "confidence")?;
        let scope_str = required_str(params, "scope")?;

        let confidence = FindingConfidence::parse(confidence_str).ok_or_else(|| {
            Error::InvalidInput(format!(
                "ResearchFindingTool: unknown confidence `{confidence_str}`. \
                 Allowed: high | medium | low"
            ))
        })?;
        let scope = FindingScope::parse(scope_str).ok_or_else(|| {
            Error::InvalidInput(format!(
                "ResearchFindingTool: unknown scope `{scope_str}`. \
                 Allowed: api_contract | domain | other"
            ))
        })?;

        let payload = ResearchFindingPayload {
            source_url: source_url.to_string(),
            confidence,
            scope,
        };
        let payload_json = serde_json::to_string(&payload).map_err(|e| {
            Error::InvalidInput(format!("failed to serialise finding payload: {e}"))
        })?;

        let session_id = &ctx.conversation_id;
        let id = notes
            .write_note_full(
                "research_finding",
                claim,
                Vec::new(),
                Vec::new(),
                session_id,
                NoteScope::Feature,
                Some(feature_id),
                None,
                NoteSource::Agent,
                None,
                Some(&payload_json),
            )
            .await
            .map_err(|e| {
                Error::Storage(format!("research_finding write failed: {e}"))
            })?;

        Ok(StepOutput::Json(json!({"finding_id": id})))
    }
}

fn required_str<'a>(
    params: &'a serde_json::Value,
    key: &str,
) -> Result<&'a str> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            Error::InvalidInput(format!("ResearchFindingTool requires `{key}`"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine_atos::FeatureStore;

    async fn fresh_stores() -> (Arc<NoteStore>, Arc<FeatureStore>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let notes = Arc::new(NoteStore::open(&dir.path().join("notes.db")).unwrap());
        let features =
            Arc::new(FeatureStore::open(&dir.path().join("features.db")).unwrap());
        (notes, features, dir)
    }

    fn ctx() -> ToolContext {
        ToolContext {
            conversation_id: ConversationId::new(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
        }
    }

    #[tokio::test]
    async fn writes_a_finding_with_payload() {
        let (notes, features, _dir) = fresh_stores().await;
        let project = features
            .provision_recipe_project("p1", "test", "draft charter")
            .await
            .unwrap();
        let tool = ResearchFindingTool::with_notes(Arc::clone(&notes));
        let out = tool
            .execute(
                &json!({
                    "feature_id": project.id,
                    "claim": "CourtListener v4 returns `next` as a full URL.",
                    "source_url": "https://www.courtlistener.com/api/rest/v4/opinions/?page_size=2",
                    "confidence": "high",
                    "scope": "api_contract"
                }),
                &ctx(),
            )
            .await
            .unwrap();
        let v = match out {
            StepOutput::Json(v) => v,
            other => panic!("expected Json, got {other:?}"),
        };
        let id = v["finding_id"].as_str().unwrap().to_string();

        let scope_filter = corpus_engine_notes::ScopeFilter {
            scopes: vec![NoteScope::Feature],
            feature_id: Some(project.id.clone()),
        };
        let rows = notes
            .read_notes_scoped(None, &[], &[], &[], 10, false, &scope_filter)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, id);
        assert_eq!(rows[0].kind, "research_finding");
        let payload: ResearchFindingPayload =
            serde_json::from_str(rows[0].payload_json.as_deref().unwrap()).unwrap();
        assert_eq!(payload.confidence, FindingConfidence::High);
        assert_eq!(payload.scope, FindingScope::ApiContract);
        assert!(payload.source_url.starts_with("https://"));
    }

    #[tokio::test]
    async fn rejects_unknown_confidence() {
        let (notes, features, _dir) = fresh_stores().await;
        let project = features
            .provision_recipe_project("p1", "test", "draft")
            .await
            .unwrap();
        let tool = ResearchFindingTool::with_notes(Arc::clone(&notes));
        let err = tool
            .execute(
                &json!({
                    "feature_id": project.id,
                    "claim": "x",
                    "source_url": "https://example.com",
                    "confidence": "very-sure",
                    "scope": "api_contract"
                }),
                &ctx(),
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("confidence"));
    }

    #[tokio::test]
    async fn rejects_unknown_scope() {
        let (notes, features, _dir) = fresh_stores().await;
        let project = features
            .provision_recipe_project("p1", "test", "draft")
            .await
            .unwrap();
        let tool = ResearchFindingTool::with_notes(Arc::clone(&notes));
        let err = tool
            .execute(
                &json!({
                    "feature_id": project.id,
                    "claim": "x",
                    "source_url": "https://example.com",
                    "confidence": "high",
                    "scope": "thursday"
                }),
                &ctx(),
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("scope"));
    }
}
