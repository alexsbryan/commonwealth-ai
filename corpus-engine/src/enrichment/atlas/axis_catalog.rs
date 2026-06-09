// SPDX-License-Identifier: AGPL-3.0-or-later
//! Typed-axis catalog — registry of axes the resolver projects into
//! atoms.json and the bench scores against.
//!
//! One entry per (discourse_mode, axis) pair. Adding a new axis costs
//! one `TypedAxis` const entry here plus a corresponding arm in
//! `resolve_type_extensions` (the projection writer) and an optional
//! `[axes.<key>]` block in a golden TOML. No per-axis bench code.
//!
//! The catalog is descriptive of the resolver — when
//! `resolve_type_extensions` learns a new typed-extension shape, the
//! matching catalog entry must land in the same change. Drift between
//! projection and catalog is silent: the bench simply won't have an
//! axis to score, and atoms.json will carry data the bench can't see.
//! The invariant lives in `~/.claude/.../memory/` as a `[note]
//! invariant`.
//!
//! v1 ships the five argumentative axes that Gap B's resolver
//! already projects (mechanism / named_position / evidence /
//! opposition / concession). Narrative / descriptive / procedural /
//! lyric / reflective axes land when their `resolve_type_extensions`
//! arms land.

use crate::enrichment::pipeline::types::DiscourseMode;

/// One row in the catalog. All fields are compile-time constants;
/// the catalog is a flat `&'static [TypedAxis]`, not a runtime
/// registry, so adding an axis is a code change rather than a
/// runtime registration. This is deliberate — see the plan's
/// "Restraint patterns" section for why a `dyn TypedAxis` trait was
/// rejected for v1.
#[derive(Debug, Clone, Copy)]
pub struct TypedAxis {
    /// Stable snake_case identifier. Used as:
    ///   - the `BTreeMap` key in `GoldenSet.axes`,
    ///   - the `[axes.<key>]` table name in a canonical golden TOML,
    ///   - the row label in the bench scoreboard.
    /// Once published, never rename — golden TOMLs reference these.
    pub key: &'static str,

    /// Which MECE Discourse Mode this axis is associated with.
    /// The bench groups scoreboard rows by mode when rendering.
    pub discourse_mode: DiscourseMode,

    /// Which atom shape the resolver projects this axis onto.
    /// Drives `collect_axis_atoms` (the bench-side accessor) and
    /// is the contract the resolver's projection arm must honour.
    pub atom_kind: AtomKind,

    /// Fields whose mismatch makes a candidate atom NOT a match.
    /// `[GatingField::Name]` is the minimum; some axes layer
    /// additional gates (e.g. Position gates on `Stance` too).
    pub gating_fields: &'static [GatingField],

    /// Field names that the golden's expectation entries can carry
    /// for informational notes. Listed here so `bench scaffold`
    /// (Move 3) and TOML linters know which `*_contains_any` keys
    /// are valid on this axis. Mismatch produces a `PhaseScore.note`,
    /// not a missed expectation.
    pub informational_fields: &'static [&'static str],

    /// One-line human-readable description. Surfaced by
    /// `sovereign bench axes --list` and `bench scaffold` template
    /// comments. Keep under 80 chars.
    pub description: &'static str,
}

/// Which atom shape the axis projects onto.
///
/// Most variants are `AtomEnvelope` variants directly. The two
/// "WithKind" variants pin a qualifier tag so multiple axes can
/// share the Entity or Claim envelope without colliding (e.g.
/// `mechanism` lives on `Entity` with `concept_kind="mechanism"`;
/// `evidence` and `concession` both live on `Claim` distinguished
/// by `claim_kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtomKind {
    Entity,
    /// `Entity` filtered by `concept_kind == <tag>` (snake_case).
    EntityWithConceptKind(&'static str),
    Event,
    State,
    Relation,
    Claim,
    /// `Claim` filtered by `claim_kind == <tag>` (snake_case).
    ClaimWithKind(&'static str),
    Question,
    Configuration,
    Position,
    Opposition,
    ArgumentReconstruction,
}

/// What gates a match.
///
/// Each variant corresponds to one of the legacy `score_*_atoms`
/// functions' match policies. The driver in `scorers::score_axis`
/// matches on this enum to decide whether a candidate atom counts as
/// hitting an expectation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatingField {
    /// Substring match against `canonical_name` (Entity / Position)
    /// or against the leading portion of `content` (Claim variants
    /// where there is no explicit name field). Always present —
    /// every axis has a name-shaped primary signal.
    Name,
    /// Position / Opposition only. Case-insensitive exact match
    /// against the stance string ("endorse" / "rebut" / ...).
    /// Gating because a "rebut" stance on a position is materially
    /// a different atom from an "endorse" stance.
    Stance,
    /// Qualifier match for `EntityWithConceptKind` /
    /// `ClaimWithKind`. The qualifier tag is already in the
    /// collector's filter, so `GatingField::Kind` is informational
    /// here — preserved as a variant so the policy table is uniform
    /// and the catalog explicitly declares "kind matters" for these
    /// axes.
    Kind,
    /// Opposition only. Order-independent left/right label match —
    /// `(left=X, right=Y)` and `(left=Y, right=X)` are the same atom.
    Opposition,
}

