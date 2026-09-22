// SPDX-License-Identifier: AGPL-3.0-or-later
//! Fair admission policy for a scarce pool of inference slots.
//!
//! A shared-model fleet has exactly one distributed primary with only a
//! handful of decode slots, divided across everyone (see
//! `sovereign/docs/RUN_GLM_5_2_ON_THE_MESH.md`). Two boundaries gate access
//! to that pool, and both were flat counters before this module existed:
//!
//!   - the host's mesh inference endpoint (`commonwealth-api` admission),
//!     where every consumer's turn converges as a peer request; and
//!   - each node's own chat server (`sovereign-server`), where local turns
//!     compete for the same slots.
//!
//! A flat counter is first-come with no fairness: one chatty origin can
//! hold every slot, and a refused caller learns nothing about when to try
//! again. [`SchedCore`] is the shared *policy* that fixes both — it is the
//! one place that decides who is admitted, in what order, and where a
//! waiter sits in line. It is deliberately:
//!
//!   - **Generic over the key `K`** — a "user" is whatever the caller says
//!     it is (a tenant id, a peer `NodeId`). The policy never names a
//!     domain type, so the same core backs both gates.
//!   - **Pure and synchronous** — no `tokio`, no I/O, no wall clock. Every
//!     decision is a deterministic function of the current state, which is
//!     exactly what makes the fairness guarantees unit-testable rather than
//!     asserted. The async waiting/waking lives in each caller's shell.
//!
//! ## Two interaction modes, one policy
//!
//! - **Queue** ([`SchedCore::admit`]): the caller is willing to wait. Used
//!   by the human chat path — a queued turn is told its position and ETA
//!   and served when a slot frees, highest-weight first. Never hangs: past
//!   the depth cap it sheds.
//! - **Shed** ([`SchedCore::try_grant`]): the caller will not wait (a peer
//!   load balancer just routes elsewhere on refusal). Grants if a slot is
//!   free and the origin is under its cap; otherwise refuses with a
//!   position hint.
//!
//! ## Reciprocity
//!
//! The core does not read the contribution ledger — that would couple it to
//! the mesh. Instead each admission carries two caller-supplied numbers
//! derived from reciprocity:
//!   - `weight` — orders the queue (a contributor's turn is served first);
//!   - `cap` — the origin's concurrency allowance (a contributor may hold
//!     more slots at once).
//! Pure consumers pass `weight = 1.0` and a base `cap`; the policy is
//! identical, the inputs differ. This keeps the ledger logic in the shells
//! and the *fairness mechanism* here, testable in isolation.

use std::collections::HashMap;
use std::hash::Hash;

/// Seed for the turn-duration EWMA before any turn completes, so early ETA
/// estimates aren't zero. Seconds-order is right for a many-hop shared
/// primary.
const DEFAULT_TURN_MS: u64 = 3_000;

/// EWMA smoothing `new = (sample + 3·old) / 4` (α = 0.25): one anomalous
/// turn can't swing the ETA, but it tracks a shifting steady state.
const EWMA_NUM_OLD: u64 = 3;
const EWMA_DEN: u64 = 4;

/// The queue-ETA estimator: an EWMA of observed turn durations, plus the
/// `ceil(position / slots) · avg` prediction built on it.
///
/// Extracted so there is exactly ONE implementation of "how long will this
/// caller wait" (ARCH_PRINCIPLES §10.6). Two very different queues need it:
/// [`SchedCore`] for chat turns in `sovereign-server`, and the inference
/// slot gate in `sovereign-inference`, which bounds waits on the model
/// permit itself. Those queues are genuinely separate — the same formula
/// written twice would be the smell, not the sharing.
#[derive(Clone, Copy, Debug)]
pub struct EtaEwma {
    avg_turn_ms: u64,
}

