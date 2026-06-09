// SPDX-License-Identifier: AGPL-3.0-or-later
//! Retention garbage collector for the mesh store.
//!
//! Runs on a periodic interval and deletes entries older than `ttl_seconds`.
//! Shuts down when the watch channel fires.

use std::sync::Arc;
use std::time::Duration;

use tracing::{debug, warn};

use crate::store::MeshStore;

pub struct RetentionGc {
    store: Arc<MeshStore>,
    ttl_seconds: u64,
    interval: Duration,
}

impl RetentionGc {
    pub fn new(store: Arc<MeshStore>, ttl_seconds: u64, interval: Duration) -> Self {
        Self {
            store,
            ttl_seconds,
            interval,
        }
    }

    /// Run until the shutdown watch fires `true`.
    pub async fn run(self, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        let mut interval = tokio::time::interval(self.interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    match self.store.gc(self.ttl_seconds) {
                        Ok(n) if n > 0 => {
                            debug!(deleted = n, ttl_secs = self.ttl_seconds, "RetentionGc: deleted expired entries");
                        }
                        Ok(_) => {}
                        Err(e) => {
                            warn!("RetentionGc error: {e}");
                        }
                    }
                }
                Ok(()) = shutdown.changed() => {
                    if *shutdown.borrow() {
                        debug!("RetentionGc: shutdown signal received");
                        break;
                    }
                }
            }
        }
    }
}
