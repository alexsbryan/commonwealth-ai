//! Entity / Relationship / PatternFinding data model for the
//! investigation pipeline.
//!
//! Mirrors the literary atlas's atom/edge model conceptually but
//! uses **typed-attribute** records instead of the atom-tag enum
//! the atlas uses. The recipe author declares what entity / edge
//! types exist (`[[enrichment.entity_types]]` etc.), and the LLM
//! extraction populates instances of those types from each chunk.
//!
//! Persistence is JSON, three files alongside the corpus index:
//!
//! ```text
//! <index_dir>/<corpus_id>/investigation/
//! ├── entities.json
//! ├── relationships.json
//! └── pattern_findings.json
//! ```
//!
//! The same on-disk shape is what the audit step reads to render
//! the "Findings" section, and what the desktop UI surfaces as a
//! relationship graph.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// One typed-entity instance extracted by the investigation
/// pipeline. The `entity_type` references one of the
/// `[[enrichment.entity_types]]` declarations from the recipe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entity {
    /// Stable identifier — generated as a slug of `canonical_name`
    /// + a short hash of the type. Stable across reruns of the
    /// same chunks so the relationship graph stays coherent under
    /// re-extraction.
    pub id: String,
    /// Resolved canonical name (e.g. `"NVIDIA Corporation"` for
    /// mentions like `"Nvidia"`, `"NVDA"`, `"Nvidia Corp."`). The
    /// coalesce phase populates this; raw extraction stores the
    /// surface form here pending coalesce.
    pub canonical_name: String,
    /// Type name, references a `[[enrichment.entity_types]] name`
    /// declaration.
    pub entity_type: String,
    /// LLM-extracted attribute values keyed by the
    /// `attributes: [...]` list on the entity_type declaration.
    /// Missing keys are absent (not null), so the recipe author
    /// can iterate on the prompt to harvest more attributes
    /// without churning the persisted shape.
    #[serde(default)]
    pub attributes: serde_json::Map<String, serde_json::Value>,
    /// Surface forms observed in the corpus that resolve to this
    /// entity. Populated by the coalesce phase from the raw
    /// extractions.
    #[serde(default)]
    pub aliases: Vec<String>,
}

/// One typed-relationship instance. References two entity ids and
/// the recipe-declared relationship type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Relationship {
    pub id: String,
    pub from_entity_id: String,
    pub to_entity_id: String,
    /// Type name, references a `[[enrichment.relationship_types]] name`.
    pub relationship_type: String,
    /// LLM-extracted attribute values keyed by the
    /// `attributes: [...]` list on the relationship_type declaration.
    #[serde(default)]
    pub attributes: serde_json::Map<String, serde_json::Value>,
    /// Verbatim excerpt the LLM produced as evidence for the
    /// extraction. Carries the source chunk id so the audit step
    /// can cite back to the source.
    pub evidence: Evidence,
    /// LLM-self-reported confidence (0.0..=1.0). Free-form for
    /// now; the test harness asserts shape only.
    #[serde(default = "default_confidence")]
    pub confidence: f32,
}

fn default_confidence() -> f32 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Evidence {
    pub chunk_id: String,
    /// Verbatim excerpt the LLM identified as evidence. May be
    /// trimmed to fit prompt budgets but should be present so the
    /// audit step can quote it directly.
    pub excerpt: String,
}

/// One match of a declared graph-level pattern. The
/// `pattern_name` references a `[[enrichment.patterns]] name`
/// declaration so the audit step can group findings by pattern.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PatternFinding {
    pub pattern_name: String,
    pub pattern_type: PatternKind,
    /// Entities that participate in this finding, ordered by the
    /// pattern's semantic meaning (e.g. cycle order for
    /// CircularFlow; investor-then-customer for RoleOverlap).
    pub entity_ids: Vec<String>,
    /// Relationship ids that connect the entities (e.g. the edges
    /// of a circular flow). Empty for patterns that don't
    /// reference specific edges (rare).
    #[serde(default)]
    pub relationship_ids: Vec<String>,
    /// Pattern-specific attributes — for Threshold, the matched
    /// numeric value; for RoleOverlap, the role bindings; for
    /// CircularFlow, the cycle length. Free-form JSON to keep
    /// future detectors additive.
    #[serde(default)]
    pub attributes: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PatternKind {
    CircularFlow,
    RoleOverlap,
    Threshold,
    /// Reserved for the future SQL escape hatch — see
    /// [`crate::recipe::PatternDecl::CustomSql`]. The runtime
    /// emits a `pattern_findings.json` row with this kind + an
    /// "unimplemented" attribute so the recipe author can see
    /// the pattern was declared but not yet executable, rather
    /// than silently disappearing.
    CustomSql,
}

