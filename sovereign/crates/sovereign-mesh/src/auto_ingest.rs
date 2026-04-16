use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use commonwealth_api::state::AppState;
use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::NodeStatus;

const CHECK_INTERVAL: Duration = Duration::from_secs(30);
const COOLDOWN: Duration = Duration::from_secs(30 * 60);

/// Spawn a background task that automatically triggers `POST
/// /internal/corpus/collaborate` when in-progress corpora are
/// detected and at least one compatible peer is available.
///
/// **Trigger conditions**
/// - Daemon startup (first iteration) — handles the restart scenario
///   where Machine A had been ingesting before it was stopped.
/// - New peer appears in the mesh — handles the scenario where Machine
///   A is partway through ingestion and Machine B comes online.
///
/// **Guard**: if an ingest task is actively running (corpus_id is in
/// `AppStateInner::active_ingests`) the trigger is skipped that round
/// to avoid starting a conflicting second task on the same output path.
pub fn spawn_auto_collaborate_loop(state: AppState, daemon_port: u16) {
    tokio::spawn(async move {
        auto_collaborate_loop(state, daemon_port).await;
    });
}

async fn auto_collaborate_loop(state: AppState, daemon_port: u16) {
    let mut triggered: HashMap<String, Instant> = HashMap::new();
    let mut last_known_peers: HashSet<NodeId> = HashSet::new();
    let mut first_iteration = true;
    let client = reqwest::Client::new();

    // Brief startup delay so the HTTP server is accepting connections.
    tokio::time::sleep(Duration::from_secs(10)).await;

    loop {
        let self_id = state.inner.self_node_id;

        let current_peers: HashSet<NodeId> = {
            let mesh = state.inner.mesh.read().await;
            mesh.members
                .values()
                .filter(|m| m.node_id != self_id && m.status == NodeStatus::Online)
                .map(|m| m.node_id)
                .collect()
        };
        let new_peer_appeared = current_peers.iter().any(|id| !last_known_peers.contains(id));
        last_known_peers = current_peers.clone();

        let should_check = first_iteration || new_peer_appeared;
        first_iteration = false;

        if !should_check || current_peers.is_empty() {
            tokio::time::sleep(CHECK_INTERVAL).await;
            continue;
        }

        let Some(engine) = state.inner.corpus_engine.as_ref() else {
            tokio::time::sleep(CHECK_INTERVAL).await;
            continue;
        };

        let in_progress = engine.in_progress_ingestions();

        // Retire cooldown entries for corpora that have since completed.
        triggered.retain(|id, _| in_progress.contains(id));

        let active_ingests: HashSet<String> = {
            state.inner.active_ingests.read().await.clone()
        };

        for corpus_id in &in_progress {
            if active_ingests.contains(corpus_id) {
                tracing::debug!(
                    corpus = %corpus_id,
                    "auto_ingest: ingest task is active — skipping auto-collaborate this round"
                );
                continue;
            }

            if triggered.get(corpus_id).map_or(false, |t| t.elapsed() < COOLDOWN) {
                continue;
            }

            tracing::info!(
                corpus = %corpus_id,
                new_peer = new_peer_appeared,
                is_restart = triggered.get(corpus_id).is_none(),
                "auto_ingest: triggering collaboration"
            );

            let url = format!("http://127.0.0.1:{daemon_port}/internal/corpus/collaborate");
            let body = serde_json::json!({ "corpus_id": corpus_id });
            match client.post(&url).json(&body).send().await {
                Ok(resp) if resp.status().is_success() => {
                    tracing::info!(corpus = %corpus_id, "auto_ingest: collaboration started");
                    triggered.insert(corpus_id.clone(), Instant::now());
                }
                Ok(resp) if resp.status().as_u16() == 409 => {
                    tracing::debug!(corpus = %corpus_id, "auto_ingest: corpus already complete");
                    triggered.insert(corpus_id.clone(), Instant::now());
                }
                Ok(resp) if resp.status().as_u16() == 422 => {
                    // No compatible peers — don't cooldown, retry on next peer join.
                    tracing::debug!(corpus = %corpus_id, "auto_ingest: no compatible peers yet");
                }
                Ok(resp) => {
                    tracing::warn!(
                        corpus = %corpus_id,
                        status = resp.status().as_u16(),
                        "auto_ingest: unexpected response"
                    );
                }
                Err(e) => {
                    tracing::debug!(corpus = %corpus_id, error = %e, "auto_ingest: request failed");
                }
            }
        }

        tokio::time::sleep(CHECK_INTERVAL).await;
    }
}
