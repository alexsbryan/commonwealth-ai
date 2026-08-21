// SPDX-License-Identifier: AGPL-3.0-or-later
//! `extract` — the workflow **document-text** leaf: read a file and return its
//! text, dispatched by extension (PDF / Office / HTML / epub / Markdown / txt).
//!
//! The same extractor the watched-folder corpus ingest uses
//! ([`crate::local_corpus::extract_stage::extract_text`]), exposed as a workflow
//! tool — so a workflow's `folder → extract → chunk → embed → corpus_store`
//! notebook reads a PDF/Office/HTML file *identically* to a recipe ingest (no
//! quality fork; that's the recipe×workflow convergence at the leaf level). Pairs
//! with the folder source's `{item.text}`, which only covers UTF-8 text files:
//! `tool:extract` handles the binary/structured formats `{item.text}` skips.
//!
//! `Read`-effect + idempotent: pure over the file, so the workflow cache skips it
//! on an unchanged file. An unsupported extension or a corrupt file is a loud
//! per-item error (panic-safe — a bad PDF fails its own item, not the run). No
//! size cap: the output feeds the chunker, not a prompt.


use sovereign_core::error::{Error, Result};
use sovereign_core::types::*;
use std::sync::Arc;
use sovereign_core::tool_manifest::DeclaredTool;

pub struct ExtractTool;

impl ExtractTool {
    /// Bind this tool's state to its `extract` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        sovereign_core::tool_manifest::declared("extract", move |params, ctx| {
            let state = Arc::clone(&state);
            async move { state.run(&params, &ctx).await }
        })
    }

    /// The executable half of `extract`.
    async fn run(
        &self,
        params: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<StepOutput> {
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Execution("extract: missing required `path`".into()))?;
        let text = crate::local_corpus::extract_stage::extract_text(std::path::Path::new(path))
            .map_err(|e| Error::Execution(format!("extract: {path}: {e}")))?;
        Ok(StepOutput::Text(text))
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
            ..Default::default()
        }
    }

    /// txt + md extract to text; a missing path and an unsupported extension are
    /// loud errors (not panics).
    #[tokio::test]
    async fn extract_reads_text_formats_and_errors_loudly() {
        let dir = tempfile::tempdir().unwrap();
        let txt = dir.path().join("a.txt");
        std::fs::write(&txt, "plain body").unwrap();
        let md = dir.path().join("b.md");
        std::fs::write(&md, "# Title\n\nbody text").unwrap();

        let out = ExtractTool
            .run(
                &serde_json::json!({ "path": txt.to_string_lossy() }),
                &ctx(),
            )
            .await
            .unwrap();
        match out {
            StepOutput::Text(t) => assert!(t.contains("plain body")),
            o => panic!("expected text, got {o:?}"),
        }

        // Markdown extracts (the body text comes back).
        let md_out = ExtractTool
            .run(&serde_json::json!({ "path": md.to_string_lossy() }), &ctx())
            .await
            .unwrap();
        assert!(matches!(md_out, StepOutput::Text(t) if t.contains("body text")));

        // Missing path → loud error.
        assert!(ExtractTool
            .run(&serde_json::json!({}), &ctx())
            .await
            .is_err());
        // Unsupported extension → loud error (not a panic).
        let bin = dir.path().join("c.bin");
        std::fs::write(&bin, "x").unwrap();
        assert!(ExtractTool
            .run(
                &serde_json::json!({ "path": bin.to_string_lossy() }),
                &ctx()
            )
            .await
            .is_err());
    }
}
