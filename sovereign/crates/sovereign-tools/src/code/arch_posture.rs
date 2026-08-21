// SPDX-License-Identifier: AGPL-3.0-or-later
//! `arch_posture` — cheap headline reader over the persisted arch report
//! (the `drift_posture`/`capability_posture` pattern): answers "what's the
//! architectural posture, and is the report still current?" in one fast
//! call, without recomputing the graph.
//!
//! Freshness is a fingerprint compare: the persisted report carries a hash
//! of its inputs (Cargo.toml, Cargo.lock, ARCH_LAYERS.toml, SCIP db
//! identity); this tool recomputes the hash and reports `fresh` / `stale` /
//! `never_run`. Stale means the numbers below may not reflect current code —
//! refresh with `sovereign code arch-report`.

use std::path::PathBuf;


use sovereign_core::error::{Error, Result};
use sovereign_core::types::*;

use super::arch_report::compute_fingerprint;
use std::sync::Arc;
use sovereign_core::tool_manifest::DeclaredTool;

pub struct ArchPostureTool {
    data_dir: PathBuf,
    project_root: Option<PathBuf>,
}

impl Default for ArchPostureTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ArchPostureTool {
    pub fn new() -> Self {
        Self {
            data_dir: sovereign_contracts::rebrand::data_dir(),
            project_root: None,
        }
    }

