// SPDX-License-Identifier: AGPL-3.0-or-later
//! One fan-out for every federated question — the seam WORK_PLANE.md names
//! as design gap 1 ("the federated-query seam is corpus-shaped"), made
//! item-type-generic by extraction rather than by a second copy.
//!
//! Until 2026-09-11 this lived inline in `routes_knowledge`: offering
//! selection, one spawned task per peer under the `fanout_inflight` gauge
//! the `BoundedFanOut` soak invariant asserts on, first-reachable-endpoint-
//! wins through the transport with `note_success` feeding the shared
//! reachability hint, and a served/failed outcome per peer stamped with who
//! answered. Everything about corpora stayed there; everything about PEERS is
//! here, generic over what a peer is asked and what it answers with (`T`).
//!
//! # Every target is a row
//!
//! The cloud-peer flight of 2026-08-29 (note 60d4d79b) found that a corpus
//! nobody searched came back byte-identical to "searched, found nothing".
//! The same shape lies for peers: a member refused before dialing — offline,
//! no path, not offering — must be a [`PeerVerdict::NeverAsked`] row carrying
//! its reason, never an absence, so a caller reading the report can tell a
//! silent mesh from a quiet one. `rows.len() == targets.len()`, always.
//!
//! # A slow peer is its own problem
//!
//! Each target runs under its own timeout when the caller sets one; the
//! others finish on their own clock. Without that, one stalled relay would
//! hold every catalogue row hostage (the media case) or every corpus (the
//! knowledge case).
use std::sync::Arc;
use std::time::{Duration, Instant};

use commonwealth_core::ids::NodeId;
use commonwealth_transport::{PeerContact, PeerEndpoint, PeerTransport, TrafficClass};
use serde::Serialize;

use crate::state::AppStateInner;

/// A peer to ask: its identity as the roster names it, and how to reach it,
/// cloned out of the mesh lock so the fan-out runs without holding it.
#[derive(Debug, Clone)]
pub struct FanoutTarget {
    pub node_id: NodeId,
    pub name: String,
    pub contact: PeerContact,
}

/// Why one target produced no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerFailure {
    /// Refused before anything was sent — a fact about the roster or the
    /// transport (offline, no path, not offering), named.
    NeverAsked(String),
    /// Sent, and no usable answer came back: every endpoint failed, a
    /// non-success status, an undecodable body, a timeout, a panic.
    Failed(String),
}

/// What one target came back with.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum PeerVerdict<T> {
    Served(T),
    Failed { reason: String },
    NeverAsked { reason: String },
}

/// One row per target, whatever happened to it.
#[derive(Debug, Clone, Serialize)]
pub struct PeerRow<T> {
    pub node_id: String,
    pub name: String,
    pub elapsed_ms: u64,
    #[serde(flatten)]
    pub verdict: PeerVerdict<T>,
}

impl<T> PeerRow<T> {
    pub fn served(&self) -> Option<&T> {
        match &self.verdict {
            PeerVerdict::Served(t) => Some(t),
            _ => None,
        }
    }
}

/// Counts a caller logs in one line: the AP-isolation failure mode is
/// `served == 0 && failed > 0` — every peer tried was unreachable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct FanoutCounts {
    pub served: usize,
    pub failed: usize,
    pub never_asked: usize,
}

pub fn counts<T>(rows: &[PeerRow<T>]) -> FanoutCounts {
    let mut c = FanoutCounts::default();
    for r in rows {
        match r.verdict {
            PeerVerdict::Served(_) => c.served += 1,
            PeerVerdict::Failed { .. } => c.failed += 1,
            PeerVerdict::NeverAsked { .. } => c.never_asked += 1,
        }
    }
    c
}

/// RAII gauge guard for `AppStateInner::fanout_inflight`: increments on
/// construction and decrements on drop, so the live count of outbound peer
/// fan-out requests is correct even if a spawned fan-out task panics or is
/// cancelled. One is held inside each fan-out task; the companion read is
/// `AppState::fanout_inflight_count`, surfaced over HTTP via `glassbox_signals`
/// and asserted by the `BoundedFanOut` soak invariant.
struct FanoutGuard(Arc<AppStateInner>);