impl EtaEwma {
    /// Start from a seed estimate, used until the first real turn lands.
    pub fn new(seed_ms: u64) -> Self {
        Self {
            avg_turn_ms: seed_ms,
        }
    }

    /// Current smoothed turn duration in ms.
    pub fn avg_turn_ms(&self) -> u64 {
        self.avg_turn_ms
    }

    /// Fold a completed turn's wall time (ms) into the EWMA.
    pub fn record(&mut self, dur_ms: u64) {
        self.avg_turn_ms = (dur_ms + EWMA_NUM_OLD * self.avg_turn_ms) / EWMA_DEN;
    }

    /// Predicted wait for a caller at 1-based `position`, given `slots`
    /// draining the queue in parallel and `in_flight_elapsed_ms` already
    /// spent on the turn now holding the slot.
    ///
    /// `ceil(position / slots) · avg − elapsed`, which is honest about
    /// parallel drain rather than the `position × avg` that over-states a
    /// multi-slot host, AND honest about the turn already running.
    ///
    /// **The elapsed term is not a refinement; without it the shed rule is
    /// wrong.** At `position = 1` the caller is not queued behind other
    /// callers at all — it is waiting out the one in-flight turn — so
    /// charging it a WHOLE `avg_turn_ms` makes `predicted > bound` collapse
    /// into `avg_turn_ms > bound`, a condition with no load term in it.
    /// Measured on a 27B host 2026-08-26: 624 of 625 sheds happened at
    /// `position = 1` with an EMPTY queue, median `avg_turn_ms` 30,690
    /// against a 30,000 bound — the host refused all concurrency because its
    /// own turns are naturally slower than the bound (note `bf432b4d`).
    ///
    /// Saturating: a turn running longer than `avg` predicts `0`, i.e. "it
    /// should finish any moment". That biases toward SERVING rather than
    /// refusing, which is the correct direction for a shed decision — shed
    /// only when even the optimistic estimate exceeds the bound.
    pub fn predict_wait_ms(&self, position: u32, slots: usize, in_flight_elapsed_ms: u64) -> u64 {
        let slots = (slots.max(1)) as u64;
        ((position as u64).div_ceil(slots) * self.avg_turn_ms).saturating_sub(in_flight_elapsed_ms)
    }
}

/// Map a raw contribution magnitude to a fair-scheduling weight, normalized
/// against the fleet's heaviest contributor: `1.0 + k·(value/max)`. Returns
/// the neutral `1.0` when there's no signal (`max ≤ 0`) or reciprocity is off
/// (`k ≤ 0`). Lives here so both admission gates weight a contributor's turns
/// by the exact same rule — the chat server's queue order and the peer gate's
/// per-node cap both feed off this.
pub fn reciprocity_weight(value: f64, max: f64, k: f64) -> f64 {
    if max <= 0.0 || k <= 0.0 {
        return 1.0;
    }
    1.0 + k * (value / max).clamp(0.0, 1.0)
}

/// The equal-share concurrency allowance for ONE principal, given the host's
/// concurrency `budget` and how many principals are `active` right now.
///
/// THE ONE IMPLEMENTATION of "what is this caller's fair share" (§10.6).
/// Deliberately **not** weight-ordered: every active principal gets the same
/// allowance regardless of contribution, reciprocity, or arrival order. That
/// is the correction `MESH_SCALE_100_USERS_1000_CORPORA.md §7.1 R2` demands —
/// weight-ordering is condemned by `SCHEDULER_QUALITY.md` F6, and the fix for
/// §9.3's red is *equal* share, not *ranked* share.
///
/// Two regimes, and the boundary between them is the load-bearing part:
///
/// - **`active <= 1` → [`u32::MAX`] (no cap at all).** One principal alone on
///   the host is not competing with anyone, so capping it would only make a
///   solo caller slower than it is today for no fairness gain. The sentinel
///   is the same "not rationing" idiom `commonwealth-api`'s
///   `effective_peer_cap` already uses. This is what makes the
///   single-principal no-regression arm structural rather than remembered.
/// - **`active > 1` → `max(1, budget / active)`.** Integer division, floored
///   at 1 so a principal is never allowed *zero* concurrency (which would be
///   a shed rule, and the shed decider lives downstream in the inference
///   slot queue — this cap must never be the thing that refuses everyone).
///
/// `budget` is the number of turns the host can carry concurrently before the
/// downstream slot queue starts shedding on predicted wait; it is a property
/// of the host, not of any caller.
pub fn fair_share_cap(budget: u32, active: usize) -> u32 {
    if active <= 1 {
        return u32::MAX;
    }
    let active = u32::try_from(active).unwrap_or(u32::MAX);
    (budget / active).max(1)
}

