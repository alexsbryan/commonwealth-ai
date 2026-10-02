// SPDX-License-Identifier: AGPL-3.0-or-later
//! The schema-validation report's types — the §12 report `atlas/schema_validation.json`
//! holds, with the declared-ontology coverage dimension — and its one typed
//! door. A build writes the report (corpus-engine's `schema_validation` and
//! `ontology_coverage` builders); the atlas view reads it here without the
//! engine (pb-ingest-dial-tools).

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Did the declared ontology reach the atoms?
///
/// The question the whole program exists to answer, in the one artefact an
/// operator already runs after a build. A declared type with zero atoms is
/// the headline failure — it is what the as-built probe measured (0 of 1
/// surviving) — so it gets its own gap signature and the comparator can see
/// it recur across corpora.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
    /// One row per (declared type, declared attribute) — how much of the
    /// author's attribute surface the build actually filled. Empty when no
    /// declared type declares an attribute.
    pub attribute_fill: Vec<AttributeFill>,
}

/// How much of one declared attribute the build actually filled.
///
/// A declared type can land perfectly BY NAME and carry nothing. That is what
/// the wessex-hoard probe measured on 2026-09-02: `coin` reached 14 atoms and
/// not one of them carried `metal`, `weight` or `catalogue_ref`, while the
/// type-count dimension above reported the type as fully covered. A count of
/// atoms is not a measurement of the declaration reaching them, so the fill
/// rate is its own dimension.
///
/// One row per attribute rather than an average per type: "the model never
/// fills `weight`" and "it fills every attribute half the time" are different
/// findings needing different fixes, and an average hides both (§18.3).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttributeFill {
    /// The declaring type — the author's noun.
    pub type_name: String,
    /// The declared attribute name. Inherited attributes appear under the
    /// type that inherits them, so `sceatta` carries `coin`'s rows too.
    pub attribute: String,
    /// Atoms whose subtype is this type or a `specializes` descendant — every
    /// atom the attribute could conceivably describe.
    pub atoms: usize,
    /// Of `atoms`, those whose atom KIND has an attributes slot at all. A
    /// `role_of` type lands as a State and States carry no attributes, so
    /// `atoms > 0 && with_slot == 0` means the declaration has nowhere to
    /// land — a declaration defect, not a model failure.
    pub with_slot: usize,
    /// Of `with_slot`, those carrying a value for this attribute.
    pub filled: usize,
}

/// Atoms carrying one declared type.
///
/// Counted by SUBTYPE across every atom kind, not within `kind` — because a
/// declared type does not always produce the kind it declares. A `role_of`
/// type is declared `kind = "entity"` and produces `State` atoms on the rigid
/// entity (`ruler role_of person`: the atoms are people, the roles are
/// states), so counting `ruler` inside the Entity bucket would report zero for
/// a role that landed perfectly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IdentityCriterion {
    pub type_name: String,
    /// `external:<keys>`, `fallback:<keys>`, or `default:canonical_name` —
    /// the last being what a type that declares neither resolves on, stated
    /// rather than left blank.
    pub criterion: String,
}

impl OntologyCoverage {
    pub fn gap_signatures(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .by_type
            .iter()
            .filter(|t| t.count_with_subtypes == 0)
            .map(|t| format!("coverage:zero:{}", t.name))
            .collect();
        // A declared attribute that reached no atom, and a declared attribute
        // that had nowhere to land, are separate signatures: the first is
        // answered by the extraction prompt, the second by editing the recipe.
        for f in &self.attribute_fill {
            if f.atoms > 0 && f.with_slot == 0 {
                let sig = format!("attribute:unlandable:{}", f.type_name);
                if !out.contains(&sig) {
                    out.push(sig);
                }
            } else if f.with_slot > 0 && f.filled == 0 {
                out.push(format!("attribute:zero:{}:{}", f.type_name, f.attribute));
            }
        }
        out
    }
}

// ── Report types ─────────────────────────────────────────────

