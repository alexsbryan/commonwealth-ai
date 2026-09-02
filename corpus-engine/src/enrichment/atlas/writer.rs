// SPDX-License-Identifier: AGPL-3.0-or-later
//! Atlas on-disk writer — serialise `AtomsFile`, `EdgesFile`, and
//! the (currently placeholder) trajectories index into the
//! `atlas/` subdirectory of a corpus.
//!
//! Step 3a only emits entity + event atoms and `Involves` edges;
//! the trajectories index is written as an empty object so the
//! directory layout is complete and `sovereign enrich query` can
//! resolve paths without special-casing per phase. Phase 3b will
//! replace the empty trajectories payload with real state chains.
//!
//! Writes are atomic: each file is written to a sibling `.tmp`
//! path and renamed into place. Callers should treat the whole
//! atlas directory as "present or absent" — partial states that
//! survive a crash are not a concern because nothing outside the
//! writer touches these files mid-run.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::atoms::{
    AtomEnvelope, AtomsFile, Claim, Configuration, Entity, Event, Question, Relation, State,
};
use super::edges::{Edge, EdgesFile};
use super::resolution::Trajectory;

/// Directory name for atlas output under a corpus's index root.
/// Full path is `~/.svrnmesh/indexes/<corpus>/atlas/`.
pub const ATLAS_DIRNAME: &str = "atlas";

/// On-disk layout of `atlas/trajectories.json`. Empty at Step 3a —
/// Phase 3b populates `trajectories` with per-entity and per-
/// relation state sequences per spec §6.4.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrajectoriesFile {
    pub schema_version: String,
    #[serde(default)]
    pub trajectories: serde_json::Value,
}

impl TrajectoriesFile {
    pub const SCHEMA_VERSION: &'static str = "2.0";

    pub fn empty() -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION.to_string(),
            trajectories: serde_json::json!({}),
        }
    }
}

/// Write the Step 3a artifacts into `atlas_dir`. Step 3a output only
/// carries entities + events + Involves edges, so states /
/// relations / claims / questions land as empty vecs and the
/// trajectories index is an empty object.
///
/// Thin wrapper over [`write_atlas_full`] for the common Step 3a
/// case. Callers that run Step 3a + Step 3b in one pass go through
/// `write_atlas_full` directly.
pub fn write_atlas(
    atlas_dir: &Path,
    entities: &[Entity],
    events: &[Event],
    edges: &[Edge],
) -> io::Result<AtlasWritten> {
    write_atlas_full(
        atlas_dir,
        entities,
        events,
        &[],
        &[],
        &[],
        &[],
        &[],
        &[],
        &[],
        &[],
        edges,
        &std::collections::BTreeMap::new(),
    )
}

