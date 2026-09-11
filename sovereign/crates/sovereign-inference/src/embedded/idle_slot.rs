// SPDX-License-Identifier: AGPL-3.0-or-later
//! Idle residency for the slots that used to be pinned for the life of
//! the process — `fast` and `embed`.
//!
//! **Why this exists.** The daemon is a mesh node. It has to stay up and
//! reachable for peers even when nobody is touching the desktop app, and
//! "stays up" and "holds tens of GB" are not the same requirement. The
//! `primary` slot has released its weights on an idle timer since the
//! beginning (`EmbeddedLlamaCpp::start_idle_monitor`) and the
//! operator-declared `extras` since the multi-slot campaign
//! (`start_extras_idle_monitor`); `fast` and `embed` were loaded eagerly
//! at boot and never dropped, so an idle node still held the fast model's
//! several GB plus the embedder's few hundred MB. That is the footprint
//! people are actually looking at when they ask to turn the daemon off.
//!
//! **What it does NOT do.** Nothing here exits the process, and nothing
//! here decides a peer is dead. Unloading a slot releases *weights*; the
//! node stays listening and the next request pays a load.
//!
//! **Why a module rather than a third copy of the loop.** The two
//! existing monitors each re-derived "is it idle?" and "is someone using
//! it right now?" inline, with different shapes — `primary` compares an
//! `Instant` under a `Mutex`, `extras` compares epoch millis in an
//! `AtomicU64` and skips any slot whose `Arc` strong count is above one.
//! Adding two more inline copies would make four implementations of one
//! threshold, which is the smell ARCH principle 8 names. [`slot_is_idle`]
//! is the single decider, and [`IdleSlot`] is the cell that owns the
//! load/unload transition for both new slots.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use sovereign_core::error::Error;
use sovereign_core::Result;

pub(crate) use sovereign_core::time::unix_millis as now_millis;

/// How often an idle monitor wakes to look. Matches the cadence the
/// `primary` and `extras` monitors already poll at, so all four slots
/// answer on the same beat and an operator reading the log sees one
/// rhythm rather than three.
pub(crate) const IDLE_SWEEP_INTERVAL_SECS: u64 = 10;

/// THE decider for "has this slot gone idle?" — one implementation, used
/// by every sweep (ARCH principle 8).
///
/// `idle_secs == 0` means the operator disabled unloading for this slot,
/// and is answered here rather than at each call site so "disabled" can
/// never be spelled differently in two places.
///
/// Saturating subtraction is deliberate: a `last_used_ms` in the future
/// (a clock step, or a request that stamped between the sweep's `now` and
/// this call) yields 0 elapsed, i.e. "not idle". Erring toward keeping
/// weights costs memory; erring the other way would yank a model out from
/// under a request that just arrived.
pub(crate) fn slot_is_idle(last_used_ms: u64, now_ms: u64, idle_secs: u64) -> bool {
    if idle_secs == 0 {
        return false;
    }
    now_ms.saturating_sub(last_used_ms) >= idle_secs.saturating_mul(1_000)
}

/// What one sweep decided about one slot.
///
/// Every branch is a value rather than an early `return` so the monitor
/// can trace the decision it actually made — including the boring ones. A
/// decision invisible at `tracing=debug` is not finished (ARCH principle
/// 1), and "the monitor is running but never unloads" is exactly the
/// question an operator needs the log to answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IdleVerdict {
    /// Weights dropped. `bytes` is the footprint captured at load time —
    /// `LlamaModel::size()` for a chat slot, the gguf file size for the
    /// embedder — so the log can say what the unload actually bought.
    Unloaded { bytes: u64 },
    /// Nothing was resident; the slot is already paying nothing.
    AlreadyCold,
    /// Used too recently. `idle_ms` is how long it has actually been.
    NotIdle { idle_ms: u64 },
    /// A request is holding these weights right now. Skipped, not
    /// forced — the next sweep will find it again. Same rule the extras
    /// monitor uses (`Arc::strong_count > 1`).
    Busy,
    /// `idle_secs == 0`: the operator turned unloading off for this slot.
    Disabled,
}

impl IdleVerdict {
    /// Did this sweep actually release memory?
    pub(crate) fn released(&self) -> Option<u64> {
        match self {
            IdleVerdict::Unloaded { bytes } => Some(*bytes),
            _ => None,
        }
    }

