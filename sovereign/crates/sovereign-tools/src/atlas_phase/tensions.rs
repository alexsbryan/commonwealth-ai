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

use corpus_engine::enrichment::atlas::{
    analysis::tensions::{
        drop_same_named_speaker_pairs, select_candidates, CandidateSelectionInput,
        TensionCandidatesOutput,
    },
    read_atlas_atoms, write_tension_candidates, AtomEnvelope,
};
use sovereign_core::error::{Error, Result};
use sovereign_core::types::*;

use crate::atlas_phase::atlas_dir_for;
use sovereign_core::tool_manifest::DeclaredTool;
use std::sync::Arc;

pub struct AtlasTensionsTool;

impl AtlasTensionsTool {
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

        // Claim + State drive the entity-overlap signal; Entity atoms feed the
        // cross-position concept-overlap signal.
        let mut claims = Vec::new();
        let mut states = Vec::new();
        let mut entities = Vec::new();
        for a in atoms.atoms {
            match a {
                AtomEnvelope::Claim(c) => claims.push(c),
                AtomEnvelope::State(s) => states.push(s),
                AtomEnvelope::Entity(e) => entities.push(e),
                _ => {}
            }
        }

        let mut candidates = select_candidates(CandidateSelectionInput {
            claims: &claims,
            states: &states,
            // Intra-cluster candidates aren't wired in the deterministic path
            // (same as the bespoke command — pending a stable sketch→atom map).
            claim_clusters: &[],
            entities: &entities,
        });
        // De-noise: drop pairs where both claims share a named speaker.
        drop_same_named_speaker_pairs(&mut candidates, &claims, &entities);

        let out = TensionCandidatesOutput::new(candidates);
        let n = out.candidates.len();
        let path = write_tension_candidates(&atlas_dir, &out).map_err(|e| {
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

    /// The leaf wraps the real graph-strategy candidate selector: read atoms →
    /// `select_candidates` + de-noise → write `tension_candidates.json`, on the
    /// canonical atlas paths. Hermetic: a fresh atlas with no atoms yields zero
    /// candidates and a well-formed file (the selection logic itself is covered
    /// by corpus-engine's own tests).
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

        let params = serde_json::json!({
            "corpus": "c1",
            "index_dir": dir.path().to_string_lossy()
        });
        let out = AtlasTensionsTool
            .run(&params, &ToolContext::default())
            .await
            .unwrap();
        match out {
            StepOutput::Text(t) => assert!(t.contains("0 candidate"), "{t}"),
            o => panic!("unexpected output: {o:?}"),
        }

        // tension_candidates.json is written in the schema the classifier reads.
        let v: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(atlas.join("tension_candidates.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(v["candidates"].as_array().unwrap().len(), 0);
        assert_eq!(v["schema_version"], "2.0");

        // A missing atlas is a loud error.
        let bad =
            serde_json::json!({ "corpus": "nope", "index_dir": dir.path().to_string_lossy() });
        assert!(AtlasTensionsTool
            .run(&bad, &ToolContext::default())
            .await
            .is_err());
    }
}
