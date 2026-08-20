// SPDX-License-Identifier: AGPL-3.0-or-later
//! `capability_map` — derive a map of *what a codebase does* from the SCIP call
//! graph, on demand, over MCP.
//!
//! A capability is a cluster of entry points (CLI verbs, HTTP routes, tools,
//! handlers) that share a reachable call spine. Deterministic — no model. The
//! heavy lifting lives in the pure `corpus_engine_scip::capability_map` module;
//! this tool just resolves the corpus, loads its graph, and renders the result.

use std::path::PathBuf;

use async_trait::async_trait;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine_scip::{build_capability_map, MapOptions, ProviderKind, ScipGraph};

pub struct CapabilityMapTool {
    /// The `~/.svrnmesh/indexes` directory (or `$SOVEREIGN_DATA_DIR/indexes`).
    indexes_dir: PathBuf,
}

impl Default for CapabilityMapTool {
    fn default() -> Self {
        Self::new()
    }
}

impl CapabilityMapTool {
    /// Resolve the indexes dir the same way the runtime locates code corpora:
    /// the shared data root (`rebrand::data_dir()`) `/indexes`.
    pub fn new() -> Self {
        Self {
            indexes_dir: sovereign_contracts::rebrand::data_dir().join("indexes"),
        }
    }

    /// Explicit indexes dir — for tests and callers with a configured data dir.
    pub fn with_indexes_dir(indexes_dir: PathBuf) -> Self {
        Self { indexes_dir }
    }

    /// Code corpora = subdirectories of the indexes dir that carry a
    /// `scip_graph.db` (the robust "this is a code corpus" signal).
    fn code_corpora(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.indexes_dir) {
            for e in entries.flatten() {
                if e.path().join("scip_graph.db").exists() {
                    if let Some(name) = e.file_name().to_str() {
                        out.push(name.to_string());
                    }
                }
            }
        }
        out.sort();
        out
    }
}

#[async_trait]
impl Tool for CapabilityMapTool {
    fn descriptor(&self) -> ToolDescriptor {
        sovereign_core::tool_manifest::require("capability_map").to_descriptor()
    }

    fn required_permissions(&self) -> Vec<Permission> {
        sovereign_core::tool_manifest::require("capability_map")
            .permissions
            .clone()
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        if let Some(c) = params.get("corpus_id").and_then(|v| v.as_str()) {
            // corpus_id becomes a path component — keep it to a safe charset.
            if c.is_empty()
                || !c
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
            {
                return Err(Error::InvalidInput(format!(
                    "invalid corpus_id '{c}': alphanumeric plus '-' and '_' only"
                )));
            }
        }
        Ok(())
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        // Resolve the corpus: explicit, or the sole indexed code corpus.
        let corpus_id = match params.get("corpus_id").and_then(|v| v.as_str()) {
            Some(c) => c.to_string(),
            None => {
                let corpora = self.code_corpora();
                match corpora.len() {
                    1 => corpora[0].clone(),
                    0 => {
                        return Ok(StepOutput::Text(format!(
                            "No code corpus found under {}. Run `sovereign project init` in a \
                             repository first.",
                            self.indexes_dir.display()
                        )))
                    }
                    _ => {
                        return Ok(StepOutput::Text(format!(
                            "Multiple code corpora are indexed — pass `corpus_id`. Available: {}",
                            corpora.join(", ")
                        )))
                    }
                }
            }
        };

        let provider = match params.get("provider").and_then(|v| v.as_str()) {
            Some("fallback") => ProviderKind::Fallback,
            _ => ProviderKind::Heuristic,
        };
        let jaccard = params
            .get("jaccard")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.5);

        let db_path = self.indexes_dir.join(&corpus_id).join("scip_graph.db");
        if !db_path.exists() {
            return Ok(StepOutput::Text(format!(
                "No SCIP graph at {} — `{corpus_id}` may not be indexed, or has no call graph \
                 (a missing language exporter).",
                db_path.display()
            )));
        }

        let tool_err = |stage: &str, e: corpus_engine_scip::Error| Error::Tool {
            tool_id: "capability_map".to_string(),
            message: format!("{stage}: {e}"),
        };
        let graph =
            ScipGraph::open(&db_path, &corpus_id).map_err(|e| tool_err("open SCIP graph", e))?;
        let symbols = graph
            .iter_all_symbols()
            .await
            .map_err(|e| tool_err("read symbols", e))?;
        let refs = graph
            .iter_all_refs()
            .await
            .map_err(|e| tool_err("read refs", e))?;

        let opts = MapOptions {
            jaccard,
            provider,
            ..Default::default()
        };
        let map = build_capability_map(&symbols, &refs, &opts);
        Ok(StepOutput::Text(
            corpus_engine_scip::capability_map::render_markdown(&corpus_id, &map),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_core::traits::Tool;

    // The descriptor id is load-bearing: it must match the `"capability_map"`
    // entry in `mcp_surface::MCP_TOOLS_ALWAYS`, or the tool is advertised over
    // MCP yet not callable. Guard the contract.
    #[test]
    fn descriptor_id_matches_mcp_surface() {
        let tool = CapabilityMapTool::with_indexes_dir(PathBuf::from("/nonexistent"));
        assert_eq!(tool.descriptor().id, "capability_map");
        assert!(crate::mcp_surface::MCP_TOOLS_ALWAYS.contains(&tool.descriptor().id.as_str()));
    }

    #[test]
    fn corpus_id_validation_rejects_path_traversal() {
        let tool = CapabilityMapTool::with_indexes_dir(PathBuf::from("/nonexistent"));
        assert!(tool
            .validate(&serde_json::json!({"corpus_id": "../etc"}))
            .is_err());
        assert!(tool
            .validate(&serde_json::json!({"corpus_id": "commonwealth-ai"}))
            .is_ok());
        assert!(tool.validate(&serde_json::json!({})).is_ok());
    }
}