/// Write the full atlas artifact set — every atom type plus
/// trajectories.json — into `atlas_dir`. Called after a combined
/// Step 3a + Step 3b resolution pass; the individual `Vec<T>`
/// arguments can be empty for atom types the current pass didn't
/// produce.
///
/// - `atlas/atoms.json` — `AtomEnvelope` per atom, all types.
/// - `atlas/edges.json` — every edge.
/// - `atlas/trajectories.json` — per-entity / per-relation state
///   sequences per spec §6.4.
///
/// Atomic: each file lands via sibling `.tmp` + rename so a crash
/// mid-write can't leave partial state.
pub fn write_atlas_full(
    atlas_dir: &Path,
    entities: &[Entity],
    events: &[Event],
    states: &[State],
    relations: &[Relation],
    claims: &[Claim],
    questions: &[Question],
    configurations: &[Configuration],
    argument_reconstructions: &[crate::enrichment::atlas::atoms::ArgumentReconstruction],
    positions: &[crate::enrichment::atlas::atoms::Position],
    oppositions: &[crate::enrichment::atlas::atoms::Opposition],
    edges: &[Edge],
    trajectories: &std::collections::BTreeMap<String, Trajectory>,
) -> io::Result<AtlasWritten> {
    fs::create_dir_all(atlas_dir)?;

    // Wrap every atom in its envelope variant. Order — entities
    // first, then events, then the Step 3b atom types, then the
    // Gap-B typed extensions — is stable across runs so diffs
    // between runs stay clean.
    let atoms: Vec<AtomEnvelope> = entities
        .iter()
        .cloned()
        .map(AtomEnvelope::Entity)
        .chain(events.iter().cloned().map(AtomEnvelope::Event))
        .chain(states.iter().cloned().map(AtomEnvelope::State))
        .chain(relations.iter().cloned().map(AtomEnvelope::Relation))
        .chain(claims.iter().cloned().map(AtomEnvelope::Claim))
        .chain(questions.iter().cloned().map(AtomEnvelope::Question))
        .chain(
            configurations
                .iter()
                .cloned()
                .map(AtomEnvelope::Configuration),
        )
        .chain(
            argument_reconstructions
                .iter()
                .cloned()
                .map(AtomEnvelope::ArgumentReconstruction),
        )
        .chain(positions.iter().cloned().map(AtomEnvelope::Position))
        .chain(oppositions.iter().cloned().map(AtomEnvelope::Opposition))
        .collect();
    let atoms_file = AtomsFile::new(atoms);
    let edges_file = EdgesFile::new(edges.to_vec());

    let trajectories_file = if trajectories.is_empty() {
        TrajectoriesFile::empty()
    } else {
        TrajectoriesFile {
            schema_version: TrajectoriesFile::SCHEMA_VERSION.to_string(),
            trajectories: serde_json::to_value(trajectories).unwrap_or(serde_json::json!({})),
        }
    };

    let atoms_path = atlas_dir.join("atoms.json");
    let edges_path = atlas_dir.join("edges.json");
    let trajectories_path = atlas_dir.join("trajectories.json");

    write_atomic(&atoms_path, &atoms_file)?;
    write_atomic(&edges_path, &edges_file)?;
    write_atomic(&trajectories_path, &trajectories_file)?;

    // Write the v2 store (`atoms.lance` + `edges.csr`) beside the canonical
    // JSON — this IS the read path (ATLAS_STORAGE_V2). Fail-hard: a store the
    // runtime can't open is a failed atlas write, not a silent degrade.
    // `atoms.json` stays the canonical export + the rebuild source for
    // `sovereign atlas migrate-all`.
    write_atlas_v2_store(atlas_dir, &atoms_file.atoms, edges)?;

    Ok(AtlasWritten {
        atoms_path,
        edges_path,
        trajectories_path,
    })
}

/// Write the v2 store (`atoms.lance` + `edges.csr`) from the just-written atoms
/// + edges (ATLAS_STORAGE_V2). This is the runtime read path, so it is
/// **fail-hard**: an unwritable store fails the atlas write rather than silently
/// leaving a corpus with no loadable graph.
/// `atoms.json` is already written and remains the canonical export + the
/// `migrate-all` rebuild source, so a failed store is always recoverable. The
/// store's `corpus_id` is self-description (derived from the atlas dir's
/// parent), keeping `write_atlas_full`'s signature stable for every caller.
fn write_atlas_v2_store(
    atlas_dir: &Path,
    atoms: &[AtomEnvelope],
    edges: &[Edge],
) -> io::Result<()> {
    let corpus_id = atlas_dir
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("");
    super::store::write_store_blocking(atlas_dir, corpus_id, atoms, edges)
        .map(|_| ())
        .map_err(io::Error::other)
}

#[derive(Debug, Clone)]
pub struct AtlasWritten {
    pub atoms_path: PathBuf,
    pub edges_path: PathBuf,
    pub trajectories_path: PathBuf,
}

/// Write the resolution-failure file to
/// `atlas/resolution_failures.json`. Phase 3a/3b drops (unresolved
/// entity names, unresolved relation participants, unresolved claim
/// attributions) land here so the `sovereign enrich errors`
/// aggregator can include them alongside the per-phase cache
/// failures. Pre-Landing-3.A these drops were `debug!`-only and
/// invisible to the operator.
pub fn write_atlas_failures(
    atlas_dir: &Path,
    failures: &[crate::enrichment::pipeline::types::PhaseFailure],
) -> io::Result<PathBuf> {
    fs::create_dir_all(atlas_dir)?;
    let path = atlas_dir.join("resolution_failures.json");
    let wrapper = ResolutionFailuresFile {
        schema_version: RESOLUTION_FAILURES_SCHEMA_VERSION.to_string(),
        failures: failures.to_vec(),
    };
    write_atomic(&path, &wrapper)?;
    Ok(path)
}

