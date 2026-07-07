// SPDX-License-Identifier: AGPL-3.0-or-later
//! `DecisionLogTool` — append a decision to a recipe-author project's
//! decision log.
//!
//! Spec §5.2 distinguishes two classes of choice the agent makes:
//! domain-relevant defaults the partner should weigh in on, and
//! purely technical mechanics the agent quietly executes. Both kinds
//! land here, tagged via `decision_kind`, so the dashboard can
//! render the correct attribution (`partner`, `agent_default`,
//! `deferred`) and the partner can audit "which of these did I
//! actually decide?" in spec §9 acceptance criterion 5.
//!
//! Implementation: a thin wrapper over [`NoteStore::write_note_full`]
//! that writes `kind = "decision"`, `scope = "feature"`, `feature_id
//! = <project>`, and serialises the structured fields into the new
//! v7 `payload_json` column. No new persistence — `NoteStore` is the
//! single source of truth for the decision log, and the FTS5 surface
//! gives the dashboard cheap retrieval.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::recipe::notes::{NoteScope, NoteSource, RecipeNotes};
use sovereign_contracts::traits::Tool;
use sovereign_contracts::types::*;

/// Five decision_kind variants per spec §5.2 + §5.4. Stored in the
/// note's `payload_json` so the dashboard can group by kind without
/// reparsing free-text content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    /// A choice about which data source to draw from (CourtListener
    /// vs Justia, official API vs scrape).
    SourceChoice,
    /// A choice about how to extract / chunk / filter content.
    ExtractionChoice,
    /// A choice about the investigation schema (which entity types,
    /// which relationship types, which patterns).
    SchemaChoice,
    /// A clarification of domain framing the partner gave (e.g.
    /// "treat dissents as separate documents").
    DomainClarification,
    /// A question outside the partner's expertise that should be
    /// flagged for the maintainer (spec §5.4 light path; heavier
    /// path is `CapabilityRequestTool`).
    DeferredQuestion,
}

impl DecisionKind {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "source_choice" => Some(Self::SourceChoice),
            "extraction_choice" => Some(Self::ExtractionChoice),
            "schema_choice" => Some(Self::SchemaChoice),
            "domain_clarification" => Some(Self::DomainClarification),
            "deferred_question" => Some(Self::DeferredQuestion),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::SourceChoice => "source_choice",
            Self::ExtractionChoice => "extraction_choice",
            Self::SchemaChoice => "schema_choice",
            Self::DomainClarification => "domain_clarification",
            Self::DeferredQuestion => "deferred_question",
        }
    }
}

/// Three attribution variants per spec §6.2 (the dashboard's
/// "Recent decisions" card surfaces these per-row so the partner
/// can tell at a glance whether they decided, the agent decided
/// silently, or the question was punted to the maintainer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionAttribution {
    Partner,
    AgentDefault,
    Deferred,
}

impl DecisionAttribution {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "partner" => Some(Self::Partner),
            "agent_default" => Some(Self::AgentDefault),
            "deferred" => Some(Self::Deferred),
            _ => None,
        }
    }
}

/// Structured payload stored in the note's `payload_json` column.
/// Public so the situated-context renderer / dashboard can deserialise
/// directly without re-implementing the schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionPayload {
    pub decision_kind: DecisionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<DecisionAttribution>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives_considered: Vec<String>,
}

#[derive(Default)]
pub struct DecisionLogTool {
    notes: Option<Arc<dyn RecipeNotes>>,
}

impl DecisionLogTool {
    /// Build the tool unattached. Agents will need an attached store
    /// to actually write — the unattached form exists for tool-list
    /// rendering paths that never call `execute`.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_notes(notes: Arc<dyn RecipeNotes>) -> Self {
        Self { notes: Some(notes) }
    }
}

