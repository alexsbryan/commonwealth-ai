//! KnowledgeView — declare a perspective on existing SQLite / Postgres
//! rows so they can be ingested as a corpus by `corpus-engine`.
//!
//! This module owns:
//!
//! - the `SqliteAcquirer` (and future `PostgresAcquirer`) that
//!   materialise query results to JSONL and plug into
//!   `corpus-engine` via the `AcquirerConfig::Custom` escape hatch;
//! - the built-in recipe assembly (`personal-knowledge`,
//!   `conversation-history`, `institutional-notes`);
//! - `KnowledgeViewManager`, which owns view lifecycle, debounced
//!   Tier-3 enrichment, and the landscape-digest assembly read by
//!   `ConversationContext`;
//! - `ViewKind`, the type-safe handle for referring to a view;
//! - pure helpers for digest formatting (`digest`) and budget
//!   accounting (`tokens`).
//!
//! The corpus-engine side (domain implementations, custom-acquirer
//! registry, `AcquirerConfig::Custom`) lives in
//! `corpus-engine/src/enrichment/domains/{personal,conversational,
//! institutional}.rs` and `corpus-engine/src/recipe.rs`.

pub mod acquirers;
pub mod atlas_digest;
pub mod cross_view;
pub mod debouncer;
pub mod digest;
pub mod manager;
pub mod recipes;
pub mod relational;
#[cfg(feature = "treesitter")]
pub mod splice_extension;
pub mod strategic;
pub mod timeline;
pub mod tokens;
pub mod view_kind;

pub use manager::{
    KnowledgeViewManager, VIEW_CONVERSATION_HISTORY, VIEW_CROSS_VIEW, VIEW_INSTITUTIONAL_NOTES,
    VIEW_PERSONAL_KNOWLEDGE,
};
pub use recipes::{
    conversation_history_recipe, institutional_notes_recipe, personal_knowledge_recipe,
};
pub use tokens::estimate_tokens;
pub use view_kind::ViewKind;