const RESOLUTION_FAILURES_SCHEMA_VERSION: &str = "1.0";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolutionFailuresFile {
    pub schema_version: String,
    #[serde(default)]
    pub failures: Vec<crate::enrichment::pipeline::types::PhaseFailure>,
}

impl ResolutionFailuresFile {
    /// Read the failure file from `atlas_dir`. Returns `Ok(None)`
    /// when the file is absent (the clean case for a corpus that
    /// resolved without drops), or when the atlas directory itself
    /// doesn't exist yet. Parse errors propagate so a corrupt file
    /// surfaces loudly.
    pub fn load(atlas_dir: &Path) -> io::Result<Option<Self>> {
        let path = atlas_dir.join("resolution_failures.json");
        if !path.exists() {
            return Ok(None);
        }
        let raw = fs::read_to_string(&path)?;
        let parsed: Self = serde_json::from_str(&raw).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("parsing {}: {e}", path.display()),
            )
        })?;
        Ok(Some(parsed))
    }
}

/// On-disk layout of `atlas/ontology.json` — the declared ontology this
/// atlas was extracted under.
///
/// The atlas directory has to answer "what did this corpus declare" on its
/// own: `corpus-engine` cannot read the enrich `config.json` (that type lives
/// in `sovereign-enrichment-catalog`), and `_summary.json` is a derived cache
/// that must be reproducible from the atlas dir alone. So the resolve step
/// writes the policies down beside the atoms.
///
/// Absent for every corpus that declares no ontology, and for every atlas
/// built before ontology v1 — readers treat absence as "no declaration",
/// never as an error.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AtlasOntologyFile {
    pub schema_version: String,
    /// The `[enrichment.ontology] version` the policies were parsed under.
    #[serde(default)]
    pub ontology_version: u32,
    /// What the pipeline read. Same struct the recipe parses into, so a
    /// reader never re-derives it.
    pub policies: crate::enrichment::ontology::OntologyPolicies,
}

impl AtlasOntologyFile {
    pub const SCHEMA_VERSION: &'static str = "1.0";
    /// File name under `atlas/`. The ONE spelling — the writer and the
    /// summary reader below both go through it.
    pub const FILE: &'static str = "ontology.json";
}

/// Write `atlas/ontology.json`. Called from the resolve step after
/// [`write_atlas_full`], which is the only place that knows both the atlas
/// directory and the corpus's `EnrichConfig`.
pub fn write_atlas_ontology(
    atlas_dir: &Path,
    ontology_version: u32,
    policies: &crate::enrichment::ontology::OntologyPolicies,
) -> io::Result<PathBuf> {
    fs::create_dir_all(atlas_dir)?;
    let path = atlas_dir.join(AtlasOntologyFile::FILE);
    write_atomic(
        &path,
        &AtlasOntologyFile {
            schema_version: AtlasOntologyFile::SCHEMA_VERSION.to_string(),
            ontology_version,
            policies: policies.clone(),
        },
    )?;
    Ok(path)
}

/// Read `atlas/ontology.json`, or `None` when the atlas declares none or the
/// file cannot be parsed. Companion to [`write_atlas_ontology`]; the summary
/// reads it through this and nothing else opens the file by name.
pub fn read_atlas_ontology(atlas_dir: &Path) -> Option<AtlasOntologyFile> {
    let raw = fs::read(atlas_dir.join(AtlasOntologyFile::FILE)).ok()?;
    match serde_json::from_slice(&raw) {
        Ok(parsed) => Some(parsed),
        Err(e) => {
            tracing::warn!(
                atlas_dir = %atlas_dir.display(),
                error = %e,
                "atlas ontology: ontology.json present but unreadable; treating as undeclared"
            );
            None
        }
    }
}

/// Write a deterministic gaps file (Phase 7) to
/// `atlas/gaps.json`. Atomic sibling-tmp + rename, same contract as
/// the other atlas writers.
pub fn write_atlas_gaps(
    atlas_dir: &Path,
    gaps: &super::analysis::gaps::GapsOutput,
) -> io::Result<PathBuf> {
    fs::create_dir_all(atlas_dir)?;
    let path = atlas_dir.join("gaps.json");
    write_atomic(&path, gaps)?;
    Ok(path)
}

