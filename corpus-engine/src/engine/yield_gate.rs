// SPDX-License-Identifier: AGPL-3.0-or-later
//! The bounded wait every ingest yield checkpoint parks in.
//!
//! `corpus-engine` has two places where a long-running pipeline gives way to
//! foreground inference: before each embed batch, and before the enrichment
//! phase. Both used to spell the wait inline as
//!
//! ```ignore
//! while hook.should_yield() { …; sleep(5s).await }
//! ```
//!
//! which has no deadline, no cap and no escape. `should_yield()` is a *level*
//! predicate over "seconds since the last foreground request", so any request
//! cadence shorter than the yield window pins it true forever. On 2026-08-18 a
//! 30-second liveness probe from `sovereign-server`'s hybrid health loop did
//! exactly that against the 60-second default window and enrichment never
//! started across three consecutive runs
//! (`runs/sec-filings-ship-e2e/evidence/real-daemon.log`; `resuming enrichment`
//! appears zero times). A person chatting every 30 seconds starves it the same
//! way.
//!
//! So the wait lives here instead, once, with the bound
//! ([`DeferralBudget`], `corpus-engine-yield`) built in rather than remembered.
//! Two things are deliberately inside this function rather than at the call
//! sites:
//!
//! * **The override event.** A cap nobody can see fire is not a cap. Emitting
//!   the `warn!` here means no checkpoint can adopt the bound and forget to
//!   announce it (ARCH §9.1).
//! * **The exit reason.** [`YieldExit`] is an enum, not a bool, so the
//!   "resuming — foreground idle" line cannot be printed after an exit that
//!   was nothing of the sort (ARCH §18.3: never report a substitution as the
//!   real thing).
//!
//! What stays at the call site is what genuinely differs: the progress
//! callback the embed checkpoint fires, and how each caller turns a
//! [`YieldExit::Cancelled`] into its own error.

use std::time::Duration;

use corpus_engine_yield::{DeferralBudget, DeferralStep, YieldHook};

/// How often a parked checkpoint re-polls the hook.
///
/// One value for every checkpoint (ARCH §10.6). 5s is short enough that work
/// restarts within a few seconds of the user going idle, and long enough that
/// the poll itself costs nothing next to the multi-second GPU batches it is
/// standing aside for.
pub(crate) const YIELD_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Why a bounded yield wait ended. Every arm is a different sentence in the
/// log, which is the point of it being an enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum YieldExit {
    /// The foreground was already idle. We never parked.
    NotDeferred,
    /// We parked, and the foreground went idle within the budget. The polite
    /// exit.
    ForegroundIdle,
    /// We parked until the budget ran out and the foreground was **still**
    /// active. We are proceeding anyway; a `warn!` naming the override and the
    /// elapsed deferral has already been emitted.
    CapReached,
    /// The caller's cancellation flag fired while we were parked.
    Cancelled,
}