/// Full §12 report for one corpus. Serialises to
/// `atlas/schema_validation.json`; `sovereign enrich schema-report`
/// prints a human-readable view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaValidationReport {
    pub schema_version: String,
    pub corpus_id: String,
    pub section_count: usize,
    pub extraction: ExtractionCoverage,
    pub depth: DepthDistribution,
    pub confidence: ConfidenceDistribution,
    pub utilisation: AtomTypeUtilisation,
    pub orphans: OrphanAnalysis,
    pub discourse: DiscourseDistribution,
    pub cross_corpus: CrossCorpusConnectivity,
    pub gaps: DeterministicGapCounts,
    /// Ninth dimension, present only when the corpus DECLARED an ontology:
    /// did the author's own types come out the other end, and under what
    /// identity criterion. Absent — not zeroed — for every version-0 corpus,
    /// because "this corpus declares nothing" and "this corpus declared types
    /// and got none" are different findings (§18.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ontology: Option<OntologyCoverage>,
}

impl SchemaValidationReport {
    /// History:
    /// - `2.0` — the eight dimensions.
    /// - `2.1` — added the optional `ontology` dimension (ontology v1, P3).
    ///   Additive and optional, so a 2.0 report still deserialises.
    pub const SCHEMA_VERSION: &'static str = "2.1";