    /// Stable label for the tracing field, so a log filter can select one
    /// outcome without parsing prose.
    pub(crate) fn label(&self) -> &'static str {
        match self {
            IdleVerdict::Unloaded { .. } => "unloaded",
            IdleVerdict::AlreadyCold => "already_cold",
            IdleVerdict::NotIdle { .. } => "not_idle",
            IdleVerdict::Busy => "busy",
            IdleVerdict::Disabled => "disabled",
        }
    }
}

/// A slot whose weights may be dropped between requests and re-loaded on
/// the next one.
///
/// The cell owns three things the callers must not re-derive: whether
/// anything is resident, when it was last used, and what it costs. The
/// weights themselves sit behind an async `Mutex` so a reload serialises
/// — two requests arriving at a cold slot must not both load the model —
/// and so a sweep cannot take the cell mid-load.
///
/// `T` is the whole droppable unit, not one handle. For `fast` that is
/// the family (slot + FastShort companion + coalescer) because all three
/// share one `Arc<LlamaModel>`: dropping any one of them alone frees a KV
/// cache and leaves the weights exactly where they were.
pub(crate) struct IdleSlot<T> {
    /// Slot name as it appears in tracing and in `/status`.
    name: &'static str,
    /// The weights. `None` = cold.
    cell: tokio::sync::Mutex<Option<Arc<T>>>,
    /// Epoch millis of the last successful [`IdleSlot::acquire`]. Same
    /// unit the extras monitor uses, so the two are comparable in a log.
    last_used_ms: AtomicU64,
    /// Footprint of the resident copy, captured by the loader. 0 when
    /// cold or when the loader could not price it.
    size_bytes: AtomicU64,
    /// Lifetime counters, surfaced on every reload so a pathological
    /// thrash (idle window shorter than the load, the trap note
    /// 419e273c recorded for `primary_idle_secs=60`) is visible in the
    /// log rather than inferred from latency.
    loads: AtomicU64,
    unloads: AtomicU64,
}

