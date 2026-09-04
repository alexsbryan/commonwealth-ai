// SPDX-License-Identifier: AGPL-3.0-or-later
//! Atlas on-disk writer — serialise `AtomsFile`, `EdgesFile`, and
//! the (currently placeholder) trajectories index into the
//! `atlas/` subdirectory of a corpus, then the v2 store and the ANN seed
//! table that make it readable and groundable.
//!
//! Since ei-3-index the write produces FOUR artifacts, not one: `atoms.json`
//! (the canonical export), `atoms.lance` + `edges.csr` (the v2 store, the read
//! path) and `atoms_ann.lance` (the seed table the walk grounds on). The last
//! is why [`write_atlas_full`] takes an [`AtlasSeeding`]: it is mandatory, and
//! a caller that cannot supply an embedder says so rather than silently
//! omitting the artifact.
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

use super::ann_store::{AtlasSeeding, SeedOutcome};
use super::atoms::{
    AtomEnvelope, AtomsFile, Claim, Configuration, Entity, Event, Question, Relation, State,
};
use super::context_filter::AtlasContextFilter;
use super::context_loader::{backfill_ann_blocking, BackfillOutcome};
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
///
/// Seeding is [`AtlasSeeding::Deferred`] here, and that is the one place the
/// choice is made rather than passed: a step-3a write is a PARTIAL atlas —
/// entities and events only, no claims, questions, positions or oppositions —
/// so a seed table built from it would index a fraction of the atoms and then
/// look fresh (`ann_table_is_fresh` compares against `atoms.json`, which the
/// 3b write restamps). The 3b write through `write_atlas_full` seeds the whole
/// atlas. Callers with no 3b pass at all (`awareness` extract/filter, the
/// engine's structural sidecar) get the deferred report and the
/// `svrn atlas backfill-ann` recovery, not a half-built table.
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
        &AtlasSeeding::Deferred(
            "step-3a partial write (entities + events only); the step-3b write seeds",
        ),
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
/// - `atlas/atoms.lance` + `atlas/edges.csr` — the v2 store, the read path.
/// - `atlas/atoms_ann.lance` — the ANN seed table, when `seeding` supplies an
///   embedder. This is the artifact that decides whether the corpus can ground
///   at all, so `seeding` has no default: see [`AtlasSeeding`].
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
    seeding: &AtlasSeeding,
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

    // …then the seed table, from the `atoms.json` just written. Third artifact
    // of the same write, not a later step somebody remembers to run.
    let seed = seed_atlas(atlas_dir, seeding)?;

    Ok(AtlasWritten {
        atoms_path,
        edges_path,
        trajectories_path,
        seed,
    })
}

/// Write `atlas/atoms_ann.lance` for the atlas just written, through the ONE
/// backfill writer (`context_loader::backfill_ann`, the same call
/// `svrn atlas backfill-ann` and the `enrich build` Backfill step make —
/// ARCH §19, §10.6). Runs AFTER `atoms.json` because that is the backfill's
/// input, and after the v2 store so a seeded atlas always has a store.
///
/// **Fail-hard on `With`**, exactly as the v2 store write is: a caller that
/// said it would seed and could not has produced an atlas that cannot ground,
/// and reporting that as a warning is how SEP ended up with 22 seeded atlases
/// out of 1,770. `atoms.json` is already on disk either way, so the recovery is
/// always `svrn atlas backfill-ann <id>`.
///
/// The grounding filter is `AtlasContextFilter::default()` — the universe the
/// daemon seeds `atlas_navigate_ann` from. No other filter may be used here, or
/// the table indexes atoms the query path never looks at.
fn seed_atlas(atlas_dir: &Path, seeding: &AtlasSeeding) -> io::Result<SeedOutcome> {
    let corpus_id = corpus_id_of(atlas_dir);
    let embed = match seeding {
        AtlasSeeding::Deferred(why) => {
            tracing::info!(
                corpus = corpus_id,
                atlas = %atlas_dir.display(),
                reason = why,
                "atlas write: ANN seed table deferred; this corpus cannot ground until \
                 `svrn atlas backfill-ann` runs"
            );
            return Ok(SeedOutcome::Deferred(why));
        }
        AtlasSeeding::With(embed) => embed,
    };
    let filter = AtlasContextFilter::default();
    match backfill_ann_blocking(embed, atlas_dir, &corpus_id, &filter).map_err(io::Error::other)? {
        BackfillOutcome::Built(stats) => {
            tracing::info!(
                corpus = corpus_id,
                rows = stats.resolved,
                of = stats.total,
                "atlas write: ANN seed table written; this corpus grounds"
            );
            Ok(SeedOutcome::Written {
                rows: stats.resolved,
                of: stats.total,
            })
        }
        BackfillOutcome::NoSeedableAtoms {
            min_description_chars,
        } => Ok(SeedOutcome::NoSeedableAtoms {
            min_description_chars,
        }),
    }
}

