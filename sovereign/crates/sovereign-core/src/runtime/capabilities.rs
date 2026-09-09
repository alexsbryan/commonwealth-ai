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

use crate::traits::ApprovalChannel;

tokio::task_local! {
    /// The approval channel of the turn running on this task, installed by
    /// [`scope`]. Absent outside a scoped turn — which is every in-process
    /// host, deliberately.
    static TURN_APPROVAL: Arc<dyn ApprovalChannel>;
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

/// The ambient turn approval channel, if this task is running inside a scoped
/// turn.
fn current() -> Option<Arc<dyn ApprovalChannel>> {
    TURN_APPROVAL.try_with(Arc::clone).ok()
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::AutoApprovalChannel;

    fn channel() -> Arc<dyn ApprovalChannel> {
        Arc::new(AutoApprovalChannel)
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
}