/// Write the tension candidate list (Phase 6 deterministic half) to
/// `atlas/tension_candidates.json`. The Phase 6 LLM classifier
/// (Landing 4) consumes this and emits `Tension` edges on
/// `atlas/edges.json`.
pub fn write_tension_candidates(
    atlas_dir: &Path,
    candidates: &super::analysis::tensions::TensionCandidatesOutput,
) -> io::Result<PathBuf> {
    fs::create_dir_all(atlas_dir)?;
    let path = atlas_dir.join("tension_candidates.json");
    write_atomic(&path, candidates)?;
    Ok(path)
}

/// Read the tension candidate list back from disk. Companion to
/// [`write_tension_candidates`]. Used by the Phase 6 LLM classifier
/// to fan out per-candidate prompts.
pub fn read_tension_candidates(
    atlas_dir: &Path,
) -> io::Result<super::analysis::tensions::TensionCandidatesOutput> {
    let path = atlas_dir.join("tension_candidates.json");
    let data = fs::read(&path)?;
    serde_json::from_slice(&data).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("parse tension_candidates.json: {e}"),
        )
    })
}

/// Write a cross-corpus edges file to
/// `atlas/cross_corpus_edges.json`. Each corpus carries its own
/// view of the bridge: edges whose `source` points at *its* atom
/// and whose `peer.atom_id` points at the matching atom on the
/// other corpus. Callers who detected edges in the
/// `A → B` direction typically also call `flip_for_peer` and
/// write the mirror file into B's atlas directory.
pub fn write_atlas_cross_corpus_edges(
    atlas_dir: &Path,
    file: &super::cross_corpus::CrossCorpusEdgesFile,
) -> io::Result<PathBuf> {
    fs::create_dir_all(atlas_dir)?;
    let path = atlas_dir.join("cross_corpus_edges.json");
    write_atomic(&path, file)?;
    Ok(path)
}

/// Read the cross-corpus edges file back from disk. Used by
/// traversal + operator inspection paths.
pub fn read_atlas_cross_corpus_edges(
    atlas_dir: &Path,
) -> io::Result<super::cross_corpus::CrossCorpusEdgesFile> {
    let path = atlas_dir.join("cross_corpus_edges.json");
    let data = fs::read(&path)?;
    serde_json::from_slice(&data).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("parse cross_corpus_edges.json: {e}"),
        )
    })
}

/// Write Phase 8 configurations to `atlas/configurations.json`.
/// Called even when the pipeline returns zero configurations so an
/// operator sees the pass ran. Companion to [`read_atlas_atoms`] /
/// [`read_atlas_edges`] — Configuration atoms also land in
/// `atoms.json` via `write_atlas_full`'s `configurations` slice,
/// but the dedicated file makes it easy for a brief assembler to
/// read just the configurational layer without loading every atom.
pub fn write_atlas_configurations(
    atlas_dir: &Path,
    configurations: &super::analysis::configuration::ConfigurationsOutput,
) -> io::Result<PathBuf> {
    fs::create_dir_all(atlas_dir)?;
    let path = atlas_dir.join("configurations.json");
    write_atomic(&path, configurations)?;
    Ok(path)
}

/// Read the atoms file back from disk. Used by Phase 6 / Phase 7
/// subcommands that run standalone after Phase 3b already wrote
/// the atlas directory.
pub fn read_atlas_atoms(atlas_dir: &Path) -> io::Result<AtomsFile> {
    let path = atlas_dir.join("atoms.json");
    let data = fs::read(&path)?;
    serde_json::from_slice(&data)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("parse atoms.json: {e}")))
}

/// Read the edges file back from disk. Companion to
/// [`read_atlas_atoms`].
pub fn read_atlas_edges(atlas_dir: &Path) -> io::Result<EdgesFile> {
    // NOTE: callers on the hot atom-detail path must go through
    // `atlas_view::atom_detail::cached_edges`, not this directly — the
    // Wikipedia atlas ships a 1.3 GB edges.json and this does a full
    // fs::read + serde parse every call. See the edges cache there.
    let path = atlas_dir.join("edges.json");
    let data = fs::read(&path)?;
    serde_json::from_slice(&data)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("parse edges.json: {e}")))
}

