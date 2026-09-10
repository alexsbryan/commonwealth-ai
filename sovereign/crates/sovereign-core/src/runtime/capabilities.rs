// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a turn MAY DO — the per-turn capability, starting with the one that
//! had to move first.
//!
//! `quality/TOPOLOGY.md §3.5` draws `tools · skill · approval` as
//! Capabilities: "a VALUE BUILT PER TURN and passed down — not a member the
//! Runtime holds and stages reach back into". Approval is the one that could
//! not wait, because a daemon serving several sockets on ONE `Runtime` has a
//! process-wide `approval` member and no way to tell whose turn is asking. The
//! shipped answer was `AutoApprovalChannel` — every write-effectful step
//! granted, silently, including under an interactive client that would have
//! raised a consent card (TOPOLOGY hazard 12).
//!
//! # The carrier already existed
//!
//! [`crate::runtime::admission`] is the same shape: a `tokio::task_local!`
//! installed around the whole turn body by `turn_lease.rs`, which is the ONE
//! site every turn entry goes through. Its `stamp()` is also the answer to the
//! objection that sank an earlier design — a task-local does not reach a
//! `tokio::spawn`ed child, so the ambient value is READ while it is still
//! ambient and carried into the child as a value. Every site that builds an
//! `Executor` already reads `self.approval` in the turn's own task, before its
//! spawn, so [`Runtime::turn_approval`] slots in exactly there.
//!
//! # Nobody's signature changes
//!
//! The scope is installed by whoever STARTS the turn, not threaded through it.
//! A host that installs none — the desktop in-process, the server, `svrn chat`
//! — reads its own commissioned channel exactly as before; only the daemon's
//! turn socket installs one, and only for its own turn. That is what makes
//! this behaviour-preserving everywhere it is not the point.

use std::future::Future;
use std::sync::Arc;

use crate::traits::{ApprovalChannel, RoutingEventSink};

tokio::task_local! {
    /// The approval channel of the turn running on this task, installed by
    /// [`scope`]. Absent outside a scoped turn — which is every in-process
    /// host, deliberately.
    static TURN_APPROVAL: Arc<dyn ApprovalChannel>;
    /// The routing-event sink of the turn running on this task, installed
    /// by [`scope_routing_events`] (sv-surface G7). Same bargain as
    /// approval: the process-wide `routing_events` member cannot tell
    /// which socket's conversation a banner or a narration belongs to, and
    /// a broadcast bridge would need a conversation→socket registry to
    /// work out what the turn already knows.
    static TURN_ROUTING_EVENTS: Arc<dyn RoutingEventSink>;
}

/// Run `fut` as a turn whose consent questions go to `approval`.
///
/// `None` runs `fut` unchanged, so a caller with nobody to ask does not have
/// to branch. Wrap the WHOLE turn — acquire and drain both: the executor is
/// built during the acquire, and a scope that ended at the handle would leave
/// the turn's own steps reading the host's channel instead.
///
/// The scoped future is BOXED. A `task_local` scope stores the inner future
/// inline, so in a debug build the turn's state machine is held twice and a
/// `serve_turn` future is large enough for that to overflow the stack —
/// measured, as two `turn_surface` tests aborting with SIGABRT on the first
/// run of this seam. One heap allocation per scoped turn is nothing beside a
/// turn, and putting it here rather than at the call site means the next
/// caller does not have to rediscover it.
pub async fn scope<F: Future>(approval: Option<Arc<dyn ApprovalChannel>>, fut: F) -> F::Output {
    match approval {
        Some(a) => TURN_APPROVAL.scope(a, Box::pin(fut)).await,
        None => fut.await,
    }
}

/// Run `fut` as a turn whose routing events go to `sink` — the G7 twin of
/// [`scope`]. Same boxing reason, same "wrap the WHOLE turn" requirement:
/// the router's interpretation banner and clarification card fire during
/// the acquire, before any stream handle exists.
pub async fn scope_routing_events<F: Future>(
    sink: Option<Arc<dyn RoutingEventSink>>,
    fut: F,
) -> F::Output {
    match sink {
        Some(s) => TURN_ROUTING_EVENTS.scope(s, Box::pin(fut)).await,
        None => fut.await,
    }
}

/// Run `fut` as a turn with BOTH capabilities installed — what a host that
/// scopes the whole turn surface calls (the daemon's three turn arms).
///
/// Boxes the future ONCE, not per scope: nesting [`scope`] inside
/// [`scope_routing_events`] constructs the inner state machine on the
/// caller's stack before the outer scope boxes it, and a debug-built
/// `serve_turn` is large enough that the doubled construction overflows —
/// measured as `turn_surface` SIGABRTs the day the second scope landed.
/// One `Box::pin`, both task-locals around it, the invariant [`scope`]'s
/// own doc states kept whole.
pub async fn scope_turn<F: Future>(
    approval: Option<Arc<dyn ApprovalChannel>>,
    routing: Option<Arc<dyn RoutingEventSink>>,
    fut: F,
) -> F::Output {
    let fut = Box::pin(fut);
    match (approval, routing) {
        (Some(a), Some(r)) => {
            TURN_APPROVAL
                .scope(a, TURN_ROUTING_EVENTS.scope(r, fut))
                .await
        }
        (Some(a), None) => TURN_APPROVAL.scope(a, fut).await,
        (None, Some(r)) => TURN_ROUTING_EVENTS.scope(r, fut).await,
        (None, None) => fut.await,
    }
}

