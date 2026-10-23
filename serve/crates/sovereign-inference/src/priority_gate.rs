// SPDX-License-Identifier: AGPL-3.0-or-later
//! The reference host's half of OICP v0.6 §2.17: a writer's request first.
//!
//! The engine asks [`PriorityGate::step`] where a step of model work starts
//! (the embed slot, a generation slot's admission), tells it
//! [`Step::entered`] once it holds its stage, and holds it until the step
//! ends. An interactive step is counted from its ask. A background step waits
//! until none is, and the background steps waiting to enter one stage go one
//! at a time, so at most one stands between a writer's step and the stage it
//! queues at: a stage's queue is first in first out, and background steps
//! already queued there would otherwise go first. A background step that has
//! entered its stage holds no other back (PV3e). Its place in that line is
//! kept however long the steps ahead of it take: only its hold for
//! interactive work is bounded by the ceiling, counted from the place, then
//! it goes ahead (PV3h: places given up at the ceiling sent several to the
//! stage at once, a writer's step parked behind two to four). A call whose
//! request named no priority passes untouched.
//!
//! A step already running is not preempted, but a background one long enough
//! to matter is read in parts ([`PriorityGate::in_parts`]): a completion's
//! prompt, which holds the accelerator longest, pauses between its parts
//! while an interactive step that has entered its stage runs, never for one
//! waiting on the slot the prompt's step holds (PV3e). A background embed
//! batch lets the embed context go between its sub-batches
//! ([`PriorityGate::parts_under`]) and pauses for any interactive step.
//!
//! Why here: on 2026-10-16 a warm summon's query embed took p50 249 ms under a
//! draft's checks against 77 ms idle, and the checks' verify ran 126 primary
//! completions in the same window; the accelerator, not the lock's queue,
//! held the time (zoracite v1-pull-latency).

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
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
    /// Interactive steps asked and not ended.
    interactive: AtomicUsize,
    /// Of those, the ones inside their stage.
    running: AtomicUsize,
    ended: Notify,
    /// Each stage's one place for a background step waiting to enter it.
    waiting: Mutex<HashMap<String, Arc<Semaphore>>>,
    ceiling: Duration,
}

/// A step of model work, held until the work ends.
#[derive(Debug, Default)]
pub struct Step {
    interactive: Option<InteractiveMark>,
    running: Option<RunningMark>,
    waiting: Option<OwnedSemaphorePermit>,
    _holding: Option<Holding>,
}

impl Step {
    /// The step holds its stage: a background step gives its stage's place to
    /// the next one waiting there, and an interactive one counts as running,
    /// so a background prompt read pauses for it. A second call does nothing.
    pub fn entered(&mut self) {
        self.waiting = None;
        if let (Some(mark), None) = (&self.interactive, &self.running) {
            let running = mark.0.running.fetch_add(1, Ordering::SeqCst) + 1;
            tracing::trace!(target: TARGET, running, "priority: interactive step entered its stage");
            self.running = Some(RunningMark(Arc::clone(&mark.0)));
        }
    }
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

/// One interactive step inside its stage.
#[derive(Debug)]
struct RunningMark(Arc<PriorityGate>);

impl Drop for RunningMark {
    fn drop(&mut self) {
        self.0.running.fetch_sub(1, Ordering::SeqCst);
    }
}

impl PriorityGate {
    /// A gate whose background steps wait at most `ceiling`.
    pub fn new(ceiling: Duration) -> Arc<Self> {
        Arc::new(Self {
            interactive: AtomicUsize::new(0),
            running: AtomicUsize::new(0),
            ended: Notify::new(),
            waiting: Mutex::new(HashMap::new()),
            ceiling,
        })
    }

    /// This process's gate: its models share one accelerator.
    pub fn process() -> &'static Arc<Self> {
        static GATE: OnceLock<Arc<PriorityGate>> = OnceLock::new();
        GATE.get_or_init(|| Self::new(HOLD_CEILING))
    }

