// SPDX-License-Identifier: AGPL-3.0-or-later
//! Fair turn scheduler for the chat server — the async shell over the
//! shared [`serving_policy::fair_sched::SchedCore`] policy.
//!
//! This is the successor to `busy.rs`'s flat semaphore on the *local*
//! conversational API (`/v1/conversations/*`). The same policy core also
//! backs the mesh peer-admission gate in `commonwealth-api` — see that
//! crate's `admission.rs`. Sharing the core keeps both gates fair by the
//! identical rules; this file owns only the parts the pure policy can't:
//! the `tokio` waiting/waking and the RAII permit.
//!
//! Two entry points map to the two client transports:
//!   - [`FairScheduler::admit`] — the WebSocket chat path. The caller waits;
//!     it's told its queue position up front and on every move up, and is
//!     served highest-weight (reciprocity) first. Sheds (never hangs) past
//!     the depth cap.
//!   - [`FairScheduler::try_grant`] — the REST one-shot path. Grants if a
//!     slot is free, else sheds immediately with a position hint (no
//!     long-poll).
//!
//! `weight` is supplied by the consult site from a cached reciprocity
//! snapshot (a contributing origin ranks up); the scheduler itself never
//! touches the ledger. `cap` is the per-origin concurrency allowance
//! (`max_per_user`), so one origin can't hold every slot.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use tokio::sync::Notify;

use serving_policy::fair_sched::{AdmitOutcome, ClaimOrStatus, QueueStatus, SchedCore, TryGrant};

/// Who a turn is attributed to, for fairness and reciprocity. `String`-keyed
/// so the scheduler stays decoupled from `auth`/`mesh` types — the consult
/// sites convert at the boundary.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum UserKey {
    /// Locally-authenticated traffic, keyed by tenant id (always present).
    Tenant(String),
    /// A mesh-routed request, keyed by the origin node's hex NodeId (the
    /// `X-Node-Id` header). Lets reciprocity rank a contributing peer up.
    Node(String),
}

/// Returned when the host sheds load instead of granting — preserving the
/// never-hang property. Rendered as `503 + Retry-After` (REST) or a busy
/// `StreamError` (WS). Re-exports the core's `QueueStatus` for the live path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shed {
    /// Where the request *would* have landed (1-based) had it queued — a hint
    /// for the client's retry banner. `0` when no position is meaningful.
    pub would_be_position: u32,
    pub retry_after_secs: u64,
}

/// `SchedCore` plus the async-coupled bit it can't own: a `Notify` per waiter
/// so the shell can wake a specific queued task to claim its slot (or re-emit
/// its improved position). One `Mutex` guards both, so "core says wake seq N"
/// and "the handle for seq N" are always consistent.
struct SchedState {
    core: SchedCore<UserKey>,
    notifies: HashMap<u64, Arc<Notify>>,
}

impl SchedState {
    /// Wake every registered waiter. Each re-checks `claim_or_status`:
    /// granted → claims; still waiting → re-emits its (now possibly lower)
    /// position. `Notify::notify_one` stores a permit if the task isn't
    /// parked yet, so there's no lost-wakeup race. Bounded by the queue depth,
    /// so this is at most a few dozen cheap wakes.
    fn wake_all(&self) {
        for n in self.notifies.values() {
            n.notify_one();
        }
    }
}

