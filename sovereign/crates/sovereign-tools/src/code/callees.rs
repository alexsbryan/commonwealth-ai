//! `find_callees` — list all functions/methods that a given symbol calls.
//!
//! Backed by the SCIP symbol graph. Results include a staleness note when
//! the graph is not fresh. The note is empty when the graph is current —
//! no noise in the common case.

use std::collections::BTreeMap;
use std::sync::Arc;

use arc_swap::ArcSwap;
use async_trait::async_trait;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine::CorpusEngine;
use corpus_engine_scip::scip_graph::{CallKind, ScipGraph};

use super::index_health::IndexHealthChecker;
use super::is_valid_symbol_name;

/// A hot-reloadable SCIP graph handle. The server's polling task may swap
/// the inner `Arc<ScipGraph>` while the tool is executing; every query
/// does `load_full()` to grab the current graph atomically.
pub type ScipGraphHandle = Arc<ArcSwap<ScipGraph>>;

pub struct FindCalleesTool {
    #[allow(dead_code)]
    engine: Arc<CorpusEngine>,
    graph: ScipGraphHandle,
    checker: Option<Arc<IndexHealthChecker>>,
}

impl FindCalleesTool {
    pub fn new(engine: Arc<CorpusEngine>, graph: ScipGraphHandle) -> Self {
        Self {
            engine,
            graph,
            checker: None,
        }
    }

    pub fn with_health_checker(mut self, checker: Arc<IndexHealthChecker>) -> Self {
        self.checker = Some(checker);
        self
    }
}

#[async_trait]
impl Tool for FindCalleesTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "callees".to_string(),
            name: "Find Callees".to_string(),
            description: "Find all functions and methods that a given symbol calls, \
                          using the SCIP symbol graph. More precise than parsing \
                          function bodies — resolves trait dispatch and method calls \
                          that body-parsing misses. Staleness is communicated in \
                          the response when the graph is not fresh. Call \
                          `sovereign corpus scip <corpus-id>` to refresh."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "symbol": {
                        "type": "string",
                        "description": "Exact symbol name"
                    }
                },
                "required": ["symbol"]
            }),
            examples: vec![
                ToolExample {
                    situation: "You're about to read an entire function body to understand what it calls. Don't — this returns the exact outbound call graph, including trait dispatch that reading source would miss.".into(),
                    call: serde_json::json!({ "symbol": "handle_tools_call" }),
                },
                ToolExample {
                    situation: "You need to understand what a function depends on before refactoring it. Knowing its callees tells you what interfaces must remain stable.".into(),
                    call: serde_json::json!({ "symbol": "run_embed_batch_sync" }),
                },
            ],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Persistent,
            output_schema: Some(serde_json::json!({
                "type": "string",
                "description": "Markdown list of outbound call sites, one per line, \
                                with `file:line` locations. Empty-result line when \
                                the symbol has no outbound calls or is unknown."
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        let symbol = params
            .get("symbol")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("find_callees requires 'symbol'".to_string()))?;
        if !is_valid_symbol_name(symbol) {
            return Err(Error::InvalidInput(format!(
                "invalid symbol name '{symbol}': must be alphanumeric plus _, ::, or $, and \u{2264}256 chars"
            )));
        }
        Ok(())
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let symbol = params
            .get("symbol")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("missing 'symbol'".to_string()))?;

        let graph = self.graph.load_full();
        let (callees, caution) = graph.find_callees(symbol).await.map_err(|e| Error::Tool {
            tool_id: "callees".to_string(),
            message: e.to_string(),
        })?;

        if callees.is_empty() {
            return Ok(StepOutput::Text(format!(
                "`{symbol}` has no recorded outbound calls in the symbol graph.\n\
                 This may mean:\n\
                 - The symbol is a leaf function with no calls\n\
                 - The symbol was added after the last graph export\n\
                 - The symbol name doesn't match exactly \u{2014} try `symbol_lookup` \
                   to verify the name, then retry{staleness}",
                staleness = caution.format_note(),
            )));
        }

        let mut out = format!("**`{symbol}` calls:**\n\n");

        // Group by file for readability.
        let mut by_file: BTreeMap<&str, Vec<_>> = BTreeMap::new();
        for callee in &callees {
            by_file.entry(&callee.file_path).or_default().push(callee);
        }

        for (file, calls) in &by_file {
            if by_file.len() > 1 {
                out.push_str(&format!("*{file}:*\n"));
            }
            for c in calls.iter().take(20) {
                let kind_note = match c.call_kind {
                    CallKind::Dynamic => " *(dynamic dispatch)*",
                    CallKind::Trait => " *(trait)*",
                    _ => "",
                };
                out.push_str(&format!(
                    "- `{}` line {}{}\n",
                    c.symbol_name, c.line, kind_note
                ));
            }
            if calls.len() > 20 {
                out.push_str(&format!("  \u{2026}and {} more\n", calls.len() - 20));
            }
        }

        out.push_str(&caution.format_note());
        if let Some(checker) = &self.checker {
            let health = checker.check().await;
            if let Some(hint) = &health.hint {
                out.push_str(&format!(
                    "\n\n---\nIndex: {} | {} symbols | {} stale files\n{}",
                    format!("{:?}", health.staleness).to_lowercase(),
                    health.symbol_count,
                    health.stale_file_count,
                    hint
                ));
            }
        }
        Ok(StepOutput::Text(out))
    }
}
