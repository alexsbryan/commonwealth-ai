// SPDX-License-Identifier: AGPL-3.0-or-later
//! WHICH ATOM KINDS the ANN seed table (`atlas/atoms_ann.lance`) is built
//! from — the seed table's **population**, and the ONE place it is decided.
//!
//! Before ei-3c the population was `AtlasContextFilter::default()`: the
//! RETRIEVAL filter, whose kind admission is Entity-only in production (claims
//! env-gated off, configurations hard false). That made the reader's filter the
//! author of the writer's table, and the two disagreed. ei-4 measured the
//! consequence on 2026-09-04: every seed table on this box was Entity-only, so
//! the navigation map's `tension` row — which seeds on `Claim` + `Position`
//! (`ontology::navigation::WalkPolicy::tension`) — seeded NOTHING on any
//! corpus. wessex-hoard: 49 Claim atoms, 48 candidates, 48 dropped, 0 seeds.
//!
//! So the population is derived from the corpus's own navigation map — the
//! union of every row's [`SeedPolicy::kinds`], plus the declared types the
//! `enumeration` row's `declared` flag points at — and the retrieval filter
//! becomes a CONSUMER of the table rather than its author (ARCH §10.6, one
//! decider). [`AtlasSeeding`](super::ann_store::AtlasSeeding) gains no arm:
//! this is a derivation, not a new lifecycle.
//!
//! Two properties worth holding:
//!
//! - **The map is a FLOOR, never a ceiling.** The population is unioned with
//!   whatever the incoming filter already admitted
//!   ([`AtlasContextFilter::admits_atom`](super::context_filter::AtlasContextFilter::admits_atom)),
//!   so no corpus loses a seed it had. That matters concretely: SEP's
//!   `ArgumentReconstruction` atoms are seeded unconditionally and appear in no
//!   pre-registered row, so a narrowing derivation would have silently deleted
//!   the 8th atom type from every SEP seed table.
//! - **Undeclared means PRE-REGISTERED, not "the old population".** The walk's
//!   own decider ([`navigation_policy_for`](super::ground::navigation_policy_for))
//!   gives a corpus that declared nothing the pre-registered table, so a writer
//!   that instead fell back to the Entity-only population would re-open exactly
//!   the writer/walk disagreement this module closes. The source is reported
//!   either way ([`PolicySource`], §18.3) — it is named, never silent.
//!
//! The population is a pure function of the atlas directory, which is what lets
//! [`population_marker_is_current`] make a population change read as STALENESS
//! in [`ann_table_is_fresh`](super::ann_store::ann_table_is_fresh) without any
//! caller passing a filter down.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use super::atoms::AtomType;
use super::ground::PolicySource;
use super::writer::{corpus_id_of, read_atlas_ontology, AtlasOntologyFile};
use crate::enrichment::ontology::{NavigationPolicy, TypeKind};

/// The population DERIVATION's version, written as the marker's first line and
/// the only thing [`population_marker_is_current`] parses.
///
/// Bump it when the derivation changes in a way that makes an existing table's
/// population wrong — a new pre-registered seed row, a kind added to a default
/// row, a change to [`ALWAYS_SEEDED`]. A table written under an older version
/// is stale by definition, because nothing on disk records what the code used
/// to think. `1` is ei-3c, the first version there is. `2` is ei-7a:
/// `Summary` joined the `thematic` row's seed kinds, which is exactly the
/// "a kind added to a default row" case above — every table written under
/// `1` derives a population its map no longer agrees with, so it is stale
/// by definition and rebuilds on next read. `3` is the same case once more:
/// `Event` and `Relation` joined the `lookup` row on 2026-09-09, closing the
/// hole where a third of a section-extracted atlas carried no vector at all
/// (`chaos-secret-agent`: `ann.embedded_atoms` 151 against 226 atoms, the
/// missing 75 being Event 33 + Relation 20 + Question 22).
pub const SEED_POPULATION_SCHEMA: u32 = 3;

/// The marker file, beside `atoms_ann.lance` in the atlas dir. Small on
/// purpose: `AtlasContextManager::init()` stats it once per installed atlas
/// (1,770 of them for SEP) at daemon boot, so the freshness decision reads one
/// line and two mtimes and never parses `ontology.json`.
pub const POPULATION_FILE: &str = "atoms_ann.population";

