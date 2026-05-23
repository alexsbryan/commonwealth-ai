//! `CapabilityRequestTool` — escalate a capability gap to the maintainer.
//!
//! Spec §5.4 distinguishes two flavours of agent-to-maintainer
//! escalation:
//!
//! - **Deferred questions** — technical tradeoffs the partner can't
//!   evaluate ("Eyecite or CourtListener IDs for citation
//!   normalisation?"). These land as `kind = "deferred_question"`
//!   notes via `DecisionLogTool`; lightweight, no maintainer-inbox
//!   side effect.
//! - **Capability requests** — situations where the existing engine
//!   doesn't have what the project needs (typically a missing or
//!   inadequate extractor). These are first-class because they're
//!   the dominant maintainer interaction during the trial.
//!
//! This tool is the second path. A request carries:
//!
//! - The source format and a sample document path (or excerpt) the
//!   agent has been working from.
//! - The agent's analysis of what's needed, in concrete engineering
//!   terms ("an XML extractor that preserves nested element
//!   structure rather than flattening to text").
//! - What was tried with existing extractors and how it failed.
//! - A snapshot of the recipe state at the point of request and a
//!   list of recipe parts blocked on the gap.
//!
//! Per spec §5.4, the partner must confirm before submission. The
//! tool enforces this with a `partner_confirmed` parameter — calls
//! with `partner_confirmed = false` (or absent) return a
//! tool-validation error so the agent has to walk the partner
//! through the §5.4 confirmation step before re-submitting.
//!
//! On confirmation the request lands in two places:
//!
//! 1. `~/.sovereign/recipe-projects/<feature_id>/capability-requests/<ts>.json`
//!    — per-project record.
//! 2. `~/.sovereign/capability-requests/inbox/<feature_id>-<ts>.json`
//!    — global maintainer inbox for `sovereign maintainer inbox` to
//!    page through every project's pending requests at once. Per
//!    spec §6.5, v1 uses manual handoff (no GitHub / mesh wiring).
//!
//! A NoteStore note `kind = "capability_request"` is also written
//! so the dashboard's "Capability requests" card can render the
//! status (pending / in_progress / resolved) without reparsing
//! files.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use corpus_engine_notes::{NoteScope, NoteSource, NoteStore};
use corpus_engine_atos::{FeatureStore};
use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use super::project::{maintainer_inbox_dir, RecipeProject};

/// Persisted shape of a capability request. Kept compatible with
/// `serde_json::from_str` so the maintainer inbox CLI can read
/// without depending on this crate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityRequest {
    pub request_id: String,
    pub feature_id: String,
    pub project_title: String,
    pub format_or_source: String,
    pub analysis: String,
    pub existing_extractors_tried: Vec<String>,
    pub failure_modes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe_state_path: Option<String>,
    pub blocked_recipe_parts: Vec<String>,
    /// Submission status. v1 ships only `submitted` from the agent;
    /// the maintainer flips this to `in_progress` / `resolved` /
    /// `won't_fix` out-of-band by editing the inbox file.
    pub status: String,
    pub created_at: String,
}

#[derive(Default)]
pub struct CapabilityRequestTool {
    notes: Option<Arc<NoteStore>>,
    features: Option<Arc<FeatureStore>>,
    /// Test-only override for the maintainer inbox directory. None
    /// in production (resolves to `~/.sovereign/capability-requests/inbox/`).
    inbox_dir: Option<PathBuf>,
}

impl CapabilityRequestTool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_stores(notes: Arc<NoteStore>, features: Arc<FeatureStore>) -> Self {
        Self {
            notes: Some(notes),
            features: Some(features),
            inbox_dir: None,
        }
    }

    pub fn with_inbox_dir(mut self, dir: PathBuf) -> Self {
        self.inbox_dir = Some(dir);
        self
    }
}

