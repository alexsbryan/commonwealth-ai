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

use corpus_engine::enrichment::atlas::{
    analysis::gaps::{detect_deterministic_gaps, GapDetectionInput, GapsOutput},
    read_atlas_atoms, read_atlas_edges, write_atlas_gaps, AtomEnvelope,
};
use sovereign_core::error::{Error, Result};
use sovereign_core::types::*;

use crate::atlas_phase::atlas_dir_for;
use sovereign_core::tool_manifest::DeclaredTool;
use std::sync::Arc;

pub struct AtlasGapsTool;

impl AtlasGapsTool {
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

        // Partition atoms by kind — only Claim / State / Question drive the
        // deterministic detectors; the rest pass through untouched.
        let mut claims = Vec::new();
        let mut states = Vec::new();
        let mut questions = Vec::new();
        for a in atoms.atoms {
            match a {
                AtomEnvelope::Claim(c) => claims.push(c),
                AtomEnvelope::State(s) => states.push(s),
                AtomEnvelope::Question(q) => questions.push(q),
                _ => {}
            }
        }

        let gaps = detect_deterministic_gaps(GapDetectionInput {
            claims: &claims,
            states: &states,
            questions: &questions,
            edges: &edges.edges,
        });
        let n = gaps.len();
        let path = write_atlas_gaps(&atlas_dir, &GapsOutput::new(gaps))
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

    /// The leaf wraps the real corpus-engine gap detector: read atoms+edges →
    /// detect → write gaps.json, on the canonical `<index>/<corpus>/atlas/` paths.
    /// Hermetic: a fresh atlas with no atoms yields zero gaps and a well-formed
    /// `gaps.json` — proving the read→detect→write wiring (the detection logic
    /// itself is covered by corpus-engine's own tests).
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

        let params = serde_json::json!({
            "corpus": "c1",
            "index_dir": dir.path().to_string_lossy()
        });
        let out = AtlasGapsTool
            .run(&params, &ToolContext::default())
            .await
            .unwrap();
        match out {
            StepOutput::Text(t) => assert!(t.contains("0 gap"), "{t}"),
            o => panic!("unexpected output: {o:?}"),
        }

        // gaps.json is written in the schema the downstream reads.
        let g: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(atlas.join("gaps.json")).unwrap())
                .unwrap();
        assert_eq!(g["gaps"].as_array().unwrap().len(), 0);
        assert_eq!(g["schema_version"], "2.0");

        // A missing atlas is a loud error (points the operator at resolve).
        let bad =
            serde_json::json!({ "corpus": "nope", "index_dir": dir.path().to_string_lossy() });
        assert!(AtlasGapsTool
            .run(&bad, &ToolContext::default())
            .await
            .is_err());
    }
}