/// Replace `atlas/edges.json` with the provided file. Used by Phase
/// 6's LLM Tension classifier to merge new edges into the resolved
/// atlas without re-running the entire write_atlas pipeline. Atomic:
/// writes through a sibling `.tmp` + rename so a crash leaves the
/// pre-existing file intact rather than truncated.
pub fn write_atlas_edges(atlas_dir: &Path, edges: &EdgesFile) -> io::Result<PathBuf> {
    fs::create_dir_all(atlas_dir)?;
    let path = atlas_dir.join("edges.json");
    write_atomic(&path, edges)?;
    Ok(path)
}

/// Write a `serde_json::Serialize` value to `path` via a sibling
/// `.tmp` file + rename. Rejects a non-UTF-8 parent because we need
/// to build the sibling path as a string.
fn write_atomic<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path has no parent: {}", path.display()),
        )
    })?;
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("atlas")
    ));
    let data = serde_json::to_vec_pretty(value)
        .map_err(|e| io::Error::other(format!("serialise: {e}")))?;
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::atoms::{AtomId, ChunkRef, SectionPosition};
    use crate::enrichment::atlas::edges::{EdgeId, EdgeProvenance, EdgeType};
    use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType, EventType};
    use tempfile::tempdir;

    #[test]
    fn write_atlas_produces_three_files_with_schema_versions() {
        let dir = tempdir().unwrap();
        let atlas_dir = dir.path().join("atlas");
        let entity = Entity {
            id: AtomId::entity(1),
            canonical_name: "Alyosha".into(),
            aliases: Vec::new(),
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: "x".into(),
            defining_quote: None,
            salience: 1.0,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        };
        let event = Event {
            attributes: Default::default(),
            id: AtomId::event(1),
            description: "an event".into(),
            event_type: EventType::Other("unspecified".into()),
            participants: vec![AtomId::entity(1)],
            evidence: vec![ChunkRef::new("sec_0001", None)],
            section_position: SectionPosition::section("sec_0001"),
            causal_antecedents: Vec::new(),
            enrichment_depth: EnrichmentDepth::Extracted,
        };
        let edge = Edge {
            id: EdgeId::new(1),
            edge_type: EdgeType::Involves,
            source: event.id.clone(),
            target: entity.id.clone(),
            evidence: Vec::new(),
            trigger_event: None,
            sub_question: None,
            confidence: 1.0,
            provenance: EdgeProvenance::Derived,
        };

        let written = write_atlas(&atlas_dir, &[entity], &[event], &[edge]).unwrap();
        for p in [
            &written.atoms_path,
            &written.edges_path,
            &written.trajectories_path,
        ] {
            assert!(p.exists(), "expected file: {}", p.display());
        }
        let atoms_json = fs::read_to_string(&written.atoms_path).unwrap();
        let atoms_ver = format!(
            "\"schema_version\": \"{}\"",
            crate::enrichment::atlas::atoms::AtomsFile::SCHEMA_VERSION
        );
        assert!(
            atoms_json.contains(&atoms_ver),
            "atoms.json should carry the current schema_version"
        );
        assert!(atoms_json.contains("\"canonical_name\": \"Alyosha\""));
        let edges_json = fs::read_to_string(&written.edges_path).unwrap();
        assert!(edges_json.contains("\"edge_type\": \"Involves\""));
        let traj_json = fs::read_to_string(&written.trajectories_path).unwrap();
        assert!(traj_json.contains("\"schema_version\": \"2.0\""));
    }

    #[test]
    fn write_atlas_is_idempotent_on_rerun() {
        let dir = tempdir().unwrap();
        let atlas_dir = dir.path().join("atlas");
        write_atlas(&atlas_dir, &[], &[], &[]).unwrap();
        // Second run with a new atom overwrites cleanly.
        let entity = Entity {
            id: AtomId::entity(1),
            canonical_name: "Only".into(),
            aliases: Vec::new(),
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: "x".into(),
            defining_quote: None,
            salience: 1.0,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        };
        write_atlas(&atlas_dir, &[entity], &[], &[]).unwrap();
        let atoms_json = fs::read_to_string(atlas_dir.join("atoms.json")).unwrap();
        assert!(atoms_json.contains("\"Only\""));
        // No stray .tmp files.
        let stray: Vec<_> = fs::read_dir(&atlas_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .map(|s| s.starts_with('.') && s.ends_with(".tmp"))
                    .unwrap_or(false)
            })
            .collect();
        assert!(stray.is_empty());
    }
}
