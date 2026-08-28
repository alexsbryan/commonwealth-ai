// SPDX-License-Identifier: AGPL-3.0-or-later
//! `find_callees` — list all functions/methods that a given symbol calls.
//!
//! Backed by the SCIP symbol graph. Results include a staleness note when
//! the graph is not fresh. The note is empty when the graph is current —
//! no noise in the common case.

use std::collections::BTreeMap;
use std::sync::Arc;

use arc_swap::ArcSwap;

use sovereign_core::error::{Error, Result};
use sovereign_core::types::*;

use corpus_engine::CorpusEngine;
use corpus_engine_scip::scip_graph::{CallKind, ScipGraph};

use super::index_health::IndexHealthChecker;
use super::is_valid_symbol_name;
use sovereign_core::tool_manifest::DeclaredTool;

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

impl FindCalleesTool {
    /// Bind this tool's state to its `callees` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        let run_state = Arc::clone(&state);
        sovereign_core::tool_manifest::declared("callees", move |params, ctx| {
            let state = Arc::clone(&run_state);
            async move { state.run(&params, &ctx).await }
        })
        .with_validate({
            let state = Arc::clone(&state);
            Arc::new(move |p: &serde_json::Value| state.validate_extra(p))
        })
    }

    /// The executable half of `callees`.
    async fn run(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
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

    fn validate_extra(&self, params: &serde_json::Value) -> Result<()> {
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
}