    pub fn with_data_dir(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            project_root: None,
        }
    }

    /// Enables the fingerprint freshness check (needs the same root the
    /// report was built against).
    pub fn with_project_root(mut self, root: PathBuf) -> Self {
        self.project_root = Some(root);
        self
    }

    fn report_corpora(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(self.data_dir.join("arch")) {
            for e in entries.flatten() {
                if e.path().join("arch_report.json").exists() {
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

impl ArchPostureTool {
    /// Bind this tool's state to its `arch_posture` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        let run_state = Arc::clone(&state);
        sovereign_core::tool_manifest::declared("arch_posture", move |params, ctx| {
            let state = Arc::clone(&run_state);
            async move { state.run(&params, &ctx).await }
        })
        .with_validate({
            let state = Arc::clone(&state);
            Arc::new(move |p: &serde_json::Value| state.validate_extra(p))
        })
    }

    /// The executable half of `arch_posture`.
    async fn run(
        &self,
        params: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<StepOutput> {
        let corpus_id = match params.get("corpus_id").and_then(|v| v.as_str()) {
            Some(c) => c.to_string(),
            None => {
                let corpora = self.report_corpora();
                match corpora.len() {
                    1 => corpora[0].clone(),
                    0 => {
                        return Ok(StepOutput::Text(format!(
                            "arch_posture: never_run — no persisted report under {}. \
                             Generate one with `sovereign code arch-report`.",
                            self.data_dir.join("arch").display()
                        )))
                    }
                    _ => {
                        return Ok(StepOutput::Text(format!(
                            "Multiple persisted reports — pass `corpus_id`. Available: {}",
                            corpora.join(", ")
                        )))
                    }
                }
            }
        };

        let dir = self.data_dir.join("arch").join(&corpus_id);
        let json_path = dir.join("arch_report.json");
        let text = match std::fs::read_to_string(&json_path) {
            Ok(t) => t,
            Err(_) => {
                return Ok(StepOutput::Text(format!(
                    "arch_posture: never_run for `{corpus_id}` — no report at {}. \
                     Generate one with `sovereign code arch-report`.",
                    json_path.display()
                )))
            }
        };
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| Error::Tool {
            tool_id: "arch_posture".into(),
            message: format!("parse {}: {e}", json_path.display()),
        })?;

        // Freshness: recompute the input fingerprint when we can.
        let stored_fp = v.get("fingerprint").and_then(|f| f.as_str()).unwrap_or("");
        let db_path = self
            .data_dir
            .join("indexes")
            .join(&corpus_id)
            .join("scip_graph.db");
        let status = if stored_fp.is_empty() {
            "unknown (report carries no fingerprint)".to_string()
        } else {
            let current = compute_fingerprint(&db_path, self.project_root.as_deref());
            if current == stored_fp {
                "fresh".to_string()
            } else if self.project_root.is_none() {
                // Without the root we hash fewer inputs than the writer did —
                // a mismatch here is expected, not evidence of staleness.
                "unknown (no project root on this surface — cannot verify)".to_string()
            } else {
                "STALE — inputs changed since the report; refresh with \
                 `sovereign code arch-report`"
                    .to_string()
            }
        };

        // Headlines, defensively read (schema may grow).
        let crates = v
            .pointer("/metrics/crates")
            .and_then(|c| c.as_array())
            .cloned()
            .unwrap_or_default();
        let top = crates.first();
        let hubs: Vec<String> = crates
            .iter()
            .filter(|c| {
                c.get("fan_in").and_then(|x| x.as_u64()).unwrap_or(0) >= 6
                    && c.get("fan_out").and_then(|x| x.as_u64()).unwrap_or(0) >= 6
            })
            .filter_map(|c| c.get("name").and_then(|n| n.as_str()).map(String::from))
            .collect();
        let n_edges = v
            .pointer("/metrics/cross_edges")
            .and_then(|e| e.as_array())
            .map_or(0, Vec::len);
        let n_deltas = v
            .pointer("/metrics/deltas")
            .and_then(|e| e.as_array())
            .map_or(0, Vec::len);
        let n_cycles = v
            .pointer("/metrics/cycles")
            .and_then(|e| e.as_array())
            .map_or(0, Vec::len);
        let n_layer = v
            .get("layer_violations")
            .and_then(|l| l.as_array())
            .map(Vec::len);
        let n_hidden = v
            .pointer("/temporal/hidden_coupling")
            .and_then(|h| h.as_array())
            .map(Vec::len);

        let mut out = String::new();
        use std::fmt::Write as _;
        let _ = writeln!(out, "arch_posture — {corpus_id}");
        let _ = writeln!(out, "status: {status}");
        if let Some(t) = top {
            let _ = writeln!(
                out,
                "top god-crate: {} (fan-in {})",
                t.get("name").and_then(|n| n.as_str()).unwrap_or("?"),
                t.get("fan_in").and_then(|n| n.as_u64()).unwrap_or(0)
            );
        }
        let _ = writeln!(
            out,
            "hubs (fan-in≥6 AND fan-out≥6): {}",
            if hubs.is_empty() {
                "none".to_string()
            } else {
                hubs.join(", ")
            }
        );
        let _ = writeln!(
            out,
            "cross-crate edges: {n_edges}; deltas: {n_deltas}; file cycles: {n_cycles}"
        );
        match n_layer {
            Some(n) => {
                let _ = writeln!(out, "layer violations (observed): {n}");
            }
            None => {
                let _ = writeln!(
                    out,
                    "layer violations (observed): not computed in last report"
                );
            }
        }
        if let Some(n) = n_hidden {
            let _ = writeln!(out, "hidden temporal coupling: {n} pairs");
        }
        let _ = writeln!(out, "full report: {}", dir.join("arch_report.md").display());
        Ok(StepOutput::Text(out))
    }

    fn validate_extra(&self, params: &serde_json::Value) -> Result<()> {

        if let Some(c) = params.get("corpus_id").and_then(|v| v.as_str()) {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_core::traits::Tool;

    #[test]
    fn descriptor_id_matches_mcp_surface() {
        let tool = ArchPostureTool::with_data_dir(PathBuf::from("/nonexistent")).declared();
        assert_eq!(tool.descriptor().id, "arch_posture");
        assert!(crate::mcp_surface::MCP_TOOLS_ALWAYS.contains(&tool.descriptor().id.as_str()));
    }

    #[test]
    fn never_run_is_a_calm_answer_not_an_error() {
        let ctx = ToolContext {
            conversation_id: Default::default(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
            ..Default::default()
        };
        let tool = ArchPostureTool::with_data_dir(PathBuf::from("/nonexistent"));
        let out = futures::executor::block_on(tool.run(&serde_json::json!({}), &ctx)).unwrap();
        match out {
            StepOutput::Text(t) => assert!(t.contains("never_run")),
            other => panic!("expected text, got {other:?}"),
        }
    }
}