/// The `Arc<Inner>` shared-state idiom, not a domain noun — every
/// same-named type elsewhere in the workspace is another crate's private
/// twin of this pattern.
struct Inner {
    state: Mutex<SchedState>,
    /// Per-origin concurrency allowance handed to the core on each admission.
    max_per_user: u32,
    retry_after_secs: u64,
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, SchedState> {
        // A panic under the lock would poison it; recover the inner state
        // rather than cascade the panic into every future turn.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Fair turn scheduler. Cheap to clone (shared `Arc<Inner>`); installed as an
/// Axum `Extension`, exactly like the `BusyGuard` it replaces.
#[derive(Clone)]
pub struct FairScheduler {
    inner: Arc<Inner>,
}

impl FairScheduler {
    pub fn new(
        max_concurrent_turns: usize,
        max_per_user: u32,
        max_queue_depth: usize,
        retry_after_secs: u64,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(SchedState {
                    // Clamp to ≥ 1: a zero chat-slot budget would 503 every
                    // turn. (The core permits 0 — that's the peer gate's
                    // "reject all" ceiling, which the chat server never wants.)
                    core: SchedCore::new(max_concurrent_turns.max(1), max_queue_depth),
                    notifies: HashMap::new(),
                }),
                max_per_user: max_per_user.max(1),
                retry_after_secs,
            }),
        }
    }

    /// Free slots right now — glassbox for the `host_busy` log line.
    pub fn available(&self) -> usize {
        self.inner.lock().core.available()
    }

    /// Admit a turn that's willing to wait (the WS/chat path). Grants
    /// immediately if a slot is free and the origin is under its cap;
    /// otherwise enqueues, reports position via `on_position` (now and on
    /// every move up), and awaits its slot. Sheds (never hangs) at the depth
    /// cap. The returned [`TurnPermit`] frees the slot on drop.
    ///
    /// Cancellation-safe: if this future is dropped while queued (client
    /// disconnects), a guard removes the waiter and re-promotes the line.
    pub async fn admit(
        &self,
        key: UserKey,
        weight: f64,
        mut on_position: impl FnMut(QueueStatus),
    ) -> Result<TurnPermit, Shed> {
        // Phase 1: register under the lock. Either granted outright, shed, or
        // enqueued with a Notify + an initial position.
        let (seq, notify, first) = {
            let mut st = self.inner.lock();
            match st.core.admit(key.clone(), weight, self.inner.max_per_user) {
                AdmitOutcome::Granted => return Ok(self.permit(key)),
                AdmitOutcome::Shed { would_be_position } => {
                    return Err(Shed {
                        would_be_position,
                        retry_after_secs: self.inner.retry_after_secs,
                    })
                }
                AdmitOutcome::Enqueued { seq, status } => {
                    let notify = Arc::new(Notify::new());
                    st.notifies.insert(seq, notify.clone());
                    (seq, notify, status)
                }
            }
        };

        // Cancel-safety: drop while queued → remove our waiter + wake the
        // line. Armed until we successfully claim.
        let mut guard = WaiterGuard {
            inner: Arc::clone(&self.inner),
            seq,
            armed: true,
        };

        on_position(first);

        loop {
            notify.notified().await;
            let status = {
                let mut st = self.inner.lock();
                match st.core.claim_or_status(seq) {
                    ClaimOrStatus::Claimed => {
                        st.notifies.remove(&seq);
                        guard.armed = false; // claimed — nothing to clean up
                        return Ok(self.permit(key));
                    }
                    // `Gone` shouldn't happen (only our guard cancels us), but
                    // treat it as a shed rather than spin forever.
                    ClaimOrStatus::Gone => {
                        st.notifies.remove(&seq);
                        guard.armed = false;
                        return Err(Shed {
                            would_be_position: 0,
                            retry_after_secs: self.inner.retry_after_secs,
                        });
                    }
                    ClaimOrStatus::Waiting(status) => status,
                }
            };
            on_position(status);
        }
    }

    /// Non-blocking grant (the REST path — one-shot, never long-polls). Grants
    /// if possible, else returns a [`Shed`] carrying the position the request
    /// *would* have occupied, for the client's retry hint.
    pub fn try_grant(&self, key: UserKey, weight: f64) -> Result<TurnPermit, Shed> {
        let mut st = self.inner.lock();
        match st
            .core
            .try_grant(key.clone(), weight, self.inner.max_per_user)
        {
            TryGrant::Granted => Ok(self.permit(key)),
            TryGrant::WouldQueue { position } => Err(Shed {
                would_be_position: position,
                retry_after_secs: self.inner.retry_after_secs,
            }),
            TryGrant::Shed { would_be_position } => Err(Shed {
                would_be_position,
                retry_after_secs: self.inner.retry_after_secs,
            }),
        }
    }

    fn permit(&self, key: UserKey) -> TurnPermit {
        TurnPermit {
            inner: Arc::clone(&self.inner),
            key: Some(key),
            granted_at: Instant::now(),
        }
    }
}

/// Held for the duration of one turn. Dropping it frees the slot, folds the
/// turn's wall time into the ETA EWMA, and promotes the next waiter. RAII so
/// the slot is released on *every* exit path — return, error, or panic.
pub struct TurnPermit {
    inner: Arc<Inner>,
    /// `Option` so `Drop` can move the key out for `release`.
    key: Option<UserKey>,
    granted_at: Instant,
}

impl std::fmt::Debug for TurnPermit {
    // Manual (not derived): `Inner` holds a `Notify`/`SchedCore` that aren't
    // `Debug`. The key is the only field worth showing in a log anyway.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnPermit")
            .field("key", &self.key)
            .finish()
    }
}

impl Drop for TurnPermit {
    fn drop(&mut self) {
        let dur_ms = self.granted_at.elapsed().as_millis().min(u64::MAX as u128) as u64;
        let Some(key) = self.key.take() else { return };
        let mut st = self.inner.lock();
        st.core.release(&key);
        st.core.record_turn(dur_ms);
        // A slot just freed — wake the line so the promoted waiter claims (and
        // everyone else re-reads their improved position).
        st.wake_all();
    }
}

/// Removes a still-queued waiter when its `admit` future is dropped before
/// claiming (client disconnected mid-queue). Disarmed once the waiter claims.
/// Without this, an abandoned waiter would inflate everyone's position and —
/// if it had been granted — hold a slot hostage.
struct WaiterGuard {
    inner: Arc<Inner>,
    seq: u64,
    armed: bool,
}

