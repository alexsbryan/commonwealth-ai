// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ring journal's own replication loop — slower than gossip, and by
//! digest rather than by snapshot.
//!
//! # Why this is not a namespace on the gossip push
//!
//! `gossip.rs` Step 4 ships a **full mesh-store snapshot to every online peer
//! every ten seconds** — 8,640 rounds a day. A household writes on the order
//! of 3,500 journal ops a year, call it 1.5 MB, so riding that push would cost
//! roughly **246 GB/day of egress per node** and would tax every other
//! namespace on the same body forever. Bandwidth is the binding constraint
//! for this feature and it binds on day one.
//!
//! So: a sixty-second cadence (ample for money), and an exchange whose
//! request is a ~600-byte digest rather than the journal.
//!
//! # The exchange
//!
//! Two calls per peer per namespace, both idempotent:
//!
//! 1. **`{digest_mine, ops: []}`** → the peer ingests nothing, answers with
//!    its own digest and every op our digest says we lack. We ingest those.
//! 2. **`{digest_mine', ops: what_they_lack}`** → computed from the digest
//!    they just gave us. They ingest; we ignore what comes back except to
//!    log it.
//!
//! A dropped call costs one round of convergence and never a duplicate entry,
//! because ingest is keyed on the content-addressed op id.
//!
//! # Everyone republishes everything they hold
//!
//! Call 2 sends what the PEER lacks out of everything WE hold, with no filter
//! on who authored it. Three failure modes die at once: the author's node
//! dying before anyone else came online, a peer restart wiping in-memory
//! buffers, and a housemate leaving the ring with half the journal. It is also
//! why there is no own-origin skip to get wrong here — the mesh store's
//! `origin` names the last republisher rather than the author, and this path
//! has no origin field at all because the op carries its author in a
//! signature.

use std::time::{Duration, Instant};

use commonwealth_api::routes_internal::{RingSyncRequest, RingSyncResponse};
use commonwealth_api::state::AppState;
use commonwealth_core::mesh::NodeStatus;
use commonwealth_transport::{peer_contact, TrafficClass};
use tracing::{debug, info, warn};

/// Money does not need ten-second convergence, and the bandwidth argument in
/// the module docs says it must not have it.
pub const DEFAULT_RING_SYNC_INTERVAL: Duration = Duration::from_secs(60);

/// Handle to the spawned loop. Aborts the task on drop, matching
/// [`GossipHandle`](crate::gossip::GossipHandle) so the daemon tears both
/// down the same way.
pub struct RingSyncHandle {
    _task: tokio::task::JoinHandle<()>,
}

impl Drop for RingSyncHandle {
    fn drop(&mut self) {
        self._task.abort();
    }
}

/// What one round moved. Returned rather than only logged so a test can
/// assert convergence instead of asserting on log lines.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RoundOutcome {
    pub namespaces: usize,
    pub peers_reached: usize,
    pub peers_unreachable: usize,
    pub ops_pulled: usize,
    pub ops_pushed: usize,
}

/// Spawn the periodic ring-sync task. Call once per daemon start.
///
/// Runs one round **immediately** before entering the interval, because the
/// first thing a node that has been offline owes its ring is everything it
/// holds — waiting a full minute to boot-republish would leave a freshly
/// restarted peer confidently reporting a total over a subset for that whole
/// minute.
pub fn spawn_ring_sync_loop(app_state: AppState, interval: Duration) -> RingSyncHandle {
    let task = tokio::spawn(async move {
        info!(
            interval_secs = interval.as_secs(),
            "ring sync: loop started"
        );
        loop {
            let started = Instant::now();
            let outcome = run_one_round(&app_state).await;
            if outcome.namespaces > 0 {
                debug!(
                    namespaces = outcome.namespaces,
                    peers_reached = outcome.peers_reached,
                    peers_unreachable = outcome.peers_unreachable,
                    ops_pulled = outcome.ops_pulled,
                    ops_pushed = outcome.ops_pushed,
                    round_ms = started.elapsed().as_millis() as u64,
                    "ring sync: round"
                );
            }
            tokio::time::sleep(interval).await;
        }
    });
    RingSyncHandle { _task: task }
}

