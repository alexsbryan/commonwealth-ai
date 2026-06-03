//! `design_signals_extract` — structural parser over a DESIGN.md doc.
//!
//! Wraps `corpus_engine_atos::design_signals::extract` as an MCP tool. The
//! underlying parser is pure Rust (pulldown-cmark AST + a keyword
//! scanner); this tool only adds IO (read-the-file) and a JSON
//! rendering suitable for an agent to reason over.
//!
//! ## Primary use-case
//!
//! Called by the design-session agent (see the brief written by
//! `sovereign project design`) after each substantive user edit, so
//! the agent can see what gaps remain without re-reading the full
//! doc. Also usable from any MCP client to audit a DESIGN.md quickly.
//!
//! ## Output shape
//!
//! ```json
//! {
//!   "design_path":   "DESIGN.md",
//!   "anchors":       ["Primary persistence: sqlite", …],
//!   "gap_count":     5,
//!   "gaps": [
//!     { "section": "Data & interfaces", "reason": "TbdMarker",
//!       "line": 18, "snippet": "TBD: wire format" },
//!     …
//!   ],
//!   "keywords": {
//!     "time": true, "persistence": true, "api": false, "queue": false,
//!     "concurrency": true, "secrets": false, "consumers": true
//!   },
//!   "sections": [
//!     { "heading": "Anchors",           "level": 2, "line": 3 },
//!     { "heading": "Data & interfaces", "level": 2, "line": 10 },
//!     …
//!   ]
//! }
//! ```

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{json, Value};

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

pub struct DesignSignalsExtractTool {
    /// Optional project root. When set, relative `design_path`
    /// arguments resolve under it; absolute paths are used as-is.
    /// Mirrors the CheckDocPathsTool convention for consistency.
    project_root: Option<PathBuf>,
}

impl DesignSignalsExtractTool {
    pub fn new() -> Self {
        Self { project_root: None }
    }

    pub fn with_project_root(mut self, root: PathBuf) -> Self {
        self.project_root = Some(root);
        self
    }
}

impl Default for DesignSignalsExtractTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for DesignSignalsExtractTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "design_signals_extract".to_string(),
            name: "Design Signals Extract".to_string(),
            description: "Parse a DESIGN.md file and return the structural signals the \
                 solo-mode fallback and agent-collaborative session both rely \
                 on: the Anchors block's bullets, structural gaps (TBD \
                 markers, empty/placeholder sections, open X-vs-Y choices, \
                 literal question sentences), and keyword-bucket presence \
                 flags (time / persistence / api / queue / concurrency / \
                 secrets / consumers). Strictly structural — does NOT \
                 interpret semantics. Run after each substantive edit to a \
                 DESIGN.md to see which gaps the user should still resolve."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "design_path": {
                        "type": "string",
                        "description": "Path to the DESIGN.md file. Defaults \
                                        to `DESIGN.md` at the project root \
                                        (the canonical location). Absolute \
                                        paths are used as-is."
                    }
                },
                "required": []
            }),
            examples: vec![
                ToolExample {
                    situation: "The user just edited DESIGN.md — check which \
                         structural gaps remain before asking another \
                         question."
                        .into(),
                    call: json!({ "design_path": "DESIGN.md" }),
                },
                ToolExample {
                    situation: "Running from a subdirectory; verify the doc at \
                         the known project root."
                        .into(),
                    call: json!({ "design_path": "/absolute/path/to/DESIGN.md" }),
                },
            ],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Instant,
            scope: Scope::Session,
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "design_path": { "type": "string" },
                    "anchors":     { "type": "array", "items": { "type": "string" } },
                    "gap_count":   { "type": "integer" },
                    "gaps":        { "type": "array", "items": {
                        "type": "object",
                        "properties": {
                            "section": { "type": "string" },
                            "reason":  { "type": "string" },
                            "line":    { "type": "integer" },
                            "snippet": { "type": "string" }
                        }
                    } },
                    "keywords":    { "type": "object" },
                    "sections":    { "type": "array", "items": {
                        "type": "object",
                        "properties": {
                            "heading": { "type": "string" },
                            "level":   { "type": "integer" },
                            "line":    { "type": "integer" }
                        }
                    } }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    fn validate(&self, _params: &Value) -> Result<()> {
        // All params are optional; the tool defaults to `DESIGN.md`
        // under the project root. Validation happens at `execute`
        // time where we can surface a file-not-found message
        // pointing at exactly what we tried.
        Ok(())
    }

    async fn execute(&self, params: &Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let raw_path = params
            .get("design_path")
            .and_then(|v| v.as_str())
            .unwrap_or("DESIGN.md");

        let resolved =
            resolve_design_path(raw_path, self.project_root.as_deref()).ok_or_else(|| {
                Error::Tool {
                    tool_id: "design_signals_extract".to_string(),
                    message: format!(
                        "could not resolve '{raw_path}' — provide an absolute path, \
                     or configure the tool with a project_root."
                    ),
                }
            })?;

        let text = std::fs::read_to_string(&resolved).map_err(|e| Error::Tool {
            tool_id: "design_signals_extract".to_string(),
            message: format!("could not read '{}': {e}", resolved.display()),
        })?;

        let signals = corpus_engine_atos::design_signals::extract(&text);

        Ok(StepOutput::Json(render_signals_json(&resolved, &signals)))
    }
}

