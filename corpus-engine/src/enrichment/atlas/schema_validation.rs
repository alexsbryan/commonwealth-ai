// SPDX-License-Identifier: AGPL-3.0-or-later
//! Atlas schema validation — Phase C Step 9.
//!
//! The protocol (spec §12): for every enriched corpus, compute a
//! validation report across eight dimensions. A gap present in
//! one corpus is a prompt-tuning problem; a gap present in ≥ 2
//! corpora warrants schema revision. This module computes the
//! report from a resolved atlas on disk; the CLI drivers
//! `schema-report` and `schema-review` surface it.
//!
//! ## Design
//!
//! Computed **on demand** from atoms.json + edges.json +
//! cross_corpus_edges.json (+ gaps.json + configurations.json
//! when present). Retrofitting incremental writes into every
//! phase is strictly better for freshness — the current phase
//! would see the live values — but lands as a follow-up once the
//! on-demand version has shaken out.
//!
//! ## The eight dimensions
//!
//! 1. **Extraction coverage** — atom counts per type across all
//!    sections, plus how many sections produced zero atoms of
//!    a given type (coverage gap).
//! 2. **Enrichment-depth distribution** — tally of
//!    `Extracted`/`Structural`/`StructuralClassified` atoms.
//!    Today's atlas is all Extracted; the category is live so a
//!    future structure-first pipeline's mix shows up.
//! 3. **Confidence distribution** — histogram in 0.1 buckets
//!    across all atom types carrying confidence. Flags
//!    low-confidence clusters as extraction quality signals.
//! 4. **Atom-type utilisation** — fraction of the atom set in
//!    each type. A corpus with 0% Relations but 40% Claims
//!    suggests the domain (or prompt) under-produces relational
//!    structure.
//! 5. **Orphan analysis** — atoms with no inbound edges,
//!    sections with no atom evidence. Signals the resolver
//!    dropped things or the extractor missed sections.
//! 6. **Discourse-act distribution** — for Claim atoms only,
//!    tally of `argue`/`assert`/`define`/… usage. A 95%-assert
//!    corpus is a sign the prompt isn't exercising the full
//!    discourse vocabulary.
//! 7. **Cross-corpus connectivity** — count of Grounding edges
//!    (and future Framing/Provenance), grouped by local atom.
//!    Zero outward-pointing cross-corpus edges on a corpus that
//!    should be groundable is a diagnostic.
//! 8. **Deterministic gap counts** — by kind
//!    (`transition_without_trigger`, `ungrounded_claim`,
//!    `open_question`). Passes through to the cross-corpus
//!    comparison: if every corpus has >50% ungrounded claims,
//!    Phase 3b's grounding path is broken systematically.
//!
//! Each dimension emits both a **value** (numbers/histograms)
//! and a **gap_signature** (a stable short string the
//! cross-corpus comparator groups on). When the same signature
//! shows up in ≥ 2 corpora, the comparator flags it as a
//! schema-revision candidate rather than a prompt-tuning
//! candidate.

use serde::{Deserialize, Serialize};

use super::atoms::{
    AtomEnvelope, AtomsFile, Claim, Configuration, Entity, Event, Question, ResolutionStatus, State,
};
use super::cross_corpus::CrossCorpusEdgesFile;
use super::edges::{Edge, EdgeType, EdgesFile};
use crate::enrichment::pipeline::atlas::{DiscourseAct, EnrichmentDepth};

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