/// A queued waiter's place in line. Reported up front and again on every
/// move up; a shell forwards it to the client (e.g. a `QueuePosition` WS
/// frame).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueueStatus {
    /// 1-based rank among *waiting* requests (1 = next to be served).
    pub position: u32,
    /// Rough wait estimate: `ceil(position / slots) · avg_turn_ms` — honest
    /// about the N slots draining the queue in parallel, rather than
    /// `position × avg` which over-states the wait on a multi-slot host.
    pub estimated_wait_ms: u64,
}

/// Outcome of admitting a request that's willing to queue ([`SchedCore::admit`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmitOutcome {
    /// A slot was free and the origin was under its cap — granted outright.
    Granted,
    /// Enqueued; `seq` identifies the waiter for claim/cancel.
    Enqueued { seq: u64, status: QueueStatus },
    /// Queue is at depth — shed rather than grow it unboundedly.
    Shed { would_be_position: u32 },
}

/// Outcome of a non-blocking grant attempt ([`SchedCore::try_grant`]). Never
/// leaves a waiter behind, so a one-shot caller can't perturb the live queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TryGrant {
    Granted,
    /// Couldn't grant now; this is where it *would* sit if it queued.
    WouldQueue {
        position: u32,
    },
    Shed {
        would_be_position: u32,
    },
}

/// Result of a woken waiter re-checking its state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimOrStatus {
    /// The reserved slot is ours — the waiter is removed and the caller now
    /// holds it.
    Claimed,
    /// Still waiting; here's the current (possibly improved) position.
    Waiting(QueueStatus),
    /// No longer in the queue (cancelled). Defensive; shouldn't occur on the
    /// normal path.
    Gone,
}

/// One entry in the wait queue. `granted` flips the instant [`SchedCore::promote`]
/// reserves a slot for it; the awaiting shell then *claims* (removes) it. A
/// granted-but-unclaimed waiter still holds its slot, so a cancel must give it
/// back. `cap` is captured at admit time (reciprocity-scaled) so promote can
/// re-check eligibility without the caller re-supplying it.
struct Waiter<K> {
    key: K,
    weight: f64,
    cap: u32,
    seq: u64,
    granted: bool,
}

/// The pure scheduling policy — see the module docs. Generic over the origin
/// key `K`.
///
/// Invariant maintained after every method returns: **if `available > 0`, no
/// *eligible* waiter remains** (eligible = not yet granted and the origin is
/// under its cap). [`promote`](Self::promote) drains eligible waiters into
/// free slots; the direct-grant paths only take a slot when no waiter could.
/// So a freed slot with waiters present is always reserved for the queue,
/// never jumped by a newcomer.
pub struct SchedCore<K: Eq + Hash + Clone> {
    /// Total decode slots; the real cap. Mutable at runtime via [`set_slots`](Self::set_slots).
    slots_total: usize,
    /// Committed slots — granted-and-held plus reserved-but-unclaimed. Free
    /// slots are `slots_total − held`. Tracking `held` (rather than a separate
    /// `available`) is what makes a runtime slot-count change correct: resize
    /// `slots_total` and the free count falls out, with no counter to reconcile.
    held: usize,
    /// Max *waiting* (not-granted) requests before we shed.
    max_queue_depth: usize,
    /// In-flight turn count per origin (granted-but-unclaimed counts too —
    /// the slot is committed the moment it's reserved).
    inflight: HashMap<K, u32>,
    /// The wait queue. Small (≤ `max_queue_depth`), so linear scans for
    /// best-eligible / position are cheap and obviously correct.
    waiters: Vec<Waiter<K>>,
    /// Monotonic ticket source — the FIFO tiebreak within a weight.
    next_seq: u64,
    /// EWMA of completed-turn wall time, for ETA. Seeded so early estimates
    /// are sane. See [`EtaEwma`] — shared with the inference slot gate so the
    /// prediction has one implementation.
    eta: EtaEwma,
}