/// On-disk graph: the three JSON files that the pipeline writes
/// and the audit / desktop UI read.
pub const ENTITIES_FILENAME: &str = "entities.json";
pub const RELATIONSHIPS_FILENAME: &str = "relationships.json";
pub const FINDINGS_FILENAME: &str = "pattern_findings.json";

/// Subdirectory under the corpus index dir where the three
/// investigation JSONs live. Mirrors the atlas pipeline's
/// `atlas/` subdir convention.
pub const INVESTIGATION_DIRNAME: &str = "investigation";

/// Persist all three JSON files atomically (tmp + rename per file).
/// Creates `<dir>/investigation/` if missing.
pub fn write_outputs(
    dir: &Path,
    entities: &[Entity],
    relationships: &[Relationship],
    findings: &[PatternFinding],
) -> Result<()> {
    let invest_dir = dir.join(INVESTIGATION_DIRNAME);
    fs::create_dir_all(&invest_dir)?;
    write_atomic_json(&invest_dir.join(ENTITIES_FILENAME), entities)?;
    write_atomic_json(&invest_dir.join(RELATIONSHIPS_FILENAME), relationships)?;
    write_atomic_json(&invest_dir.join(FINDINGS_FILENAME), findings)?;
    Ok(())
}

/// Read the three JSON files. Missing files surface as empty
/// vectors so a partial run doesn't error the audit step.
pub fn read_outputs(dir: &Path) -> Result<(Vec<Entity>, Vec<Relationship>, Vec<PatternFinding>)> {
    let invest_dir = dir.join(INVESTIGATION_DIRNAME);
    Ok((
        read_json(&invest_dir.join(ENTITIES_FILENAME))?,
        read_json(&invest_dir.join(RELATIONSHIPS_FILENAME))?,
        read_json(&invest_dir.join(FINDINGS_FILENAME))?,
    ))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>> {
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(path).map_err(Error::Io)?;
    serde_json::from_str(&raw)
        .map_err(|e| Error::Serialization(format!("read {}: {e}", path.display())))
}

fn write_atomic_json<T: Serialize>(path: &Path, value: T) -> Result<()> {
    let bytes =
        serde_json::to_vec_pretty(&value).map_err(|e| Error::Serialization(e.to_string()))?;
    let part = path.with_extension("json.part");
    fs::write(&part, bytes)?;
    fs::rename(&part, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_three_files_through_disk() {
        let dir = tempfile::tempdir().unwrap();

        let entities = vec![Entity {
            id: "e-nvda".into(),
            canonical_name: "NVIDIA Corporation".into(),
            entity_type: "company".into(),
            attributes: Default::default(),
            aliases: vec!["NVDA".into(), "Nvidia".into()],
        }];
        let relationships = vec![Relationship {
            id: "r-1".into(),
            from_entity_id: "e-nvda".into(),
            to_entity_id: "e-msft".into(),
            relationship_type: "revenue".into(),
            attributes: Default::default(),
            evidence: Evidence {
                chunk_id: "chunk-42".into(),
                excerpt: "Microsoft committed to a multi-year cloud GPU contract.".into(),
            },
            confidence: 0.85,
        }];
        let findings = vec![PatternFinding {
            pattern_name: "money_cycles".into(),
            pattern_type: PatternKind::CircularFlow,
            entity_ids: vec!["e-nvda".into(), "e-msft".into(), "e-orcl".into()],
            relationship_ids: vec!["r-1".into()],
            attributes: Default::default(),
        }];

        write_outputs(dir.path(), &entities, &relationships, &findings).unwrap();
        let (e2, r2, f2) = read_outputs(dir.path()).unwrap();
        assert_eq!(entities, e2);
        assert_eq!(relationships, r2);
        assert_eq!(findings, f2);
    }

    #[test]
    fn missing_directory_returns_empty_vectors() {
        let dir = tempfile::tempdir().unwrap();
        let (e, r, f) = read_outputs(dir.path()).unwrap();
        assert!(e.is_empty());
        assert!(r.is_empty());
        assert!(f.is_empty());
    }
}
