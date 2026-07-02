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
pub mod tension_classifier;
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
pub use tension_classifier::{
    classification_to_edge, parse_phase6_classifier_response, phase6_classifier_response_schema,
    resolve_candidate_content, AtomIndex, AtomKind, CandidateContent, Phase6Classification,
};
pub use tensions::{
    select_candidates, select_embedding_topk, CandidateSource, TensionCandidate,
    TensionCandidatesOutput, TensionStrategy,
};