impl<K: Eq + Hash + Clone> SchedCore<K> {
    /// `slots` = concurrent turn budget. `0` is valid — it admits nothing
    /// (the peer gate's "reject all" ceiling, equivalent to
    /// `SOVEREIGN_DISABLE_PEER_INFERENCE`); callers that must never wedge (the
    /// chat server) clamp to ≥ 1 themselves. `max_queue_depth` = waiting
    /// requests tolerated before shedding (clamped to ≥ 1).
    pub fn new(slots: usize, max_queue_depth: usize) -> Self {
        Self {
            slots_total: slots,
            held: 0,
            max_queue_depth: max_queue_depth.max(1),
            inflight: HashMap::new(),
            waiters: Vec::new(),
            next_seq: 0,
            eta: EtaEwma::new(DEFAULT_TURN_MS),
        }
    }

    /// Free slots right now — glassbox for a `host_busy` log line.
    pub fn available(&self) -> usize {
        self.slots_total.saturating_sub(self.held)
    }

    /// Total committed (granted + reserved) slots across all origins.
    pub fn in_flight(&self) -> usize {
        self.held
    }

    /// Current total slot budget.
    pub fn slots(&self) -> usize {
        self.slots_total
    }

    /// Resize the slot budget at runtime — e.g. the operator moved the
    /// contribution ceiling. A grow frees capacity immediately (and promotes
    /// any eligible waiters, whose seqs are returned). A shrink below what's
    /// in flight evicts no one: it simply withholds new grants until usage
    /// falls back under the new ceiling.
    pub fn set_slots(&mut self, slots: usize) -> Vec<u64> {
        self.slots_total = slots;
        self.promote()
    }

    /// In-flight turns currently attributed to `key`.
    pub fn inflight_of(&self, key: &K) -> u32 {
        self.inflight.get(key).copied().unwrap_or(0)
    }

    /// Visible queue length — waiters not yet reserved a slot.
    pub fn waiting_len(&self) -> usize {
        self.waiters.iter().filter(|w| !w.granted).count()
    }

    /// How many DISTINCT origins hold at least one slot right now — the
    /// denominator of [`fair_share_cap`].
    ///
    /// Reads off `inflight`, which [`dec_inflight`](Self::dec_inflight)
    /// already prunes to zero-length on release, so an origin that has gone
    /// idle stops counting the instant its last turn ends. That pruning is
    /// what keeps the share rule *current* rather than cumulative: a caller
    /// who left is not still taking up a share.
    pub fn active_keys(&self) -> usize {
        self.inflight.len()
    }

    /// [`active_keys`](Self::active_keys), counting `key` itself even when it
    /// holds nothing yet — the denominator a caller ABOUT to be admitted
    /// should be measured against.
    ///
    /// Without this, the first arrival of a second principal would compute
    /// `active = 1`, read itself as uncontended, and be handed the uncapped
    /// allowance — the cap would then only engage one turn late, every time
    /// the population changed.
    pub fn active_keys_including(&self, key: &K) -> usize {
        self.inflight.len() + usize::from(!self.inflight.contains_key(key))
    }

