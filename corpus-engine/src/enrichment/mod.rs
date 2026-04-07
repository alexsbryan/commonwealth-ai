//! Optional epistemic enrichment layer.
//!
//! Extracts propositional claims from corpus chunks and tags each with
//! an epistemic status (consensus, majority, contested, minority,
//! established) drawn from the source text's own hedging language.
//! Optionally extracts relationships between claims across entries
//! (contradicts, supports, refines, competing answers, presupposes).
//!
//! Enrichment is opt-in per recipe. The standard `chunks` table is always
//! built; the `claims` and `relationships` tables only exist if the
//! recipe sets `[enrichment] enabled = true` and the engine has been
//! given an `InferenceFn` via `CorpusEngine::with_inference_fn()`.

pub mod article_profile;
pub mod claims;
pub mod engine;
pub mod landscape;
pub mod link_graph;
pub mod relationships;
pub mod schema;

pub use article_profile::{
    ArticleEpistemicProfile, WikiLink, WikipediaChunkMetadata, compute_article_profiles,
};
pub use claims::{ExtractedClaim, EpistemicStatus};
pub use engine::EnrichmentEngine;
pub use landscape::{ContestedCluster, EpistemicLandscape, Position};
pub use link_graph::LinkGraphBuilder;
pub use relationships::{ClaimRelationship, RelationshipType};