/// The corpus id an atlas dir belongs to — its parent directory's name. Self
/// description, so `write_atlas_full`'s signature does not grow a corpus-id
/// parameter every caller would have to thread. Same derivation
/// [`write_atlas_v2_store`] uses; one place, one answer.
fn corpus_id_of(atlas_dir: &Path) -> String {
    atlas_dir
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
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
    super::store::write_store_blocking(atlas_dir, &corpus_id_of(atlas_dir), atoms, edges)
        .map(|_| ())
        .map_err(io::Error::other)
}

#[derive(Debug, Clone)]
pub struct AtlasWritten {
    pub atoms_path: PathBuf,
    pub edges_path: PathBuf,
    pub trajectories_path: PathBuf,
    /// What this write did about `atoms_ann.lance`. Carried back so the
    /// resolve step prints it and no caller can read a deferred write as a
    /// seeded one (ARCH §18.3 — absence is reported, never defaulted).
    pub seed: SeedOutcome,
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
/// Written by EVERY pipeline since ei-2-map (`EPISTEMIC_INDEX.md` §1, Map
/// row: an atlas that cannot describe itself is not an atlas) — a built-in
/// genre writes its fixed vocabulary down through the same struct. Absent
/// only for an atlas built before then; readers treat absence as "no
/// declaration", never as an error.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AtlasOntologyFile {
    pub schema_version: String,
    /// The `[enrichment.ontology] version` the policies were parsed under —
    /// or [`Self::BUILTIN_ONTOLOGY_VERSION`] for a built-in pipeline's map,
    /// which is written in that language rather than parsed from a recipe.
    #[serde(default)]
    pub ontology_version: u32,
    /// The pipeline that extracted under these policies, as the registry
    /// spells it (`literary_atlas`, `custom_atlas`, …). Tells a reader
    /// whether the map was DECLARED by an author (`custom_atlas`) or WRITTEN
    /// DOWN by a genre. Empty on a file written before ei-2-map; readers
    /// report that, never guess.
    #[serde(default)]
    pub pipeline_id: String,
    /// What the pipeline read. Same struct the recipe parses into, so a
    /// reader never re-derives it.
    pub policies: crate::enrichment::ontology::OntologyPolicies,
}

impl AtlasOntologyFile {
    pub const SCHEMA_VERSION: &'static str = "1.0";
    /// File name under `atlas/`. The ONE spelling — the writer and the
    /// summary reader below both go through it.
    pub const FILE: &'static str = "ontology.json";
    /// The declaration language a built-in pipeline's map is written in
    /// (`pipelines/ontologies/*.toml` are version-1 block bodies). One number,
    /// one home: the resolve step records it and `declaration.rs` parses under it.
    pub const BUILTIN_ONTOLOGY_VERSION: u32 = 1;

    /// Was this map declared by a recipe author, as opposed to written down
    /// by a built-in genre? The custom pipeline reports `custom_atlas`.
    pub fn is_author_declared(&self) -> bool {
        self.pipeline_id == "custom_atlas"
    }
}

