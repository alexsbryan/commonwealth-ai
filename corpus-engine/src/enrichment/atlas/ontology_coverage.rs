// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ninth schema-validation dimension: did the DECLARED ontology reach the
//! atoms?
//!
//! The question the whole ontology-v1 program exists to answer, computed in
//! the one artefact an operator already runs after a build
//! (`svrn enrich schema-report`). It sits beside [`super::schema_validation`]
//! rather than inside it for two reasons: the other eight dimensions are about
//! the fixed atom model and this one is about the author's own nouns, and that
//! file is already past ARCH §3.1's ceiling.
//!
//! Present only when the atlas carries an `ontology.json`. Absent — not
//! zeroed — otherwise: "this corpus declares nothing" and "this corpus
//! declared types and got none" are different findings (§18.3).

use serde::{Deserialize, Serialize};

use super::atoms::AtomEnvelope;
use super::projection::subtype_of;
use super::writer::AtlasOntologyFile;
use crate::enrichment::ontology::TypeIndex;

/// Did the declared ontology reach the atoms?
///
/// The question the whole program exists to answer, in the one artefact an
/// operator already runs after a build. A declared type with zero atoms is
/// the headline failure — it is what the as-built probe measured (0 of 1
/// surviving) — so it gets its own gap signature and the comparator can see
/// it recur across corpora.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OntologyCoverage {
    /// The declaration language version the atlas was built under.
    pub ontology_version: u32,
    /// One row per declared type, in declaration order.
    pub by_type: Vec<DeclaredTypeCount>,
    /// One row per declared type, naming what makes two of them one thing.
    pub identity: Vec<IdentityCriterion>,
    /// Clusters the reconciler collapsed, from `reconciliation.json`. `None`
    /// when `svrn enrich reconcile` has not been run on this corpus — which is
    /// not the same as zero merges.
    pub merges: Option<usize>,
    /// `same_as` Claims in the atlas — the reified merges. Counted from the
    /// atoms, so this is what a reader would actually find.
    pub same_as_claims: usize,
    /// Claims of a type that declares a `subject` whose subject did not
    /// resolve. The is-about link is what a declared claim type is FOR, so a
    /// high count here means the type is present in name only.
    pub claims_missing_subject: usize,
}

/// Atoms carrying one declared type.
///
/// Counted by SUBTYPE across every atom kind, not within `kind` — because a
/// declared type does not always produce the kind it declares. A `role_of`
/// type is declared `kind = "entity"` and produces `State` atoms on the rigid
/// entity (`ruler role_of person`: the atoms are people, the roles are
/// states), so counting `ruler` inside the Entity bucket would report zero for
/// a role that landed perfectly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeclaredTypeCount {
    /// The atom kind the type specializes (`entity`, `claim`, …).
    pub kind: String,
    /// The author's noun.
    pub name: String,
    /// Atoms whose subtype IS this name.
    pub count: usize,
    /// Atoms whose subtype is this name or any `specializes` descendant —
    /// what "how many coins are in the catalogue" means when `sceatta`
    /// specializes `coin`.
    pub count_with_subtypes: usize,
}

/// What makes two mentions of a declared type one thing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityCriterion {
    pub type_name: String,
    /// `external:<keys>`, `fallback:<keys>`, or `default:canonical_name` —
    /// the last being what a type that declares neither resolves on, stated
    /// rather than left blank.
    pub criterion: String,
}

impl OntologyCoverage {
    pub(super) fn gap_signatures(&self) -> Vec<String> {
        self.by_type
            .iter()
            .filter(|t| t.count_with_subtypes == 0)
            .map(|t| format!("coverage:zero:{}", t.name))
            .collect()
    }
}

/// The ninth dimension. Pure over the atoms plus the declaration, so the same
/// atlas always reports the same coverage.
pub fn build_ontology_coverage(
    ontology: &AtlasOntologyFile,
    atoms: &[AtomEnvelope],
    merges: Option<usize>,
) -> OntologyCoverage {
    let index = TypeIndex::from_policies(&ontology.policies);
    let subtypes: Vec<String> = atoms.iter().map(subtype_of).collect();

    let by_type = ontology
        .policies
        .shape
        .types
        .iter()
        .map(|t| DeclaredTypeCount {
            kind: serde_json::to_string(&t.kind)
                .unwrap_or_default()
                .trim_matches('"')
                .to_string(),
            name: t.name.clone(),
            count: subtypes.iter().filter(|s| *s == &t.name).count(),
            count_with_subtypes: subtypes
                .iter()
                .filter(|s| *s == &t.name || index.is_a(s, &t.name))
                .count(),
        })
        .collect();

    // Every declared type gets a row, including the ones that declare nothing:
    // "resolves on its canonical name" is the answer, and leaving the row out
    // would read as "no criterion" rather than "the default one".
    let identity = ontology
        .policies
        .shape
        .types
        .iter()
        .map(|t| {
            let external = index.effective_identity(&t.name);
            let fallback = index.effective_identity_fallback(&t.name);
            let criterion = if !external.is_empty() {
                format!("external:{}", external.join(", "))
            } else if !fallback.is_empty() {
                format!("fallback:{}", fallback.join(", "))
            } else {
                "default:canonical_name".to_string()
            };
            IdentityCriterion {
                type_name: t.name.clone(),
                criterion,
            }
        })
        .collect();

    // A claim type that declares a `subject` and produced claims without one
    // is present in name only — the is-about link is the point of declaring
    // it.
    let wants_subject: std::collections::BTreeSet<&str> = ontology
        .policies
        .claim_types()
        .filter(|t| t.subject.is_some())
        .map(|t| t.name.as_str())
        .collect();
    let claims_missing_subject = atoms
        .iter()
        .filter(|a| match a {
            AtomEnvelope::Claim(c) => {
                c.subject.is_none()
                    && c.claim_kind
                        .as_deref()
                        .is_some_and(|k| wants_subject.contains(k))
            }
            _ => false,
        })
        .count();

    let same_as_claims = atoms
        .iter()
        .filter(|a| match a {
            AtomEnvelope::Claim(c) => c.claim_kind.as_deref() == Some("same_as"),
            _ => false,
        })
        .count();

    OntologyCoverage {
        ontology_version: ontology.ontology_version,
        by_type,
        identity,
        merges,
        same_as_claims,
        claims_missing_subject,
    }
}
