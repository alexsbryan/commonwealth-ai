// SPDX-License-Identifier: AGPL-3.0-or-later
//! `write_file` — the workflow **text write** leaf: persist a string to a path.
//!
//! The human-readable counterpart of [`crate::write_json`]: where `write_json`
//! validates and pretty-prints a JSON value, this writes raw text — a model's
//! summary, a rendered digest, any string an upstream step produced — so a
//! workflow can leave behind a folder of Markdown a person actually reads, not a
//! dev artifact. `content` arrives via templating (`content = "{summary.output}"`):
//! a JSON string is unwrapped to its text; any other value is written as its
//! pretty JSON form (a sensible fallback, not the intended path).
//!
//! Effect is `Write` (the content cache never skips it); the plain overwrite makes
//! re-execution idempotent; parent directories are created so a path like
//! `summaries/{item.stem}.md` just works without a prior mkdir.

use async_trait::async_trait;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

pub struct WriteFileTool;

#[async_trait]
impl Tool for WriteFileTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "write_file".to_string(),
            name: "write_file".to_string(),
            description: "Write text to a file `path` (e.g. content = {summary.output}). Creates \
                          parent directories and overwrites. Use this for human-readable output \
                          (Markdown, plain text); use write_json for structured data."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File to write (parent dirs created)" },
                    "content": { "type": "string", "description": "Text to write — e.g. {summary.output}" }
                },
                "required": ["path", "content"]
            }),
            examples: vec![],
            // A real external side effect (writes a file), so the content cache
            // must never skip it. The overwrite makes re-execution idempotent.
            effect: Effect::Write,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Persistent,
            output_schema: None,
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Execution("write_file: missing required `path`".into()))?;
        let raw = params
            .get("content")
            .ok_or_else(|| Error::Execution("write_file: missing required `content`".into()))?;

        // Templating delivers an upstream string artifact (a model's text output)
        // as a JSON string — write its text, not the quoted-and-escaped form. A
        // structured value is written as its pretty JSON (best-effort fallback).
        let body = match raw {
            serde_json::Value::String(s) => s.clone(),
            other => serde_json::to_string_pretty(other)
                .map_err(|e| Error::Execution(format!("write_file: serialize: {e}")))?,
        };

        // Create parent dirs so a path like `summaries/{item.stem}.md` works
        // without a prior mkdir — the common case for a workflow's terminal write.
        let p = std::path::Path::new(path);
        if let Some(parent) = p.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    Error::Execution(format!("write_file: create {}: {e}", parent.display()))
                })?;
            }
        }
        std::fs::write(p, body.as_bytes())
            .map_err(|e| Error::Execution(format!("write_file: write {path}: {e}")))?;

        Ok(StepOutput::Text(format!(
            "wrote {} bytes to {path}",
            body.len()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        ToolContext {
            conversation_id: Default::default(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
        }
    }

    /// Definition of done: writes a model's text output verbatim (not JSON-quoted),
    /// creates missing parent dirs, overwrites idempotently, and fails loud on a
    /// missing param. CI-safe — temp dir, no daemon.
    #[tokio::test]
    async fn write_file_persists_text_and_creates_dirs() {
        let dir = tempfile::tempdir().unwrap();
        // A nested dir that does NOT exist yet — the leaf must create it.
        let out = dir.path().join("summaries/secret-agent.md");

        // The shape `{summary.output}` carries: a model's text, delivered by
        // templating as a JSON *string*.
        let summary = "Mr Verloc keeps a shop and a secret.\n\n- He is an agent provocateur.";
        let params = serde_json::json!({
            "path": out.to_string_lossy(),
            "content": serde_json::Value::String(summary.to_string()),
        });

        WriteFileTool.execute(&params, &ctx()).await.unwrap();

        // The text is written verbatim — no surrounding quotes, no escaping — and
        // the missing parent dir was created.
        let written = std::fs::read_to_string(&out).unwrap();
        assert_eq!(written, summary, "text must be written verbatim");

        // Missing required params are loud errors.
        assert!(WriteFileTool
            .execute(&serde_json::json!({ "content": "x" }), &ctx())
            .await
            .is_err());
        assert!(WriteFileTool
            .execute(
                &serde_json::json!({ "path": out.to_string_lossy() }),
                &ctx()
            )
            .await
            .is_err());
    }
}