// ── Ontology coverage (ontology v1) ──────────────────────────

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
    fn gap_signatures(&self) -> Vec<String> {
        self.by_type
            .iter()
            .filter(|t| t.count_with_subtypes == 0)
            .map(|t| format!("coverage:zero:{}", t.name))
            .collect()
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
    fn gap_signatures(&self) -> Vec<String> {
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
    fn gap_signatures(&self) -> Vec<String> {
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
    fn gap_signatures(&self) -> Vec<String> {
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
    fn gap_signatures(&self) -> Vec<String> {
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
    fn gap_signatures(&self) -> Vec<String> {
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
    fn gap_signatures(&self) -> Vec<String> {
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
    fn gap_signatures(&self) -> Vec<String> {
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
    fn gap_signatures(&self) -> Vec<String> {
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

// ── Builder ──────────────────────────────────────────────────

/// Inputs for [`build_report`]. The atlas files are borrowed so
/// the caller owns lifecycle.
pub struct SchemaValidationInput<'a> {
    pub corpus_id: &'a str,
    pub atoms: &'a AtomsFile,
    pub edges: &'a EdgesFile,
    pub cross_corpus: Option<&'a CrossCorpusEdgesFile>,
    pub open_questions: usize,
    pub ungrounded_claims: usize,
    pub transitions_without_trigger: usize,
    /// The atlas's own `ontology.json`, verbatim — what
    /// `read_atlas_ontology` returns. One field rather than a policies +
    /// version pair, because that file is already the atlas's record of both.
    /// `None` means the corpus declares nothing and the ninth dimension is
    /// absent from the report.
    pub ontology: Option<&'a super::writer::AtlasOntologyFile>,
    /// Clusters `svrn enrich reconcile` collapsed, from `reconciliation.json`.
    /// `None` when that file is absent — reconciliation not run is not zero
    /// merges, and reporting it as zero would be a substitution (§18.3).
    pub merges: Option<usize>,
}

/// Compute the §12 validation report for one corpus. Pure —
/// same inputs always produce the same report, so CI can diff
/// the JSON across runs to catch drift.
pub fn build_report(input: SchemaValidationInput<'_>) -> SchemaValidationReport {
    // Partition atoms by type.
    let (entities, events, states, relations, claims, questions, configurations) =
        partition_atoms(&input.atoms.atoms);

    let extraction = build_extraction_coverage(
        &entities,
        &events,
        &states,
        &relations,
        &claims,
        &questions,
        &configurations,
    );
    let depth = build_depth_distribution(
        &entities,
        &events,
        &states,
        &relations,
        &claims,
        &questions,
        &configurations,
    );
    let confidence = build_confidence_distribution(&entities, &events, &states, &claims);
    let utilisation = build_atom_type_utilisation(&extraction);
    let orphans = build_orphan_analysis(&input.atoms.atoms, &input.edges.edges);
    let discourse = build_discourse_distribution(&claims);
    let cross_corpus = build_cross_corpus_connectivity(input.cross_corpus, entities.len());
    let gaps = DeterministicGapCounts {
        transition_without_trigger: input.transitions_without_trigger,
        ungrounded_claim: input.ungrounded_claims,
        open_question: input.open_questions,
        total_transitions: input
            .edges
            .edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::Transition)
            .count(),
        total_claims: claims.len(),
        total_questions: questions.len(),
    };

    let ontology = input
        .ontology
        .map(|o| build_ontology_coverage(o, &input.atoms.atoms, input.merges));

    // Section count: union of section_ids across evidence. Cheap
    // proxy for "how long is the corpus".
    let mut sections: std::collections::HashSet<String> = std::collections::HashSet::new();
    for e in &events {
        sections.insert(e.section_position.section_id.clone());
    }
    for s in &states {
        sections.insert(s.section_range.start.clone());
    }
    for ent in &entities {
        sections.insert(ent.first_appearance.chunk_id.clone());
    }

    SchemaValidationReport {
        schema_version: SchemaValidationReport::SCHEMA_VERSION.to_string(),
        corpus_id: input.corpus_id.to_string(),
        section_count: sections.len(),
        extraction,
        depth,
        confidence,
        utilisation,
        orphans,
        discourse,
        cross_corpus,
        gaps,
        ontology,
    }
}

fn partition_atoms(
    atoms: &[AtomEnvelope],
) -> (
    Vec<Entity>,
    Vec<Event>,
    Vec<State>,
    Vec<super::atoms::Relation>,
    Vec<Claim>,
    Vec<Question>,
    Vec<Configuration>,
) {
    let mut entities = Vec::new();
    let mut events = Vec::new();
    let mut states = Vec::new();
    let mut relations = Vec::new();
    let mut claims = Vec::new();
    let mut questions = Vec::new();
    let mut configurations = Vec::new();
    for a in atoms {
        match a.clone() {
            AtomEnvelope::Entity(x) => entities.push(x),
            AtomEnvelope::Event(x) => events.push(x),
            AtomEnvelope::State(x) => states.push(x),
            AtomEnvelope::Relation(x) => relations.push(x),
            AtomEnvelope::Claim(x) => claims.push(x),
            AtomEnvelope::Question(x) => questions.push(x),
            AtomEnvelope::Configuration(x) => configurations.push(x),
            AtomEnvelope::ArgumentReconstruction(_) => {
                // Schema validation §3 doesn't yet score argument
                // reconstruction atoms — they're additive and don't
                // contribute to the existing AtomTypeCount/utilisation
                // histogram. Skipped here so the partition stays
                // complete without churning every downstream
                // function. Add coverage when the validator gets a
                // dedicated bucket for arguments.
            }
            AtomEnvelope::Position(_) | AtomEnvelope::Opposition(_) => {
                // Gap-B typed-extension atoms — additive like
                // ArgumentReconstruction; the validator's coverage
                // histogram lands in a follow-up.
            }
            AtomEnvelope::Asset(_) => {
                // Described-asset atoms (AD-2). Document-level, not
                // chunk-level; the validator's chunk-anchored coverage
                // histogram does not score them. Same treatment as
                // the other additive atom kinds above.
            }
        }
    }
    (
        entities,
        events,
        states,
        relations,
        claims,
        questions,
        configurations,
    )
}

/// The ninth dimension. Pure over the atoms plus the declaration, so the same
/// atlas always reports the same coverage.
fn build_ontology_coverage(
    ontology: &super::writer::AtlasOntologyFile,
    atoms: &[AtomEnvelope],
    merges: Option<usize>,
) -> OntologyCoverage {
    use super::projection::subtype_of;
    use crate::enrichment::ontology::TypeIndex;

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

fn build_extraction_coverage(
    entities: &[Entity],
    events: &[Event],
    states: &[State],
    relations: &[super::atoms::Relation],
    claims: &[Claim],
    questions: &[Question],
    configurations: &[Configuration],
) -> ExtractionCoverage {
    let buckets = [
        ("Entity", entities.len()),
        ("Event", events.len()),
        ("State", states.len()),
        ("Relation", relations.len()),
        ("Claim", claims.len()),
        ("Question", questions.len()),
        ("Configuration", configurations.len()),
    ];
    let total_atoms: usize = buckets.iter().map(|(_, n)| *n).sum();
    let by_type = buckets
        .iter()
        .map(|(t, n)| AtomTypeCount {
            atom_type: t.to_string(),
            count: *n,
        })
        .collect();
    let zero_coverage_types = buckets
        .iter()
        .filter(|(_, n)| *n == 0)
        .map(|(t, _)| t.to_string())
        .collect();
    ExtractionCoverage {
        total_atoms,
        by_type,
        zero_coverage_types,
    }
}

fn build_depth_distribution(
    entities: &[Entity],
    events: &[Event],
    states: &[State],
    relations: &[super::atoms::Relation],
    claims: &[Claim],
    questions: &[Question],
    configurations: &[Configuration],
) -> DepthDistribution {
    let mut extracted = 0;
    let mut structural = 0;
    let mut structural_classified = 0;
    let bump = |d: &EnrichmentDepth,
                extracted: &mut usize,
                structural: &mut usize,
                structural_classified: &mut usize| match d {
        EnrichmentDepth::Extracted => *extracted += 1,
        EnrichmentDepth::Structural => *structural += 1,
        EnrichmentDepth::StructuralClassified => *structural_classified += 1,
    };
    for e in entities {
        bump(
            &e.enrichment_depth,
            &mut extracted,
            &mut structural,
            &mut structural_classified,
        );
    }
    for e in events {
        bump(
            &e.enrichment_depth,
            &mut extracted,
            &mut structural,
            &mut structural_classified,
        );
    }
    for s in states {
        bump(
            &s.enrichment_depth,
            &mut extracted,
            &mut structural,
            &mut structural_classified,
        );
    }
    for r in relations {
        bump(
            &r.enrichment_depth,
            &mut extracted,
            &mut structural,
            &mut structural_classified,
        );
    }
    for c in claims {
        bump(
            &c.enrichment_depth,
            &mut extracted,
            &mut structural,
            &mut structural_classified,
        );
    }
    for q in questions {
        bump(
            &q.enrichment_depth,
            &mut extracted,
            &mut structural,
            &mut structural_classified,
        );
    }
    for c in configurations {
        bump(
            &c.enrichment_depth,
            &mut extracted,
            &mut structural,
            &mut structural_classified,
        );
    }
    DepthDistribution {
        extracted,
        structural,
        structural_classified,
    }
}

fn build_confidence_distribution(
    entities: &[Entity],
    _events: &[Event],
    states: &[State],
    claims: &[Claim],
) -> ConfidenceDistribution {
    let mut buckets = [0usize; 10];
    let mut total = 0usize;
    let bucket_of = |conf: f32| {
        let c = conf.clamp(0.0, 0.9999);
        (c * 10.0) as usize
    };

    // Entities carry salience, not a separate confidence — we
    // treat salience as their confidence proxy here since it's
    // what the extractor emits per-atom.
    for e in entities {
        buckets[bucket_of(e.salience)] += 1;
        total += 1;
    }
    // States and Claims carry `Option<f32>` since the deterministic
    // Phase 3b resolver emits atoms without an LLM-reported score
    // (see `State.confidence` docs). We count only `Some` values so
    // the histogram reflects LLM calibration and isn't inflated by
    // derived atoms.
    for s in states {
        if let Some(c) = s.confidence {
            buckets[bucket_of(c)] += 1;
            total += 1;
        }
    }
    for c in claims {
        if let Some(v) = c.confidence {
            buckets[bucket_of(v)] += 1;
            total += 1;
        }
    }
    let low: usize = buckets[..5].iter().sum();
    let low_fraction = if total == 0 {
        0.0
    } else {
        low as f32 / total as f32
    };
    ConfidenceDistribution {
        buckets,
        total_with_confidence: total,
        low_confidence_fraction: low_fraction,
    }
}

fn build_atom_type_utilisation(cov: &ExtractionCoverage) -> AtomTypeUtilisation {
    let total = cov.total_atoms.max(1) as f32;
    let fractions: Vec<AtomTypeFraction> = cov
        .by_type
        .iter()
        .map(|b| AtomTypeFraction {
            atom_type: b.atom_type.clone(),
            fraction: b.count as f32 / total,
        })
        .collect();
    let under_utilised_types = fractions
        .iter()
        .filter(|f| f.fraction < 0.03)
        .map(|f| f.atom_type.clone())
        .collect();
    AtomTypeUtilisation {
        fractions,
        under_utilised_types,
    }
}

fn build_orphan_analysis(atoms: &[AtomEnvelope], edges: &[Edge]) -> OrphanAnalysis {
    // An atom is orphan if no edge references it as source OR
    // target. Configuration atoms are excluded (they reference
    // other atoms via `constituent_atoms`, not edges — orphaning
    // them is spec-correct).
    use std::collections::HashSet;
    let mut referenced: HashSet<&str> = HashSet::new();
    for e in edges {
        referenced.insert(e.source.as_str());
        referenced.insert(e.target.as_str());
    }
    // Per-type tallies in spec §2 atom order. We track a fixed
    // array (not a HashMap) so atom types appear in a stable order
    // in the output — operators reading the JSON or console table
    // shouldn't have to re-sort to find Claim vs. Entity.
    let mut by_type: [(&'static str, usize, usize); 6] = [
        ("Entity", 0, 0),
        ("Event", 0, 0),
        ("State", 0, 0),
        ("Relation", 0, 0),
        ("Claim", 0, 0),
        ("Question", 0, 0),
    ];
    let mut orphan_atoms = 0usize;
    let mut total_atoms = 0usize;
    for a in atoms {
        let (id, bucket_idx) = match a {
            AtomEnvelope::Entity(e) => (e.id.as_str(), Some(0)),
            AtomEnvelope::Event(e) => (e.id.as_str(), Some(1)),
            AtomEnvelope::State(s) => (s.id.as_str(), Some(2)),
            AtomEnvelope::Relation(r) => (r.id.as_str(), Some(3)),
            AtomEnvelope::Claim(c) => (c.id.as_str(), Some(4)),
            AtomEnvelope::Question(q) => (q.id.as_str(), Some(5)),
            AtomEnvelope::Configuration(_) => ("", None),
            AtomEnvelope::ArgumentReconstruction(_) => ("", None),
            AtomEnvelope::Position(_) | AtomEnvelope::Opposition(_) => ("", None),
            AtomEnvelope::Asset(_) => ("", None),
        };
        let Some(idx) = bucket_idx else {
            continue;
        };
        total_atoms += 1;
        by_type[idx].2 += 1;
        if !referenced.contains(id) {
            orphan_atoms += 1;
            by_type[idx].1 += 1;
        }
    }
    let orphan_fraction = if total_atoms == 0 {
        0.0
    } else {
        orphan_atoms as f32 / total_atoms as f32
    };
    let by_type_out: Vec<OrphanByType> = by_type
        .iter()
        .map(|(t, orph, tot)| OrphanByType {
            atom_type: (*t).to_string(),
            orphan_count: *orph,
            total_count: *tot,
            orphan_fraction: if *tot == 0 {
                0.0
            } else {
                *orph as f32 / *tot as f32
            },
        })
        .collect();
    OrphanAnalysis {
        orphan_atoms,
        total_atoms,
        orphan_fraction,
        by_type: by_type_out,
    }
}

fn build_discourse_distribution(claims: &[Claim]) -> DiscourseDistribution {
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for c in claims {
        let label = discourse_tag(&c.discourse_act);
        *counts.entry(label.to_string()).or_insert(0) += 1;
    }
    let total_claims = claims.len();
    let mut buckets: Vec<DiscourseBucket> = counts
        .into_iter()
        .map(|(act, count)| DiscourseBucket { act, count })
        .collect();
    buckets.sort_by(|a, b| b.count.cmp(&a.count));
    let (top_act, top_fraction) = match buckets.first() {
        Some(b) if total_claims > 0 => (Some(b.act.clone()), b.count as f32 / total_claims as f32),
        _ => (None, 0.0),
    };
    DiscourseDistribution {
        buckets,
        total_claims,
        top_act,
        top_fraction,
    }
}

fn discourse_tag(act: &DiscourseAct) -> &'static str {
    match act {
        DiscourseAct::Assert => "assert",
        DiscourseAct::Argue => "argue",
        DiscourseAct::Enact => "enact",
        DiscourseAct::Hypothesize => "hypothesize",
        DiscourseAct::Warn => "warn",
        DiscourseAct::Commit => "commit",
        DiscourseAct::Object => "object",
        DiscourseAct::Interpret => "interpret",
        DiscourseAct::Imply => "imply",
        DiscourseAct::Other(_) => "other",
    }
}

fn build_cross_corpus_connectivity(
    file: Option<&CrossCorpusEdgesFile>,
    local_entity_atom_count: usize,
) -> CrossCorpusConnectivity {
    let Some(file) = file else {
        return CrossCorpusConnectivity {
            available: false,
            grounding_count: 0,
            local_atoms_with_outbound: 0,
            local_entity_atom_count,
        };
    };
    let grounding_count = file
        .edges
        .iter()
        .filter(|e| e.edge.edge_type == EdgeType::Grounding)
        .count();
    let mut outbound_sources: std::collections::HashSet<String> = std::collections::HashSet::new();
    for e in &file.edges {
        outbound_sources.insert(e.edge.source.as_str().to_string());
    }
    CrossCorpusConnectivity {
        available: true,
        grounding_count,
        local_atoms_with_outbound: outbound_sources.len(),
        local_entity_atom_count,
    }
}

// ── Deterministic gap helpers ────────────────────────────────

/// Count transitions whose `trigger_event` is None.
pub fn count_transitions_without_trigger(edges: &[Edge]) -> usize {
    edges
        .iter()
        .filter(|e| e.edge_type == EdgeType::Transition && e.trigger_event.is_none())
        .count()
}

/// Count claims with no `Grounds` edge AND no own evidence.
pub fn count_ungrounded_claims(claims: &[Claim], edges: &[Edge]) -> usize {
    use std::collections::HashSet;
    let grounded: HashSet<&str> = edges
        .iter()
        .filter(|e| e.edge_type == EdgeType::Grounds)
        .map(|e| e.target.as_str())
        .collect();
    claims
        .iter()
        .filter(|c| c.evidence.is_empty() && !grounded.contains(c.id.as_str()))
        .count()
}

/// Count questions whose resolution_status is Open.
pub fn count_open_questions(questions: &[Question]) -> usize {
    questions
        .iter()
        .filter(|q| matches!(q.resolution_status, ResolutionStatus::Open))
        .count()
}

// ── Cross-corpus comparison ──────────────────────────────────

/// Result of comparing schema-validation reports across N
/// corpora. `convergent_gaps` — gap signatures present in ≥ 2
/// corpora — are schema-revision candidates per spec §12.5. All
/// other gaps are prompt-tuning candidates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaComparison {
    pub corpora: Vec<String>,
    pub convergent_gaps: Vec<ConvergentGap>,
    pub idiosyncratic_gaps: Vec<IdiosyncraticGap>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvergentGap {
    pub signature: String,
    pub present_in: Vec<String>,
    pub recommendation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdiosyncraticGap {
    pub signature: String,
    pub present_in: String,
    pub recommendation: String,
}

/// Compare N reports. A gap signature present in ≥ 2 corpora
/// lands in `convergent_gaps` with a "schema revision candidate"
/// recommendation; present in exactly 1 lands in
/// `idiosyncratic_gaps` with a "prompt tuning candidate" flag.
pub fn compare_across_corpora(reports: &[SchemaValidationReport]) -> SchemaComparison {
    use std::collections::HashMap;
    let mut signature_to_corpora: HashMap<String, Vec<String>> = HashMap::new();
    for r in reports {
        for sig in r.gap_signatures() {
            signature_to_corpora
                .entry(sig)
                .or_default()
                .push(r.corpus_id.clone());
        }
    }
    let mut convergent = Vec::new();
    let mut idiosyncratic = Vec::new();
    let mut sorted_sigs: Vec<_> = signature_to_corpora.into_iter().collect();
    sorted_sigs.sort_by(|a, b| a.0.cmp(&b.0));
    for (signature, corpora) in sorted_sigs {
        if corpora.len() >= 2 {
            convergent.push(ConvergentGap {
                signature: signature.clone(),
                present_in: corpora,
                recommendation: recommendation_for(&signature),
            });
        } else if corpora.len() == 1 {
            idiosyncratic.push(IdiosyncraticGap {
                signature: signature.clone(),
                present_in: corpora[0].clone(),
                recommendation: "prompt-tuning candidate (present in only one corpus)".into(),
            });
        }
    }
    SchemaComparison {
        corpora: reports.iter().map(|r| r.corpus_id.clone()).collect(),
        convergent_gaps: convergent,
        idiosyncratic_gaps: idiosyncratic,
    }
}

fn recommendation_for(signature: &str) -> String {
    let base = "schema revision candidate (present in ≥ 2 corpora)".to_string();
    let hint = match signature {
        s if s.starts_with("coverage:zero:") => {
            Some("review whether this atom type is a universal concept or a domain-specific one the schema shouldn't require")
        }
        s if s.starts_with("utilisation:under:") => {
            Some("prompt vocabulary may be steering away from this type; consider schema-level clarification of when to emit")
        }
        "confidence:low_fraction_over_20pct" => {
            Some("extractor is uncertain at scale — review whether the prompt asks for more certainty than the source can warrant")
        }
        "orphans:fraction_over_30pct" => {
            Some("many atoms have no edges — Phase 3b linking rules or schema expectations around connectivity may need loosening")
        }
        s if s.starts_with("orphans:type_over_80pct:") => {
            let atom_type = &s["orphans:type_over_80pct:".len()..];
            Some(match atom_type {
                "Claim" => "most claims have no Grounds edge — Phase 3b claim-grounding detector is under-wiring or the prompt rarely emits grounding evidence",
                "Entity" => "most entities have no Involves / Grounding edges — event extraction is missing participant wiring or entity resolution is dropping matches",
                "Question" => "most questions have no Raises / Addresses edges — Phase 3b hasn't linked questions to the claims that address them",
                "Event" | "State" | "Relation" => "this atom type is systematically unreferenced — resolver or schema needs review",
                _ => "orphan rate is systematically high for this atom type",
            })
        }
        s if s.starts_with("discourse:dominance:") => {
            Some("prompt isn't exercising the full discourse vocabulary; consider dropping rare acts from the schema or adding more exemplars")
        }
        "cross_corpus:bridge_coverage_under_5pct" => {
            Some("cross-corpus detectors miss most entities — the schema's canonical_name expectations may be too domain-specific")
        }
        "gaps:ungrounded_claim_over_50pct" => {
            Some("Phase 3b isn't linking claims to grounding events; consider making Grounds edges a first-class schema output rather than incidental")
        }
        "gaps:transition_without_trigger_over_80pct" => {
            Some("trigger_event is systematically missing from Transition edges; Phase 3b should treat it as required or the schema should drop the field")
        }
        _ => None,
    };
    match hint {
        Some(h) => format!("{base} — {h}"),
        None => base,
    }
}

#[cfg(test)]
mod tests {
    use super::super::atoms::{
        AtomId, ChunkRef, Entity, Event, SectionPosition, SectionRange, State,
    };
    use super::super::edges::{Edge, EdgeId, EdgeProvenance};
    use super::*;
    use crate::enrichment::pipeline::atlas::{
        ClaimScope, DiscourseAct, EnrichmentDepth, EntityType, EpistemicStatus, EventType,
        StateType,
    };

    fn entity(idx: usize, name: &str, salience: f32) -> Entity {
        Entity {
            id: AtomId::entity(idx),
            canonical_name: name.into(),
            aliases: vec![],
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: "x".into(),
            salience,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            defining_quote: None,
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        }
    }

    fn claim_with_act(idx: usize, act: DiscourseAct, confidence: f32) -> Claim {
        Claim {
            attributes: Default::default(),
            subject: None,
            id: AtomId::claim(idx),
            content: format!("claim {idx}"),
            discourse_act: act,
            epistemic_status: EpistemicStatus::Confident,
            scope: ClaimScope::Universal,
            evidence: vec![],
            attributed_to: None,
            confidence: Some(confidence),
            anchor: None,
            enrichment_depth: EnrichmentDepth::Extracted,
            quotable_excerpt: None,
            claim_kind: None,
            concession_outcome: None,
            evidence_kind: None,
        }
    }

    fn state(idx: usize, owner: usize, confidence: f32) -> State {
        State {
            id: AtomId::from_raw(format!("state-{idx:04}")),
            entity_id: AtomId::entity(owner),
            label: "x".into(),
            state_type: StateType::Other("x".into()),
            evidence: vec![],
            section_range: SectionRange::point("sec_0001"),
            confidence: Some(confidence),
            enrichment_depth: EnrichmentDepth::Extracted,
        }
    }

    fn event(idx: usize) -> Event {
        Event {
            attributes: Default::default(),
            id: AtomId::event(idx),
            description: format!("event {idx}"),
            event_type: EventType::Other("x".into()),
            participants: vec![],
            evidence: vec![],
            section_position: SectionPosition::section("sec_0001"),
            causal_antecedents: vec![],
            enrichment_depth: EnrichmentDepth::Extracted,
        }
    }

    fn transition_edge(src: usize, tgt: usize, trig: Option<usize>) -> Edge {
        Edge {
            id: EdgeId::from_raw(format!("edge-{src:04}-{tgt:04}")),
            edge_type: EdgeType::Transition,
            source: AtomId::from_raw(format!("state-{src:04}")),
            target: AtomId::from_raw(format!("state-{tgt:04}")),
            evidence: vec![],
            trigger_event: trig.map(AtomId::event),
            sub_question: None,
            confidence: 1.0,
            provenance: EdgeProvenance::Derived,
        }
    }

    fn grounds_edge(src: usize, claim: usize) -> Edge {
        Edge {
            id: EdgeId::from_raw(format!("grounds-{src:04}-{claim:04}")),
            edge_type: EdgeType::Grounds,
            source: AtomId::event(src),
            target: AtomId::claim(claim),
            evidence: vec![],
            trigger_event: None,
            sub_question: None,
            confidence: 1.0,
            provenance: EdgeProvenance::Derived,
        }
    }

    fn atoms_file(atoms: Vec<AtomEnvelope>) -> AtomsFile {
        AtomsFile::new(atoms)
    }

    fn edges_file(edges: Vec<Edge>) -> EdgesFile {
        EdgesFile::new(edges)
    }

    #[test]
    fn extraction_coverage_flags_zero_coverage_types() {
        // Only entity atoms; every other type reports as a zero-
        // coverage gap.
        let atoms = atoms_file(vec![AtomEnvelope::Entity(entity(1, "Kant", 1.0))]);
        let edges = edges_file(vec![]);
        let report = build_report(SchemaValidationInput {
            corpus_id: "test",
            atoms: &atoms,
            edges: &edges,
            cross_corpus: None,
            open_questions: 0,
            ungrounded_claims: 0,
            transitions_without_trigger: 0,
            ontology: None,
            merges: None,
        });
        assert_eq!(report.extraction.total_atoms, 1);
        let sig_list = report.extraction.gap_signatures();
        assert!(sig_list.contains(&"coverage:zero:Event".to_string()));
        assert!(sig_list.contains(&"coverage:zero:Claim".to_string()));
        assert!(!sig_list.contains(&"coverage:zero:Entity".to_string()));
    }

    #[test]
    fn confidence_distribution_flags_low_fraction_over_20pct() {
        // 3 atoms at 0.3, 1 at 0.9 → 75% below 0.5 → gap fires.
        let atoms = atoms_file(vec![
            AtomEnvelope::Entity(entity(1, "A", 0.3)),
            AtomEnvelope::Entity(entity(2, "B", 0.3)),
            AtomEnvelope::Entity(entity(3, "C", 0.3)),
            AtomEnvelope::Entity(entity(4, "D", 0.9)),
        ]);
        let report = build_report(SchemaValidationInput {
            corpus_id: "test",
            atoms: &atoms,
            edges: &edges_file(vec![]),
            cross_corpus: None,
            open_questions: 0,
            ungrounded_claims: 0,
            transitions_without_trigger: 0,
            ontology: None,
            merges: None,
        });
        assert!(report.confidence.low_confidence_fraction > 0.5);
        assert!(report
            .confidence
            .gap_signatures()
            .contains(&"confidence:low_fraction_over_20pct".to_string()));
    }

    #[test]
    fn discourse_distribution_flags_95pct_dominance() {
        // 19 asserts + 1 argue → 95% assert → dominance gap.
        let mut atoms = Vec::new();
        for i in 1..=19 {
            atoms.push(AtomEnvelope::Claim(claim_with_act(
                i,
                DiscourseAct::Assert,
                0.9,
            )));
        }
        atoms.push(AtomEnvelope::Claim(claim_with_act(
            20,
            DiscourseAct::Argue,
            0.9,
        )));
        let report = build_report(SchemaValidationInput {
            corpus_id: "test",
            atoms: &atoms_file(atoms),
            edges: &edges_file(vec![]),
            cross_corpus: None,
            open_questions: 0,
            ungrounded_claims: 0,
            transitions_without_trigger: 0,
            ontology: None,
            merges: None,
        });
        assert_eq!(report.discourse.top_act.as_deref(), Some("assert"));
        assert!(report.discourse.top_fraction >= 0.90);
        assert!(report
            .discourse
            .gap_signatures()
            .contains(&"discourse:dominance:assert".to_string()));
    }

    #[test]
    fn deterministic_gap_ungrounded_over_50pct_flags_signature() {
        let gaps = DeterministicGapCounts {
            transition_without_trigger: 0,
            ungrounded_claim: 30,
            open_question: 0,
            total_transitions: 0,
            total_claims: 40,
            total_questions: 0,
        };
        assert!(gaps
            .gap_signatures()
            .contains(&"gaps:ungrounded_claim_over_50pct".to_string()));
    }

    #[test]
    fn deterministic_gap_transition_over_80pct_flags_signature() {
        let gaps = DeterministicGapCounts {
            transition_without_trigger: 9,
            ungrounded_claim: 0,
            open_question: 0,
            total_transitions: 10,
            total_claims: 0,
            total_questions: 0,
        };
        assert!(gaps
            .gap_signatures()
            .contains(&"gaps:transition_without_trigger_over_80pct".to_string()));
    }

    #[test]
    fn count_transitions_without_trigger_reports_only_missing_triggers() {
        let edges = vec![
            transition_edge(1, 2, None),
            transition_edge(2, 3, Some(1)),
            transition_edge(3, 4, None),
        ];
        assert_eq!(count_transitions_without_trigger(&edges), 2);
    }

    #[test]
    fn count_ungrounded_claims_excludes_claims_with_grounds_edge() {
        let claims = vec![
            claim_with_act(1, DiscourseAct::Assert, 0.8),
            claim_with_act(2, DiscourseAct::Assert, 0.8),
        ];
        let edges = vec![grounds_edge(10, 1)];
        assert_eq!(count_ungrounded_claims(&claims, &edges), 1);
    }

    #[test]
    fn orphan_analysis_counts_unreferenced_atoms_excluding_configurations() {
        // entity-0001 is referenced by the grounds edge; event-0001
        // is unreferenced. Total = 2, orphans = 1.
        let atoms = vec![
            AtomEnvelope::Entity(entity(1, "A", 1.0)),
            AtomEnvelope::Event(event(1)),
        ];
        let edges = vec![Edge {
            id: EdgeId::from_raw("x"),
            edge_type: EdgeType::Involves,
            source: AtomId::event(42),
            target: AtomId::entity(1),
            evidence: vec![],
            trigger_event: None,
            sub_question: None,
            confidence: 1.0,
            provenance: EdgeProvenance::Derived,
        }];
        let analysis = build_orphan_analysis(&atoms, &edges);
        assert_eq!(analysis.total_atoms, 2);
        assert_eq!(analysis.orphan_atoms, 1);
    }

    #[test]
    fn orphan_analysis_breaks_down_by_atom_type() {
        // 2 entities (both referenced via Involves), 1 event (orphan),
        // 2 claims (1 grounded, 1 orphan). Per-type breakdown should
        // show Entity 0/2, Event 1/1, Claim 1/2, and fractions
        // computed accordingly — this is the signal that lets an
        // operator tell "Question orphans expected" from "Claim
        // orphans = grounding regression".
        let atoms = vec![
            AtomEnvelope::Entity(entity(1, "A", 1.0)),
            AtomEnvelope::Entity(entity(2, "B", 1.0)),
            AtomEnvelope::Event(event(1)),
            AtomEnvelope::Claim(claim_with_act(1, DiscourseAct::Assert, 0.9)),
            AtomEnvelope::Claim(claim_with_act(2, DiscourseAct::Assert, 0.9)),
        ];
        let edges = vec![
            Edge {
                id: EdgeId::from_raw("involves-1"),
                edge_type: EdgeType::Involves,
                source: AtomId::event(42),
                target: AtomId::entity(1),
                evidence: vec![],
                trigger_event: None,
                sub_question: None,
                confidence: 1.0,
                provenance: EdgeProvenance::Derived,
            },
            Edge {
                id: EdgeId::from_raw("involves-2"),
                edge_type: EdgeType::Involves,
                source: AtomId::event(43),
                target: AtomId::entity(2),
                evidence: vec![],
                trigger_event: None,
                sub_question: None,
                confidence: 1.0,
                provenance: EdgeProvenance::Derived,
            },
            grounds_edge(99, 1),
        ];
        let analysis = build_orphan_analysis(&atoms, &edges);
        let by = |t: &str| {
            analysis
                .by_type
                .iter()
                .find(|b| b.atom_type == t)
                .expect("type in breakdown")
        };
        assert_eq!(by("Entity").total_count, 2);
        assert_eq!(by("Entity").orphan_count, 0);
        assert_eq!(by("Event").total_count, 1);
        assert_eq!(by("Event").orphan_count, 1);
        assert_eq!(by("Claim").total_count, 2);
        assert_eq!(by("Claim").orphan_count, 1);
        // Types with zero atoms still appear in stable order but are
        // filtered by the CLI printer, not by this builder.
        assert_eq!(by("Question").total_count, 0);
        assert_eq!(analysis.by_type.len(), 6);
    }

    #[test]
    fn orphan_analysis_flags_per_type_signature_over_80pct() {
        // 10 claims, 9 orphan → 90% orphan rate on Claim, above the
        // 80% per-type threshold. Emits
        // `orphans:type_over_80pct:Claim`. The single grounded claim
        // via grounds_edge(1, 10) keeps the edge count nonzero.
        let mut atoms = Vec::new();
        for i in 1..=10 {
            atoms.push(AtomEnvelope::Claim(claim_with_act(
                i,
                DiscourseAct::Assert,
                0.9,
            )));
        }
        let edges = vec![grounds_edge(99, 10)];
        let report = build_report(SchemaValidationInput {
            corpus_id: "test",
            atoms: &atoms_file(atoms),
            edges: &edges_file(edges),
            cross_corpus: None,
            open_questions: 0,
            ungrounded_claims: 9,
            transitions_without_trigger: 0,
            ontology: None,
            merges: None,
        });
        assert!(
            report
                .orphans
                .gap_signatures()
                .contains(&"orphans:type_over_80pct:Claim".to_string()),
            "expected Claim per-type orphan signature, got: {:?}",
            report.orphans.gap_signatures()
        );
    }

    #[test]
    fn orphan_per_type_signature_ignores_low_volume_buckets() {
        // 2 events, both orphan → 100% but total_count < 10. We
        // should NOT fire the per-type signature on a tiny population
        // (one odd corpus shouldn't trigger a schema-review
        // recommendation on a type that barely appears).
        let atoms = vec![AtomEnvelope::Event(event(1)), AtomEnvelope::Event(event(2))];
        let report = build_report(SchemaValidationInput {
            corpus_id: "test",
            atoms: &atoms_file(atoms),
            edges: &edges_file(vec![]),
            cross_corpus: None,
            open_questions: 0,
            ungrounded_claims: 0,
            transitions_without_trigger: 0,
            ontology: None,
            merges: None,
        });
        assert!(
            !report
                .orphans
                .gap_signatures()
                .iter()
                .any(|s| s.starts_with("orphans:type_over_80pct:")),
            "should not fire on tiny populations, got: {:?}",
            report.orphans.gap_signatures()
        );
    }

    #[test]
    fn comparison_flags_convergent_signatures_across_two_corpora() {
        // Both corpora have the ungrounded-claim-over-50pct gap;
        // only the second has discourse:dominance:assert. The
        // first is convergent, the second is idiosyncratic.
        let atoms_a = atoms_file(vec![
            AtomEnvelope::Claim(claim_with_act(1, DiscourseAct::Assert, 0.9)),
            AtomEnvelope::Claim(claim_with_act(2, DiscourseAct::Argue, 0.9)),
        ]);
        let atoms_b = atoms_file(
            (1..=10)
                .map(|i| AtomEnvelope::Claim(claim_with_act(i, DiscourseAct::Assert, 0.9)))
                .collect(),
        );
        let report_a = build_report(SchemaValidationInput {
            corpus_id: "a",
            atoms: &atoms_a,
            edges: &edges_file(vec![]),
            cross_corpus: None,
            open_questions: 0,
            ungrounded_claims: 2, // 2/2 claims = 100%, over 50%
            transitions_without_trigger: 0,
            ontology: None,
            merges: None,
        });
        let report_b = build_report(SchemaValidationInput {
            corpus_id: "b",
            atoms: &atoms_b,
            edges: &edges_file(vec![]),
            cross_corpus: None,
            open_questions: 0,
            ungrounded_claims: 10, // 10/10 claims = 100%
            transitions_without_trigger: 0,
            ontology: None,
            merges: None,
        });

        let cmp = compare_across_corpora(&[report_a, report_b]);
        // ungrounded_claim_over_50pct present in both → convergent
        let ung = cmp
            .convergent_gaps
            .iter()
            .find(|c| c.signature == "gaps:ungrounded_claim_over_50pct");
        assert!(ung.is_some(), "convergent ungrounded gap expected");
        assert!(ung.unwrap().present_in.len() == 2);
        // discourse:dominance:assert only in b
        let dom = cmp
            .idiosyncratic_gaps
            .iter()
            .find(|g| g.signature == "discourse:dominance:assert");
        assert!(dom.is_some(), "idiosyncratic discourse gap expected");
        assert_eq!(dom.unwrap().present_in, "b");
    }

    #[test]
    fn utilisation_flags_atom_types_under_3pct() {
        // 100 entities, 1 claim → Claim is 1% of the atom budget,
        // under the 3% threshold.
        let mut atoms = Vec::new();
        for i in 1..=100 {
            atoms.push(AtomEnvelope::Entity(entity(i, &format!("e{i}"), 1.0)));
        }
        atoms.push(AtomEnvelope::Claim(claim_with_act(
            1,
            DiscourseAct::Assert,
            0.9,
        )));
        let report = build_report(SchemaValidationInput {
            corpus_id: "test",
            atoms: &atoms_file(atoms),
            edges: &edges_file(vec![]),
            cross_corpus: None,
            open_questions: 0,
            ungrounded_claims: 0,
            transitions_without_trigger: 0,
            ontology: None,
            merges: None,
        });
        assert!(report
            .utilisation
            .under_utilised_types
            .contains(&"Claim".to_string()));
    }

    #[test]
    fn transition_with_state_keeps_confidence_bucket_separate() {
        // States with confidence 0.3 and 0.9 land in different
        // buckets. Pin the bucket indices so bucket placement
        // doesn't drift.
        let atoms = atoms_file(vec![
            AtomEnvelope::State(state(1, 1, 0.3)),
            AtomEnvelope::State(state(2, 1, 0.9)),
        ]);
        let report = build_report(SchemaValidationInput {
            corpus_id: "test",
            atoms: &atoms,
            edges: &edges_file(vec![]),
            cross_corpus: None,
            open_questions: 0,
            ungrounded_claims: 0,
            transitions_without_trigger: 0,
            ontology: None,
            merges: None,
        });
        // Bucket 3 (0.3–0.4) has 1; bucket 9 (0.9–1.0) has 1.
        assert_eq!(report.confidence.buckets[3], 1);
        assert_eq!(report.confidence.buckets[9], 1);
    }
}
