// SPDX-License-Identifier: AGPL-3.0-or-later
//! The reference host's half of OICP v0.6 §2.17: a writer's request first.
//!
//! The engine asks [`PriorityGate::step`] where a step of model work starts
//! (the embed slot, a generation slot's admission) and holds what it returns
//! until the step ends. An interactive step is counted while it runs. A
//! background step waits until none is, and the request's background steps
//! go one at a time, so at most one stands between a writer's step and the
//! stage it queues at: the embed context's lock is first in first out, and
//! background steps already queued there would otherwise go first. Each wait
//! is bounded by the ceiling, then the step goes ahead. A call whose request
//! named no priority passes untouched.
//!
//! A step already running is not preempted, but a background one long enough
//! to matter is read in parts ([`PriorityGate::in_parts`]): a completion's
//! prompt, which holds the accelerator longest, pauses between its parts
//! while an interactive step is in flight.
//!
//! Why here: on 2026-10-16 a warm summon's query embed took p50 249 ms under a
//! draft's checks against 77 ms idle, and the checks' verify ran 126 primary
//! completions in the same window; the accelerator, not the lock's queue,
//! held the time (zoracite v1-pull-latency).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use oicp_types::RequestPriority;
use sovereign_contracts::request_priority::{self, Asked, Holding};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};

const TARGET: &str = "priority";

/// How long a background step waits, in all, before it goes ahead (§2.17
/// "Bounded").
pub const HOLD_CEILING: Duration = Duration::from_secs(2);

/// Tokens a background completion's prompt is read in, so an interactive
/// step that arrives mid-read shares the accelerator with one part, not the
/// whole prompt (PV3b). On 2026-10-16 a checker's 170-340-token prompt, read
/// in one part, held the 35B primary 500-770 ms, and a summon's query embed
/// running beside it took 420-738 ms against 83 ms alone (the window of
/// zoracite 0432626).
pub const BACKGROUND_PART_TOKENS: usize = 64;

/// How often a paused part looks again: the thread doing model work is a
/// blocking one, with no runtime to await `ended` on.
const PAUSE_POLL: Duration = Duration::from_millis(2);

/// Holds interactive steps first and background steps one at a time.
#[derive(Debug)]
pub struct PriorityGate {
    interactive: AtomicUsize,
    ended: Notify,
    background: Arc<Semaphore>,
    ceiling: Duration,
}

/// A step of model work, held until the work ends.
#[derive(Debug, Default)]
pub struct Step {
    _interactive: Option<InteractiveMark>,
    _background: Option<(OwnedSemaphorePermit, Holding)>,
}

/// One interactive step in flight; dropping it wakes held background steps.
#[derive(Debug)]
struct InteractiveMark(Arc<PriorityGate>);

impl Drop for InteractiveMark {
    fn drop(&mut self) {
        let left = self.0.interactive.fetch_sub(1, Ordering::SeqCst) - 1;
        if left == 0 {
            self.0.ended.notify_waiters();
        }
    }
}

impl PriorityGate {
    /// A gate whose background steps wait at most `ceiling`.
    pub fn new(ceiling: Duration) -> Arc<Self> {
        Arc::new(Self {
            interactive: AtomicUsize::new(0),
            ended: Notify::new(),
            background: Arc::new(Semaphore::new(1)),
            ceiling,
        })
    }