/// One anti-entropy pass over every namespace this node holds, against every
/// online peer.
pub async fn run_one_round(app_state: &AppState) -> RoundOutcome {
    let mut outcome = RoundOutcome::default();
    let Some(rail) = app_state.ring_rail() else {
        return outcome;
    };
    let namespaces = match rail.namespaces() {
        Ok(n) => n,
        Err(e) => {
            warn!(error = %e, "ring sync: cannot enumerate namespaces");
            return outcome;
        }
    };
    if namespaces.is_empty() {
        return outcome;
    }
    let http = match crate::gossip::gossip_client() {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "ring sync: no http client");
            return outcome;
        }
    };

    let self_id = *app_state.inner.self_node_id_swap.load_full().as_ref();
    let peers: Vec<commonwealth_transport::PeerContact> = {
        let mesh = app_state.inner.mesh.read().await;
        mesh.members
            .values()
            .filter(|m| m.node_id != self_id && m.status == NodeStatus::Online)
            .map(peer_contact)
            .collect()
    };
    if peers.is_empty() {
        return outcome;
    }
    let transport = app_state.peer_transport();

    for namespace in &namespaces {
        outcome.namespaces += 1;
        let journal = match rail.journal(namespace) {
            Ok(l) => l,
            Err(e) => {
                warn!(namespace, error = %e, "ring sync: cannot open journal");
                continue;
            }
        };
        for contact in &peers {
            let endpoints = transport.endpoints(contact, TrafficClass::Gossip).await;
            let mut reached = false;
            for ep in &endpoints {
                let url = format!("{}/internal/ring/sync", ep.base_url);
                match exchange(http, &url, namespace, &journal).await {
                    Ok((pulled, pushed)) => {
                        outcome.ops_pulled += pulled;
                        outcome.ops_pushed += pushed;
                        reached = true;
                        break; // one working address is enough
                    }
                    Err(detail) => {
                        debug!(
                            peer = %contact.node_id,
                            url = %url,
                            detail,
                            "ring sync: exchange failed, trying next address"
                        );
                    }
                }
            }
            if reached {
                outcome.peers_reached += 1;
            } else {
                outcome.peers_unreachable += 1;
            }
        }
    }
    outcome
}

/// The two-call exchange with one peer address. `(pulled, pushed)`.
async fn exchange(
    http: &reqwest::Client,
    url: &str,
    namespace: &str,
    journal: &commonwealth_rail::RingJournal,
) -> Result<(usize, usize), String> {
    // Call 1 — learn what they have, take what we lack.
    let mine = journal.digest().map_err(|e| e.to_string())?;
    let first = post(
        http,
        url,
        &RingSyncRequest {
            namespace: namespace.to_string(),
            digest: mine,
            ops: Vec::new(),
        },
    )
    .await?;
    let pulled = journal.ingest_all(&first.ops).map_err(|e| e.to_string())?;

    // Call 2 — give them what they lack, out of everything we now hold.
    let for_peer = journal
        .ops_missing_from(&first.digest)
        .map_err(|e| e.to_string())?;
    if for_peer.is_empty() {
        return Ok((pulled, 0));
    }
    let refreshed = journal.digest().map_err(|e| e.to_string())?;
    let second = post(
        http,
        url,
        &RingSyncRequest {
            namespace: namespace.to_string(),
            digest: refreshed,
            ops: for_peer,
        },
    )
    .await?;
    Ok((pulled, second.ingested))
}

async fn post(
    http: &reqwest::Client,
    url: &str,
    body: &RingSyncRequest,
) -> Result<RingSyncResponse, String> {
    let resp = http
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("HTTP {status}"));
    }
    resp.json::<RingSyncResponse>()
        .await
        .map_err(|e| e.to_string())
}
