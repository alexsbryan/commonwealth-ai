// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ingest program's faces, composed into this process by a distribution
//! (pb-ingest-dial-tools-close, FIVE_PROGRAMS §2c). svrn names no type of
//! ingest's: the distribution builds each port's implementor and hands it
//! here. Today that is the enrichment-config port, whose implementor lives in
//! ingest's catalog, which svrn does not link. svrn alone (no
//! [`HostedIngest`]) reports ingest absent by name where it needs one.

use std::sync::Arc;

use corpus_index::ingest_port::enrich_config::EnrichConfigPort;

/// The distribution's composition of ingest.
pub struct HostedIngest {
    enrich_config: Arc<dyn EnrichConfigPort>,
}

impl HostedIngest {
    /// `enrich_config` reads and writes corpora's enrichment configs.
    pub fn new(enrich_config: Arc<dyn EnrichConfigPort>) -> Self {
        Self { enrich_config }
    }

    /// The enrichment-config port.
    pub fn enrich_config(&self) -> Arc<dyn EnrichConfigPort> {
        Arc::clone(&self.enrich_config)
    }
}
