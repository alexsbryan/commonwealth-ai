// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reciprocity weights for the fair scheduler.
//!
//! The scheduler ([`crate::scheduler`]) orders its queue by a caller-supplied
//! `weight` and never reads the contribution ledger itself — that keeps the
//! policy decoupled and unit-testable. This module is the boundary that turns
//! the mesh's contribution ledger into that weight.
//!
//! The ledger lives in the Commonwealth daemon, not this server, so we can't
//! read it in-process. Instead a background task polls the daemon's
//! `GET /internal/contribution/view` every [`REFRESH_INTERVAL`] and caches a
//! `node_id → weight` table in an [`ArcSwap`] for lock-free per-request reads.
//! A request keyed to a contributing peer ranks up; everything else is
//! neutral (`1.0`). If the daemon is unreachable the last-known table is
//! kept — reciprocity degrades to "stale", never to a hang.
//!
//! `weight = 1.0 + k · (wall_seconds / max_wall_seconds)`, so the heaviest
//! contributor approaches `1.0 + k` and a non-contributor stays at `1.0`.
//! `k` is `[server] reciprocity_k`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use axum::http::HeaderMap;
use commonwealth_core::fair_sched::reciprocity_weight;

use crate::auth::TenantId;
use crate::scheduler::UserKey;

/// How often the background task refreshes the weight table. Contribution
/// totals move slowly (30-day window), so a 30 s cadence is ample and keeps
/// the daemon load negligible.
const REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// Lock-free cache of `node_id (hex) → reciprocity weight (≥ 1.0)`. Read on
/// every admission; refreshed out-of-band. Nodes absent from the map (and all
/// local tenants) are neutral.
pub struct ReciprocityTable {
    weights: ArcSwap<HashMap<String, f64>>,
}

impl ReciprocityTable {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            weights: ArcSwap::from_pointee(HashMap::new()),
        })
    }

    /// The reciprocity weight for an admission's origin. Contributing peers
    /// (by hex `NodeId`) rank up; local tenants and unknown peers are neutral.
    pub fn weight_for(&self, key: &UserKey) -> f64 {
        match key {
            UserKey::Node(hex) => self.weights.load().get(hex).copied().unwrap_or(1.0),
            UserKey::Tenant(_) => 1.0,
        }
    }

    fn install(&self, weights: HashMap<String, f64>) {
        self.weights.store(Arc::new(weights));
    }
}

/// Map a request's identity to a scheduler [`UserKey`]. A mesh-routed request
/// carries the origin node in `X-Node-Id` (the established mesh convention) —
/// key on that so reciprocity and the per-origin cap apply to the *true*
/// origin, not the local default tenant the auth layer assigned. Otherwise
/// key on the tenant.
pub fn user_key(tenant: &TenantId, headers: &HeaderMap) -> UserKey {
    match headers.get("x-node-id").and_then(|v| v.to_str().ok()) {
        Some(hex) if !hex.is_empty() => UserKey::Node(hex.to_string()),
        _ => UserKey::Tenant(tenant.0.clone()),
    }
}

/// One peer's contribution view, as returned by the daemon's
/// `/internal/contribution/view`. We deserialize only the field we weight on;
/// serde ignores the rest of `NodeContributionsView`.
#[derive(serde::Deserialize)]
struct ContributionView {
    node_id: String,
    inference_served_wall_seconds: f64,
}

/// Turn the per-node contribution views into a weight table.
/// `weight = 1.0 + k · normalize(wall_seconds)`, normalized against the
/// heaviest contributor so the scale is relative to the actual fleet. Nodes
/// with no served wall-time are omitted (they read as neutral `1.0`).
fn compute_weights(views: &[ContributionView], k: f64) -> HashMap<String, f64> {
    let max = views
        .iter()
        .map(|v| v.inference_served_wall_seconds)
        .fold(0.0_f64, f64::max);
    // Keep only above-neutral contributors; absent nodes read as 1.0 on
    // lookup. `reciprocity_weight` returns 1.0 when there's no signal or
    // reciprocity is off, so those fall away here.
    views
        .iter()
        .filter_map(|v| {
            let weight = reciprocity_weight(v.inference_served_wall_seconds, max, k);
            (weight > 1.0).then(|| (v.node_id.clone(), weight))
        })
        .collect()
}