    /// This process's gate: its models share one accelerator.
    pub fn process() -> &'static Arc<Self> {
        static GATE: OnceLock<Arc<PriorityGate>> = OnceLock::new();
        GATE.get_or_init(|| Self::new(HOLD_CEILING))
    }

    /// The step `phase` starts, for the request the running call serves.
    pub async fn step(self: &Arc<Self>, phase: &'static str) -> Step {
        let Some(asked) = request_priority::current() else {
            tracing::trace!(target: TARGET, phase, "priority: unasked step, served as before");
            return Step::default();
        };
        match asked.priority() {
            RequestPriority::Interactive => {
                let in_flight = self.interactive.fetch_add(1, Ordering::SeqCst) + 1;
                tracing::debug!(target: TARGET, phase, in_flight, "priority: interactive step");
                Step {
                    _interactive: Some(InteractiveMark(Arc::clone(self))),
                    _background: None,
                }
            }
            RequestPriority::Background if asked.holding() => {
                tracing::debug!(
                    target: TARGET,
                    phase,
                    "priority: background step inside the one its request holds"
                );
                Step::default()
            }
            RequestPriority::Background => self.background_step(phase, &asked).await,
        }
    }

    async fn background_step(&self, phase: &'static str, asked: &Asked) -> Step {
        let asked_at = Instant::now();
        let permit =
            tokio::time::timeout(self.ceiling, Arc::clone(&self.background).acquire_owned())
                .await
                .ok()
                .and_then(Result::ok);
        let behind = self.until_no_interactive(asked_at).await;
        let waited_ms = asked_at.elapsed().as_millis() as u64;
        let at_ceiling = permit.is_none() || asked_at.elapsed() >= self.ceiling;
        if at_ceiling {
            tracing::debug!(
                target: TARGET,
                phase,
                waited_ms,
                behind,
                one_at_a_time = permit.is_some(),
                "priority: background step went ahead at the hold ceiling"
            );
        } else {
            tracing::debug!(
                target: TARGET,
                phase,
                waited_ms,
                behind,
                "priority: background step admitted"
            );
        }
        Step {
            _interactive: None,
            _background: permit.map(|p| (p, asked.hold())),
        }
    }

    /// Reads `0..len` through `read` for the request the running thread
    /// serves: in parts of `part` for a background request, pausing between
    /// parts while an interactive step is in flight (each pause bounded by
    /// the ceiling), and in one part for any other. Blocking, for the thread
    /// doing the model work. Where the parts fall depends on `len` and
    /// `part` alone, never on timing.
    pub fn in_parts<E>(
        &self,
        phase: &'static str,
        len: usize,
        part: usize,
        mut read: impl FnMut(std::ops::Range<usize>) -> Result<(), E>,
    ) -> Result<(), E> {
        let background = request_priority::current()
            .is_some_and(|a| a.priority() == RequestPriority::Background);
        let part = part.max(1);
        if !background || len <= part {
            return read(0..len);
        }
        tracing::trace!(target: TARGET, phase, len, part, "priority: background read in parts");
        let mut from = 0;
        while from < len {
            if from > 0 {
                self.pause_between_parts(phase, from);
            }
            let to = (from + part).min(len);
            read(from..to)?;
            from = to;
        }
        Ok(())
    }

    /// Blocks while an interactive step is in flight, at most the ceiling.
    fn pause_between_parts(&self, phase: &'static str, at: usize) {
        let asked_at = Instant::now();
        let mut behind = 0;
        while let n @ 1.. = self.interactive.load(Ordering::SeqCst) {
            behind = behind.max(n);
            if asked_at.elapsed() >= self.ceiling {
                tracing::debug!(
                    target: TARGET,
                    phase,
                    at,
                    behind,
                    waited_ms = asked_at.elapsed().as_millis() as u64,
                    "priority: background part went ahead at the hold ceiling"
                );
                return;
            }
            std::thread::sleep(PAUSE_POLL);
        }
        if behind > 0 {
            tracing::debug!(
                target: TARGET,
                phase,
                at,
                behind,
                waited_ms = asked_at.elapsed().as_millis() as u64,
                "priority: background step paused between parts"
            );
        }
    }

    /// Waits until no interactive step is in flight, or the ceiling counted
    /// from `asked_at`; the most interactive steps it waited behind.
    async fn until_no_interactive(&self, asked_at: Instant) -> usize {
        let mut behind = 0;
        loop {
            let ended = self.ended.notified();
            tokio::pin!(ended);
            ended.as_mut().enable();
            let n = self.interactive.load(Ordering::SeqCst);
            if n == 0 {
                return behind;
            }
            behind = behind.max(n);
            let left = self.ceiling.saturating_sub(asked_at.elapsed());
            if left.is_zero() || tokio::time::timeout(left, ended).await.is_err() {
                return behind;
            }
        }
    }
}

#[cfg(test)]
#[path = "priority_gate_tests.rs"]
mod tests;