impl FanoutGuard {
    fn new(inner: Arc<AppStateInner>) -> Self {
        inner
            .fanout_inflight
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self(inner)
    }
}

impl Drop for FanoutGuard {
    fn drop(&mut self) {
        self.0
            .fanout_inflight
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Ask every target the same question concurrently and hand back one row
/// per target, in the order given.
///
/// `ask` runs once per target with that target's `payload` (what the caller
/// wants this peer asked — the corpora it holds, or nothing). `per_peer`
/// caps each ask; `None` leaves each to its own timeouts. A task that panics
/// is a [`PeerVerdict::Failed`] row, and the gauge is released either way.
pub async fn fan_out<T, P, F, Fut>(
    inner: Arc<AppStateInner>,
    targets: Vec<(FanoutTarget, P)>,
    per_peer: Option<Duration>,
    ask: F,
) -> Vec<PeerRow<T>>
where
    T: Send + 'static,
    P: Send + 'static,
    F: Fn(FanoutTarget, P) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<T, PeerFailure>> + Send + 'static,
{
    let ask = Arc::new(ask);
    let mut handles = Vec::with_capacity(targets.len());
    for (target, payload) in targets {
        let ask = ask.clone();
        let guard = FanoutGuard::new(inner.clone());
        let identity = (target.node_id.to_string(), target.name.clone());
        let handle = tokio::spawn(async move {
            let _guard = guard;
            let started = Instant::now();
            let outcome = match per_peer {
                None => ask(target, payload).await,
                Some(cap) => match tokio::time::timeout(cap, ask(target, payload)).await {
                    Ok(o) => o,
                    Err(_) => Err(PeerFailure::Failed(format!(
                        "no answer within {}ms",
                        cap.as_millis()
                    ))),
                },
            };
            (outcome, started.elapsed())
        });
        handles.push((identity, Instant::now(), handle));
    }
    let mut rows = Vec::with_capacity(handles.len());
    for ((node_id, name), started, handle) in handles {
        let (verdict, elapsed) = match handle.await {
            Ok((Ok(t), elapsed)) => (PeerVerdict::Served(t), elapsed),
            Ok((Err(PeerFailure::Failed(reason)), elapsed)) => {
                (PeerVerdict::Failed { reason }, elapsed)
            }
            Ok((Err(PeerFailure::NeverAsked(reason)), elapsed)) => {
                (PeerVerdict::NeverAsked { reason }, elapsed)
            }
            Err(join) => (
                PeerVerdict::Failed {
                    reason: format!("the ask task ended abnormally: {join}"),
                },
                started.elapsed(),
            ),
        };
        rows.push(PeerRow {
            node_id,
            name,
            elapsed_ms: elapsed.as_millis() as u64,
            verdict,
        });
    }
    let c = counts(&rows);
    tracing::info!(
        target: "fanout",
        served = c.served,
        failed = c.failed,
        never_asked = c.never_asked,
        "fan-out complete"
    );
    rows
}

/// Try a peer's candidate endpoints for `class` in the transport's order
/// until one answers; the first that does is pinned as next time's starting
/// point (`note_success`). `attempt` says per endpoint whether to keep going:
/// `Err(reason)` tries the next one, and the last reason is what the caller
/// gets when every endpoint failed. No endpoints at all is [`PeerFailure::NeverAsked`].
pub async fn first_endpoint_that_answers<T, F, Fut>(
    transport: &Arc<dyn PeerTransport>,
    node_id: NodeId,
    contact: &PeerContact,
    class: TrafficClass,
    mut attempt: F,
) -> Result<T, PeerFailure>
where
    F: FnMut(PeerEndpoint) -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let endpoints = transport.endpoints(contact, class).await;
    if endpoints.is_empty() {
        return Err(PeerFailure::NeverAsked(format!(
            "no {class:?} endpoint for this peer — it gossips no address this transport can use"
        )));
    }
    let mut last = String::from("no endpoint tried");
    for ep in endpoints {
        match attempt(ep.clone()).await {
            Ok(t) => {
                transport.note_success(node_id, class, &ep);
                return Ok(t);
            }
            Err(reason) => {
                tracing::info!(
                    target: "fanout",
                    peer = %node_id,
                    addr = %ep.label,
                    reason = %reason,
                    "fan-out: endpoint did not answer, trying next"
                );
                last = reason;
            }
        }
    }
    Err(PeerFailure::Failed(last))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_app_state;

    fn target(n: u128, name: &str) -> FanoutTarget {
        FanoutTarget {
            node_id: NodeId::from_u128(n),
            name: name.into(),
            contact: PeerContact {
                node_id: NodeId::from_u128(n),
                node_pubkey: None,
                addresses: Vec::new(),
                relay_url: None,
                iroh_direct_addrs: Vec::new(),
            },
        }
    }

    /// The failing input is the timeout removed: with `per_peer` = `None`
    /// the slow peer holds the whole fan-out for its full sleep and the
    /// elapsed bound below fails. With it, the quick peer is served, the slow
    /// one is a failed row naming the cap, and nothing waited on it.
    #[tokio::test]
    async fn a_slow_peer_does_not_delay_the_others_and_is_a_failed_row() {
        let state = test_app_state();
        let started = Instant::now();
        let rows = fan_out(
            state.inner.clone(),
            vec![(target(1, "Slow"), 5_000u64), (target(2, "Quick"), 0u64)],
            Some(Duration::from_millis(200)),
            |_t, sleep_ms| async move {
                tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
                Ok::<_, PeerFailure>("hi")
            },
        )
        .await;
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].served(), Some(&"hi"));
        match &rows[0].verdict {
            PeerVerdict::Failed { reason } => assert!(reason.contains("200ms"), "{reason}"),
            other => panic!("the slow peer must be a failed row, got {other:?}"),
        }
    }

    /// Every target is a row. A peer refused before dialing is
    /// `NeverAsked` with its reason — the shape that stops a silent mesh
    /// from reading as a quiet one. Failing input: a row dropped.
    #[tokio::test]
    async fn a_target_refused_before_dialing_is_a_never_asked_row_not_an_absence() {
        let state = test_app_state();
        let rows = fan_out(
            state.inner.clone(),
            vec![(target(1, "Offline"), ()), (target(2, "Up"), ())],
            None,
            |t, ()| async move {
                if t.name == "Offline" {
                    Err(PeerFailure::NeverAsked("'Offline' is offline".into()))
                } else {
                    Ok(1u32)
                }
            },
        )
        .await;
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].verdict,
            PeerVerdict::NeverAsked {
                reason: "'Offline' is offline".into()
            }
        );
        assert_eq!(rows[1].verdict, PeerVerdict::Served(1));
        assert_eq!(
            counts(&rows),
            FanoutCounts {
                served: 1,
                failed: 0,
                never_asked: 1
            }
        );
    }

    /// The gauge the soak invariant reads returns to zero even when a peer
    /// task panics, and the panic is a failed row rather than a lost one.
    #[tokio::test]
    async fn the_gauge_returns_to_zero_after_a_peer_task_panics() {
        let state = test_app_state();
        let rows = fan_out(
            state.inner.clone(),
            vec![(target(1, "Boom"), ()), (target(2, "Fine"), ())],
            None,
            |t, ()| async move {
                if t.name == "Boom" {
                    panic!("peer task panicked on purpose");
                }
                Ok::<_, PeerFailure>(())
            },
        )
        .await;
        assert_eq!(state.fanout_inflight_count(), 0);
        assert!(
            matches!(rows[0].verdict, PeerVerdict::Failed { .. }),
            "{:?}",
            rows[0]
        );
        assert_eq!(rows[1].verdict, PeerVerdict::Served(()));
    }
}