#[async_trait]
impl Tool for CapabilityRequestTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "capability_request".into(),
            name: "CapabilityRequest".into(),
            description:
                "Escalate a capability gap to the maintainer. Use when the \
                 existing extractors / acquirers / chunkers don't handle the \
                 source format and you have CONCRETE evidence of failure — \
                 not just \"this might be tricky.\" \
                 \n\nWORKFLOW: \
                 (1) Try existing tools first and characterise how they fail. \
                 (2) Compose the request and surface it to the partner in \
                 their domain language (\"I'm asking the maintainer to add \
                 support for this court system's XML format. Here's what I'm \
                 sending.\"). \
                 (3) Only after the partner confirms, call this tool with \
                 `partner_confirmed = true`. Calls without confirmation will \
                 be rejected. \
                 \n\nFor lighter technical tradeoffs the partner can't \
                 evaluate, use DecisionLog with `kind = deferred_question` \
                 instead — that's a NoteStore-only path with no maintainer \
                 inbox side effect."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "feature_id": {
                        "type": "string",
                        "description": "Recipe-author project id"
                    },
                    "format_or_source": {
                        "type": "string",
                        "description":
                            "Short description of the source format / API \
                             that needs new capability (e.g. \"PACER XML \
                             filings\")."
                    },
                    "analysis": {
                        "type": "string",
                        "description":
                            "Concrete engineering analysis of what's needed. \
                             E.g. \"An XML extractor that preserves nested \
                             element structure rather than flattening to \
                             text — the existing `xml` extractor drops the \
                             docket-entry hierarchy and we need it for the \
                             citation graph.\""
                    },
                    "existing_extractors_tried": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description":
                            "Names of extractors / acquirers / chunkers you \
                             tried before escalating."
                    },
                    "failure_modes": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description":
                            "Concrete failure modes you observed. One \
                             sentence per mode. The maintainer reads these \
                             to scope the engineering work."
                    },
                    "recipe_state_path": {
                        "type": "string",
                        "description":
                            "Optional path to the in-progress recipe TOML \
                             that's blocked on this capability."
                    },
                    "blocked_recipe_parts": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description":
                            "Which sections of the recipe (e.g. \"acquire\", \
                             \"extract\", \"investigation.entity_types\") \
                             can't make progress until the maintainer ships \
                             the capability."
                    },
                    "partner_confirmed": {
                        "type": "boolean",
                        "description":
                            "MUST be `true` — set only after the partner has \
                             reviewed and approved the request payload. \
                             Calls with `false` (or absent) are rejected."
                    }
                },
                "required": [
                    "feature_id", "format_or_source", "analysis",
                    "partner_confirmed"
                ]
            }),
            examples: vec![ToolExample {
                situation:
                    "After the partner has reviewed and approved the request \
                     for an XML extractor that preserves docket-entry \
                     nesting."
                        .into(),
                call: json!({
                    "feature_id": "<project-uuid>",
                    "format_or_source": "PACER docket XML",
                    "analysis": "Existing xml extractor flattens to text, \
                                 losing the docket-entry hierarchy needed \
                                 for the citation graph.",
                    "existing_extractors_tried": ["xml", "html"],
                    "failure_modes": [
                        "xml flattens nested docket-entry elements",
                        "html splits filings on entry boundaries we want \
                         preserved"
                    ],
                    "recipe_state_path": "courtlistener-trial",
                    "blocked_recipe_parts": ["extract", "investigation.relationship_types"],
                    "partner_confirmed": true
                }),
            }],
            effect: Effect::Write,
            idempotency: Idempotency::NonIdempotent,
            latency: Latency::Instant,
            scope: Scope::Persistent,
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "request_id": {"type": "string"},
                    "inbox_path": {"type": "string"},
                    "project_path": {"type": "string"}
                }
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
                "CapabilityRequestTool was constructed without a NoteStore".into(),
            )
        })?;
        let features = self.features.as_ref().ok_or_else(|| {
            Error::InvalidInput(
                "CapabilityRequestTool was constructed without a FeatureStore".into(),
            )
        })?;

        // Partner-confirmation gate. Spec §5.4: composing the request
        // is one confirmation, not a back-and-forth — but it is a
        // confirmation, full stop. The tool refuses unconfirmed
        // submissions with a message that nudges the agent through
        // the right surface.
        let partner_confirmed = params
            .get("partner_confirmed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !partner_confirmed {
            return Err(Error::InvalidInput(
                "CapabilityRequestTool: `partner_confirmed` must be true. \
                 Per spec §5.4, surface the composed request to the partner \
                 in their domain language and get explicit approval before \
                 submitting. If this is a technical tradeoff the partner \
                 can't evaluate (rather than a missing engine capability), \
                 use DecisionLog with kind = deferred_question instead."
                    .into(),
            ));
        }

        let feature_id = params
            .get("feature_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                Error::InvalidInput(
                    "CapabilityRequestTool requires `feature_id`".into(),
                )
            })?;
        let format_or_source = params
            .get("format_or_source")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                Error::InvalidInput(
                    "CapabilityRequestTool requires `format_or_source`".into(),
                )
            })?;
        let analysis = params
            .get("analysis")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                Error::InvalidInput(
                    "CapabilityRequestTool requires `analysis`".into(),
                )
            })?;
        let extractors_tried =
            string_array(params.get("existing_extractors_tried"));
        let failure_modes = string_array(params.get("failure_modes"));
        let recipe_state_path = params
            .get("recipe_state_path")
            .and_then(|v| v.as_str())
            .map(String::from);
        let blocked_parts = string_array(params.get("blocked_recipe_parts"));

        let project = RecipeProject::load(
            feature_id,
            Arc::clone(notes),
            Arc::clone(features),
        )
        .await?;

        let timestamp_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let request_id = format!("{timestamp_secs}-{}", uuid_short());
        let now_rfc = chrono::DateTime::<chrono::Utc>::from_timestamp(
            timestamp_secs as i64,
            0,
        )
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_else(|| timestamp_secs.to_string());

        let request = CapabilityRequest {
            request_id: request_id.clone(),
            feature_id: project.feature_id().to_string(),
            project_title: project.title().to_string(),
            format_or_source: format_or_source.to_string(),
            analysis: analysis.to_string(),
            existing_extractors_tried: extractors_tried,
            failure_modes,
            recipe_state_path,
            blocked_recipe_parts: blocked_parts,
            status: "submitted".to_string(),
            created_at: now_rfc,
        };

        // 1. Write per-project record.
        let project_path = project
            .capability_requests_dir()
            .join(format!("{request_id}.json"));
        let bytes = serde_json::to_vec_pretty(&request).map_err(|e| {
            Error::InvalidInput(format!("failed to serialise request: {e}"))
        })?;
        if let Some(parent) = project_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| io_err("create_dir_all", parent, e))?;
        }
        std::fs::write(&project_path, &bytes)
            .map_err(|e| io_err("write", &project_path, e))?;

        // 2. Mirror into the global maintainer inbox.
        let inbox_root = match self.inbox_dir.as_ref() {
            Some(d) => d.clone(),
            None => maintainer_inbox_dir()?,
        };
        std::fs::create_dir_all(&inbox_root)
            .map_err(|e| io_err("create_dir_all", &inbox_root, e))?;
        let inbox_path = inbox_root.join(format!(
            "{}-{}.json",
            project.feature_id(),
            request_id
        ));
        std::fs::write(&inbox_path, &bytes)
            .map_err(|e| io_err("write", &inbox_path, e))?;

        // 3. NoteStore note for the dashboard.
        let payload = json!({
            "request_id": request_id,
            "format_or_source": format_or_source,
            "status": "submitted",
            "inbox_path": inbox_path.display().to_string(),
        });
        notes
            .write_note_full(
                "capability_request",
                analysis,
                Vec::new(),
                Vec::new(),
                &ctx.conversation_id,
                NoteScope::Feature,
                Some(project.feature_id()),
                None,
                NoteSource::Agent,
                None,
                Some(&payload.to_string()),
            )
            .await
            .map_err(ce_err)?;

        Ok(StepOutput::Json(json!({
            "request_id": request_id,
            "inbox_path": inbox_path.display().to_string(),
            "project_path": project_path.display().to_string(),
        })))
    }
}