impl Drop for WaiterGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut st = self.inner.lock();
        st.notifies.remove(&self.seq);
        st.core.cancel(self.seq);
        // Cancel may have freed a granted-but-unclaimed slot (re-promote ran
        // inside `cancel`); wake the line so the new grant lands.
        st.wake_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tenant(s: &str) -> UserKey {
        UserKey::Tenant(s.to_string())
    }

    #[tokio::test]
    async fn admit_grants_immediately_when_free() {
        let s = FairScheduler::new(2, 1, 32, 2);
        let p = s
            .admit(tenant("a"), 1.0, |_| panic!("should not queue"))
            .await;
        assert!(p.is_ok());
        assert_eq!(s.available(), 1);
    }

    #[tokio::test]
    async fn queued_waiter_is_woken_when_slot_frees() {
        let s = FairScheduler::new(1, 4, 32, 2);
        let held = s.admit(tenant("h"), 1.0, |_| {}).await.expect("granted");
        assert_eq!(s.available(), 0);

        // A waiter that must queue; it reports its first position over a
        // oneshot so the main task can synchronise on "it's enqueued".
        let s2 = s.clone();
        let (tx, rx) = tokio::sync::oneshot::channel::<u32>();
        let mut tx = Some(tx);
        let waiter = tokio::spawn(async move {
            s2.admit(tenant("w"), 1.0, move |status| {
                if let Some(tx) = tx.take() {
                    let _ = tx.send(status.position);
                }
            })
            .await
        });

        assert_eq!(rx.await.unwrap(), 1, "queued at position 1");
        drop(held); // frees the slot → waiter wakes, claims
        let permit = waiter.await.unwrap().expect("eventually granted");
        drop(permit);
        assert_eq!(s.available(), 1);
    }

    #[tokio::test]
    async fn try_grant_sheds_with_position_hint_when_busy() {
        let s = FairScheduler::new(1, 4, 32, 7);
        let _held = s.admit(tenant("h"), 1.0, |_| {}).await.expect("granted");
        let shed = s.try_grant(tenant("r"), 1.0).expect_err("busy → shed");
        assert_eq!(shed.would_be_position, 1);
        assert_eq!(shed.retry_after_secs, 7);
    }

    #[tokio::test]
    async fn cancelled_waiter_frees_the_line() {
        let s = FairScheduler::new(1, 4, 32, 2);
        let held = s.admit(tenant("h"), 1.0, |_| {}).await.expect("granted");
        {
            // Build a queued admit future, poll it once so it enqueues + parks,
            // then drop it — the WaiterGuard must cancel the waiter cleanly.
            let fut = s.admit(tenant("w1"), 1.0, |_| {});
            tokio::pin!(fut);
            let polled = futures::poll!(&mut fut);
            assert!(polled.is_pending(), "queued waiter parks on its notify");
            // `fut` drops at scope end → cancel.
        }
        // The queue is empty again: a fresh REST attempt sees would-be pos 1,
        // not 2 (the cancelled waiter left no residue).
        let shed = s.try_grant(tenant("w2"), 1.0).expect_err("still busy");
        assert_eq!(
            shed.would_be_position, 1,
            "cancelled waiter left no residue"
        );
        drop(held);
    }

    #[tokio::test]
    async fn higher_weight_waiter_is_served_first() {
        // One slot, held. Queue a low-weight then a high-weight waiter; when
        // the slot frees, the high-weight one must be promoted first.
        let s = FairScheduler::new(1, 4, 32, 2);
        let held = s.admit(tenant("h"), 1.0, |_| {}).await.expect("granted");

        let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let spawn_waiter = |label: &'static str, weight: f64| {
            let s = s.clone();
            let order = Arc::clone(&order);
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            let mut tx = Some(tx);
            let h = tokio::spawn(async move {
                let permit = s
                    .admit(UserKey::Tenant(label.to_string()), weight, move |_| {
                        if let Some(tx) = tx.take() {
                            let _ = tx.send(());
                        }
                    })
                    .await
                    .expect("granted");
                order.lock().unwrap().push(label);
                permit
            });
            (h, rx)
        };

        // Enqueue low first, then high — and wait until BOTH have reported a
        // queue position, so both are parked before we free the slot.
        let (low_h, low_rx) = spawn_waiter("low", 1.0);
        low_rx.await.unwrap();
        let (high_h, high_rx) = spawn_waiter("high", 5.0);
        high_rx.await.unwrap();

        // Free the slot: promote serves "high" first. Drop that permit to free
        // the slot again so "low" is served next.
        drop(held);
        let high_permit = high_h.await.unwrap();
        drop(high_permit);
        let _low_permit = low_h.await.unwrap();

        assert_eq!(*order.lock().unwrap(), vec!["high", "low"]);
    }
}
