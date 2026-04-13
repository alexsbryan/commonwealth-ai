//! `find_callers` — list all call sites of a given symbol.
//!
//! Backed by the SCIP symbol graph. No false positives from string
//! matching — results are compiler-resolved. Use `depth=2` to find
//! callers of callers (impact radius).

use std::sync::Arc;

use async_trait::async_trait;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine::scip_graph::ScipGraph;
use corpus_engine::CorpusEngine;

use super::is_valid_symbol_name;

pub struct FindCallersTool {
    #[allow(dead_code)]
    engine: Arc<CorpusEngine>,
    graph: Arc<ScipGraph>,
}

impl FindCallersTool {
    pub fn new(engine: Arc<CorpusEngine>, graph: Arc<ScipGraph>) -> Self {
        Self { engine, graph }
    }
}

#[async_trait]
impl Tool for FindCallersTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "find_callers".to_string(),
            name: "Find Callers".to_string(),
            description: "Find all call sites of a given symbol in the codebase, \
                          using the SCIP symbol graph. No false positives from \
                          string matching — results are compiler-resolved. Use \
                          depth=2 to find callers of callers (impact radius). \
                          Staleness is communicated in the response when the graph \
                          is not fresh."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "symbol": {
                        "type": "string",
                        "description": "Exact symbol name"
                    },
                    "depth": {
                        "type": "integer",
                        "description": "Traversal depth: 1 (direct callers only) or 2 (callers of callers). Default: 1.",
                        "default": 1
                    }
                },
                "required": ["symbol"]
            }),
            examples: vec![],
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        let symbol = params
            .get("symbol")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("find_callers requires 'symbol'".to_string()))?;
        if !is_valid_symbol_name(symbol) {
            return Err(Error::InvalidInput(format!(
                "invalid symbol name '{symbol}': must be alphanumeric plus _, ::, or $, and \u{2264}256 chars"
            )));
        }
        Ok(())
    }

    async fn execute(
        &self,
        params: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<StepOutput> {
        let symbol = params
            .get("symbol")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("missing 'symbol'".to_string()))?;

        let depth = params
            .get("depth")
            .and_then(|v| v.as_u64())
            .unwrap_or(1)
            .min(2) as usize;

        let (callers, caution) = self
            .graph
            .find_callers(symbol, depth)
            .await
            .map_err(|e| Error::Tool {
                tool_id: "find_callers".to_string(),
                message: e.to_string(),
            })?;

        if callers.is_empty() {
            return Ok(StepOutput::Text(format!(
                "No callers of `{symbol}` found in the symbol graph.\n\
                 This symbol may be a public API entry point, called only \
                 from outside the indexed codebase, or added after the last \
                 graph export.{staleness}",
                staleness = caution.format_note(),
            )));
        }

        let mut out = format!(
            "**`{symbol}` is called by** ({} site{}):\n\n",
            callers.len(),
            if callers.len() == 1 { "" } else { "s" }
        );

        for c in callers.iter().take(20) {
            out.push_str(&format!(
                "- `{}` in `{}` line {}\n",
                c.symbol_name, c.file_path, c.line
            ));
        }

        if callers.len() > 20 {
            out.push_str(&format!("\u{2026}and {} more\n", callers.len() - 20));
        }

        if depth == 2 {
            out.push_str("\n*(depth-2 traversal \u{2014} includes callers of callers)*\n");
        }

        out.push_str(&caution.format_note());
        Ok(StepOutput::Text(out))
    }
}
