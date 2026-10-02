// SPDX-License-Identifier: AGPL-3.0-or-later
//! `atlas_gaps` — literary-atlas **Phase 7** as a workflow leaf: detect
//! structural gaps in the resolved atlas (transitions without a trigger event,
//! ungrounded claims, still-open questions).
//!
//! One atomic op: read `atoms.json` + `edges.json`, run the real
//! `detect_deterministic_gaps`, write `gaps.json`. Deterministic and pure — no
//! model, no embed — so it's a clean leaf wrapping the exact corpus-engine
//! function the bespoke `enrich atlas-gaps` runs. Effect is `Write` (it writes
//! `gaps.json`); idempotent (same atoms → same gaps → same ids).

use corpus_engine_atlas_reader::ports::AtlasPort;
use sovereign_core::error::{Error, Result};
use sovereign_core::types::*;
use understanding_vocab::read::{read_atlas_atoms, read_atlas_edges};

use crate::atlas_phase::atlas_dir_for;
use sovereign_core::tool_manifest::DeclaredTool;
use std::sync::Arc;

pub struct AtlasGapsTool {
    atlas: Arc<dyn AtlasPort>,
}

impl AtlasGapsTool {
    /// The tool over ingest's atlas port.
    pub fn new(atlas: Arc<dyn AtlasPort>) -> Self {
        Self { atlas }
    }

    /// Bind this tool's state to its `atlas_gaps` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        sovereign_core::tool_manifest::declared("atlas_gaps", move |params, ctx| {
            let state = Arc::clone(&state);
            async move { state.run(&params, &ctx).await }
        })
    }

    /// The executable half of `atlas_gaps`.
    async fn run(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let corpus = params
            .get("corpus")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Execution("atlas_gaps: missing required `corpus`".into()))?;
        let atlas_dir = atlas_dir_for(params, corpus);

        let atoms = read_atlas_atoms(&atlas_dir).map_err(|e| {
            Error::Execution(format!(
                "atlas_gaps: read {}/atoms.json: {e} — run the resolve phase first",
                atlas_dir.display()
            ))
        })?;
        let edges = read_atlas_edges(&atlas_dir).map_err(|e| {
            Error::Execution(format!(
                "atlas_gaps: read {}/edges.json: {e}",
                atlas_dir.display()
            ))
        })?;

        let (n, path) = self
            .atlas
            .write_deterministic_gaps(&atlas_dir, atoms.atoms(), &edges.edges)
            .map_err(|e| Error::Execution(format!("atlas_gaps: write gaps.json: {e}")))?;

        Ok(StepOutput::Text(format!(
            "atlas_gaps: wrote {n} gap(s) to {}",
            path.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine_atlas_reader::ports::double::AtlasPortDouble;
    use std::sync::Mutex;

    /// The tool reads atoms + edges on the canonical `<index>/<corpus>/atlas/`
    /// paths, hands them to the port, and reports what the port wrote. The
    /// detection and the gaps.json schema are ingest's, proven on
    /// `IngestAtlas` (corpus-engine's atlas_port_parity
    /// `write_deterministic_gaps_writes_the_schema_the_downstream_reads`).
    #[tokio::test]
    async fn atlas_gaps_reads_detects_and_writes_on_canonical_paths() {
        let dir = tempfile::tempdir().unwrap();
        let atlas = dir.path().join("c1").join("atlas");
        std::fs::create_dir_all(&atlas).unwrap();
        std::fs::write(
            atlas.join("atoms.json"),
            r#"{"schema_version":"2.0","atoms":[]}"#,
        )
        .unwrap();
        std::fs::write(
            atlas.join("edges.json"),
            r#"{"schema_version":"2.0","edges":[]}"#,
        )
        .unwrap();

        let seen: Arc<Mutex<Vec<std::path::PathBuf>>> = Arc::default();
        let seen_in = Arc::clone(&seen);
        let port = AtlasPortDouble::new().on_write_deterministic_gaps(move |dir, atoms, edges| {
            seen_in.lock().unwrap().push(dir.to_path_buf());
            Ok((atoms.len() + edges.len(), dir.join("gaps.json")))
        });
        let params = serde_json::json!({
            "corpus": "c1",
            "index_dir": dir.path().to_string_lossy()
        });
        let out = AtlasGapsTool::new(Arc::new(port))
            .run(&params, &ToolContext::default())
            .await
            .unwrap();
        match out {
            StepOutput::Text(t) => {
                assert!(t.contains("0 gap"), "{t}");
                assert!(
                    t.contains(&atlas.join("gaps.json").display().to_string()),
                    "{t}"
                );
            }
            o => panic!("unexpected output: {o:?}"),
        }
        assert_eq!(*seen.lock().unwrap(), vec![atlas.clone()]);

        // A missing atlas is a loud error (points the operator at resolve),
        // and the port is never asked to write.
        let bad =
            serde_json::json!({ "corpus": "nope", "index_dir": dir.path().to_string_lossy() });
        let port = Arc::new(AtlasPortDouble::new());
        assert!(AtlasGapsTool::new(port.clone())
            .run(&bad, &ToolContext::default())
            .await
            .is_err());
        assert!(port.calls().is_empty());
    }
}