    /// The step `phase` starts at `stage` (the embed context, a generation
    /// slot's queue), for the request the running call serves.
    pub async fn step(self: &Arc<Self>, stage: &str, phase: &'static str) -> Step {
        let Some(asked) = request_priority::current() else {
            tracing::trace!(target: TARGET, phase, "priority: unasked step, served as before");
            return Step::default();
        };
        match asked.priority() {
            RequestPriority::Interactive => {
                let in_flight = self.interactive.fetch_add(1, Ordering::SeqCst) + 1;
                tracing::debug!(target: TARGET, phase, in_flight, "priority: interactive step");
                Step {
                    interactive: Some(InteractiveMark(Arc::clone(self))),
                    ..Step::default()
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
            RequestPriority::Background => self.background_step(stage, phase, &asked).await,
        }
    }

    async fn background_step(&self, stage: &str, phase: &'static str, asked: &Asked) -> Step {
        let asked_at = Instant::now();
        let place = Arc::clone(
            self.waiting
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(stage.to_string())
                .or_insert_with(|| Arc::new(Semaphore::new(1))),
        );
        // The place is never given up at the ceiling (PV3h); it is closed
        // only if the gate is, and then the step goes on without one.
        let permit = match place.acquire_owned().await {
            Ok(permit) => Some(permit),
            Err(_closed) => {
                tracing::warn!(target: TARGET, stage, phase, "priority: the stage's place is closed; the background step goes on without one");
                None
            }
        };
        let placed_at = Instant::now();
        let place_ms = placed_at.duration_since(asked_at).as_millis() as u64;
        let behind = self.until_no_interactive(placed_at).await;
        let waited_ms = asked_at.elapsed().as_millis() as u64;
        if placed_at.elapsed() >= self.ceiling {
            tracing::debug!(
                target: TARGET,
                stage,
                phase,
                waited_ms,
                place_ms,
                behind,
                "priority: background step went ahead at the hold ceiling"
            );
        } else {
            tracing::debug!(
                target: TARGET,
                stage,
                phase,
                waited_ms,
                place_ms,
                behind,
                "priority: background step admitted"
            );
        }
        Step {
            waiting: permit,
            _holding: Some(asked.hold()),
            ..Step::default()
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
        let part = part.max(1);
        if !Self::in_background() || len <= part {
            return read(0..len);
        }
        tracing::trace!(target: TARGET, phase, len, part, "priority: background read in parts");
        let mut from = 0;
        while from < len {
            if from > 0 {
                self.pause_between_parts(phase, from, &self.running);
            }
            let to = (from + part).min(len);
            read(from..to)?;
            from = to;
        }
        Ok(())
    }

    /// Runs `part` under the hold `acquire` returns until `part` answers that
    /// none is left. A background request lets the hold go between parts and
    /// takes it again after pausing while an interactive step is in flight
    /// (each pause bounded by the ceiling), so a writer's step queued at the
    /// same lock goes between them (PV3c); any other request runs every part
    /// under one hold, as before. Blocking, for the thread doing the model
    /// work. Where parts fall is `part`'s to say, never timing's.
    pub fn parts_under<G, E>(
        &self,
        phase: &'static str,
        mut acquire: impl FnMut() -> G,
        mut part: impl FnMut(&mut G) -> Result<bool, E>,
    ) -> Result<(), E> {
        let background = Self::in_background();
        let mut held = acquire();
        let mut at = 0;
        while part(&mut held)? {
            at += 1;
            if background {
                drop(held);
                self.pause_between_parts(phase, at, &self.interactive);
                held = acquire();
            }
        }
        Ok(())
    }

    /// Whether the request the running thread serves asked as background.
    fn in_background() -> bool {
        request_priority::current().is_some_and(|a| a.priority() == RequestPriority::Background)
    }

    /// Blocks while `count` (the interactive steps running, or all asked) is
    /// above zero, at most the ceiling. A prompt read holds its slot across
    /// its parts, so it counts only running steps: one waiting on that slot
    /// cannot run until the read ends, and pausing for it would hold both
    /// (zoracite 1747f71: a writer's verify took 29 s behind checks paused
    /// for it, against 8 s with neither asked).
    fn pause_between_parts(&self, phase: &'static str, at: usize, count: &AtomicUsize) {
        let asked_at = Instant::now();
        let mut behind = 0;
        while let n @ 1.. = count.load(Ordering::SeqCst) {
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
    /// from `asked_at` (a background step's place); the most interactive
    /// steps it waited behind.
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
