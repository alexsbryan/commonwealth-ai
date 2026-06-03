//! `commonwealth-state` — distributed KV store for mesh applications.
//!
//! Provides `MeshStore` (SQLite-backed, LWW-merged) and `RetentionGc`.

pub mod activity;
mod backend;
pub mod contributions;
pub mod error;
pub mod gc;
pub mod peer_preferences;
pub mod processed_shards;
pub mod store;

pub use activity::{current_activity, served_for, ActivityEmitter, ACTIVITY_APP_ID};
pub use contributions::{current_contributions, ContributionEmitter, CONTRIBUTIONS_APP_ID};
pub use error::{Error, Result};
pub use gc::RetentionGc;
pub use peer_preferences::{
    is_gossip_excluded, PeerPreference, PeerPreferenceStore, GOSSIP_EXCLUDED_APP_IDS,
    PEER_PREFERENCES_APP_ID,
};
pub use processed_shards::{processed_shards_key, union_processed_shards, PROCESSED_SHARDS_APP_ID};
pub use store::{MeshStore, StoreEntry};