/// Write `atlas/ontology.json`. Called from the resolve step after
/// [`write_atlas_full`] for every pipeline: `policies` is
/// `Pipeline::declared_ontology()`, `pipeline_id` is `Pipeline::id()`, and
/// `ontology_version` is the recipe's for the custom path or
/// [`AtlasOntologyFile::BUILTIN_ONTOLOGY_VERSION`] otherwise.
pub fn write_atlas_ontology(
    atlas_dir: &Path,
    pipeline_id: &str,
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
            pipeline_id: pipeline_id.to_string(),
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

/// Read the stored `atlas/schema_validation.json` as the typed report, or
/// `None` when the report step has not run or the file cannot be parsed.
///
/// The report is what the LAST build found — it is not recomputed here, so a
/// caller showing it to a user is showing a build's verdict, not a live one.
/// That is the point: it is the artefact `svrn enrich schema-report` writes,
/// and re-deriving it would mean re-reading every atom.
///
/// The one typed door to this file. Two callers poke individual keys out of it
/// as untyped JSON (`read_code_walk_visibility`, and the source-corpus lookup
/// in `atlas_patch_code`) because they predate the report being deserializable
/// as a whole; they are not folded in here, but nothing NEW should open this
/// file by name (§10.6).
pub fn read_schema_validation_report(
    atlas_dir: &Path,
) -> Option<super::schema_validation::SchemaValidationReport> {
    let raw = fs::read(atlas_dir.join("schema_validation.json")).ok()?;
    match serde_json::from_slice(&raw) {
        Ok(parsed) => Some(parsed),
        Err(e) => {
            tracing::warn!(
                atlas_dir = %atlas_dir.display(),
                error = %e,
                "atlas report: schema_validation.json present but unreadable; \
                 treating as not-yet-reported"
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

/// Write the declared-pattern findings to `atlas/pattern_findings.json`.
/// Atomic sibling-tmp + rename, same contract as the other atlas writers.
pub fn write_atlas_pattern_findings(
    atlas_dir: &Path,
    findings: &super::analysis::patterns_adapter::PatternFindingsOutput,
) -> io::Result<PathBuf> {
    fs::create_dir_all(atlas_dir)?;
    let path = atlas_dir.join("pattern_findings.json");
    write_atomic(&path, findings)?;
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

/// Replace `atlas/atoms.json` with the provided file, and rebuild the v2
/// store from it. The atom-side companion to [`write_atlas_edges`]: Phase 6's
/// classifier replaces the atom set with one carrying the `same_as` Claims an
/// `equivalent` verdict reifies, dropping what a prior run wrote, so it cannot
/// use [`append_atoms_and_edges`] — the contract is REPLACE, not append.
///
/// It rebuilds `atoms.lance` for the same reason that function does, and the
/// reason is worth stating once for both: `atoms.json` is the export and the
/// store is what the runtime reads. Writing only the JSON leaves the read path
/// missing atoms the export claims are there, silently, until something
/// queries for one. (Merging P3 and P4 on 2026-09-02 put the two writers side
/// by side and made the omission visible; the appending one had it right.)
///
/// The JSON write is atomic — sibling `.tmp` + rename — so a crash leaves the
/// pre-existing file intact rather than truncated. The pair is not atomic
/// ACROSS the two artefacts: the store is rebuilt after the rename, which is
/// the ordering `write_atlas_full` and `append_atoms_and_edges` both use.
pub fn write_atlas_atoms(atlas_dir: &Path, atoms: &AtomsFile) -> io::Result<PathBuf> {
    fs::create_dir_all(atlas_dir)?;
    let path = atlas_dir.join("atoms.json");
    write_atomic(&path, atoms)?;
    let edges_file = read_atlas_edges(atlas_dir)?;
    write_atlas_v2_store(atlas_dir, &atoms.atoms, &edges_file.edges)?;
    Ok(path)
}

/// Append atoms and edges to a written atlas, keeping the v2 store in step.
///
/// The one supported way to add to an atlas after `write_atlas_full` ran.
/// Reconciliation is the first caller: it runs over a resolved atlas and
/// reifies each merge as a `same_as` Claim, which has to reach the same
/// `atoms.lance` the runtime reads — writing only `atoms.json` would leave the
/// read path missing atoms the export claims are there.
///
/// Not atomic ACROSS the three artefacts (JSON, JSON, store): each is written
/// through its own rename, and the store is rebuilt last from the merged set,
/// which is the same ordering `write_atlas_full` uses. Ids are the caller's
/// problem — nothing here renumbers.
pub fn append_atoms_and_edges(
    atlas_dir: &Path,
    atoms: &[AtomEnvelope],
    edges: &[Edge],
) -> io::Result<()> {
    if atoms.is_empty() && edges.is_empty() {
        return Ok(());
    }
    let mut atoms_file = read_atlas_atoms(atlas_dir)?;
    let mut edges_file = read_atlas_edges(atlas_dir)?;
    atoms_file.atoms.extend(atoms.iter().cloned());
    edges_file.edges.extend(edges.iter().cloned());
    write_atomic(&atlas_dir.join("atoms.json"), &atoms_file)?;
    write_atomic(&atlas_dir.join("edges.json"), &edges_file)?;
    write_atlas_v2_store(atlas_dir, &atoms_file.atoms, &edges_file.edges)
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

    // ── the four-artifact contract (ei-3-index) ──────────────────────────────

    /// One extracted Entity with enough signal to clear the grounding filter's
    /// floor, in a corpus dir shaped like a real index root so
    /// `corpus_id_of` finds a name.
    fn seedable_entity() -> Entity {
        Entity {
            id: AtomId::entity(1),
            canonical_name: "guest logbook".into(),
            aliases: Vec::new(),
            entity_type: EntityType::Concept,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: "A physical record kept by the front door to track \
                          overnight guests."
                .into(),
            defining_quote: None,
            salience: 0.33,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        }
    }

    /// Deterministic 4-d embedder. Same shape as `context_loader`'s.
    fn unit_embed() -> crate::types::EmbedFn {
        std::sync::Arc::new(|text: &str| {
            let n = text.len() as f32;
            Box::pin(async move { Ok(vec![n, 1.0, 0.0, 0.0]) })
        })
    }

    /// An embedder that always refuses, for the fail-hard case.
    fn dead_embed() -> crate::types::EmbedFn {
        std::sync::Arc::new(|_: &str| {
            Box::pin(async move { Err(crate::error::Error::Embed("embed slot down".into())) })
        })
    }

    fn write_seeded(atlas_dir: &Path, seeding: &AtlasSeeding) -> io::Result<AtlasWritten> {
        write_atlas_full(
            atlas_dir,
            &[seedable_entity()],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &std::collections::BTreeMap::new(),
            seeding,
        )
    }

    /// ei-3-index bar 1 — the whole point of the order. Watched failing before
    /// the change with `atoms_ann.lance` absent from a write that produced
    /// `atoms.json`, `atoms.lance` and `edges.csr`: the exact on-disk shape
    /// 1,748 of 1,770 SEP atlases were in, which loads and enumerates and
    /// cannot ground.
    #[test]
    fn a_seeded_write_leaves_all_four_artifacts() {
        let dir = tempdir().unwrap();
        let atlas_dir = dir.path().join("wessex-fixture").join("atlas");
        let written = write_seeded(&atlas_dir, &AtlasSeeding::With(unit_embed())).unwrap();

        for artifact in ["atoms.json", "atoms.lance", "edges.csr", "atoms_ann.lance"] {
            assert!(
                atlas_dir.join(artifact).exists(),
                "{artifact} missing from a seeded atlas write"
            );
        }
        assert_eq!(written.seed, SeedOutcome::Written { rows: 1, of: 1 });
    }

    /// ei-3-index bar 2 — a deferral is REPORTED, not silently identical to a
    /// seeded write. Watched failing with `AtlasWritten` carrying no seed
    /// field at all, which is how a caller could not tell the two apart.
    #[test]
    fn a_deferred_write_names_the_reason_and_writes_no_table() {
        let dir = tempdir().unwrap();
        let atlas_dir = dir.path().join("deferred-fixture").join("atlas");
        let written =
            write_seeded(&atlas_dir, &AtlasSeeding::Deferred("no embedder here")).unwrap();

        assert_eq!(written.seed, SeedOutcome::Deferred("no embedder here"));
        assert!(!atlas_dir.join("atoms_ann.lance").exists());
        assert!(
            atlas_dir.join("atoms.lance").exists(),
            "a deferred seed still writes the v2 store"
        );
        assert!(written.seed.describe().contains("deferred"));
    }

    /// ei-3-index bar 3 — fail-hard. Watched failing with the pre-change
    /// behaviour (`tracing::warn!` and carry on), which is the mechanism that
    /// let SEP accumulate 1,748 seedless atlases without one red build.
    #[test]
    fn a_seed_that_cannot_embed_fails_the_atlas_write() {
        let dir = tempdir().unwrap();
        let atlas_dir = dir.path().join("dead-embed-fixture").join("atlas");
        let err = write_seeded(&atlas_dir, &AtlasSeeding::With(dead_embed()))
            .expect_err("an atlas that cannot ground is a failed write, not a warning");

        assert!(
            atlas_dir.join("atoms.json").exists(),
            "atoms.json lands before the seed, so `svrn atlas backfill-ann` can recover"
        );
        assert!(!atlas_dir.join("atoms_ann.lance").exists());
        // The message must send the operator to the embed slot, not to the
        // filter knobs. Before `EmbedRefusedAll` existed this read
        // "no atom-bearing entries for dead-embed-fixture (0/0) -- nothing to
        // index", which says the atlas is empty when the embedder is down.
        let msg = err.to_string();
        assert!(
            msg.contains("refused every one of the 1 atom"),
            "seed failure must name the refusal and its size: {msg}"
        );
        assert!(
            msg.contains("NOT a filter problem"),
            "seed failure must not be mistakable for an over-tight filter: {msg}"
        );
    }

    /// ei-3-index bar 4 — `write_atlas` is the step-3a partial write and must
    /// NOT seed. Watched failing with `write_atlas` inheriting a `With`
    /// seeding: it wrote a table from entities-only, which then read as fresh
    /// against the step-3b `atoms.json` it predated.
    #[test]
    fn the_step_3a_wrapper_defers_seeding() {
        let dir = tempdir().unwrap();
        let atlas_dir = dir.path().join("step3a-fixture").join("atlas");
        let written = write_atlas(&atlas_dir, &[seedable_entity()], &[], &[]).unwrap();
        assert!(matches!(written.seed, SeedOutcome::Deferred(_)));
        assert!(!atlas_dir.join("atoms_ann.lance").exists());
    }
}
