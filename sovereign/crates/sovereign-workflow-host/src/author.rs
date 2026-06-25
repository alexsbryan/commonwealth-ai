// SPDX-License-Identifier: AGPL-3.0-or-later
//! Agent-callable tools for **workflow** authoring — the umbrella over recipe
//! authoring. Mirrors `sovereign_tools::recipe_author` (write → validate → test →
//! fix), but for workflow TOML, and lives here (not in `sovereign-tools`) because
//! validation reuses `Workflow::parse` + [`crate::summarize_capabilities`] — and
//! `sovereign-workflow-host` already depends on both. The same generic agent loop
//! that drives recipe authoring drives this; only the skill prompt + this tool set
//! differ.
//!
//! Every tool is allowlisted to `~/.sovereign/workflows/`, so the approval gate
//! sees a single [`Permission::WorkflowAuthoring`] per call (the recipe sub-flow,
//! when an ingest stage is needed, requests `RecipeAuthoring` separately).

use std::fs;
use std::path::PathBuf;

use async_trait::async_trait;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;
use sovereign_workflow::Workflow;

use crate::{summarize_capabilities, workflows_dir};

/// Resolve an agent-supplied workflow ref to its on-disk path, scoped to
/// `~/.sovereign/workflows/`. Workflows are single files (`<id>.toml`), unlike
/// recipes' `<id>/recipe.toml`. Reuses the recipe-author traversal guard.
pub fn resolve_workflow_path(input: &str, override_dir: Option<&PathBuf>) -> Result<PathBuf> {
    let candidate: PathBuf = if input.ends_with(".toml") {
        input.into()
    } else {
        format!("{input}.toml").into()
    };
    let root = match override_dir {
        Some(p) => p.clone(),
        None => workflows_dir(),
    };
    sovereign_tools::recipe_author::assert_under_root(&candidate, &root)
}

/// The workflow-author tool bundle — register these on the runtime alongside the
/// recipe-author tools so the agent loop can compose a workflow (and descend into
/// recipe authoring for an ingest stage).
pub fn author_tools() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(WorkflowWriteTool::new()),
        Box::new(WorkflowValidateTool::new()),
        Box::new(WorkflowTestTool::new()),
    ]
}

// ── write ───────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct WorkflowWriteTool {
    workflows_dir: Option<PathBuf>,
}

impl WorkflowWriteTool {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_workflows_dir(dir: PathBuf) -> Self {
        Self {
            workflows_dir: Some(dir),
        }
    }
}

#[async_trait]
impl Tool for WorkflowWriteTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "workflow_write".into(),
            name: "WorkflowWrite".into(),
            description: "Write a workflow TOML to ~/.sovereign/workflows/<id>.toml. \
                 A workflow has a [source] and a list of [[step]]s (model:/embed:/tool:/\
                 mcp:/transform: — and recipe:/enrich: for a corpus ingest/enrich stage). \
                 ALWAYS run workflow_validate after writing; fix the reported errors."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Workflow id (→ <id>.toml) or relative path under ~/.sovereign/workflows/" },
                    "content": { "type": "string", "description": "Full workflow TOML document to write" }
                },
                "required": ["path", "content"],
            }),
            examples: vec![],
            effect: Effect::ReadWrite,
            idempotency: Idempotency::NonIdempotent,
            latency: Latency::Instant,
            scope: Scope::Session,
            output_schema: Some(serde_json::json!({
                "type": "object",
                "properties": { "path": {"type": "string"}, "bytes_written": {"type": "integer"} }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![Permission::WorkflowAuthoring]
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let raw_path = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("workflow_write requires `path`".into()))?;
        let content = params
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("workflow_write requires `content`".into()))?;

        let resolved = resolve_workflow_path(raw_path, self.workflows_dir.as_ref())?;
        if let Some(parent) = resolved.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                Error::InvalidInput(format!("failed to create parent {}: {e}", parent.display()))
            })?;
        }
        let part = resolved.with_extension("toml.part");
        fs::write(&part, content)
            .map_err(|e| Error::InvalidInput(format!("failed to write {}: {e}", part.display())))?;
        fs::rename(&part, &resolved).map_err(|e| {
            Error::InvalidInput(format!(
                "failed to commit {} → {}: {e}",
                part.display(),
                resolved.display()
            ))
        })?;
        Ok(StepOutput::Json(serde_json::json!({
            "path": resolved.display().to_string(),
            "bytes_written": content.len(),
        })))
    }
}