/// The seed population for one atlas: the kinds, and whose decision they were.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedPopulation {
    /// The atom kinds the table is built from. A `BTreeSet` so the
    /// fingerprint is order-independent — a map that lists `[Claim, Position]`
    /// and one that lists `[Position, Claim]` are the same population and must
    /// not force a re-embed.
    pub kinds: BTreeSet<AtomType>,
    /// Which map this came from — the corpus's own declaration, or the
    /// pre-registered table. Reused from the walk rather than minted here, so
    /// the writer and the walk report provenance in one vocabulary (§19).
    pub source: PolicySource,
}

impl SeedPopulation {
    /// The stable string a written table records, and the thing a future
    /// derivation is compared against by eye when a table looks wrong.
    /// Version first, kinds second — [`population_marker_is_current`] reads
    /// only the first line.
    pub fn fingerprint(&self) -> String {
        format!(
            "{SEED_POPULATION_SCHEMA}\n{}\n{}\n",
            self.kinds
                .iter()
                .map(AtomType::label)
                .collect::<Vec<_>>()
                .join(","),
            self.source.label(),
        )
    }
}

/// The kinds the atlas writer has seeded since before any navigation map
/// existed, and which no map can therefore be read as switching OFF.
///
/// `Entity` is every corpus's baseline grounding surface and is named by the
/// `lookup` row anyway. `ArgumentReconstruction` is named by NO pre-registered
/// row and is seeded unconditionally by the loader — SEP's named arguments live
/// there — so leaving it out would delete a whole atom type from 1,770 seed
/// tables on the next re-seed. Named here rather than left implicit in the
/// filter, because a floor you cannot see is a floor you will trip over.
const ALWAYS_SEEDED: [AtomType; 2] = [AtomType::Entity, AtomType::ArgumentReconstruction];

/// The atom kind a DECLARED type materialises as. The `enumeration` row's
/// `declared` flag seeds on the corpus's declared types, and a declared type is
/// written to disk as one of five atom kinds. No wildcard arm: a sixth
/// [`TypeKind`] must be answered here, not defaulted (ARCH §2).
fn declared_atom_type(kind: TypeKind) -> AtomType {
    match kind {
        TypeKind::Entity => AtomType::Entity,
        TypeKind::Relation => AtomType::Relation,
        TypeKind::Claim => AtomType::Claim,
        TypeKind::Event => AtomType::Event,
        TypeKind::State => AtomType::State,
    }
}

/// Derive the seed population for the atlas at `atlas_dir`.
///
/// Reads `atlas/ontology.json` through the one typed door
/// ([`read_atlas_ontology`]) and unions:
///
/// - every navigation row's `seed.kinds`,
/// - the declared types' atom kinds, for each row whose `seed.declared` is set,
/// - [`ALWAYS_SEEDED`].
///
/// `seed.entity_types` is deliberately NOT applied: it narrows one row's Entity
/// seeds to `concept`, and the union of five rows admits Entity unnarrowed
/// anyway. Narrowing the TABLE by it would starve the `lookup` row, which seeds
/// on any Entity.
///
/// [`PolicySource::Declared`] exactly when `ontology.json` is present — types
/// or no types, the same test [`navigation_policy_for`](super::ground::navigation_policy_for)
/// applies to `AtlasProvider::navigation()` since map-conversion rung 3, so
/// the writer and the walk agree on whose map is in force by construction
/// rather than by comment. (Before rung 3 a typeless file's rows were used
/// here and labelled pre-registered, which was neither.)
pub fn seed_population(atlas_dir: &Path) -> SeedPopulation {
    let (navigation, declared, source) = match read_atlas_ontology(atlas_dir) {
        Some(f) => {
            let declared: Vec<TypeKind> = f.policies.shape.types.iter().map(|t| t.kind).collect();
            (
                f.policies.navigation,
                declared,
                PolicySource::Declared(corpus_id_of(atlas_dir)),
            )
        }
        None => (
            NavigationPolicy::default(),
            Vec::new(),
            PolicySource::PreRegistered,
        ),
    };

    let mut kinds: BTreeSet<AtomType> = ALWAYS_SEEDED.into_iter().collect();
    for (_, walk) in navigation.rows() {
        kinds.extend(walk.seed.kinds.iter().copied());
        if walk.seed.declared {
            kinds.extend(declared.iter().copied().map(declared_atom_type));
        }
    }
    SeedPopulation { kinds, source }
}