impl<T: Send + Sync + 'static> IdleSlot<T> {
    /// A cell that starts out holding weights — the eager boot load.
    pub(crate) fn resident(name: &'static str, value: Arc<T>, size_bytes: u64) -> Self {
        Self {
            name,
            cell: tokio::sync::Mutex::new(Some(value)),
            last_used_ms: AtomicU64::new(now_millis()),
            size_bytes: AtomicU64::new(size_bytes),
            loads: AtomicU64::new(1),
            unloads: AtomicU64::new(0),
        }
    }

    /// A cell that starts cold, to be filled by the first [`acquire`].
    ///
    /// [`acquire`]: IdleSlot::acquire
    #[cfg(test)]
    pub(crate) fn cold(name: &'static str) -> Self {
        Self {
            name,
            cell: tokio::sync::Mutex::new(None),
            last_used_ms: AtomicU64::new(now_millis()),
            size_bytes: AtomicU64::new(0),
            loads: AtomicU64::new(0),
            unloads: AtomicU64::new(0),
        }
    }

    pub(crate) fn name(&self) -> &'static str {
        self.name
    }

    /// Is anything resident right now?
    ///
    /// Sync and lock-free-ish: takes the cell only if it is uncontended,
    /// and reports `true` under contention because a contended cell means
    /// a request or a load is in progress, which is residency in every
    /// sense `/status` cares about. Callers that need the weights must
    /// use [`IdleSlot::acquire`].
    pub(crate) fn is_resident(&self) -> bool {
        match self.cell.try_lock() {
            Ok(g) => g.is_some(),
            Err(_) => true,
        }
    }

    /// The resident weights if they are loaded right now, without
    /// loading them if they are not.
    ///
    /// For the sync accessors — `/status`, the token counter, the
    /// FastShort busy probe — that must answer from a non-async context
    /// and have a defined answer when the slot is cold. Returns `None`
    /// under contention too: a contended cell means a load or an unload
    /// is mid-flight, and neither is a moment to hand out a handle.
    pub(crate) fn try_resident(&self) -> Option<Arc<T>> {
        self.cell.try_lock().ok()?.as_ref().map(Arc::clone)
    }

    /// Bytes the resident copy costs, or `None` when cold.
    pub(crate) fn resident_bytes(&self) -> Option<u64> {
        if !self.is_resident() {
            return None;
        }
        match self.size_bytes.load(Ordering::Relaxed) {
            0 => None,
            b => Some(b),
        }
    }

    /// Hand out the weights, loading them first if the slot is cold.
    ///
    /// THE ONE PLACE a caller obtains these weights — the reload is not
    /// optional and not the caller's business, which is what makes
    /// "a request arriving after an unload is served" structural rather
    /// than remembered (ARCH principle 10).
    ///
    /// `load` runs on the blocking pool: a model load is seconds to
    /// minutes of FFI, and the note on cold `primary` loads measured
    /// 15-95s depending on page cache. The cell lock is held across it so
    /// concurrent arrivals queue rather than each loading their own copy.
    pub(crate) async fn acquire<F>(&self, why: &'static str, load: F) -> Result<Arc<T>>
    where
        F: FnOnce() -> Result<(T, u64)> + Send + 'static,
    {
        let mut guard = self.cell.lock().await;
        if let Some(existing) = guard.as_ref() {
            self.last_used_ms.store(now_millis(), Ordering::Relaxed);
            return Ok(Arc::clone(existing));
        }

        let started = Instant::now();
        tracing::info!(
            slot = self.name,
            why,
            reload_count = self.loads.load(Ordering::Relaxed),
            "idle slot is cold — loading on demand"
        );
        let (value, size_bytes) = tokio::task::spawn_blocking(load)
            .await
            .map_err(|e| Error::Inference(format!("{} slot load task failed: {e}", self.name)))??;

        let value = Arc::new(value);
        *guard = Some(Arc::clone(&value));
        self.size_bytes.store(size_bytes, Ordering::Relaxed);
        self.last_used_ms.store(now_millis(), Ordering::Relaxed);
        let loads = self.loads.fetch_add(1, Ordering::Relaxed) + 1;
        tracing::info!(
            slot = self.name,
            why,
            latency_ms = started.elapsed().as_millis() as u64,
            size_bytes,
            loads,
            unloads = self.unloads.load(Ordering::Relaxed),
            "idle slot reloaded — request proceeds"
        );
        Ok(value)
    }

    /// One sweep. Drops the weights when the slot has been idle past
    /// `idle_secs` and nothing else is holding them.
    ///
    /// `now_ms` is a parameter rather than read inside so the decision is
    /// testable without sleeping.
    pub(crate) async fn sweep(&self, idle_secs: u64, now_ms: u64) -> IdleVerdict {
        if idle_secs == 0 {
            return IdleVerdict::Disabled;
        }
        let last_used = self.last_used_ms.load(Ordering::Relaxed);
        if !slot_is_idle(last_used, now_ms, idle_secs) {
            return IdleVerdict::NotIdle {
                idle_ms: now_ms.saturating_sub(last_used),
            };
        }

        let mut guard = self.cell.lock().await;
        let Some(resident) = guard.as_ref() else {
            return IdleVerdict::AlreadyCold;
        };
        // Someone is mid-request with these weights. Skipping is the same
        // rule `start_extras_idle_monitor` applies — RAII, never a forced
        // drop out from under an in-flight decode.
        if Arc::strong_count(resident) > 1 {
            return IdleVerdict::Busy;
        }

        let bytes = self.size_bytes.swap(0, Ordering::Relaxed);
        *guard = None;
        self.unloads.fetch_add(1, Ordering::Relaxed);
        IdleVerdict::Unloaded { bytes }
    }

    /// Drop the weights regardless of idleness, if nothing holds them.
    /// Used by the operator-facing paths that already know the slot
    /// should go (reconfiguration), not by the monitors.
    #[cfg(test)]
    pub(crate) async fn force_unload(&self) -> IdleVerdict {
        self.sweep(1, u64::MAX).await
    }
}