    fn eligible(&self, w: &Waiter<K>) -> bool {
        !w.granted && self.inflight_of(&w.key) < w.cap
    }

    /// `a` ranks strictly ahead of `b`: higher weight wins; equal weight
    /// breaks to the lower seq (FIFO). Centralised so admit, position and
    /// promote order identically.
    fn ranks_ahead(a_weight: f64, a_seq: u64, b_weight: f64, b_seq: u64) -> bool {
        if a_weight != b_weight {
            a_weight > b_weight
        } else {
            a_seq < b_seq
        }
    }

    /// Index of the best eligible waiter (the one [`promote`](Self::promote)
    /// serves next), or `None` if none is currently eligible.
    fn best_eligible(&self) -> Option<usize> {
        let mut best: Option<usize> = None;
        for (i, w) in self.waiters.iter().enumerate() {
            if !self.eligible(w) {
                continue;
            }
            match best {
                Some(bi) => {
                    let b = &self.waiters[bi];
                    if Self::ranks_ahead(w.weight, w.seq, b.weight, b.seq) {
                        best = Some(i);
                    }
                }
                None => best = Some(i),
            }
        }
        best
    }

    /// 1-based position of `seq` among *waiting* requests. Granted
    /// (about-to-run) waiters have left the line and are excluded.
    fn position_of(&self, seq: u64) -> u32 {
        let me = match self.waiters.iter().find(|w| w.seq == seq) {
            Some(w) => w,
            None => return 0,
        };
        let ahead = self
            .waiters
            .iter()
            .filter(|w| !w.granted && w.seq != seq)
            .filter(|w| Self::ranks_ahead(w.weight, w.seq, me.weight, me.seq))
            .count();
        ahead as u32 + 1
    }

    /// Position a hypothetical new entry with `weight` would occupy, without
    /// mutating — the REST shed hint. A new entry has the largest seq, so
    /// equal-weight existing waiters rank ahead of it.
    fn hypothetical_position(&self, weight: f64) -> u32 {
        let ahead = self
            .waiters
            .iter()
            .filter(|w| !w.granted)
            .filter(|w| w.weight >= weight)
            .count();
        ahead as u32 + 1
    }

    fn grant_direct(&mut self, key: K) {
        self.held += 1;
        *self.inflight.entry(key).or_insert(0) += 1;
    }