#[async_trait]
impl Tool for DecisionLogTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "decision_log".into(),
            name: "DecisionLog".into(),
            description: "Record a decision in the active recipe-author project's decision \
                 log. Use ONE of five kinds: \
                 `source_choice` (which data source / API to pull from), \
                 `extraction_choice` (extractor / filter / chunker shape), \
                 `schema_choice` (entity / relationship / pattern types), \
                 `domain_clarification` (a framing the partner gave you in their \
                 own language that you'll honor going forward), \
                 `deferred_question` (a technical tradeoff outside the partner's \
                 expertise that needs maintainer follow-up). \
                 Set `attribution` to `partner` when the partner picked, \
                 `agent_default` when you chose without them, `deferred` for \
                 questions the partner can't evaluate. Include \
                 `alternatives_considered` whenever the choice was non-obvious \
                 — the partner reads these in retrospect to audit which \
                 decisions were theirs."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "feature_id": {
                        "type": "string",
                        "description":
                            "Feature id of the recipe-author project (the \
                             project this decision belongs to)"
                    },
                    "kind": {
                        "type": "string",
                        "enum": [
                            "source_choice", "extraction_choice",
                            "schema_choice", "domain_clarification",
                            "deferred_question"
                        ]
                    },
                    "summary": {
                        "type": "string",
                        "description":
                            "One- or two-sentence record in the partner's own \
                             domain language. The dashboard renders this verbatim."
                    },
                    "attribution": {
                        "type": "string",
                        "enum": ["partner", "agent_default", "deferred"],
                        "description":
                            "Who picked. Defaults to `agent_default` when omitted."
                    },
                    "alternatives_considered": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description":
                            "Other options you weighed before landing here. \
                             Empty / omitted is fine for purely mechanical choices."
                    }
                },
                "required": ["feature_id", "kind", "summary"]
            }),
            examples: vec![ToolExample {
                situation: "Agent chose paragraph chunking without consulting \
                            the partner — purely mechanical choice."
                    .into(),
                call: json!({
                    "feature_id": "<project-uuid>",
                    "kind": "extraction_choice",
                    "summary": "Paragraph chunking with 2048-char windows; \
                                opinion text breaks naturally at paragraphs.",
                    "attribution": "agent_default"
                }),
            }],
            effect: Effect::Write,
            idempotency: Idempotency::NonIdempotent,
            latency: Latency::Instant,
            scope: Scope::Persistent,
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "decision_id": {"type": "string"},
                    "decision_kind": {"type": "string"}
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![Permission::RecipeAuthoring]
    }

    async fn execute(&self, params: &serde_json::Value, ctx: &ToolContext) -> Result<StepOutput> {
        let notes = self.notes.as_ref().ok_or_else(|| {
            Error::InvalidInput(
                "DecisionLogTool was constructed without a NoteStore; \
                 cannot write decision."
                    .into(),
            )
        })?;
        let feature_id = params
            .get("feature_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("DecisionLogTool requires `feature_id`".into()))?;
        let kind_str = params
            .get("kind")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("DecisionLogTool requires `kind`".into()))?;
        let kind = DecisionKind::parse(kind_str).ok_or_else(|| {
            Error::InvalidInput(format!(
                "DecisionLogTool: unknown kind `{kind_str}`. Allowed: \
                 source_choice | extraction_choice | schema_choice | \
                 domain_clarification | deferred_question"
            ))
        })?;
        let summary = params
            .get("summary")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("DecisionLogTool requires `summary`".into()))?;
        let attribution: Option<DecisionAttribution> = params
            .get("attribution")
            .and_then(|v| v.as_str())
            .and_then(DecisionAttribution::parse);
        let alternatives: Vec<String> = params
            .get("alternatives_considered")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let payload = DecisionPayload {
            decision_kind: kind,
            attribution,
            alternatives_considered: alternatives,
        };
        let payload_json = serde_json::to_string(&payload).map_err(|e| {
            Error::InvalidInput(format!("failed to serialise decision payload: {e}"))
        })?;

        let session_id = &ctx.conversation_id;
        let id = notes
            .write_note_full(
                "decision",
                summary,
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
            .map_err(|e| Error::Storage(format!("decision_log write failed: {e}")))?;

        Ok(StepOutput::Json(json!({
            "decision_id": id,
            "decision_kind": kind.as_str(),
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipe_project_store::RecipeProjectStore;
    use crate::test_support::InMemoryRecipeNotes;
    use sovereign_contracts::recipe::notes::ScopeFilter;

    async fn fresh_stores() -> (
        Arc<dyn RecipeNotes>,
        Arc<RecipeProjectStore>,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let notes: Arc<dyn RecipeNotes> = Arc::new(InMemoryRecipeNotes::new());
        let features = Arc::new(RecipeProjectStore::open(&dir.path().join("features.db")).unwrap());
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
    async fn writes_a_decision_with_payload() {
        let (notes, features, _dir) = fresh_stores().await;
        let project = features
            .provision_recipe_project("p1", "test", "draft charter")
            .await
            .unwrap();
        let tool = DecisionLogTool::with_notes(Arc::clone(&notes));
        let out = tool
            .execute(
                &json!({
                    "feature_id": project.id,
                    "kind": "schema_choice",
                    "summary": "Investigate counsel-of-record overlaps",
                    "attribution": "partner",
                    "alternatives_considered": ["judge co-authorship overlap"]
                }),
                &ctx(),
            )
            .await
            .unwrap();
        let v = match out {
            StepOutput::Json(v) => v,
            other => panic!("expected Json, got {other:?}"),
        };
        assert_eq!(v["decision_kind"], "schema_choice");
        let id = v["decision_id"].as_str().unwrap().to_string();

        let scope = ScopeFilter {
            scopes: vec![NoteScope::Feature],
            feature_id: Some(project.id.clone()),
        };
        let rows = notes
            .read_notes_scoped(None, &[], &[], &[], 10, false, &scope)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, id);
        assert_eq!(rows[0].kind, "decision");
        let payload: DecisionPayload =
            serde_json::from_str(rows[0].payload_json.as_deref().unwrap()).unwrap();
        assert_eq!(payload.decision_kind, DecisionKind::SchemaChoice);
        assert_eq!(payload.attribution, Some(DecisionAttribution::Partner));
        assert_eq!(payload.alternatives_considered.len(), 1);
    }

    #[tokio::test]
    async fn rejects_unknown_kind() {
        let (notes, features, _dir) = fresh_stores().await;
        let project = features
            .provision_recipe_project("p1", "test", "draft")
            .await
            .unwrap();
        let tool = DecisionLogTool::with_notes(Arc::clone(&notes));
        let err = tool
            .execute(
                &json!({
                    "feature_id": project.id,
                    "kind": "tuesday_choice",
                    "summary": "x"
                }),
                &ctx(),
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("unknown kind"));
    }

    #[tokio::test]
    async fn covers_all_five_decision_kinds() {
        let (notes, features, _dir) = fresh_stores().await;
        let project = features
            .provision_recipe_project("p1", "test", "draft")
            .await
            .unwrap();
        let tool = DecisionLogTool::with_notes(Arc::clone(&notes));
        for kind in [
            "source_choice",
            "extraction_choice",
            "schema_choice",
            "domain_clarification",
            "deferred_question",
        ] {
            tool.execute(
                &json!({
                    "feature_id": project.id,
                    "kind": kind,
                    "summary": format!("a {kind}")
                }),
                &ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("{kind}: {e}"));
        }
        let scope = ScopeFilter {
            scopes: vec![NoteScope::Feature],
            feature_id: Some(project.id.clone()),
        };
        let rows = notes
            .read_notes_scoped(None, &[], &[], &[], 100, false, &scope)
            .await
            .unwrap();
        assert_eq!(rows.len(), 5);
    }
}
