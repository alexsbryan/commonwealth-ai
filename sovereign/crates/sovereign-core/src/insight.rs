use std::sync::Arc;

use crate::error::Result;
use crate::traits::{InferenceProvider, InsightSink, InsightStore};
use crate::types::*;

// ─── InsightSinkRegistry ─────────────────────────────────────

/// Registry of active sinks. Injected at Runtime construction.
/// Empty by default. Obsidian sink added when vault is configured.
pub struct InsightSinkRegistry {
    sinks: Vec<Arc<dyn InsightSink>>,
}

impl InsightSinkRegistry {
    pub fn new() -> Self {
        Self { sinks: vec![] }
    }

    pub fn register(&mut self, sink: Arc<dyn InsightSink>) {
        self.sinks.push(sink);
    }

    pub fn is_empty(&self) -> bool {
        self.sinks.is_empty()
    }

    pub async fn any_connected(&self) -> bool {
        for sink in &self.sinks {
            if sink.is_connected().await {
                return true;
            }
        }
        false
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn InsightSink>> {
        self.sinks.iter()
    }
}

impl Default for InsightSinkRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ─── InsightService ──────────────────────────────────────────

/// Coordinates clip actions: embed, find adjacent, persist, sync.
pub struct InsightService {
    pub store: Arc<dyn InsightStore>,
    pub sinks: Arc<InsightSinkRegistry>,
    inference: Arc<dyn InferenceProvider>,
}

impl InsightService {
    pub fn new(
        store: Arc<dyn InsightStore>,
        sinks: Arc<InsightSinkRegistry>,
        inference: Arc<dyn InferenceProvider>,
    ) -> Self {
        Self {
            store,
            sinks,
            inference,
        }
    }

    /// Called when the user makes a clip action.
    /// Returns the created InsightNode so the UI can render it immediately.
    pub async fn clip(
        &self,
        clipped_text: &str,
        message_id: uuid::Uuid,
        paragraph_index: usize,
        source: InsightSource,
        position: Option<InsightPosition>,
    ) -> Result<InsightNode> {
        // 1. Embed the clipped text — the only async inference call (<200ms).
        let embedding = self.inference.embed(clipped_text).await?;

        // 2. Find adjacent existing nodes by embedding similarity.
        let adjacent: Vec<String> = self
            .store
            .adjacent_by_embedding(&embedding, 4)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter_map(|n| n.source.article_title)
            .collect();

        // 3. Build and persist the node.
        let node = InsightNode {
            id: uuid::Uuid::new_v4(),
            clipped_text: clipped_text.to_string(),
            message_id,
            paragraph_index,
            source,
            position,
            adjacent,
            embedding: Some(embedding),
            created_at: chrono::Utc::now(),
            sink_state: InsightSinkState::Local,
        };

        self.store.save(&node).await?;

        // 4. Push to sinks if configured (non-blocking).
        if !self.sinks.is_empty() {
            let sinks = self.sinks.clone();
            let store = self.store.clone();
            let node_clone = node.clone();
            tokio::spawn(async move {
                push_to_sinks(&node_clone, &sinks, &store).await;
            });
        }

        Ok(node)
    }
}

async fn push_to_sinks(
    node: &InsightNode,
    sinks: &InsightSinkRegistry,
    store: &Arc<dyn InsightStore>,
) {
    for sink in sinks.iter() {
        match sink.push(node).await {
            Ok(()) => {
                let _ = store
                    .update_sink_state(
                        node.id,
                        InsightSinkState::Synced {
                            sink_id: sink.id().to_string(),
                            synced_at: chrono::Utc::now(),
                        },
                    )
                    .await;
            }
            Err(e) => {
                let _ = store
                    .update_sink_state(
                        node.id,
                        InsightSinkState::SyncFailed {
                            sink_id: sink.id().to_string(),
                            error: e.to_string(),
                        },
                    )
                    .await;
                tracing::warn!(
                    sink = sink.id(),
                    error = %e,
                    "Insight sync to sink failed"
                );
            }
        }
    }
}
