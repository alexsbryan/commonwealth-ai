// SPDX-License-Identifier: AGPL-3.0-or-later
//! Post-resolution atlas analysis passes.
//!
//! These run after Phases 3a/3b populate atoms and structural edges.
//! They read the resolved atlas, surface relationships the resolver
//! left implicit, and write their output to the atlas directory
//! alongside the atoms/edges files.
//!
//! Current scope (Landing 3, deterministic half):
//!
//! - `tensions::select_candidates` — narrow O(N²) claim/state pairs
//!   down to plausible-tension candidates using three signals:
//!   intra-cluster, entity-overlap, and embedding top-K. Produces
//!   `Vec<TensionCandidate>` for the LLM classifier in Landing 4.
//! - `gaps::detect_deterministic_gaps` — structural gap detection
//!   that needs no LLM: state transitions without trigger events,
//!   claims without grounding edges, questions still unresolved at
//!   the end of Phase 3b.
//!
//! Landing 4 will extend this module with an LLM-driven tension
//! classifier that consumes the candidate list and decides per-pair
//! whether a real tension exists (and, if so, the sub-question it
//! turns on).

pub mod configuration;
pub mod gaps;
pub mod holistic_classifier;
pub mod parcel_analytics;
pub mod patterns_adapter;
pub mod sec_facts;
pub mod tension_classifier;
pub mod tension_fields;
pub mod tension_policy;
pub mod tension_shape;
pub mod tensions;

pub use configuration::{
    parse_configurations, summarise_atlas, AtlasSummary, AtlasSummaryParams, ClaimSynopsis,
    ConfigurationsOutput, EntitySynopsis, EventSynopsis, Phase8ParseItem, QuestionSynopsis,
    RelationSynopsis, TrajectorySynopsis,
};
pub use gaps::{detect_deterministic_gaps, Gap, GapKind, GapsOutput};
pub use holistic_classifier::{
    parse_holistic_response, render_holistic_user_body, HolisticTension,
};
pub use parcel_analytics::{
    compute_aggregates, flags, per_parcel_deltas, FlagKind, ParcelAggregates, ParcelDelta,
    ParcelFlag,
};
pub use patterns_adapter::{to_investigation_graph, InvestigationGraph, PatternFindingsOutput};
pub use sec_facts::{
    authoritative_store as sec_authoritative_store, change as sec_change,
    coverage_card as sec_coverage_card, coverage_summary as sec_coverage_summary,
    discover_authoritative_stores as sec_discover_authoritative_stores, lookup as sec_lookup,
    ratio as sec_ratio, store_claims as sec_store_claims, AnsweredConcept, ConceptKind,
    CoverageCard, CoverageLimit, LimitKind, SecFact, SecFactStore, SecRefusal,
    SEC_FACTS_AUTHORITY_TOOL, SEC_FACTS_SIDECAR,
};
pub use tension_classifier::{
    classification_to_edge, classification_to_same_as_claim, merge_same_as_claims,
    next_claim_ordinal, parse_phase6_classifier_response, phase6_classifier_response_schema,
    phase6_classifier_response_schema_with_relation, resolve_candidate_content, AtomIndex,
    CandidateContent, Phase6Classification, Phase6Relation, Phase6Verdict, TensionSide,
    SAME_AS_CLAIM_KIND, SAME_AS_GRADE_CLASSIFIER, SAME_AS_GRADE_KEY, SAME_AS_MERGED_KEY,
};
pub use tension_policy::{
    drop_non_comparable_pairs, restrict_claims_to_types, BetweenOutcome, ComparabilityReport,
};
pub use tension_shape::{derive_declared_strategy, CorpusShape};
pub use tensions::{
    select_candidates, select_embedding_topk, CandidateSource, TensionCandidate,
    TensionCandidatesOutput, TensionStrategy,
};