/// The marker's path for an atlas dir.
pub fn population_marker_path(atlas_dir: &Path) -> PathBuf {
    atlas_dir.join(POPULATION_FILE)
}

/// Record the population a seed table was just written under. Called by the ONE
/// writer immediately after the table lands, so the two are always written
/// together and the marker is never newer than the table it describes.
pub fn write_population_marker(atlas_dir: &Path, population: &SeedPopulation) -> io::Result<()> {
    std::fs::write(population_marker_path(atlas_dir), population.fingerprint())
}

/// Is the recorded population still the one this build would derive?
///
/// Deliberately cheap on the common path — one small read and two mtimes, no
/// `ontology.json` parse — because the daemon runs it once per installed atlas
/// at boot. Three ways to be stale, and each is a real table on this box:
///
/// - **No marker.** Every table written before ei-3c, including the ones the
///   SEP backfill battery is producing right now. They are Entity-only and must
///   be rebuilt, not trusted.
/// - **A different [`SEED_POPULATION_SCHEMA`].** The code's derivation moved
///   under a table that cannot know it.
/// - **`ontology.json` newer than the marker, AND the population it derives is
///   not already in the table.** The corpus re-declared its map after the
///   table was embedded. A re-declaration is not by itself a different
///   population: map-conversion rung 3 (2026-09-08) writes `ontology.json`
///   beside 1,770 SEP tables whose rows it does not change, and the philosophy
///   map's population is the pre-registered one minus `Position` — a kind SEP
///   has no atoms of. So on this branch, and only on this branch, the marker's
///   recorded kinds are compared with the derived ones: the table is current
///   when it was seeded on a SUPERSET of what the map now asks for and the
///   atlas holds no atoms of the surplus kinds. The walk filters seeds by row
///   kind at query time (`ground::seed_admits`), so such a table is the same
///   table; the surplus-kind clause is for the unfiltered row, which seeds on
///   whatever the table returns. Absent census (`_summary.json` missing or
///   stale) reads as stale — rebuild rather than trust.
///
/// Failing inputs: write a table, then rewrite `ontology.json` with a
/// `navigation` section that adds a kind (stale); rewrite it with one that
/// drops a kind the atlas has no atoms of (current) — both in the tests below.
pub fn population_marker_is_current(atlas_dir: &Path) -> bool {
    let path = population_marker_path(atlas_dir);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return false;
    };
    let mut lines = raw.lines();
    match lines.next() {
        Some(v) if v.trim().parse::<u32>() == Ok(SEED_POPULATION_SCHEMA) => {}
        _ => return false,
    }
    let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    match (
        mtime(&path),
        mtime(&atlas_dir.join(AtlasOntologyFile::FILE)),
    ) {
        (Some(marker), Some(ontology)) if marker >= ontology => true,
        // No map on disk: nothing newer than the marker exists to honour.
        (Some(_), None) => true,
        // The map moved after the table was embedded: ask whether the table
        // already holds the population the map now derives.
        (Some(_), Some(_)) => {
            let Some(recorded) = lines.next().map(kinds_from_marker_line) else {
                return false;
            };
            recorded_population_covers(atlas_dir, &recorded)
        }
        // The marker read but will not stat — rebuild rather than trust it.
        _ => false,
    }
}

/// The kinds line of a marker (`entity,claim,…`), parsed back through the same
/// labels [`SeedPopulation::fingerprint`] wrote. An unknown label is dropped:
/// a kind this build no longer knows cannot be one it needs.
fn kinds_from_marker_line(line: &str) -> BTreeSet<AtomType> {
    line.split(',')
        .map(str::trim)
        .filter_map(|label| AtomType::ALL.iter().copied().find(|k| k.label() == label))
        .collect()
}