    /// Admit a request willing to queue. `cap` is the origin's (possibly
    /// reciprocity-scaled) concurrency allowance; `weight` orders the queue.
    pub fn admit(&mut self, key: K, weight: f64, cap: u32) -> AdmitOutcome {
        let cap = cap.max(1);
        // Fast path: a free slot and the origin is under its cap. The
        // invariant guarantees any waiter present is ineligible (its origin
        // is at cap), so taking the slot jumps no one who could use it.
        if self.held < self.slots_total && self.inflight_of(&key) < cap {
            self.grant_direct(key);
            return AdmitOutcome::Granted;
        }
        if self.waiting_len() >= self.max_queue_depth {
            return AdmitOutcome::Shed {
                would_be_position: self.max_queue_depth as u32 + 1,
            };
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        self.waiters.push(Waiter {
            key,
            weight,
            cap,
            seq,
            granted: false,
        });
        // No promote needed: we only reach here because we couldn't grant
        // directly, so promote would grant us nothing.
        let status = self.status(self.position_of(seq));
        AdmitOutcome::Enqueued { seq, status }
    }

    /// Non-blocking grant. Never leaves a waiter behind, so a one-shot caller
    /// can't perturb the live queue's positions.
    pub fn try_grant(&mut self, key: K, weight: f64, cap: u32) -> TryGrant {
        let cap = cap.max(1);
        if self.held < self.slots_total && self.inflight_of(&key) < cap {
            self.grant_direct(key);
            return TryGrant::Granted;
        }
        if self.waiting_len() >= self.max_queue_depth {
            return TryGrant::Shed {
                would_be_position: self.max_queue_depth as u32 + 1,
            };
        }
        TryGrant::WouldQueue {
            position: self.hypothetical_position(weight),
        }
    }

    /// Reserve free slots for the best eligible waiters until no slot is free
    /// or no waiter is eligible. Returns their seqs so the shell can wake
    /// them to claim.
    pub fn promote(&mut self) -> Vec<u64> {
        let mut woken = Vec::new();
        while self.held < self.slots_total {
            match self.best_eligible() {
                Some(i) => {
                    self.waiters[i].granted = true;
                    self.held += 1;
                    let key = self.waiters[i].key.clone();
                    *self.inflight.entry(key).or_insert(0) += 1;
                    woken.push(self.waiters[i].seq);
                }
                None => break,
            }
        }
        woken
    }

    /// A woken waiter re-checks: claim its reserved slot, or report its
    /// current position and keep waiting.
    pub fn claim_or_status(&mut self, seq: u64) -> ClaimOrStatus {
        match self.waiters.iter().position(|w| w.seq == seq) {
            Some(i) if self.waiters[i].granted => {
                self.waiters.remove(i);
                ClaimOrStatus::Claimed
            }
            Some(_) => ClaimOrStatus::Waiting(self.status(self.position_of(seq))),
            None => ClaimOrStatus::Gone,
        }
    }

    /// Free a slot held by an in-flight turn for `key`, then promote. Returns
    /// the seqs of any waiters newly reserved a slot.
    pub fn release(&mut self, key: &K) -> Vec<u64> {
        self.dec_inflight(key);
        self.held = self.held.saturating_sub(1);
        self.promote()
    }

    /// Remove an abandoned waiter (its shell future was dropped — client
    /// disconnected mid-queue). If it had already been *granted* a slot, give
    /// the slot back and re-promote so the line keeps moving. Returns any
    /// newly-reserved seqs.
    pub fn cancel(&mut self, seq: u64) -> Vec<u64> {
        let Some(i) = self.waiters.iter().position(|w| w.seq == seq) else {
            return Vec::new();
        };
        let w = self.waiters.remove(i);
        if w.granted {
            self.dec_inflight(&w.key);
            self.held = self.held.saturating_sub(1);
            return self.promote();
        }
        Vec::new()
    }

    fn dec_inflight(&mut self, key: &K) {
        if let Some(c) = self.inflight.get_mut(key) {
            *c = c.saturating_sub(1);
            if *c == 0 {
                self.inflight.remove(key);
            }
        }
    }

    /// Fold a completed turn's wall time (ms) into the ETA EWMA.
    pub fn record_turn(&mut self, dur_ms: u64) {
        self.eta.record(dur_ms);
    }

    /// The shared ETA estimator, for callers that want to predict a wait
    /// without going through [`Self::status`].
    pub fn eta(&self) -> EtaEwma {
        self.eta
    }

    /// Decorate a bare position with an ETA, accounting for the N slots
    /// draining the queue in parallel.
    ///
    /// Passes `0` elapsed deliberately: this scheduler tracks principals and
    /// positions, not per-turn start times, so it has no in-flight signal to
    /// subtract. `0` is the CONSERVATIVE reading — "assume the running turn
    /// just started" — which over-states the wait rather than under-stating
    /// it. Stated rather than silently defaulted (ARCH §18.3); if this path
    /// ever gates a shed the way `SlotQueue` does, it needs a real elapsed
    /// term first, because the same omission is what made that gate refuse an
    /// empty queue (note `bf432b4d`).
    pub fn status(&self, position: u32) -> QueueStatus {
        QueueStatus {
            position,
            estimated_wait_ms: self.eta.predict_wait_ms(position, self.slots_total, 0),
        }
    }
}

#[cfg(test)]
mod tests;
