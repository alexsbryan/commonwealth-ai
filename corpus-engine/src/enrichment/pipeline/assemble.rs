// SPDX-License-Identifier: AGPL-3.0-or-later
//! Canonical assembly of a phase's parsed atoms into its persisted output
//! struct — the single source of truth for "given the per-element parse
//! results, what does `cache/<phase>.json` look like".
//!
//! The bespoke runner constructs these structs inline at the end of each phase
//! (stamping `written_at` via the wall clock). A workflow-composed phase reaches
//! the SAME struct through [`assemble_phase_output`] (invoked by the
//! `pipeline_assemble` adapter), so the domain output types stay untouched — no
//! workflow-shaped lookalike assembled in TOML, no loosened `serde` on
//! `written_at`. The output struct is the domain's knowledge, so building it
//! lives here in the domain, exactly as prompt/parse logic lives in the
//! pipeline. The phase selects which struct to build (phase-as-data), mirroring
//! `pipeline_compose` / `pipeline_parse`.

use serde_json::Value;

use super::types::{ExtractedQuestion, NamedCluster, Phase1Output, Phase3AtlasOutput};
use super::PipelinePhase;
use crate::error::{Error, Result};

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Wrap a collection of parsed atoms into the canonical phase-output struct,
/// stamping `written_at` exactly as the runner does. `atoms` is the JSON array a
/// `for_each` parse step produced (the phase's atom type); `phase` selects the
/// struct. Returns the struct as JSON, ready for `write_json` to persist to the
/// phase's cache path — byte-faithful to the bespoke output for the all-success
/// case (per-element failures are surfaced in the workflow run trace rather than
/// the struct's optional `failures` field).
pub fn assemble_phase_output(pipeline_id: &str, phase: PipelinePhase, atoms: Value) -> Result<Value> {
    match phase {
        PipelinePhase::Questions => {
            let questions_by_chapter: Vec<ExtractedQuestion> =
                serde_json::from_value(atoms).map_err(|e| {
                    Error::Serialization(format!(
                        "assemble questions: atoms are not [ExtractedQuestion]: {e}"
                    ))
                })?;
            let out = Phase1Output {
                schema_version: Phase1Output::SCHEMA_VERSION,
                pipeline_id: pipeline_id.to_string(),
                questions_by_chapter,
                failures: Vec::new(),
                written_at: now_rfc3339(),
            };
            serde_json::to_value(out)
                .map_err(|e| Error::Serialization(format!("assemble questions: {e}")))
        }
        PipelinePhase::AtlasNamedClusters => {
            // The name atoms carry {cluster_id, facet, label, metadata} but no
            // `id` — `ncl_NNNN` is the runner's to assign (`named.len()+1` over
            // successes). Inject it sequentially here, then deserialize as the
            // untouched NamedCluster, so id assignment stays domain knowledge.
            let drafts: Vec<Value> = serde_json::from_value(atoms).map_err(|e| {
                Error::Serialization(format!("assemble name: atoms are not a JSON array: {e}"))
            })?;
            let named_clusters: Vec<NamedCluster> = drafts
                .into_iter()
                .enumerate()
                .map(|(i, mut atom)| {
                    if let Some(obj) = atom.as_object_mut() {
                        obj.insert("id".to_string(), Value::String(format!("ncl_{:04}", i + 1)));
                    }
                    serde_json::from_value(atom).map_err(|e| {
                        Error::Serialization(format!(
                            "assemble name: atom is not a NamedCluster: {e}"
                        ))
                    })
                })
                .collect::<Result<_>>()?;
            let out = Phase3AtlasOutput {
                schema_version: Phase3AtlasOutput::SCHEMA_VERSION,
                pipeline_id: pipeline_id.to_string(),
                named_clusters,
                failures: Vec::new(),
                written_at: now_rfc3339(),
            };
            serde_json::to_value(out)
                .map_err(|e| Error::Serialization(format!("assemble name: {e}")))
        }
        other => Err(Error::InvalidInput(format!(
            "assemble_phase_output: phase `{}` not supported (questions|atlas-named-clusters)",
            other.id()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Questions atoms assemble into a Phase1Output with the schema version,
    /// pipeline id, and a stamped `written_at` — and round-trip back to the
    /// untouched domain struct (the whole point: no serde loosening needed).
    #[test]
    fn assembles_questions_into_phase1output() {
        let out = assemble_phase_output(
            "literary_atlas",
            PipelinePhase::Questions,
            serde_json::json!([]),
        )
        .unwrap();
        assert_eq!(out["schema_version"], 1);
        assert_eq!(out["pipeline_id"], "literary_atlas");
        assert_eq!(out["questions_by_chapter"].as_array().unwrap().len(), 0);
        assert!(!out["written_at"].as_str().unwrap().is_empty(), "written_at stamped");
        let _typed: Phase1Output = serde_json::from_value(out).unwrap();
    }

    /// Name atoms (no `id`) assemble into Phase3AtlasOutput with the domain
    /// assigning sequential `ncl_NNNN` ids — id assignment stays domain
    /// knowledge, exactly as the bespoke name loop does it.
    #[test]
    fn assembles_name_and_assigns_sequential_ids() {
        let atoms = serde_json::json!([
            { "cluster_id": "cl_0001", "facet": "question", "label": "First", "metadata": {} },
            { "cluster_id": "cl_0002", "facet": "claim", "label": "Second", "metadata": {} }
        ]);
        let out =
            assemble_phase_output("literary_atlas", PipelinePhase::AtlasNamedClusters, atoms).unwrap();
        let nc = out["named_clusters"].as_array().unwrap();
        assert_eq!(nc.len(), 2);
        assert_eq!(nc[0]["id"], "ncl_0001");
        assert_eq!(nc[1]["id"], "ncl_0002");
        assert_eq!(nc[0]["cluster_id"], "cl_0001");
        let _typed: Phase3AtlasOutput = serde_json::from_value(out).unwrap();
    }

    /// An unsupported phase is a loud error, not a silent empty struct.
    #[test]
    fn rejects_unsupported_phase() {
        assert!(assemble_phase_output("literary_atlas", PipelinePhase::Gaps, serde_json::json!([]))
            .is_err());
    }
}
