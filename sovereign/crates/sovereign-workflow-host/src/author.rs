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
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;
use sovereign_tools::recipe_author::json_to_toml::{
    json_to_toml, sanitize_for_toml, toml_value_to_string,
};
use sovereign_workflow::Workflow;

use crate::author_schema::workflow_json_schema;
use crate::{summarize_capabilities, workflows_dir};

/// Validate a workflow TOML the way `workflow_validate` does — the shared core of
/// [`WorkflowValidateTool`] and [`WorkflowWriteStructuredTool`]. Parse (syntax,
/// duplicate step ids, cycles) then resolve every step's capabilities; an
/// unresolved step (typo id, an MCP server not connected, a `recipe:<id>` not in
/// the registry) is a warning, not a hard error. Returns the uniform
/// `{passed, errors, warnings}` shape both tools report.
pub(crate) async fn validate_workflow_toml(content: &str) -> serde_json::Value {
    let wf = match Workflow::parse(content) {
        Ok(w) => w,
        Err(e) => {
            return json!({
                "passed": false,
                "errors": [format!("parse: {e}")],
                "warnings": [],
            });
        }
    };
    // Resolve `uses` + `{ref}`s by reusing the capability derivation.
    let caps = summarize_capabilities(&wf).await;
    let warnings: Vec<String> = caps
        .unresolved
        .iter()
        .map(|u| {
            format!("step `{u}` could not be resolved — check the id, or connect its MCP server")
        })
        .collect();
    json!({
        "passed": true,
        "errors": [],
        "warnings": warnings,
    })
}

/// First `max_chars` of `s`, ellipsized — keeps a TOML preview bounded in the
/// tool's JSON response.
fn preview(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let cut: String = s.chars().take(max_chars - 1).collect();
    format!("{cut}…")
}

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
        Box::new(WorkflowWriteStructuredTool::new()),
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

// ── write (structured) ────────────────────────────────────────────────────

/// Write a workflow from a structured JSON object instead of a raw TOML string —
/// the workflow analog of `recipe_write_structured`. The agent emits a
/// workflow-shaped object whose JSON Schema ([`workflow_json_schema`]) lives in the
/// tool's `parameters.workflow`, so the daemon's LLGuidance sampler grammar-
/// constrains the output: invalid top-level keys, a bad `source.type`, or a step
/// `uses` with an unknown `<kind>:` prefix can't be emitted. The tool serializes
/// JSON → TOML mechanically (no per-character TOML mistakes), writes atomically,
/// then runs the same validator `workflow_validate` does so anything the schema
/// can't pin (a dangling `{ref}`, a disconnected `mcp:` tool, an unknown
/// `recipe:<id>`) still surfaces.
#[derive(Default)]
pub struct WorkflowWriteStructuredTool {
    workflows_dir: Option<PathBuf>,
}

