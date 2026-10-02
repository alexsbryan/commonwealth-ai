// SPDX-License-Identifier: AGPL-3.0-or-later
//! `atlas_tensions` — literary-atlas **Phase 6 (deterministic half)** as a
//! workflow leaf: enumerate tension *candidates* (claim↔claim and claim↔state
//! pairs sharing an entity) from the resolved atlas.
//!
//! One atomic op: read `atoms.json`, run the real graph-strategy
//! `select_candidates` + the `drop_same_named_speaker_pairs` de-noise, write
//! `tension_candidates.json`. Pure for the literary/philosophy atlas (graph
//! signals, no model, no embed) — a clean leaf wrapping the exact corpus-engine
//! functions the bespoke `enrich atlas-tensions` runs. (The custom-ontology
//! embedding-top-K strategy needs the daemon and is out of scope for this
//! deterministic leaf.) The LLM classification pass that promotes candidates to
//! real `Tension` edges is a separate `model:` step.

use corpus_engine_atlas_reader::ports::AtlasPort;
use sovereign_core::error::{Error, Result};
use sovereign_core::types::*;
use understanding_vocab::read::read_atlas_atoms;

use crate::atlas_phase::atlas_dir_for;
use sovereign_core::tool_manifest::DeclaredTool;
use std::sync::Arc;

pub struct AtlasTensionsTool {
    atlas: Arc<dyn AtlasPort>,
}

impl AtlasTensionsTool {
    /// The tool over ingest's atlas port.
    pub fn new(atlas: Arc<dyn AtlasPort>) -> Self {
        Self { atlas }
    }

    /// Bind this tool's state to its `atlas_tensions` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        sovereign_core::tool_manifest::declared("atlas_tensions", move |params, ctx| {
            let state = Arc::clone(&state);
            async move { state.run(&params, &ctx).await }
        })
    }

    /// The executable half of `atlas_tensions`.
    async fn run(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let corpus = params
            .get("corpus")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Execution("atlas_tensions: missing required `corpus`".into()))?;
        let atlas_dir = atlas_dir_for(params, corpus);

        let atoms = read_atlas_atoms(&atlas_dir).map_err(|e| {
            Error::Execution(format!(
                "atlas_tensions: read {}/atoms.json: {e} — run the resolve phase first",
                atlas_dir.display()
            ))
        })?;

        let (n, path) = self
            .atlas
            .write_tension_candidates(&atlas_dir, atoms.atoms())
            .map_err(|e| {
                Error::Execution(format!(
                    "atlas_tensions: write tension_candidates.json: {e}"
                ))
            })?;

        Ok(StepOutput::Text(format!(
            "atlas_tensions: wrote {n} candidate pair(s) to {}",
            path.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine_atlas_reader::ports::double::AtlasPortDouble;
    use std::sync::Mutex;

    /// The tool reads atoms on the canonical atlas paths, hands them to the
    /// port, and reports what the port wrote. Candidate selection and the
    /// tension_candidates.json schema are ingest's, proven on `IngestAtlas`
    /// (corpus-engine's atlas_port_parity
    /// `write_tension_candidates_writes_the_schema_the_classifier_reads`).
    #[tokio::test]
    async fn atlas_tensions_reads_selects_and_writes_on_canonical_paths() {
        let dir = tempfile::tempdir().unwrap();
        let atlas = dir.path().join("c1").join("atlas");
        std::fs::create_dir_all(&atlas).unwrap();
        std::fs::write(
            atlas.join("atoms.json"),
            r#"{"schema_version":"2.0","atoms":[]}"#,
        )
        .unwrap();

        let seen: Arc<Mutex<Vec<std::path::PathBuf>>> = Arc::default();
        let seen_in = Arc::clone(&seen);
        let port = AtlasPortDouble::new().on_write_tension_candidates(move |dir, atoms| {
            seen_in.lock().unwrap().push(dir.to_path_buf());
            Ok((atoms.len(), dir.join("tension_candidates.json")))
        });
        let params = serde_json::json!({
            "corpus": "c1",
            "index_dir": dir.path().to_string_lossy()
        });
        let out = AtlasTensionsTool::new(Arc::new(port))
            .run(&params, &ToolContext::default())
            .await
            .unwrap();
        match out {
            StepOutput::Text(t) => {
                assert!(t.contains("0 candidate"), "{t}");
                assert!(t.contains("tension_candidates.json"), "{t}");
            }
            o => panic!("unexpected output: {o:?}"),
        }
        assert_eq!(*seen.lock().unwrap(), vec![atlas.clone()]);

        // A missing atlas is a loud error, and the port is never asked.
        let bad =
            serde_json::json!({ "corpus": "nope", "index_dir": dir.path().to_string_lossy() });
        let port = Arc::new(AtlasPortDouble::new());
        assert!(AtlasTensionsTool::new(port.clone())
            .run(&bad, &ToolContext::default())
            .await
            .is_err());
        assert!(port.calls().is_empty());
    }
}