// ─── Resolution ───────────────────────────────────────────────────

fn resolve_design_path(path_str: &str, project_root: Option<&Path>) -> Option<PathBuf> {
    let p = Path::new(path_str);
    if p.is_absolute() {
        return Some(p.to_path_buf());
    }
    if let Some(root) = project_root {
        return Some(root.join(p));
    }
    std::env::current_dir().ok().map(|c| c.join(p))
}

// ─── JSON rendering ───────────────────────────────────────────────

fn render_signals_json(
    path: &Path,
    signals: &corpus_engine_atos::design_signals::DesignSignals,
) -> Value {
    let anchors: Vec<Value> = signals
        .anchors
        .iter()
        .map(|a| Value::String(a.text.clone()))
        .collect();

    let gaps: Vec<Value> = signals
        .gaps
        .iter()
        .map(|g| {
            json!({
                "section": g.section,
                "reason":  gap_reason_label(&g.reason),
                "line":    g.line,
                "snippet": g.snippet,
            })
        })
        .collect();

    let sections: Vec<Value> = signals
        .sections
        .iter()
        .map(|s| {
            json!({
                "heading": s.heading,
                "level":   s.level,
                "line":    s.heading_line,
            })
        })
        .collect();

    let k = &signals.keywords;
    let keywords = json!({
        "time":        k.time,
        "persistence": k.persistence,
        "api":         k.api,
        "queue":       k.queue,
        "concurrency": k.concurrency,
        "secrets":     k.secrets,
        "consumers":   k.consumers,
    });

    json!({
        "design_path": path.to_string_lossy(),
        "anchors":     anchors,
        "gap_count":   signals.gaps.len(),
        "gaps":        gaps,
        "keywords":    keywords,
        "sections":    sections,
    })
}

fn gap_reason_label(reason: &corpus_engine_atos::design_signals::GapReason) -> &'static str {
    use corpus_engine_atos::design_signals::GapReason::*;
    match reason {
        TbdMarker => "TbdMarker",
        EmptySection => "EmptySection",
        UnclearMarker => "UnclearMarker",
        OpenChoice => "OpenChoice",
        LiteralQuestion => "LiteralQuestion",
    }
}

