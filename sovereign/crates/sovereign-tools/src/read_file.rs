// SPDX-License-Identifier: AGPL-3.0-or-later
//! `read_file` — the workflow **text read** leaf: surface one file's contents as
//! a `Text` artifact.
//!
//! The single-file counterpart to the folder source's `{item.text}` (which only
//! covers files enumerated by a `folder` source) and to [`crate::read_json`]
//! (which parses JSON): pull a named file's text into a step — a path you know, a
//! path a prior step produced — for a `model:` or `transform:` step to consume.
//! Size-capped so a stray large file can't blow the prompt budget; `Read`-effect
//! and idempotent, so the content cache skips it on an unchanged file.

use async_trait::async_trait;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

/// Cap on returned text (bytes). A larger file is truncated with a marker —
/// matches the FileTool read cap so behaviour is consistent across surfaces.
const MAX_READ_BYTES: usize = 1 << 20; // 1 MiB

pub struct ReadFileTool;

#[async_trait]
impl Tool for ReadFileTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "read_file".to_string(),
            name: "read_file".to_string(),
            description: "Read a text file `path` and surface its contents as text for a \
                          downstream model/transform step. Size-capped (large files are \
                          truncated). Use read_json for structured data."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File to read" }
                },
                "required": ["path"]
            }),
            examples: vec![],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Session,
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
            .ok_or_else(|| Error::Execution("read_file: missing required `path`".into()))?;
        let content = std::fs::read_to_string(path)
            .map_err(|e| Error::Execution(format!("read_file: read {path}: {e}")))?;

        if content.len() > MAX_READ_BYTES {
            // Truncate on a char boundary so the slice is valid UTF-8.
            let mut end = MAX_READ_BYTES;
            while end > 0 && !content.is_char_boundary(end) {
                end -= 1;
            }
            Ok(StepOutput::Text(format!(
                "{}\n\n[truncated: {} bytes total]",
                &content[..end],
                content.len()
            )))
        } else {
            Ok(StepOutput::Text(content))
        }
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

    /// Returns a file's text as a Text artifact and fails loud on a missing file.
    #[tokio::test]
    async fn read_file_surfaces_text() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("note.md");
        std::fs::write(&p, "# Standup\nShipped the substrate.").unwrap();

        let out = ReadFileTool
            .execute(&serde_json::json!({ "path": p.to_string_lossy() }), &ctx())
            .await
            .unwrap();
        match out {
            StepOutput::Text(t) => assert_eq!(t, "# Standup\nShipped the substrate."),
            o => panic!("expected text, got {o:?}"),
        }

        // A missing file is a loud error.
        assert!(ReadFileTool
            .execute(&serde_json::json!({ "path": "/no/such/file.md" }), &ctx())
            .await
            .is_err());
    }
}