/// Park while `hook` asks us to yield, for at most `budget`.
///
/// `checkpoint` names the call site in the log (`"embed"`, `"enrichment"`) so
/// the three events below are attributable without a stack trace.
/// `on_first_defer` runs once, the first time we actually park — the embed
/// checkpoint uses it to publish a progress row so the UI does not look hung.
pub(crate) async fn defer_to_foreground<C, A>(
    hook: &dyn YieldHook,
    corpus_id: &str,
    checkpoint: &'static str,
    budget: DeferralBudget,
    poll: Duration,
    is_cancelled: C,
    mut on_first_defer: A,
) -> YieldExit
where
    C: Fn() -> bool,
    A: FnMut(),
{
    let mut parked = false;
    loop {
        match budget.step(hook) {
            DeferralStep::Proceed => {
                if !parked {
                    return YieldExit::NotDeferred;
                }
                tracing::info!(
                    corpus = %corpus_id,
                    checkpoint,
                    deferred_secs = budget.waited().as_secs(),
                    "yield: resuming — foreground idle"
                );
                return YieldExit::ForegroundIdle;
            }
            DeferralStep::CapReached { waited } => {
                // WARN, not INFO: this is the liveness bound firing, which
                // means the foreground is still busy and is about to contend
                // with us. Someone reading the log after a slow chat turn
                // needs to find this line.
                tracing::warn!(
                    corpus = %corpus_id,
                    checkpoint,
                    deferred_secs = waited.as_secs(),
                    cap_secs = budget.cap().as_secs(),
                    "yield: deferral cap reached — proceeding despite foreground inference"
                );
                return YieldExit::CapReached;
            }
            DeferralStep::Defer { .. } => {
                if is_cancelled() {
                    return YieldExit::Cancelled;
                }
                if !parked {
                    parked = true;
                    tracing::info!(
                        corpus = %corpus_id,
                        checkpoint,
                        cap_secs = budget.cap().as_secs(),
                        "yield: deferring for foreground inference"
                    );
                    on_first_defer();
                }
                tokio::time::sleep(poll).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A hook whose answer the test controls.
    struct Switch(Arc<AtomicBool>);
    impl YieldHook for Switch {
        fn should_yield(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }

    fn switch(on: bool) -> (Switch, Arc<AtomicBool>) {
        let flag = Arc::new(AtomicBool::new(on));
        (Switch(flag.clone()), flag)
    }

    /// Capture what the events really render to. Same shape as the sec_facts
    /// instrument test — a rendered event, not a string the test composed.
    #[derive(Clone, Default)]
    struct CaptureWriter(Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for CaptureWriter {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
        type Writer = CaptureWriter;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    fn capture() -> (CaptureWriter, tracing::subscriber::DefaultGuard) {
        let buf = CaptureWriter::default();
        let sub = tracing_subscriber::fmt()
            .with_writer(buf.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::DEBUG)
            .finish();
        let guard = tracing::subscriber::set_default(sub);
        (buf, guard)
    }

    fn rendered(buf: &CaptureWriter) -> String {
        String::from_utf8(buf.0.lock().unwrap().clone()).unwrap()
    }

    #[tokio::test]
    async fn idle_foreground_is_not_a_deferral() {
        let (hook, _) = switch(false);
        let exit = defer_to_foreground(
            &hook,
            "c",
            "enrichment",
            DeferralBudget::new(),
            Duration::from_millis(1),
            || false,
            || panic!("must not park when the foreground is idle"),
        )
        .await;
        assert_eq!(exit, YieldExit::NotDeferred);
    }

    /// THE BOUND, WATCHED FIRING. A hook that says "yield" forever — exactly
    /// the 30s-probe-vs-60s-window condition, with the arithmetic removed —
    /// and the loop still exits, with the override event on the wire carrying
    /// the elapsed deferral.
    #[tokio::test]
    async fn cap_fires_against_a_foreground_that_never_goes_idle() {
        let (buf, _guard) = capture();
        let (hook, flag) = switch(true);

        // The cap is 1.1s rather than something instant so `deferred_secs`
        // carries a REAL elapsed value in the rendered event. A cap of 120ms
        // fires just as well but logs `deferred_secs=0`, and a field that only
        // ever renders zero has not been shown to work.
        let exit = defer_to_foreground(
            &hook,
            "folder-corpus-2918e9ebc0b5",
            "enrichment",
            DeferralBudget::with_cap_at_most(Duration::from_millis(1100)),
            Duration::from_millis(50),
            || false,
            || {},
        )
        .await;

        // The foreground never relented — that is the premise, not an
        // accident of timing.
        assert!(flag.load(Ordering::SeqCst));
        assert_eq!(exit, YieldExit::CapReached);

        let out = rendered(&buf);
        assert!(
            out.contains("yield: deferral cap reached"),
            "the override must be announced; got:\n{out}"
        );
        assert!(
            out.contains("deferred_secs=1"),
            "the override must carry the REAL elapsed deferral, not a zero; \
             got:\n{out}"
        );
        assert!(
            out.contains("cap_secs="),
            "the override must name the bound it hit; got:\n{out}"
        );
        assert!(
            out.contains("checkpoint=\"enrichment\""),
            "the override must name which checkpoint overrode; got:\n{out}"
        );
        assert!(
            !out.contains("foreground idle"),
            "a capped exit must never claim the foreground went idle; got:\n{out}"
        );
    }

    /// The negative arm of the same gate: when the foreground DOES go idle
    /// inside the budget we take the polite exit and emit no override.
    #[tokio::test]
    async fn foreground_going_idle_exits_politely_and_emits_no_override() {
        let (buf, _guard) = capture();
        let (hook, flag) = switch(true);

        let releaser = flag.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            releaser.store(false, Ordering::SeqCst);
        });

        let exit = defer_to_foreground(
            &hook,
            "c",
            "embed",
            DeferralBudget::with_cap_at_most(Duration::from_secs(30)),
            Duration::from_millis(5),
            || false,
            || {},
        )
        .await;

        assert_eq!(exit, YieldExit::ForegroundIdle);
        let out = rendered(&buf);
        assert!(out.contains("yield: resuming — foreground idle"), "{out}");
        assert!(
            !out.contains("deferral cap reached"),
            "no override may be reported when the wait ended politely; got:\n{out}"
        );
    }

    #[tokio::test]
    async fn first_defer_side_effect_runs_exactly_once() {
        let (hook, flag) = switch(true);
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();

        let releaser = flag.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(40)).await;
            releaser.store(false, Ordering::SeqCst);
        });

        let exit = defer_to_foreground(
            &hook,
            "c",
            "embed",
            DeferralBudget::with_cap_at_most(Duration::from_secs(30)),
            Duration::from_millis(5),
            || false,
            move || {
                counter.fetch_add(1, Ordering::SeqCst);
            },
        )
        .await;

        assert_eq!(exit, YieldExit::ForegroundIdle);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cancel_beats_yield() {
        let (hook, _flag) = switch(true);
        let exit = defer_to_foreground(
            &hook,
            "c",
            "enrichment",
            DeferralBudget::new(),
            Duration::from_millis(5),
            || true,
            || {},
        )
        .await;
        assert_eq!(exit, YieldExit::Cancelled);
    }

    /// A cancel that arrives while parked still wins — the flag is not only
    /// read on entry.
    #[tokio::test]
    async fn cancel_arriving_mid_wait_still_wins() {
        let (hook, _flag) = switch(true);
        let cancelled = Arc::new(AtomicBool::new(false));
        let setter = cancelled.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            setter.store(true, Ordering::SeqCst);
        });

        let probe = cancelled.clone();
        let exit = defer_to_foreground(
            &hook,
            "c",
            "enrichment",
            DeferralBudget::with_cap_at_most(Duration::from_secs(30)),
            Duration::from_millis(5),
            move || probe.load(Ordering::SeqCst),
            || {},
        )
        .await;
        assert_eq!(exit, YieldExit::Cancelled);
    }
}