// ─── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, body: &str) {
        fs::write(path, body).unwrap();
    }

    #[tokio::test]
    async fn extracts_signals_from_design_md() {
        let tmp = tempfile::tempdir().unwrap();
        let design = tmp.path().join("DESIGN.md");
        write(
            &design,
            "# Project — Design\n\n## Anchors\n\n- Primary persistence: sqlite\n- Primary interface: HTTP\n- Language: Rust\n\n## Data & interfaces\n\nTBD: wire format\n",
        );

        let tool = DesignSignalsExtractTool::new().with_project_root(tmp.path().to_path_buf());
        let ctx = ToolContext {
            conversation_id: "design-signals-test".to_string(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
        };
        let out = tool
            .execute(&json!({ "design_path": "DESIGN.md" }), &ctx)
            .await
            .expect("execute");
        let StepOutput::Json(v) = out else {
            panic!("expected Json output");
        };

        let anchors = v["anchors"].as_array().unwrap();
        assert_eq!(anchors.len(), 3);
        assert_eq!(anchors[0], "Primary persistence: sqlite");

        assert!(v["gap_count"].as_u64().unwrap() >= 1);
        let reasons: Vec<&str> = v["gaps"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| g["reason"].as_str().unwrap())
            .collect();
        assert!(
            reasons.contains(&"TbdMarker"),
            "expected TbdMarker gap in {reasons:?}"
        );

        assert_eq!(v["keywords"]["persistence"], Value::Bool(true));
    }

    #[tokio::test]
    async fn reports_missing_file_clearly() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = DesignSignalsExtractTool::new().with_project_root(tmp.path().to_path_buf());
        let ctx = ToolContext {
            conversation_id: "design-signals-test".to_string(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
        };
        let err = tool
            .execute(&json!({ "design_path": "MISSING.md" }), &ctx)
            .await
            .expect_err("should error on missing file");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("could not read") && msg.contains("MISSING.md"),
            "error should name the missing file; got: {msg}"
        );
    }

    #[tokio::test]
    async fn defaults_to_design_md_under_project_root() {
        let tmp = tempfile::tempdir().unwrap();
        let design = tmp.path().join("DESIGN.md");
        write(&design, "# X\n\n## Plan\n\nstuff.\n");
        let tool = DesignSignalsExtractTool::new().with_project_root(tmp.path().to_path_buf());
        let ctx = ToolContext {
            conversation_id: "design-signals-test".to_string(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
        };
        // Omit `design_path` entirely — should default to DESIGN.md.
        let out = tool.execute(&json!({}), &ctx).await.expect("execute");
        let StepOutput::Json(v) = out else { panic!() };
        assert!(v["design_path"].as_str().unwrap().ends_with("DESIGN.md"));
    }

    #[tokio::test]
    async fn absolute_path_bypasses_project_root() {
        let tmp = tempfile::tempdir().unwrap();
        let design = tmp.path().join("elsewhere.md");
        write(&design, "# Y\n\n## Anchors\n\n- one\n- two\n- three\n");
        // Note: no project_root set on the tool — absolute path must still work.
        let tool = DesignSignalsExtractTool::new();
        let ctx = ToolContext {
            conversation_id: "design-signals-test".to_string(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
        };
        let out = tool
            .execute(&json!({ "design_path": design.to_string_lossy() }), &ctx)
            .await
            .expect("execute");
        let StepOutput::Json(v) = out else { panic!() };
        assert_eq!(v["anchors"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn descriptor_declares_read_effect_and_idempotent() {
        let tool = DesignSignalsExtractTool::new();
        let d = tool.descriptor();
        assert_eq!(d.id, "design_signals_extract");
        assert!(matches!(d.effect, Effect::Read));
        assert!(matches!(d.idempotency, Idempotency::Idempotent));
        // No required params — default works.
        let required = d.parameters.get("required").unwrap().as_array().unwrap();
        assert!(required.is_empty());
    }

    #[test]
    fn validate_accepts_empty_params() {
        let tool = DesignSignalsExtractTool::new();
        tool.validate(&json!({})).expect("empty params ok");
        tool.validate(&json!({ "design_path": "foo.md" }))
            .expect("path param ok");
    }
}