async fn fetch_weights(
    client: &reqwest::Client,
    url: &str,
    k: f64,
) -> Result<HashMap<String, f64>, String> {
    let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("status {}", resp.status()));
    }
    let views: Vec<ContributionView> = resp.json().await.map_err(|e| e.to_string())?;
    Ok(compute_weights(&views, k))
}

/// Spawn the background refresh loop. No-op forever if `commonwealth_url` is
/// `None` (mesh integration disabled) — the table stays empty and every
/// origin is neutral, which is the correct degraded behaviour.
pub fn spawn_refresh(
    commonwealth_url: Option<String>,
    reciprocity_k: f64,
    table: Arc<ReciprocityTable>,
) {
    let Some(base) = commonwealth_url else {
        tracing::info!("reciprocity: no commonwealth url — weights stay neutral");
        return;
    };
    let url = format!("{}/internal/contribution/view", base.trim_end_matches('/'));
    tokio::spawn(async move {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        let mut ticker = tokio::time::interval(REFRESH_INTERVAL);
        loop {
            ticker.tick().await;
            match fetch_weights(&client, &url, reciprocity_k).await {
                Ok(weights) => {
                    let n = weights.len();
                    table.install(weights);
                    tracing::debug!(contributors = n, "reciprocity: weights refreshed");
                }
                Err(e) => {
                    // Keep the last-known table — a transient daemon hiccup
                    // must not flap everyone to neutral mid-contention.
                    tracing::warn!(error = %e, "reciprocity: refresh failed; keeping last-known weights");
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(id: &str, wall: f64) -> ContributionView {
        ContributionView {
            node_id: id.to_string(),
            inference_served_wall_seconds: wall,
        }
    }

    #[test]
    fn heaviest_contributor_approaches_one_plus_k() {
        let views = vec![view("aa", 100.0), view("bb", 50.0), view("cc", 0.0)];
        let w = compute_weights(&views, 0.5);
        assert_eq!(w.get("aa"), Some(&1.5), "max contributor → 1 + k");
        assert_eq!(w.get("bb"), Some(&1.25), "half → 1 + k·0.5");
        assert_eq!(w.get("cc"), None, "no contribution → neutral (absent)");
    }

    #[test]
    fn no_signal_is_all_neutral() {
        let views = vec![view("aa", 0.0), view("bb", 0.0)];
        assert!(compute_weights(&views, 0.5).is_empty());
    }

    #[test]
    fn k_zero_disables_reciprocity() {
        let views = vec![view("aa", 100.0)];
        assert!(
            compute_weights(&views, 0.0).is_empty(),
            "k=0 → everyone neutral"
        );
    }

    #[test]
    fn table_lookup_neutral_for_tenant_and_unknown_node() {
        let table = ReciprocityTable::new();
        table.install(HashMap::from([("aa".to_string(), 1.4)]));
        assert_eq!(table.weight_for(&UserKey::Node("aa".into())), 1.4);
        assert_eq!(
            table.weight_for(&UserKey::Node("zz".into())),
            1.0,
            "unknown node neutral"
        );
        assert_eq!(
            table.weight_for(&UserKey::Tenant("t".into())),
            1.0,
            "tenant neutral"
        );
    }

    #[test]
    fn user_key_prefers_x_node_id() {
        let tenant = TenantId("default".to_string());
        let mut headers = HeaderMap::new();
        assert_eq!(
            user_key(&tenant, &headers),
            UserKey::Tenant("default".to_string()),
            "no header → tenant"
        );
        headers.insert("x-node-id", "deadbeef".parse().unwrap());
        assert_eq!(
            user_key(&tenant, &headers),
            UserKey::Node("deadbeef".to_string()),
            "header present → node origin"
        );
    }
}