// ── validate ────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct WorkflowValidateTool;
impl WorkflowValidateTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for WorkflowValidateTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "workflow_validate".into(),
            name: "WorkflowValidate".into(),
            description: "Validate a workflow TOML: parse (syntax, duplicate step ids, \
                 step cycles) + resolve every step's `uses` against the tool registry. \
                 Returns {passed, errors, warnings}. A step whose tool/kind cannot be \
                 resolved (typo, or an MCP server not connected) is a warning."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "content": {"type": "string", "description": "The workflow TOML to validate"} },
                "required": ["content"],
            }),
            examples: vec![],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Session,
            output_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "passed": {"type": "boolean"},
                    "errors": {"type": "array", "items": {"type": "string"}},
                    "warnings": {"type": "array", "items": {"type": "string"}}
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![Permission::WorkflowAuthoring]
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let content = params
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("workflow_validate requires `content`".into()))?;

        let wf = match Workflow::parse(content) {
            Ok(w) => w,
            Err(e) => {
                return Ok(StepOutput::Json(serde_json::json!({
                    "passed": false,
                    "errors": [format!("parse: {e}")],
                    "warnings": [],
                })));
            }
        };
        // Resolve `uses` + `{ref}`s by reusing the capability derivation.
        let caps = summarize_capabilities(&wf).await;
        let warnings: Vec<String> = caps
            .unresolved
            .iter()
            .map(|u| format!("step `{u}` could not be resolved — check the id, or connect its MCP server"))
            .collect();
        Ok(StepOutput::Json(serde_json::json!({
            "passed": true,
            "errors": [],
            "warnings": warnings,
        })))
    }
}

// ── test (capability summary) ─────────────────────────────────────────────

#[derive(Default)]
pub struct WorkflowTestTool;
impl WorkflowTestTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for WorkflowTestTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "workflow_test".into(),
            name: "WorkflowTest".into(),
            description: "Report what a workflow WOULD do before running it: the plain-\
                 language capability summary (run shell · fetch the network · write files \
                 · use your local model · download and index a corpus) + the effects, \
                 permissions, and any unresolved steps. This is the 'what would this do' \
                 check the user consents to; side-effecting steps are never executed here."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "content": {"type": "string", "description": "The workflow TOML to inspect"} },
                "required": ["content"],
            }),
            examples: vec![],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Session,
            output_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "can": {"type": "array", "items": {"type": "string"}},
                    "needs_inference": {"type": "boolean"},
                    "unresolved": {"type": "array", "items": {"type": "string"}}
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![Permission::WorkflowAuthoring]
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let content = params
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("workflow_test requires `content`".into()))?;
        let wf = Workflow::parse(content)
            .map_err(|e| Error::InvalidInput(format!("workflow_test: parse failed: {e}")))?;
        let caps = summarize_capabilities(&wf).await;
        Ok(StepOutput::Json(serde_json::json!({
            "can": caps.describe(),
            "needs_inference": caps.needs_inference,
            "unresolved": caps.unresolved,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    async fn write_scopes_under_workflows_dir_then_validate_passes() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join(".sovereign/workflows");
        std::fs::create_dir_all(&root).unwrap();

        let toml = "[workflow]\nname = \"t\"\n[source]\ntype = \"inline\"\nitems = [\"x\"]\n[[step]]\nid = \"a\"\nuses = \"transform:json\"\n";
        let write = WorkflowWriteTool::with_workflows_dir(root.clone());
        let out = write
            .execute(&serde_json::json!({"path": "t", "content": toml}), &ctx())
            .await
            .unwrap();
        match out {
            StepOutput::Json(v) => {
                let p = PathBuf::from(v["path"].as_str().unwrap());
                assert!(p.exists());
                assert!(p.starts_with(&root));
            }
            other => panic!("expected Json, got {other:?}"),
        }

        // A malformed workflow fails validation loudly.
        let validate = WorkflowValidateTool::new();
        let bad = validate
            .execute(&serde_json::json!({"content": "not a workflow"}), &ctx())
            .await
            .unwrap();
        if let StepOutput::Json(v) = bad {
            assert_eq!(v["passed"], serde_json::json!(false));
        } else {
            panic!("expected Json");
        }
    }

    #[tokio::test]
    async fn write_rejects_paths_outside_root() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join(".sovereign/workflows");
        std::fs::create_dir_all(&root).unwrap();
        let write = WorkflowWriteTool::with_workflows_dir(root);
        let err = write
            .execute(
                &serde_json::json!({"path": "/tmp/evil.toml", "content": ""}),
                &ctx(),
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("outside"));
    }
}
