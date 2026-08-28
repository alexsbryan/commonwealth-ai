// SPDX-License-Identifier: AGPL-3.0-or-later
//! The atlas snapshot the eval scores AGAINST — every artefact the pipeline
//! wrote for one corpus, loaded once.
//!
//! A phase whose artefact is absent is reported absent, never scored zero
//! (ARCH §18.3): a partial pipeline run must not read as a regression.

// The eval surface is ONE cooperating unit split for size, not a set of
// independent modules: the golden schema, the snapshot, the match primitives
// and the scorers all name each other's types. `use super::*` keeps that one
// import surface in `mod.rs` rather than duplicating it eight ways.
use super::*;

// ── Atlas snapshot ─────────────────────────────────────────────────

#[derive(Debug)]
pub(crate) struct AtlasSnapshot {
    pub(super) skeleton: Option<FieldSkeleton>,
    pub(super) atoms: Option<AtomsFile>,
    pub(super) edges: Option<EdgesFile>,
    pub(super) gaps: Option<GapsOutput>,
    pub(super) configurations: Option<ConfigurationsOutput>,
}

impl AtlasSnapshot {
    pub(crate) fn load(atlas_dir: &Path, skeleton_path: &Path) -> Result<Self, String> {
        let skeleton = if skeleton_path.exists() {
            let raw = std::fs::read_to_string(skeleton_path)
                .map_err(|e| format!("read {}: {e}", skeleton_path.display()))?;
            Some(
                serde_json::from_str::<FieldSkeleton>(&raw)
                    .map_err(|e| format!("parse {}: {e}", skeleton_path.display()))?,
            )
        } else {
            None
        };

        let atoms_path = atlas_dir.join("atoms.json");
        let atoms = if atoms_path.exists() {
            Some(read_json(&atoms_path)?)
        } else {
            None
        };
        let edges_path = atlas_dir.join("edges.json");
        let edges = if edges_path.exists() {
            Some(read_json(&edges_path)?)
        } else {
            None
        };
        let gaps_path = atlas_dir.join("gaps.json");
        let gaps = if gaps_path.exists() {
            Some(read_json(&gaps_path)?)
        } else {
            None
        };
        let cfg_path = atlas_dir.join("configurations.json");
        let configurations = if cfg_path.exists() {
            Some(read_json(&cfg_path)?)
        } else {
            None
        };

        Ok(Self {
            skeleton,
            atoms,
            edges,
            gaps,
            configurations,
        })
    }

    // ─── Typed-extension accessors (Phase 1 cache) ────────────────
    //
    // Each accessor walks `questions_by_chapter[*].section_extraction`
    // and visits every active `TypeExtension` on that section — both
    // the v2 plural slot (`type_extensions: Vec<TypeExtension>`) and
    // the v1 legacy singular (`type_extension: Option<TypeExtension>`).
    // Returns the sketches paired with the originating section_id so
    // scorers can attribute hits and misses to specific sections in
    // the report.

    // Typed-axis candidate collection moved to
    // `collect_axis_atoms` in the catalog-driven scoring block —
    // search for "Catalog-driven axis scoring" below. Adding a new
    // typed axis no longer means adding a snapshot accessor.

    pub(super) fn entities_of_type(&self, kind: EntityType) -> Vec<&Entity> {
        let Some(file) = &self.atoms else {
            return Vec::new();
        };
        file.atoms
            .iter()
            .filter_map(|a| match a {
                AtomEnvelope::Entity(e) if e.entity_type == kind => Some(e),
                _ => None,
            })
            .collect()
    }

    /// All Entity atoms regardless of type. Used by forbidden-atom
    /// checks so that a `forbidden_person_atoms` rule for "narrator"
    /// fires even when the model evaded the type tag by emitting it as
    /// `entity_type: unspecified`. The semantic is "this concept must
    /// not be lifted to an entity at all" — not "this concept must
    /// not be a Person specifically".
    pub(super) fn all_entities(&self) -> Vec<&Entity> {
        let Some(file) = &self.atoms else {
            return Vec::new();
        };
        file.atoms
            .iter()
            .filter_map(|a| match a {
                AtomEnvelope::Entity(e) => Some(e),
                _ => None,
            })
            .collect()
    }

    pub(super) fn questions(&self) -> Vec<&Question> {
        let Some(file) = &self.atoms else {
            return Vec::new();
        };
        file.atoms
            .iter()
            .filter_map(|a| match a {
                AtomEnvelope::Question(q) => Some(q),
                _ => None,
            })
            .collect()
    }

    pub(super) fn events(&self) -> Vec<&Event> {
        let Some(file) = &self.atoms else {
            return Vec::new();
        };
        file.atoms
            .iter()
            .filter_map(|a| match a {
                AtomEnvelope::Event(e) => Some(e),
                _ => None,
            })
            .collect()
    }

    pub(super) fn states(&self) -> Vec<&State> {
        let Some(file) = &self.atoms else {
            return Vec::new();
        };
        file.atoms
            .iter()
            .filter_map(|a| match a {
                AtomEnvelope::State(s) => Some(s),
                _ => None,
            })
            .collect()
    }

    pub(super) fn relations(&self) -> Vec<&Relation> {
        let Some(file) = &self.atoms else {
            return Vec::new();
        };
        file.atoms
            .iter()
            .filter_map(|a| match a {
                AtomEnvelope::Relation(r) => Some(r),
                _ => None,
            })
            .collect()
    }

    pub(super) fn claims(&self) -> Vec<&corpus_engine::enrichment::atlas::atoms::Claim> {
        let Some(file) = &self.atoms else {
            return Vec::new();
        };
        file.atoms
            .iter()
            .filter_map(|a| match a {
                AtomEnvelope::Claim(c) => Some(c),
                _ => None,
            })
            .collect()
    }

    pub(super) fn configurations_inline(&self) -> Vec<&Configuration> {
        let Some(file) = &self.atoms else {
            return Vec::new();
        };
        file.atoms
            .iter()
            .filter_map(|a| match a {
                AtomEnvelope::Configuration(c) => Some(c),
                _ => None,
            })
            .collect()
    }

    pub(super) fn entity_name_by_id(&self, id: &AtomId) -> Option<&str> {
        let file = self.atoms.as_ref()?;
        file.atoms.iter().find_map(|a| match a {
            AtomEnvelope::Entity(e) if e.id == *id => Some(e.canonical_name.as_str()),
            _ => None,
        })
    }

    /// Every name the entity is known by (canonical + aliases). Used by
    /// participant-keyword matchers so a golden listing "Alyosha" still
    /// credits an event whose participant resolves to entity
    /// `Alexey Fyodorovich Karamazov` with `aliases: ["Alyosha"]`. The
    /// canonical-only version is kept for display contexts (miss
    /// labels, fault-line endpoint resolution) where one name is wanted.
    pub(super) fn entity_match_strings_by_id(&self, id: &AtomId) -> Vec<&str> {
        let Some(file) = self.atoms.as_ref() else {
            return Vec::new();
        };
        file.atoms
            .iter()
            .find_map(|a| match a {
                AtomEnvelope::Entity(e) if e.id == *id => {
                    let mut names: Vec<&str> = Vec::with_capacity(1 + e.aliases.len());
                    names.push(e.canonical_name.as_str());
                    names.extend(e.aliases.iter().map(String::as_str));
                    Some(names)
                }
                _ => None,
            })
            .unwrap_or_default()
    }
}

pub(super) fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_str::<T>(&raw).map_err(|e| format!("parse {}: {e}", path.display()))
}