/// Does a table seeded on `recorded` already hold the population this atlas
/// derives today? True when `recorded` ⊇ derived and every surplus kind has
/// zero atoms in the atlas's census; false without a census.
///
/// The census is the cached `_summary.json` when its key still matches, else
/// one computed in memory from `atoms.json` — computed and NOT persisted,
/// because this runs inside a freshness probe and a probe writes nothing.
/// The cache key covers the CSR's mtime, so right after `migrate-all` rebuilds
/// a store the cached summary is stale by construction; reading only the cache
/// here made every rebuilt store re-seed on the first run (2026-09-08).
fn recorded_population_covers(atlas_dir: &Path, recorded: &BTreeSet<AtomType>) -> bool {
    let derived = seed_population(atlas_dir).kinds;
    if !recorded.is_superset(&derived) {
        return false;
    }
    let Some(summary) = super::summary::read_current_summary(atlas_dir)
        .or_else(|| super::summary::compute_summary(atlas_dir).ok())
    else {
        return false;
    };
    recorded
        .difference(&derived)
        .all(|surplus| summary.atom_counts.get(surplus).copied().unwrap_or(0) == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn atlas_with_ontology(json: &str) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join(AtlasOntologyFile::FILE), json).expect("write ontology");
        tmp
    }

    /// The pre-registered table's union, which is what a corpus that declared
    /// nothing seeds on. Failing input: drop a kind from any
    /// `WalkPolicy::*()` row, or from `ALWAYS_SEEDED`.
    #[test]
    fn an_undeclared_atlas_seeds_the_pre_registered_union() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pop = seed_population(tmp.path());
        assert_eq!(pop.source, PolicySource::PreRegistered);
        assert_eq!(
            pop.kinds,
            BTreeSet::from([
                AtomType::Entity,
                AtomType::Event,
                AtomType::Relation,
                AtomType::ArgumentReconstruction,
                AtomType::Configuration,
                AtomType::Claim,
                AtomType::Position,
                AtomType::State,
                AtomType::Summary,
            ]),
            "thematic seeds Configuration+Entity+Summary, trajectory Entity+State, \
             tension Claim+Position; Entity and ArgumentReconstruction are the floor"
        );
    }

    /// A map that switches every row down to one kind still keeps the floor —
    /// the map cannot delete `ArgumentReconstruction` out from under SEP.
    /// Failing input: derive the population as the bare union.
    #[test]
    fn a_narrow_map_cannot_drop_the_always_seeded_kinds() {
        let one_row = r#"{"schema_version":"1","ontology_version":1,
            "pipeline_id":"custom_atlas","policies":{
              "shape":{"types":[{"name":"coin","kind":"entity"}]},
              "navigation":{
                "thematic":{"seed":{"kinds":["Claim"]},"walk":[],"hops":1,"budget":6},
                "trajectory":{"seed":{"kinds":[]},"walk":[],"hops":1,"budget":6},
                "tension":{"seed":{"kinds":[]},"walk":[],"hops":1,"budget":6},
                "enumeration":{"seed":{"kinds":[]},"walk":[],"hops":0,"budget":6},
                "lookup":{"seed":{"kinds":[]},"walk":[],"hops":1,"budget":6}}}}"#;
        let tmp = atlas_with_ontology(one_row);
        let pop = seed_population(tmp.path());
        assert_eq!(
            pop.kinds,
            BTreeSet::from([
                AtomType::Entity,
                AtomType::ArgumentReconstruction,
                AtomType::Claim
            ])
        );
        assert!(matches!(pop.source, PolicySource::Declared(_)));
    }

    /// `declared: true` on the enumeration row pulls the corpus's declared
    /// types in by their atom kind. Failing input: ignore `seed.declared`.
    #[test]
    fn the_enumeration_row_pulls_in_the_declared_types() {
        let declared = r#"{"schema_version":"1","ontology_version":1,
            "pipeline_id":"custom_atlas","policies":{
              "shape":{"types":[
                {"name":"coin","kind":"entity"},
                {"name":"attribution","kind":"claim"},
                {"name":"minted","kind":"event"}]},
              "navigation":{
                "thematic":{"seed":{"kinds":[]},"walk":[],"hops":1,"budget":6},
                "trajectory":{"seed":{"kinds":[]},"walk":[],"hops":1,"budget":6},
                "tension":{"seed":{"kinds":[]},"walk":[],"hops":1,"budget":6},
                "enumeration":{"seed":{"declared":true},"walk":[],"hops":0,"budget":6},
                "lookup":{"seed":{"kinds":[]},"walk":[],"hops":1,"budget":6}}}}"#;
        let tmp = atlas_with_ontology(declared);
        let pop = seed_population(tmp.path());
        assert!(pop.kinds.contains(&AtomType::Claim));
        assert!(pop.kinds.contains(&AtomType::Event));
    }

    /// The three ways a marker goes stale, each watched failing (§18.1).
    #[test]
    fn the_marker_is_stale_when_absent_when_versioned_apart_and_when_the_map_moves() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(
            !population_marker_is_current(tmp.path()),
            "absent marker is stale"
        );

        let pop = seed_population(tmp.path());
        write_population_marker(tmp.path(), &pop).expect("write marker");
        assert!(
            population_marker_is_current(tmp.path()),
            "just-written marker is current"
        );

        std::fs::write(population_marker_path(tmp.path()), "0\nentity\n").expect("write");
        assert!(
            !population_marker_is_current(tmp.path()),
            "a marker from another derivation version is stale"
        );

        write_population_marker(tmp.path(), &pop).expect("rewrite marker");
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(tmp.path().join(AtlasOntologyFile::FILE), "{}").expect("write ontology");
        assert!(
            !population_marker_is_current(tmp.path()),
            "a map re-declared after the table was embedded is stale"
        );
    }

    /// A map re-declared AFTER the table was embedded is current when the
    /// table was seeded on a superset of what the map now derives and the
    /// atlas holds no atoms of the surplus kinds — the map-conversion case
    /// (rung 3, 2026-09-08): 1,770 SEP tables seeded under the pre-registered
    /// union receive philosophy's map, which asks for that union minus
    /// `Position`, a kind SEP has no atoms of. Watched failing in both
    /// directions (§18.1, §18.6): no census is stale; a map that ADDS a kind
    /// is stale; the covering map with a census is current.
    #[test]
    fn a_map_written_after_the_table_is_current_when_the_table_already_holds_its_population() {
        use crate::enrichment::atlas::atoms::AtomsFile;

        let tmp = tempfile::tempdir().expect("tempdir");
        // The table was seeded under the pre-registered union: seven kinds.
        let seeded = seed_population(tmp.path());
        write_population_marker(tmp.path(), &seeded).expect("write marker");
        std::thread::sleep(std::time::Duration::from_millis(20));

        // The map arrives later and derives a SUBSET: Entity + Argument (floor) + Claim.
        let narrower = r#"{"schema_version":"1","ontology_version":1,
            "pipeline_id":"philosophy_atlas","policies":{
              "shape":{"types":[]},
              "navigation":{
                "thematic":{"seed":{"kinds":["Claim"]},"walk":[],"hops":1,"budget":6},
                "trajectory":{"seed":{"kinds":[]},"walk":[],"hops":1,"budget":6},
                "tension":{"seed":{"kinds":[]},"walk":[],"hops":1,"budget":6},
                "enumeration":{"seed":{"kinds":[]},"walk":[],"hops":0,"budget":6},
                "lookup":{"seed":{"kinds":[]},"walk":[],"hops":1,"budget":6}}}}"#;
        std::fs::write(tmp.path().join(AtlasOntologyFile::FILE), narrower).expect("write ontology");
        assert!(
            seeded.kinds.is_superset(&seed_population(tmp.path()).kinds),
            "fixture: the recorded population must cover the new map's"
        );
        assert!(
            !population_marker_is_current(tmp.path()),
            "no atoms.json, so no census: the surplus kinds cannot be shown absent, so stale"
        );

        // A census with zero atoms of every kind — computed in memory, no
        // `_summary.json` written, which is the state right after a store
        // rebuild invalidates the cached one.
        std::fs::write(
            tmp.path().join("atoms.json"),
            serde_json::to_vec_pretty(&AtomsFile::new(Vec::new())).expect("json"),
        )
        .expect("write atoms");
        assert!(
            population_marker_is_current(tmp.path()),
            "the table holds every kind the map asks for and the surplus kinds have no atoms"
        );
        assert!(
            !tmp.path().join("_summary.json").exists(),
            "a freshness probe writes nothing into the atlas dir"
        );

        // The other direction: a map that asks for a kind the table was never
        // seeded on is stale. `Question` is that kind — it is in no
        // pre-registered row, deliberately, and the reachability ratchet
        // `every_kind_a_pipeline_emits_is_reachable_by_some_row` names it in
        // `UNREACHABLE_BY_DESIGN`. (This read `Event` until 2026-09-09, when
        // `Event` and `Relation` joined the `lookup` row and stopped being
        // absent from the union.)
        let wider = narrower.replace(r#""kinds":["Claim"]"#, r#""kinds":["Claim","Question"]"#);
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(tmp.path().join(AtlasOntologyFile::FILE), wider).expect("write ontology");
        assert!(
            !population_marker_is_current(tmp.path()),
            "a map that adds a kind the table lacks is stale"
        );
    }
}