/// The ambient turn approval channel, if this task is running inside a scoped
/// turn.
fn current() -> Option<Arc<dyn ApprovalChannel>> {
    TURN_APPROVAL.try_with(Arc::clone).ok()
}

fn current_routing_events() -> Option<Arc<dyn RoutingEventSink>> {
    TURN_ROUTING_EVENTS.try_with(Arc::clone).ok()
}

impl super::Runtime {
    /// The channel THIS turn's consent questions go to.
    ///
    /// The one place that decides it: the turn's own channel when the host
    /// installed one, else the channel this `Runtime` was commissioned with.
    /// Every `Executor` construction reads it here rather than reaching for
    /// `self.approval`, so there is one answer to "who is being asked" and it
    /// cannot drift between the four sites that used to each clone the field
    /// (ARCH §10.6).
    ///
    /// Read in the turn's task, BEFORE any `tokio::spawn` that carries the
    /// result — see the module docs.
    pub(crate) fn turn_approval(&self) -> Arc<dyn ApprovalChannel> {
        current().unwrap_or_else(|| Arc::clone(&self.approval))
    }

    /// The sink THIS turn's routing events go to — the G7 twin of
    /// [`Self::turn_approval`]. Same fallback (the commissioned member),
    /// same read-before-spawn contract; `conation.rs`'s detached
    /// lesson-capture reads it beside its approval read so the
    /// post-turn narration survives the spawn.
    pub(crate) fn turn_routing_events(&self) -> Arc<dyn RoutingEventSink> {
        current_routing_events().unwrap_or_else(|| Arc::clone(&self.routing_events))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::AutoApprovalChannel;

    fn channel() -> Arc<dyn ApprovalChannel> {
        Arc::new(AutoApprovalChannel)
    }

    /// Two calls give two distinct allocations, so `Arc::ptr_eq` can tell an
    /// installed sink from any other.
    fn sink() -> Arc<dyn RoutingEventSink> {
        Arc::new(crate::traits::NoOpRoutingEventSink)
    }

    #[tokio::test]
    async fn a_scoped_turn_sees_its_channel_and_an_unscoped_one_sees_none() {
        assert!(current().is_none(), "no scope, no ambient channel");
        let installed = channel();
        let seen = scope(Some(Arc::clone(&installed)), async { current() }).await;
        assert!(
            Arc::ptr_eq(&seen.expect("inside the scope"), &installed),
            "the turn reads the channel its host installed, not another"
        );
        assert!(current().is_none(), "the scope ends with the turn");
    }

    /// The property the module docs turn on, and the one an earlier design
    /// read backwards: a `tokio::spawn`ed child does NOT inherit the scope, so
    /// the value has to be read while it is still ambient and carried in. Both
    /// halves are asserted, because only the pair explains why every
    /// `Executor` site reads before its spawn.
    #[tokio::test]
    async fn the_ambient_channel_must_be_read_before_the_spawn_not_inside_it() {
        let installed = channel();
        let (carried, found_inside) = scope(Some(Arc::clone(&installed)), async {
            // Read HERE — the shape `Runtime::turn_approval` is called in.
            let carried = current().expect("ambient in the turn's own task");
            let found_inside = tokio::spawn(async { current().is_some() }).await.unwrap();
            (carried, found_inside)
        })
        .await;

        assert!(
            Arc::ptr_eq(&carried, &installed),
            "the value read before the spawn is the turn's own"
        );
        assert!(
            !found_inside,
            "a spawned child inherits NO scope — reading inside it would \
             silently fall back to the host's channel"
        );
    }

    /// The G7 twin of the test above. Written because `scope_routing_events`
    /// shipped untested: the sink is the half that is SILENT when it is
    /// wrong — a lost clarification card looks like a model that had nothing
    /// to ask, not like a dropped event.
    #[tokio::test]
    async fn a_routing_scoped_turn_sees_its_sink_and_an_unscoped_one_sees_none() {
        assert!(
            current_routing_events().is_none(),
            "no scope, no ambient sink"
        );
        let installed = sink();
        let seen = scope_routing_events(Some(Arc::clone(&installed)), async {
            current_routing_events()
        })
        .await;
        assert!(
            Arc::ptr_eq(&seen.expect("inside the scope"), &installed),
            "the turn reads the sink its host installed, not another"
        );
        assert!(
            current_routing_events().is_none(),
            "the scope ends with the turn"
        );
    }

    /// The read-before-spawn property for the sink. `Runtime::turn_routing_events`
    /// is called at ~20 sites that hand the result to a `tokio::spawn`ed
    /// child — `streaming.rs`'s gate-progress reader, `conation.rs`'s
    /// lesson capture. Every one of them is only correct because the read
    /// happens in the turn's own task; reading inside the child would fall
    /// back to the commissioned member, which on the daemon is the no-op sink.
    #[tokio::test]
    async fn the_ambient_sink_must_be_read_before_the_spawn_not_inside_it() {
        let installed = sink();
        let (carried, found_inside) = scope_routing_events(Some(Arc::clone(&installed)), async {
            let carried = current_routing_events().expect("ambient in the turn's own task");
            let found_inside = tokio::spawn(async { current_routing_events().is_some() })
                .await
                .unwrap();
            (carried, found_inside)
        })
        .await;

        assert!(
            Arc::ptr_eq(&carried, &installed),
            "the value read before the spawn is the turn's own"
        );
        assert!(
            !found_inside,
            "a spawned child inherits NO scope — reading inside it would \
             silently fall back to the host's sink, which on the daemon \
             drops every event"
        );
    }

    /// `scope_turn` is not "install both or install neither": the daemon's
    /// three turn arms pass whatever the socket actually has, and a host with
    /// one capability and not the other must get exactly that. All four arms,
    /// because the match in `scope_turn` has four and an untested arm is an
    /// arm that can be typed wrong.
    #[tokio::test]
    async fn scope_turn_installs_each_capability_independently() {
        let a = channel();
        let r = sink();
        let read = || async { (current(), current_routing_events()) };

        let (ca, cr) = scope_turn(Some(Arc::clone(&a)), Some(Arc::clone(&r)), read()).await;
        assert!(
            ca.is_some_and(|c| Arc::ptr_eq(&c, &a)) && cr.is_some_and(|c| Arc::ptr_eq(&c, &r)),
            "both installed: the turn must see both"
        );

        let (ca, cr) = scope_turn(Some(Arc::clone(&a)), None, read()).await;
        assert!(
            ca.is_some() && cr.is_none(),
            "approval only: the sink must fall through to the commissioned member"
        );

        let (ca, cr) = scope_turn(None, Some(Arc::clone(&r)), read()).await;
        assert!(
            ca.is_none() && cr.is_some(),
            "sink only: the channel must fall through to the commissioned member"
        );

        let (ca, cr) = scope_turn(None, None, read()).await;
        assert!(
            ca.is_none() && cr.is_none(),
            "neither installed: an in-process host reads exactly what it always did"
        );
    }

    /// Regression for ce3e6e4a2 — the stack overflow that landed with the
    /// second scope and shipped with no test.
    ///
    /// A `task_local` scope stores the inner future INLINE. Nesting the two
    /// helpers therefore builds the turn's state machine once for the inner
    /// scope and again for the outer, on the caller's stack, before anything
    /// is boxed — and a debug-built `serve_turn` is large enough that the
    /// doubling aborts the process (observed as two `turn_surface` tests
    /// SIGABRTing). `scope_turn`'s fix is ONE `Box::pin` with both
    /// task-locals around it.
    ///
    /// The bar is asserted as a SIZE and not as a literal overflow: a test
    /// that overflowed on regression would abort the whole test binary
    /// instead of failing one case, which is a worse signal than the bug.
    /// Both shapes are constructed here, so the assertion names its own
    /// failing input — swap `Box::pin(turn())` for `turn()` in `scope_turn`
    /// and the second number becomes the first.
    #[tokio::test]
    async fn scope_turn_boxes_the_turn_once_for_both_scopes() {
        /// Stands in for a turn: a state machine big enough that carrying it
        /// twice is the difference between fitting on the stack and not.
        fn turn() -> impl Future<Output = usize> {
            async {
                let pad = [0u8; 32 * 1024];
                tokio::task::yield_now().await;
                std::hint::black_box(&pad).len()
            }
        }

        let raw = std::mem::size_of_val(&turn());
        assert!(
            raw >= 32 * 1024,
            "instrument check: the probe future is only {raw} bytes, so it \
             cannot show the difference this test exists to measure"
        );

        // What `scope_turn` does today: box once, then both task-locals
        // around the pointer.
        let boxed_once = TURN_APPROVAL.scope(
            channel(),
            TURN_ROUTING_EVENTS.scope(sink(), Box::pin(turn())),
        );
        // What nesting the two public helpers does: each scope holds the
        // thing below it inline, so the turn is built on the stack twice.
        let nested_inline =
            TURN_APPROVAL.scope(channel(), TURN_ROUTING_EVENTS.scope(sink(), turn()));

        let one = std::mem::size_of_val(&boxed_once);
        let two = std::mem::size_of_val(&nested_inline);
        assert!(
            one < raw / 8,
            "scope_turn no longer boxes before scoping: the scoped future is \
             {one} bytes against a {raw}-byte turn, so the turn is inline in \
             the task-local scope again and a debug-built serve_turn will \
             overflow the stack (ce3e6e4a2)"
        );
        assert!(
            two >= raw,
            "instrument check: the un-boxed shape measured {two} bytes for a \
             {raw}-byte turn, so this test is not measuring inline storage \
             and its verdict above means nothing"
        );

        drop(nested_inline);
        assert_eq!(boxed_once.await, 32 * 1024, "the scoped turn still runs");
    }
}
