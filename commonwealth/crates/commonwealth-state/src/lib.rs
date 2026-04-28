//! `commonwealth-state` — distributed KV store for mesh applications.
//!
//! Provides `MeshStore` (SQLite-backed, LWW-merged) and `RetentionGc`.

mod backend;
pub mod contributions;
pub mod error;
pub mod gc;
pub mod peer_preferences;
pub mod store;

pub use contributions::{
    current_contributions, ContributionEmitter, CONTRIBUTIONS_APP_ID,
};
pub use error::{Error, Result};
pub use gc::RetentionGc;
pub use peer_preferences::{
    is_gossip_excluded, PeerPreference, PeerPreferenceStore,
    GOSSIP_EXCLUDED_APP_IDS, PEER_PREFERENCES_APP_ID,
};
pub use store::{MeshStore, StoreEntry};
