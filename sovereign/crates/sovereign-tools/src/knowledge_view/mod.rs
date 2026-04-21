//! KnowledgeView — declare a perspective on existing SQLite / Postgres
//! rows so they can be ingested as a corpus by `corpus-engine`.
//!
//! This module owns:
//!
//! - the `SqliteAcquirer` (and future `PostgresAcquirer`) that
//!   materialize query results to JSONL and plug into
//!   `corpus-engine` via the `AcquirerConfig::Custom` escape hatch;
//! - the built-in recipe assembly (`personal-knowledge`,
//!   `conversation-history`);
//! - `KnowledgeViewManager`, which owns view lifecycle, debounced
//!   Tier-3 enrichment, and the landscape-digest assembly read by
//!   `ConversationContext`.
//!
//! The corpus-engine side (domain implementations, custom-acquirer
//! registry, `AcquirerConfig::Custom`) lives in
//! `corpus-engine/src/enrichment/domains/{personal,conversational}.rs`
//! and `corpus-engine/src/recipe.rs`.

pub mod acquirers;
pub mod manager;
pub mod recipes;

pub use manager::{
    KnowledgeViewManager, VIEW_CONVERSATION_HISTORY, VIEW_PERSONAL_KNOWLEDGE,
};
pub use recipes::{conversation_history_recipe, personal_knowledge_recipe};
