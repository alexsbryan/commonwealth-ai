//! `blast_radius` — transitive impact analysis for a symbol.
//!
//! Performs a BFS traversal over the SCIP call graph to find all callers
//! at every depth level. Separates production callers from test callers
//! and groups each by module for readability.
//!
//! Use before modifying a function signature, removing a method, or
//! changing a trait definition to understand the full scope of impact.

use std::collections::BTreeMap;
use std::sync::Arc;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine::scip_graph::{BlastEntry, ScipGraph, StalenessCaution};

use super::is_valid_symbol_name;

pub type ScipGraphHandleRef = Arc<ArcSwap<ScipGraph>>;

pub struct BlastRadiusTool {
    graph: ScipGraphHandleRef,
}

impl BlastRadiusTool {
    pub fn new(graph: ScipGraphHandleRef) -> Self {
        Self { graph }
    }
}

#[async_trait]
impl Tool for BlastRadiusTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "blast_radius".to_string(),
            name: "Blast Radius".to_string(),
            description: "Compute the transitive impact of changing a symbol: \
                          all callers at every depth level up to max_depth. \
                          Use before modifying a function signature, removing a method, \
                          or changing a trait definition. Separates production callers \
                          from test callers and groups by module. Backed by the SCIP \
                          call graph — compiler-resolved, not grep. \
                          IMPORTANT: Before using on a large refactor, call \
                          read_notes(kinds=[\"reflection\"], query=\"blast_radius\") \
                          to check for known limitations recorded by previous sessions \
                          (e.g. macro-generated call sites not traversed by SCIP)."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "symbol": {
                        "type": "string",
                        "description": "Symbol name to analyse (function, method, struct, trait)"
                    },
                    "max_depth": {
                        "type": "integer",
                        "default": 3,
                        "description": "BFS depth (1=direct callers, 2=callers of callers, …). Capped at 5."
                    },
                    "max_symbols": {
                        "type": "integer",
                        "default": 100,
                        "description": "Maximum total callers to return. Capped at 200."
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
            .ok_or_else(|| Error::InvalidInput("blast_radius requires 'symbol'".to_string()))?;
        if !is_valid_symbol_name(symbol) {
            return Err(Error::InvalidInput(format!(
                "invalid symbol name '{symbol}'"
            )));
        }
        Ok(())
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let symbol = params
            .get("symbol")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("missing 'symbol'".to_string()))?;

        let max_depth = params
            .get("max_depth")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(3);

        let max_symbols = params
            .get("max_symbols")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(100);

        let graph = self.graph.load_full();
        let result = graph
            .blast_radius(symbol, max_depth, max_symbols)
            .await
            .map_err(|e| Error::Tool {
                tool_id: "blast_radius".to_string(),
                message: e.to_string(),
            })?;

        if result.entries.is_empty() {
            return Ok(StepOutput::Json(json!({
                "symbol": symbol,
                "production": {},
                "tests": {},
                "total": 0,
                "capped": false,
                "depth_reached": 0,
                "staleness": staleness_label(&result.caution),
                "hint": "No callers found — symbol may be unused, unexported, or not yet in the call graph. Run `sovereign project refresh` if the graph is stale."
            })));
        }

        // Separate production from test callers.
        let (prod_entries, test_entries): (Vec<_>, Vec<_>) =
            result.entries.iter().partition(|e| !e.is_test);

        let production = group_by_module(&prod_entries);
        let tests = group_by_module(&test_entries);

        Ok(StepOutput::Json(json!({
            "symbol": symbol,
            "production": production,
            "tests": tests,
            "total": result.entries.len(),
            "capped": result.capped,
            "depth_reached": result.depth_reached,
            "staleness": staleness_label(&result.caution),
            "staleness_note": result.caution.format_note().trim().to_string()
        })))
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Group blast entries by module name, returning a JSON object.
///
/// Module extraction: strip leading `src/`, `crates/`, `tests/`; take up to
/// two path segments; remove the filename portion.
///
/// Examples:
/// - `crates/foo/src/bar/baz.rs` → `"foo"`
/// - `src/auth/login.rs` → `"auth"`
/// - `main.rs` → `"(root)"`
fn group_by_module(entries: &[&BlastEntry]) -> serde_json::Value {
    let mut by_module: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();

    for entry in entries {
        let module = extract_module(&entry.file_path);
        by_module.entry(module).or_default().push(json!({
            "symbol": entry.symbol_name,
            "file": entry.file_path,
            "line": entry.line
        }));
    }

    serde_json::Value::Object(
        by_module
            .into_iter()
            .map(|(k, v)| (k, serde_json::Value::Array(v)))
            .collect(),
    )
}

fn extract_module(file_path: &str) -> String {
    // Normalise separators.
    let path = file_path.replace('\\', "/");

    // Strip well-known prefixes.
    let stripped = strip_prefix(&path, &["crates/", "src/", "tests/", "test/"]);

    // Take the first component (the crate or top-level dir name).
    let first = stripped
        .split('/')
        .next()
        .unwrap_or("")
        .trim_end_matches(".rs");

    if first.is_empty() {
        "(root)".to_string()
    } else {
        first.to_string()
    }
}

fn strip_prefix<'a>(s: &'a str, prefixes: &[&str]) -> &'a str {
    for prefix in prefixes {
        if let Some(rest) = s.strip_prefix(prefix) {
            return rest;
        }
    }
    s
}

fn staleness_label(caution: &StalenessCaution) -> &'static str {
    match caution {
        StalenessCaution::None => "none",
        StalenessCaution::SomeCallSitesMayBeStale { .. } => "some_files_may_be_stale",
        StalenessCaution::GraphIsAging { .. } => "aging",
        StalenessCaution::GraphIsStale { .. } => "stale",
        StalenessCaution::LanguageNotIndexed { .. } => "stale",
    }
}