    /// Collect every gap signature this report carries. Used by
    /// `compare_across_corpora` — any signature present in ≥ 2
    /// reports is a schema-revision candidate.
    pub fn gap_signatures(&self) -> Vec<String> {
        let mut out = Vec::new();
        out.extend(self.extraction.gap_signatures());
        out.extend(self.depth.gap_signatures());
        out.extend(self.confidence.gap_signatures());
        out.extend(self.utilisation.gap_signatures());
        out.extend(self.orphans.gap_signatures());
        out.extend(self.discourse.gap_signatures());
        out.extend(self.cross_corpus.gap_signatures());
        out.extend(self.gaps.gap_signatures());
        if let Some(o) = &self.ontology {
            out.extend(o.gap_signatures());
        }
        out
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionCoverage {
    pub total_atoms: usize,
    pub by_type: Vec<AtomTypeCount>,
    /// Atom types with 0 atoms across the whole corpus. Each
    /// contributes a gap signature `coverage:zero:<atom_type>`.
    pub zero_coverage_types: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtomTypeCount {
    pub atom_type: String,
    pub count: usize,
}

impl ExtractionCoverage {
    pub fn gap_signatures(&self) -> Vec<String> {
        self.zero_coverage_types
            .iter()
            .map(|t| format!("coverage:zero:{t}"))
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepthDistribution {
    pub extracted: usize,
    pub structural: usize,
    pub structural_classified: usize,
}

impl DepthDistribution {
    pub fn gap_signatures(&self) -> Vec<String> {
        // No gaps surface here today — all corpora are 100%
        // Extracted. When a structure-first pipeline ships and
        // mixes Structural in, a ≥ 80% Extracted ratio
        // alongside a structure-first ingest becomes a diagnostic.
        Vec::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfidenceDistribution {
    /// Buckets 0.0–0.1, 0.1–0.2, …, 0.9–1.0 (ten buckets).
    pub buckets: [usize; 10],
    pub total_with_confidence: usize,
    /// Fraction of atoms with confidence < 0.5.
    pub low_confidence_fraction: f32,
}

impl ConfidenceDistribution {
    pub fn gap_signatures(&self) -> Vec<String> {
        let mut out = Vec::new();
        // >= 20% low-confidence is a systematic extraction gap
        // worth schema review. The specific threshold is tuned
        // conservatively; we'd rather miss a one-off bad run than
        // flag a healthy corpus.
        if self.low_confidence_fraction >= 0.20 {
            out.push("confidence:low_fraction_over_20pct".to_string());
        }
        out
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtomTypeUtilisation {
    /// Fraction 0.0–1.0 per type, summing to 1.0 across types.
    pub fractions: Vec<AtomTypeFraction>,
    /// Types appearing at less than 3% of the total atom budget.
    /// Each contributes `utilisation:under:<atom_type>`.
    pub under_utilised_types: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtomTypeFraction {
    pub atom_type: String,
    pub fraction: f32,
}

impl AtomTypeUtilisation {
    pub fn gap_signatures(&self) -> Vec<String> {
        self.under_utilised_types
            .iter()
            .map(|t| format!("utilisation:under:{t}"))
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrphanAnalysis {
    pub orphan_atoms: usize,
    pub total_atoms: usize,
    pub orphan_fraction: f32,
    /// Per-type breakdown of orphans and totals. Lets the reader see
    /// whether a headline orphan fraction is dominated by atom types
    /// where orphan-ness is expected (Question — until addressed_by
    /// fills, many sit with no inbound edges) vs. atom types where
    /// it's a red flag (Entity — should be pulled in by Involves;
    /// Claim — should be pulled in by Grounds). The resolver's job is
    /// to minimise orphans on the red-flag types, not the expected
    /// ones, so a single aggregate number hid that distinction.
    pub by_type: Vec<OrphanByType>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrphanByType {
    pub atom_type: String,
    pub orphan_count: usize,
    pub total_count: usize,
    pub orphan_fraction: f32,
}

impl OrphanAnalysis {
    pub fn gap_signatures(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.orphan_fraction >= 0.30 {
            out.push("orphans:fraction_over_30pct".to_string());
        }
        // Per-type dominance signals: an atom type where ≥ 80% of
        // instances are orphaned points at a specific resolver gap.
        // Emit one signature per offending type so cross-corpus
        // comparison can tell a Claim-grounding regression from an
        // Entity-wiring regression.
        for b in &self.by_type {
            if b.total_count >= 10 && b.orphan_fraction >= 0.80 {
                out.push(format!("orphans:type_over_80pct:{}", b.atom_type));
            }
        }
        out
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscourseDistribution {
    pub buckets: Vec<DiscourseBucket>,
    pub total_claims: usize,
    /// Dominant discourse act as a fraction. >= 90% dominance
    /// flags `discourse:dominance:<act>` — the prompt isn't
    /// exercising the full vocabulary.
    pub top_act: Option<String>,
    pub top_fraction: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscourseBucket {
    pub act: String,
    pub count: usize,
}

impl DiscourseDistribution {
    pub fn gap_signatures(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.top_fraction >= 0.90 {
            if let Some(act) = &self.top_act {
                out.push(format!("discourse:dominance:{act}"));
            }
        }
        out
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossCorpusConnectivity {
    /// True when `atlas/cross_corpus_edges.json` was present and
    /// loaded. When false, the dimension is not evaluated
    /// (neither present nor absent is a gap — just "not applicable").
    pub available: bool,
    pub grounding_count: usize,
    pub local_atoms_with_outbound: usize,
    pub local_entity_atom_count: usize,
}

impl CrossCorpusConnectivity {
    pub fn gap_signatures(&self) -> Vec<String> {
        if !self.available {
            return Vec::new();
        }
        // If cross-corpus was run but < 5% of local entities got
        // any grounding edge, this is a systematic bridging gap.
        if self.local_entity_atom_count == 0 {
            return Vec::new();
        }
        let fraction = self.local_atoms_with_outbound as f32 / self.local_entity_atom_count as f32;
        if fraction < 0.05 {
            vec!["cross_corpus:bridge_coverage_under_5pct".to_string()]
        } else {
            Vec::new()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeterministicGapCounts {
    pub transition_without_trigger: usize,
    pub ungrounded_claim: usize,
    pub open_question: usize,
    /// Totals against which the fractions are computed.
    pub total_transitions: usize,
    pub total_claims: usize,
    pub total_questions: usize,
}

impl DeterministicGapCounts {
    pub fn gap_signatures(&self) -> Vec<String> {
        let mut out = Vec::new();
        // >= 50% ungrounded claims → systematic Phase 3b grounding
        // weakness. This is the bellwether gap for the "claims
        // without Event grounding" problem Landing 3 surfaced.
        if self.total_claims > 0
            && (self.ungrounded_claim as f32 / self.total_claims as f32) >= 0.50
        {
            out.push("gaps:ungrounded_claim_over_50pct".to_string());
        }
        // >= 80% transitions without triggers → Phase 3b isn't
        // linking Events to Transitions.
        if self.total_transitions > 0
            && (self.transition_without_trigger as f32 / self.total_transitions as f32) >= 0.80
        {
            out.push("gaps:transition_without_trigger_over_80pct".to_string());
        }
        out
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
pub fn read_schema_validation_report(atlas_dir: &Path) -> Option<SchemaValidationReport> {
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