fn string_array(v: Option<&serde_json::Value>) -> Vec<String> {
    v.and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn uuid_short() -> String {
    let id = uuid::Uuid::new_v4().to_string();
    id.split('-').next().unwrap_or(&id).to_string()
}

fn io_err<P: AsRef<Path>>(op: &str, path: P, e: std::io::Error) -> Error {
    Error::InvalidInput(format!("{op} {}: {e}", path.as_ref().display()))
}

fn ce_err(e: corpus_engine_notes::Error) -> Error {
    Error::Storage(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Process-wide HOME lock. `RecipeProject::new` captures
    /// `~/.sovereign/recipe-projects/...` at construction time and
    /// every method that touches the project's filesystem layout
    /// reads it back via `dirs::home_dir()`. Without serialising
    /// HOME, two parallel async tests both call `fresh()`, the
    /// second stomps the first's `HOME`, and the first's later
    /// `submits_when_confirmed_writes_both_paths` looks for its
    /// project_path under the *second* tempdir — file missing,
    /// test fails. Pinned 2026-05-10 after a repo-wide
    /// `sovereign-test.sh` run surfaced the flake.
    static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    async fn fresh() -> (
        Arc<NoteStore>,
        Arc<FeatureStore>,
        RecipeProject,
        tempfile::TempDir,
        std::sync::MutexGuard<'static, ()>,
    ) {
        let guard = HOME_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let notes = Arc::new(NoteStore::open(&dir.path().join("notes.db")).unwrap());
        let features =
            Arc::new(FeatureStore::open(&dir.path().join("features.db")).unwrap());
        std::env::set_var("HOME", dir.path());
        let project = RecipeProject::new(
            "trial",
            "Federal case law",
            Arc::clone(&notes),
            Arc::clone(&features),
        )
        .await
        .unwrap();
        (notes, features, project, dir, guard)
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
    async fn rejects_without_partner_confirmation() {
        let (notes, features, project, _dir, _home_lock) = fresh().await;
        let inbox = tempfile::tempdir().unwrap();
        let tool = CapabilityRequestTool::with_stores(
            Arc::clone(&notes),
            Arc::clone(&features),
        )
        .with_inbox_dir(inbox.path().to_path_buf());
        let err = tool
            .execute(
                &json!({
                    "feature_id": project.feature_id(),
                    "format_or_source": "PACER XML",
                    "analysis": "needs new extractor",
                    "partner_confirmed": false
                }),
                &ctx(),
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("partner_confirmed"));
    }

    #[tokio::test]
    async fn rejects_when_partner_confirmed_absent() {
        let (notes, features, project, _dir, _home_lock) = fresh().await;
        let inbox = tempfile::tempdir().unwrap();
        let tool = CapabilityRequestTool::with_stores(
            Arc::clone(&notes),
            Arc::clone(&features),
        )
        .with_inbox_dir(inbox.path().to_path_buf());
        let err = tool
            .execute(
                &json!({
                    "feature_id": project.feature_id(),
                    "format_or_source": "PACER XML",
                    "analysis": "needs new extractor"
                }),
                &ctx(),
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("partner_confirmed"));
    }

    #[tokio::test]
    async fn submits_when_confirmed_writes_both_paths() {
        let (notes, features, project, _dir, _home_lock) = fresh().await;
        let inbox = tempfile::tempdir().unwrap();
        let tool = CapabilityRequestTool::with_stores(
            Arc::clone(&notes),
            Arc::clone(&features),
        )
        .with_inbox_dir(inbox.path().to_path_buf());
        let out = tool
            .execute(
                &json!({
                    "feature_id": project.feature_id(),
                    "format_or_source": "PACER XML",
                    "analysis": "xml extractor flattens docket-entry hierarchy",
                    "existing_extractors_tried": ["xml"],
                    "failure_modes": ["loses nesting"],
                    "blocked_recipe_parts": ["extract"],
                    "partner_confirmed": true
                }),
                &ctx(),
            )
            .await
            .unwrap();
        let v = match out {
            StepOutput::Json(v) => v,
            other => panic!("expected Json, got {other:?}"),
        };
        let inbox_path = PathBuf::from(v["inbox_path"].as_str().unwrap());
        let project_path = PathBuf::from(v["project_path"].as_str().unwrap());
        assert!(inbox_path.exists(), "inbox path missing: {}", inbox_path.display());
        assert!(
            project_path.exists(),
            "project path missing: {}",
            project_path.display()
        );

        // The two writes carry the same JSON shape. Reading either
        // back through the public type confirms compatibility with
        // the maintainer inbox CLI's reader.
        let body = std::fs::read_to_string(&inbox_path).unwrap();
        let parsed: CapabilityRequest = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed.format_or_source, "PACER XML");
        assert_eq!(parsed.status, "submitted");
        assert_eq!(parsed.feature_id, project.feature_id());

        // Also a NoteStore note `kind = capability_request`.
        let scope = corpus_engine_notes::ScopeFilter {
            scopes: vec![NoteScope::Feature],
            feature_id: Some(project.feature_id().to_string()),
        };
        let rows = notes
            .read_notes_scoped(
                None,
                &[],
                &[],
                &["capability_request".to_string()],
                10,
                false,
                &scope,
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
    }
}