impl WorkflowWriteStructuredTool {
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
impl Tool for WorkflowWriteStructuredTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "workflow_write_structured".into(),
            name: "WorkflowWriteStructured".into(),
            description: "Write a workflow from a structured JSON object (NOT raw \
                 TOML). The `workflow` argument is a workflow-shaped object — a \
                 `workflow` table ({name}), an optional `source`, and a `step` array. \
                 The tool serializes it to TOML and writes atomically to \
                 ~/.sovereign/workflows/<path>.toml.\n\nALWAYS prefer this over \
                 workflow_write for new drafts: the JSON Schema for `workflow` \
                 (declared in this tool's parameters) lets the daemon grammar-\
                 constrain your output to the workflow shape, so you cannot emit \
                 invalid keys, a malformed `source.type`, or a step `uses` with an \
                 unknown `<kind>:` prefix. Returns the TOML the tool wrote plus the \
                 validator's report so you can see at a glance whether the workflow \
                 is ready to test."
                .into(),
            parameters: json!({
                "type": "object",
                "required": ["path", "workflow"],
                "additionalProperties": false,
                "properties": {
                    "path": {
                        "type": "string",
                        "description":
                            "Workflow id (writes to <id>.toml) or relative path under \
                             ~/.sovereign/workflows/."
                    },
                    "workflow": workflow_json_schema(),
                }
            }),
            examples: vec![ToolExample {
                situation: "Draft a fresh workflow from scratch. The agent emits a \
                     structured JSON object; the tool produces clean TOML on disk."
                    .into(),
                call: json!({
                    "path": "summarize-folder",
                    "workflow": {
                        "workflow": { "name": "summarize-folder" },
                        "source": { "type": "folder", "path": "{param.folder}", "glob": "*.md,*.txt" },
                        "step": [
                            {
                                "id": "summary",
                                "uses": "model:thoughtful",
                                "prompt": "Summarize this in 3 sentences:\n\n{item.text}"
                            }
                        ]
                    }
                }),
            }],
            effect: Effect::ReadWrite,
            idempotency: Idempotency::NonIdempotent,
            latency: Latency::Instant,
            scope: Scope::Persistent,
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "path":          { "type": "string" },
                    "bytes_written": { "type": "integer" },
                    "toml_preview":  { "type": "string" },
                    "validation": {
                        "type": "object",
                        "properties": {
                            "passed":   { "type": "boolean" },
                            "errors":   { "type": "array" },
                            "warnings": { "type": "array" }
                        }
                    }
                }
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
            .ok_or_else(|| {
                Error::InvalidInput("workflow_write_structured requires `path`".into())
            })?;

        // Accept either shape, mirroring recipe_write_structured's tolerance:
        //
        //   {"path": "...", "workflow": {<document>}}      — canonical (the schema)
        //   {"path": "...", "workflow": {name}, "source": …, "step": […]}  — flattened
        //
        // The document always carries `step` (steps are required), so that key
        // disambiguates: if the `workflow` arg has its own `step`, it IS the
        // document; otherwise the model flattened the wrapper and the document is
        // the args minus `path`.
        let doc_owned: serde_json::Value;
        let doc: &serde_json::Value = match params.get("workflow") {
            Some(v) if v.is_object() && v.get("step").is_some() => v,
            _ => {
                let mut map = serde_json::Map::new();
                if let Some(obj) = params.as_object() {
                    for (k, v) in obj {
                        if k == "path" {
                            continue;
                        }
                        map.insert(k.clone(), v.clone());
                    }
                }
                if map.is_empty() {
                    return Err(Error::InvalidInput(
                        "workflow_write_structured requires either a `workflow` object \
                         argument or workflow fields (workflow, source, step) at the \
                         args root."
                            .into(),
                    ));
                }
                doc_owned = serde_json::Value::Object(map);
                &doc_owned
            }
        };

        // 1. JSON → TOML. Reuse the recipe author's sanitizer (drops the null-valued
        //    optional keys + repairs the stray-escaped-quote keys small models emit)
        //    and converter, so a well-formed workflow survives instead of forcing a
        //    raw-workflow_write fallback; the validator below catches anything real.
        let sanitized = sanitize_for_toml(doc);
        let toml_value = json_to_toml(&sanitized).map_err(|e| {
            Error::InvalidInput(format!("workflow → TOML conversion failed: {e}"))
        })?;
        let toml_text = toml_value_to_string(&toml_value)
            .map_err(|e| Error::InvalidInput(format!("TOML serialization failed: {e}")))?;

        // 2. Atomic write to <workflows>/<path>.toml (same scoping as workflow_write).
        let resolved = resolve_workflow_path(raw_path, self.workflows_dir.as_ref())?;
        if let Some(parent) = resolved.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                Error::InvalidInput(format!("failed to create parent {}: {e}", parent.display()))
            })?;
        }
        let part = resolved.with_extension("toml.part");
        fs::write(&part, &toml_text)
            .map_err(|e| Error::InvalidInput(format!("failed to write {}: {e}", part.display())))?;
        fs::rename(&part, &resolved).map_err(|e| {
            Error::InvalidInput(format!(
                "failed to commit {} → {}: {e}",
                part.display(),
                resolved.display(),
            ))
        })?;

        // 3. Validate the written TOML (parse + capability resolution) so the agent
        //    sees in this same response whether the workflow is ready to test.
        let validation = validate_workflow_toml(&toml_text).await;
        let validation_failed = validation
            .get("passed")
            .and_then(|v| v.as_bool())
            .map(|b| !b)
            .unwrap_or(false);

        let mut payload = json!({
            "path": resolved.display().to_string(),
            "bytes_written": toml_text.len(),
            "toml_preview": preview(&toml_text, 1200),
            "validation": validation,
        });
        if validation_failed {
            // Same in-turn nudge recipe_write_structured uses: keep the agent in the
            // read-errors → rewrite → re-validate cycle rather than yielding a plan.
            payload["nudge"] = json!(
                "Workflow is on disk but validation FAILED. Read `validation.errors`, \
                 fix the workflow, and call `workflow_write_structured` AGAIN in this \
                 same turn — don't yield to the partner with a narrated plan."
            );
        }
        Ok(StepOutput::Json(payload))
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
        Ok(StepOutput::Json(validate_workflow_toml(content).await))
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

    #[tokio::test]
    async fn structured_write_produces_valid_toml_and_passes_validation() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join(".sovereign/workflows");
        std::fs::create_dir_all(&root).unwrap();

        let tool = WorkflowWriteStructuredTool::with_workflows_dir(root.clone());
        let out = tool
            .execute(
                &serde_json::json!({
                    "path": "summarize-folder",
                    "workflow": {
                        "workflow": { "name": "summarize-folder" },
                        "source": { "type": "folder", "path": "{param.folder}", "glob": "*.md,*.txt" },
                        "step": [
                            {
                                "id": "summary",
                                "uses": "model:thoughtful",
                                "prompt": "Summarize this:\n\n{item.text}"
                            }
                        ]
                    }
                }),
                &ctx(),
            )
            .await
            .unwrap();

        let StepOutput::Json(v) = out else {
            panic!("expected json output");
        };
        // The tool generated the TOML (not the model), so the on-disk file is the
        // canonical shape and re-parses.
        let on_disk = root.join("summarize-folder.toml");
        assert!(on_disk.exists(), "structured write should land on disk");
        let body = std::fs::read_to_string(&on_disk).unwrap();
        assert!(body.contains("[workflow]"), "got:\n{body}");
        assert!(body.contains("name = \"summarize-folder\""));
        assert!(body.contains("[source]"));
        assert!(body.contains("type = \"folder\""));
        assert!(body.contains("[[step]]"));
        assert!(body.contains("uses = \"model:thoughtful\""));
        // A model: step → validation passes (no unresolved-step warnings).
        assert_eq!(v["validation"]["passed"], serde_json::json!(true), "report: {v}");
        assert_eq!(
            v["validation"]["warnings"],
            serde_json::json!([]),
            "a model: step should not be unresolved: {v}"
        );
    }

    #[tokio::test]
    async fn structured_write_accepts_flattened_args() {
        // Tolerant shape: the model puts workflow/source/step at the args root
        // instead of nesting under `workflow`. The tool still produces valid TOML.
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join(".sovereign/workflows");
        std::fs::create_dir_all(&root).unwrap();

        let tool = WorkflowWriteStructuredTool::with_workflows_dir(root.clone());
        let out = tool
            .execute(
                &serde_json::json!({
                    "path": "flat-flow",
                    "workflow": { "name": "flat-flow" },
                    "source": { "type": "inline", "items": ["one"] },
                    "step": [
                        { "id": "a", "uses": "transform:json" }
                    ]
                }),
                &ctx(),
            )
            .await
            .unwrap();

        let StepOutput::Json(v) = out else {
            panic!("expected json output");
        };
        let on_disk = root.join("flat-flow.toml");
        assert!(on_disk.exists());
        let body = std::fs::read_to_string(&on_disk).unwrap();
        assert!(body.contains("name = \"flat-flow\""), "got:\n{body}");
        assert!(body.contains("[[step]]"));
        assert_eq!(v["validation"]["passed"], serde_json::json!(true), "report: {v}");
    }

    #[tokio::test]
    async fn structured_write_rejects_paths_outside_root() {
        // Same scoping guarantee as `write_rejects_paths_outside_root`: an absolute
        // path outside ~/.sovereign/workflows/ is refused ("outside"). (A `..`
        // traversal is refused too, but with a distinct "traversal" message — see
        // `assert_under_root`; the absolute case is the canonical escape attempt.)
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join(".sovereign/workflows");
        std::fs::create_dir_all(&root).unwrap();
        let tool = WorkflowWriteStructuredTool::with_workflows_dir(root);
        let err = tool
            .execute(
                &serde_json::json!({
                    "path": "/tmp/evil.toml",
                    "workflow": { "workflow": { "name": "x" }, "step": [{ "id": "a", "uses": "transform:json" }] }
                }),
                &ctx(),
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("outside"), "got: {err}");
    }
}