/// Spawn the background monitor for one [`IdleSlot`].
///
/// Mirrors `start_extras_idle_monitor`: `idle_secs == 0` does not spawn a
/// task at all, so an operator who disabled the monitor pays nothing and
/// the absence is stated once at boot rather than implied by silence.
pub(crate) fn spawn_idle_monitor<T: Send + Sync + 'static>(slot: Arc<IdleSlot<T>>, idle_secs: u64) {
    if idle_secs == 0 {
        tracing::info!(
            slot = slot.name(),
            "idle monitor disabled (idle_secs=0) — weights stay resident for the life of \
             the process"
        );
        return;
    }
    tokio::spawn(async move {
        tracing::info!(
            slot = slot.name(),
            idle_secs,
            sweep_interval_secs = IDLE_SWEEP_INTERVAL_SECS,
            "idle monitor started"
        );
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(IDLE_SWEEP_INTERVAL_SECS)).await;
            let verdict = slot.sweep(idle_secs, now_millis()).await;
            match verdict.released() {
                Some(bytes) => tracing::info!(
                    slot = slot.name(),
                    verdict = verdict.label(),
                    idle_secs,
                    bytes_released = bytes,
                    "idle-unload: weights dropped, slot stays available and will reload on \
                     the next request"
                ),
                None => tracing::debug!(
                    slot = slot.name(),
                    verdict = verdict.label(),
                    idle_secs,
                    "idle sweep: no unload"
                ),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    // ── the decider ──────────────────────────────────────────────────

    #[test]
    fn zero_means_disabled_not_immediate() {
        // The trap this guards: reading `idle_secs == 0` as "0 seconds of
        // grace" would unload on the first sweep. Both existing monitors
        // spell 0 as "off"; so does this.
        assert!(!slot_is_idle(0, u64::MAX, 0));
    }

    #[test]
    fn idle_at_the_threshold_not_before() {
        let last = 1_000_000u64;
        // 59s idle, 60s threshold: not yet.
        assert!(!slot_is_idle(last, last + 59_000, 60));
        // Exactly 60s: yes. The boundary is inclusive.
        assert!(slot_is_idle(last, last + 60_000, 60));
        assert!(slot_is_idle(last, last + 60_001, 60));
    }

    #[test]
    fn a_clock_step_backward_never_unloads() {
        // `now` behind `last_used` saturates to 0 elapsed rather than
        // wrapping to ~584 million years, which would unload a slot that
        // was used one millisecond ago.
        assert!(!slot_is_idle(1_000_000, 999_000, 60));
    }

    // ── the cell ─────────────────────────────────────────────────────

    /// Stand-in for a model: carries the build number that produced it,
    /// so a test can tell a reload from a cache hit. No gguf, no FFI —
    /// the residency transition is what is under test, and it is the
    /// same transition for a 9B chat model and for this.
    struct Weights {
        generation: usize,
    }

    /// Per-test build counter. Deliberately NOT a `static`: the test
    /// binary runs these concurrently, and a shared counter made four of
    /// them read each other's loads (observed: `left: 3, right: 1`).
    #[derive(Clone)]
    struct Builder(Arc<AtomicUsize>);

    impl Builder {
        fn new() -> Self {
            Self(Arc::new(AtomicUsize::new(0)))
        }
        fn builds(&self) -> usize {
            self.0.load(Ordering::SeqCst)
        }
        /// A loader closure for [`IdleSlot::acquire`].
        fn load(&self) -> impl FnOnce() -> Result<(Weights, u64)> + Send + 'static {
            let counter = Arc::clone(&self.0);
            move || {
                let generation = counter.fetch_add(1, Ordering::SeqCst) + 1;
                Ok((Weights { generation }, 4_096))
            }
        }
    }

    #[tokio::test]
    async fn request_after_unload_reloads_and_is_served() {
        // THE load-bearing property: an unload must be invisible to the
        // next caller except in latency. Anything less turns "the node
        // stays available" into a lie the monitor tells.
        let b = Builder::new();
        let slot = IdleSlot::<Weights>::cold("test-fast");

        // First request on a cold slot loads.
        let first = slot.acquire("first-request", b.load()).await.unwrap();
        assert_eq!(first.generation, 1);
        assert!(slot.is_resident());
        assert_eq!(slot.resident_bytes(), Some(4_096));
        drop(first);

        // Idle long enough: the sweep drops the weights and says what it
        // released.
        let verdict = slot.sweep(60, now_millis() + 61_000).await;
        assert_eq!(verdict, IdleVerdict::Unloaded { bytes: 4_096 });
        assert!(!slot.is_resident());
        assert_eq!(slot.resident_bytes(), None);

        // The next request is served anyway — that is the whole contract.
        let second = slot.acquire("post-unload-request", b.load()).await.unwrap();
        assert_eq!(
            second.generation, 2,
            "a request after an unload must get freshly loaded weights, not a stale handle"
        );
        assert!(slot.is_resident());
        assert_eq!(slot.resident_bytes(), Some(4_096));
        assert_eq!(b.builds(), 2, "exactly one reload");
    }

    #[tokio::test]
    async fn a_slot_in_use_is_never_yanked() {
        let b = Builder::new();
        let slot = IdleSlot::<Weights>::cold("test-busy");
        // A caller is holding the weights — an in-flight decode.
        let inflight = slot.acquire("request", b.load()).await.unwrap();

        let verdict = slot.sweep(60, now_millis() + 61_000).await;
        assert_eq!(verdict, IdleVerdict::Busy);
        assert!(slot.is_resident(), "busy slot must survive the sweep");

        // Once the request finishes, the next sweep takes it.
        drop(inflight);
        let verdict = slot.sweep(60, now_millis() + 61_000).await;
        assert_eq!(verdict, IdleVerdict::Unloaded { bytes: 4_096 });
    }

    #[tokio::test]
    async fn a_recent_request_keeps_the_slot() {
        let b = Builder::new();
        let slot = IdleSlot::<Weights>::cold("test-recent");
        let handle = slot.acquire("request", b.load()).await.unwrap();
        drop(handle);

        // 5s into a 60s window.
        let verdict = slot.sweep(60, now_millis() + 5_000).await;
        assert!(matches!(verdict, IdleVerdict::NotIdle { .. }));
        assert!(slot.is_resident());
        assert_eq!(b.builds(), 1, "no reload happened");
    }

    #[tokio::test]
    async fn acquire_refreshes_the_idle_clock() {
        // A slot answering a steady trickle of requests must never be
        // unloaded, however long the daemon has been up. This is the
        // regression the `primary_idle_secs=60` trap note describes from
        // the other side.
        let b = Builder::new();
        let slot = IdleSlot::<Weights>::cold("test-refresh");
        let _ = slot.acquire("request", b.load()).await.unwrap();
        let long_after = now_millis() + 10 * 60_000;

        // Without a touch it would be idle...
        assert!(slot_is_idle(
            slot.last_used_ms.load(Ordering::Relaxed),
            long_after,
            60
        ));
        // ...but a fresh request re-stamps it.
        let _ = slot.acquire("request", b.load()).await.unwrap();
        assert!(!slot_is_idle(
            slot.last_used_ms.load(Ordering::Relaxed),
            now_millis() + 1_000,
            60
        ));
        assert_eq!(b.builds(), 1, "cache hit, no reload");
    }

    #[tokio::test]
    async fn sweeping_a_cold_slot_is_a_no_op() {
        let slot = IdleSlot::<Weights>::cold("test-cold");
        assert_eq!(
            slot.sweep(60, now_millis() + 61_000).await,
            IdleVerdict::AlreadyCold
        );
        assert_eq!(slot.sweep(0, u64::MAX).await, IdleVerdict::Disabled);
    }

    #[tokio::test]
    async fn disabled_never_unloads_a_resident_slot() {
        let b = Builder::new();
        let slot = IdleSlot::<Weights>::cold("test-disabled");
        let _ = slot.acquire("request", b.load()).await.unwrap();
        assert_eq!(slot.sweep(0, u64::MAX).await, IdleVerdict::Disabled);
        assert!(slot.is_resident());
    }

    #[tokio::test]
    async fn a_failed_reload_leaves_the_slot_cold_and_reports_it() {
        // Never silently substitute (ARCH principle 6): a load that fails
        // must surface as an error to the caller, not as an empty-but-ok
        // slot that makes the next reader think the model is resident.
        let slot = IdleSlot::<Weights>::cold("test-failing");
        let err = slot
            .acquire("request", || {
                Err(Error::Inference("gguf vanished".to_string()))
            })
            .await;
        assert!(err.is_err());
        assert!(!slot.is_resident());
        assert_eq!(slot.resident_bytes(), None);

        // And a later good load still works.
        let b = Builder::new();
        let ok = slot.acquire("retry", b.load()).await.unwrap();
        assert_eq!(ok.generation, 1);
    }

    #[tokio::test]
    async fn concurrent_arrivals_at_a_cold_slot_load_once() {
        // Two peers hitting an idle node at the same instant must not
        // each load their own copy of the model — that is the OOM the
        // whole always-on posture is trying to avoid.
        let b = Builder::new();
        let slot = Arc::new(IdleSlot::<Weights>::cold("test-race"));
        let (sa, sb) = (Arc::clone(&slot), Arc::clone(&slot));
        let (la, lb) = (b.load(), b.load());
        let (ra, rb) = tokio::join!(
            async move { sa.acquire("peer-a", la).await.unwrap().generation },
            async move { sb.acquire("peer-b", lb).await.unwrap().generation },
        );
        assert_eq!(b.builds(), 1, "exactly one load");
        assert_eq!(ra, 1);
        assert_eq!(rb, 1);
    }

    #[tokio::test]
    async fn force_unload_drops_an_idle_slot_immediately() {
        let b = Builder::new();
        let slot = IdleSlot::<Weights>::cold("test-force");
        let _ = slot.acquire("request", b.load()).await.unwrap();
        assert_eq!(
            slot.force_unload().await,
            IdleVerdict::Unloaded { bytes: 4_096 }
        );
        assert!(!slot.is_resident());
    }
}