/// The catalog itself. Order is the bench scoreboard render order
/// within each discourse-mode group.
pub const AXIS_CATALOG: &[TypedAxis] = &[
    TypedAxis {
        key: "mechanism",
        discourse_mode: DiscourseMode::Argumentative,
        atom_kind: AtomKind::EntityWithConceptKind("mechanism"),
        gating_fields: &[GatingField::Name],
        informational_fields: &["description_keywords_any", "domain_contains_any"],
        description: "Named causal/explanatory mechanism the section's argument turns on.",
    },
    TypedAxis {
        key: "named_position",
        discourse_mode: DiscourseMode::Argumentative,
        atom_kind: AtomKind::Position,
        gating_fields: &[GatingField::Name, GatingField::Stance],
        informational_fields: &["content_contains_any", "proponent_contains_any"],
        description: "Named position with author stance (endorse / rebut / survey / mixed).",
    },
    TypedAxis {
        key: "evidence",
        discourse_mode: DiscourseMode::Argumentative,
        atom_kind: AtomKind::ClaimWithKind("evidence"),
        gating_fields: &[GatingField::Name, GatingField::Kind],
        informational_fields: &["content_contains_any", "supports_contains_any"],
        description:
            "Evidence invocation (study / figure / example); `supports` edge-walk deferred.",
    },
    TypedAxis {
        key: "opposition",
        discourse_mode: DiscourseMode::Argumentative,
        atom_kind: AtomKind::Opposition,
        gating_fields: &[GatingField::Opposition],
        informational_fields: &["axis_contains_any"],
        description: "Symmetric opposition between two readings; left/right are order-independent.",
    },
    TypedAxis {
        key: "concession",
        discourse_mode: DiscourseMode::Argumentative,
        atom_kind: AtomKind::ClaimWithKind("concession"),
        gating_fields: &[GatingField::Name],
        informational_fields: &["addresses_contains_any", "outcome"],
        description: "Concessive move; `outcome` and `addresses` edge-walk are informational.",
    },
];

/// Look up a catalog entry by its stable key.
pub fn axis_by_key(key: &str) -> Option<&'static TypedAxis> {
    AXIS_CATALOG.iter().find(|a| a.key == key)
}

/// Iterate catalog entries belonging to a discourse mode, preserving
/// catalog order.
pub fn axes_for_mode(mode: DiscourseMode) -> impl Iterator<Item = &'static TypedAxis> {
    AXIS_CATALOG
        .iter()
        .filter(move |a| a.discourse_mode == mode)
}

/// Iterate every catalog entry, preserving catalog order.
pub fn all_axes() -> impl Iterator<Item = &'static TypedAxis> {
    AXIS_CATALOG.iter()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argumentative_v1_entries_present() {
        let keys: Vec<&str> = axes_for_mode(DiscourseMode::Argumentative)
            .map(|a| a.key)
            .collect();
        assert_eq!(
            keys,
            vec![
                "mechanism",
                "named_position",
                "evidence",
                "opposition",
                "concession"
            ],
            "v1 catalog must ship exactly these five argumentative axes in this order"
        );
    }

    #[test]
    fn axis_by_key_roundtrip() {
        for axis in AXIS_CATALOG {
            let looked_up = axis_by_key(axis.key).unwrap_or_else(|| {
                panic!("axis {:?} must be retrievable by its own key", axis.key)
            });
            assert_eq!(looked_up.key, axis.key);
        }
    }

    #[test]
    fn axis_by_key_unknown_returns_none() {
        assert!(axis_by_key("not-a-real-axis").is_none());
    }

    #[test]
    fn every_mode_queryable_even_when_empty() {
        // Argumentative is populated; the other five modes have no
        // entries yet but must still return an iterator (zero items).
        // Pins the contract — if someone changes axes_for_mode to
        // panic when the filter is empty, this test catches it.
        for mode in DiscourseMode::ALL {
            let _ = axes_for_mode(*mode).count();
        }
    }

    #[test]
    fn keys_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for axis in AXIS_CATALOG {
            assert!(
                seen.insert(axis.key),
                "duplicate axis key in catalog: {:?}",
                axis.key
            );
        }
    }

    #[test]
    fn gating_fields_nonempty() {
        // Every axis must declare at least one gate. A zero-gate
        // axis would match anything — almost certainly a mistake.
        for axis in AXIS_CATALOG {
            assert!(
                !axis.gating_fields.is_empty(),
                "axis {:?} declares no gating fields",
                axis.key
            );
        }
    }
}
