//! Streaming detectors shared by every agent runner.
//!
//! Per ARCH §10.3 (helper extraction at the third caller): the
//! `ThrashTracker` originated in `runners/pi.rs`. With the native
//! runner landing as a second consumer, it moves here so both can
//! depend on one impl + one threshold constant.
//!
//! Detectors are pure data — they observe tool events and emit
//! kill signals. Subprocess control (SIGTERM, exit-reason
//! mapping) stays in the per-runner module that owns the
//! subprocess.

/// Maximum consecutive `write` tool calls to the **same path**
/// without an interleaving `bash` (or `cargo_*`) call. Tuned per
/// the 2026-05-21 sweep: productive trials write ≤2 times to the
/// same path before verifying; thrash trials write 3+. M=3 catches
/// the canonical incoherent-overlay pattern without false-positive
/// on iterate-after-read.
pub const SAME_PATH_WRITE_THRESHOLD: u32 = 3;

/// Streaming state for the write-thrash detector. The
/// `observe_*` methods return `ThrashSignal::Kill` when the caller
/// should terminate the agent run.
#[derive(Debug, Default, Clone)]
pub struct ThrashTracker {
    same_path_writes: u32,
    last_write_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThrashSignal {
    Continue,
    Kill { same_path_writes: u32 },
}

impl ThrashTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Observe a `write` to the supplied path (None when the parser
    /// failed to surface the path field — treated as a fresh
    /// write). Returns `Kill` when the same-path counter crosses
    /// `SAME_PATH_WRITE_THRESHOLD`.
    pub fn observe_write(&mut self, path: Option<&str>) -> ThrashSignal {
        match (&self.last_write_path, path) {
            (Some(prev), Some(curr)) if prev.as_str() == curr => {
                self.same_path_writes = self.same_path_writes.saturating_add(1);
            }
            _ => {
                self.same_path_writes = 1;
                self.last_write_path = path.map(str::to_string);
            }
        }
        if self.same_path_writes >= SAME_PATH_WRITE_THRESHOLD {
            ThrashSignal::Kill {
                same_path_writes: self.same_path_writes,
            }
        } else {
            ThrashSignal::Continue
        }
    }

    /// Observe a verification step (`bash`, `cargo_build`,
    /// `cargo_smoke`). Verification happened — slate is clean.
    pub fn observe_verify(&mut self) {
        self.same_path_writes = 0;
        self.last_write_path = None;
    }

    pub fn same_path_writes(&self) -> u32 {
        self.same_path_writes
    }

    pub fn last_write_path(&self) -> Option<&str> {
        self.last_write_path.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_write_does_not_kill() {
        let mut t = ThrashTracker::new();
        assert_eq!(t.observe_write(Some("src/lib.rs")), ThrashSignal::Continue);
        assert_eq!(t.same_path_writes(), 1);
    }

    #[test]
    fn same_path_three_kills_at_threshold() {
        let mut t = ThrashTracker::new();
        assert_eq!(t.observe_write(Some("src/lib.rs")), ThrashSignal::Continue);
        assert_eq!(t.observe_write(Some("src/lib.rs")), ThrashSignal::Continue);
        let sig = t.observe_write(Some("src/lib.rs"));
        assert!(matches!(
            sig,
            ThrashSignal::Kill {
                same_path_writes: 3
            }
        ));
    }

    #[test]
    fn two_same_path_writes_does_not_kill() {
        let mut t = ThrashTracker::new();
        assert_eq!(t.observe_write(Some("src/lib.rs")), ThrashSignal::Continue);
        assert_eq!(t.observe_write(Some("src/lib.rs")), ThrashSignal::Continue);
        assert_eq!(t.same_path_writes(), 2);
    }

    #[test]
    fn verify_resets_between_writes() {
        let mut t = ThrashTracker::new();
        t.observe_write(Some("src/lib.rs"));
        t.observe_verify();
        assert_eq!(t.observe_write(Some("src/lib.rs")), ThrashSignal::Continue);
        assert_eq!(t.same_path_writes(), 1);
    }

    #[test]
    fn post_verify_three_writes_fires() {
        // Reproduce trial-3 thrash mode from historic 2.2 retest:
        // write→bash→write→bash→write→bash→write→bash→write→write→write
        // The three trailing writes (no intervening verify) fire at
        // threshold 3.
        let mut t = ThrashTracker::new();
        for _ in 0..4 {
            t.observe_write(Some("src/lib.rs"));
            t.observe_verify();
        }
        t.observe_write(Some("src/lib.rs")); // fresh after verify
        t.observe_write(Some("src/lib.rs")); // same path, no verify
        let sig = t.observe_write(Some("src/lib.rs")); // fires
        assert!(matches!(
            sig,
            ThrashSignal::Kill {
                same_path_writes: 3
            }
        ));
    }

    #[test]
    fn different_paths_do_not_kill() {
        // FromScratch tier: model legitimately scaffolds multiple
        // files before its first verify. Cross-file writes reset
        // the counter.
        let mut t = ThrashTracker::new();
        assert_eq!(t.observe_write(Some("Cargo.toml")), ThrashSignal::Continue);
        assert_eq!(t.observe_write(Some("src/lib.rs")), ThrashSignal::Continue);
        assert_eq!(
            t.observe_write(Some("tests/integration.rs")),
            ThrashSignal::Continue
        );
        assert_eq!(t.same_path_writes(), 1);
    }

    #[test]
    fn missing_path_treated_as_fresh() {
        let mut t = ThrashTracker::new();
        assert_eq!(t.observe_write(None), ThrashSignal::Continue);
        assert_eq!(t.observe_write(None), ThrashSignal::Continue);
        assert_eq!(t.same_path_writes(), 1);
    }
}

// ── VerifyStuckTracker ────────────────────────────────────────────────
//
// Closes loop classes L5 (same compiler error N cycles) and L6 (same
// smoke failure N cycles) — and most instances of L4 (alternating
// non-convergent Implementer↔Evaluator). Mechanism: hash the
// stdout_tail of each FAILING verification; when the same hash
// repeats `VERIFY_STUCK_THRESHOLD` times in a row, the model isn't
// learning from the error and the next iteration won't either.
//
// Successful verifications RESET the counter — the agent earned
// forward progress and shouldn't pay for the prior failures.
//
// Distinct from L1 (same-primitive-in-role): a model can call build
// → handoff → write → build (with a flip between each build) and
// L1 never trips because consecutive_same_primitive resets on role
// flip. VerifyStuck is role-flip-invariant by design.
//
// Universal across languages: rustc, go vet, tsc, pytest all produce
// deterministic stdout for deterministic inputs. Hash collisions
// across different errors are astronomically unlikely.

/// Same failing verification this many times in a row → kill.
/// Tuning: 2 is too aggressive (a model that's making 1-line fixes
/// to a multi-error file may see the same first error twice while
/// productively whittling); 4 wastes a cycle. 3 is the sweet spot.
pub const VERIFY_STUCK_THRESHOLD: u32 = 3;

#[derive(Debug, Default, Clone)]
pub struct VerifyStuckTracker {
    last_failing_hash: Option<u64>,
    consecutive_same: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifySignal {
    Continue,
    Kill { hash_repeats: u32 },
}

impl VerifyStuckTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Observe one Build or Smoke result. `ok=true` resets the
    /// counter (forward progress). `ok=false` hashes the stdout_tail
    /// and increments / resets the same-hash counter.
    pub fn observe(&mut self, ok: bool, stdout_tail: &str) -> VerifySignal {
        if ok {
            self.last_failing_hash = None;
            self.consecutive_same = 0;
            return VerifySignal::Continue;
        }
        let hash = hash_stdout(stdout_tail);
        match self.last_failing_hash {
            Some(prev) if prev == hash => {
                self.consecutive_same = self.consecutive_same.saturating_add(1);
            }
            _ => {
                self.consecutive_same = 1;
                self.last_failing_hash = Some(hash);
            }
        }
        if self.consecutive_same >= VERIFY_STUCK_THRESHOLD {
            VerifySignal::Kill {
                hash_repeats: self.consecutive_same,
            }
        } else {
            VerifySignal::Continue
        }
    }

    pub fn consecutive_same(&self) -> u32 {
        self.consecutive_same
    }
}

fn hash_stdout(s: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

// ── HandoffCycleCounter ───────────────────────────────────────────────
//
// Closes loop class L4/L7/L17 — non-convergent Implementer↔Evaluator
// alternation. Hard ceiling regardless of whether the model varies
// its output. A productive 2.1-class SINGLE-FILE problem converges
// in 1-3 cycles. But multi-bug / multi-file problems (4.x, 5.x) fix
// one bug per cycle by design, so they legitimately need ~bug-count
// cycles: cap=6 mis-fired on 5.1-minilang (7 bugs across 3 files),
// killing a CONVERGENT run at 16/24 (judge dim_b=3, "one bug per
// cycle with precision") while it was still climbing. Genuine
// non-convergence is caught independently and tighter by
// ThrashTracker (same-signature sticky-retry) and VerifyStuckTracker,
// so this secondary alternation ceiling can be generous without
// reopening the token-burn loop class.

/// Maximum complete Implementer↔Evaluator round-trips (counted on
/// `handoff_to_implementer` — Evaluator giving up on this attempt)
/// before the run terminates as non-convergent. Raised 6 → 14 on
/// 2026-06-03 after 5.1-minilang (7-bug multi-file) was capped
/// mid-convergence; ≈ 2× the hardest current problem's bug count,
/// still well short of the 30+ turn token-burn shape, and the
/// sticky / verify-stuck detectors guard genuine loops.
pub const HANDOFF_CYCLE_CAP: u32 = 14;

#[derive(Debug, Default, Clone)]
pub struct HandoffCycleCounter {
    cycles: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CycleSignal {
    Continue,
    Kill { cycles: u32 },
}

impl HandoffCycleCounter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Increment on every `handoff_to_implementer` (Evaluator
    /// concluding "not done yet — Implementer retry").
    pub fn observe_handoff_to_implementer(&mut self) -> CycleSignal {
        self.cycles = self.cycles.saturating_add(1);
        if self.cycles >= HANDOFF_CYCLE_CAP {
            CycleSignal::Kill {
                cycles: self.cycles,
            }
        } else {
            CycleSignal::Continue
        }
    }

    pub fn cycles(&self) -> u32 {
        self.cycles
    }
}

#[cfg(test)]
mod verify_stuck_tests {
    use super::*;

    #[test]
    fn ok_verification_resets_counter() {
        let mut v = VerifyStuckTracker::new();
        v.observe(false, "error A");
        v.observe(false, "error A");
        assert_eq!(v.consecutive_same(), 2);
        let sig = v.observe(true, "");
        assert_eq!(sig, VerifySignal::Continue);
        assert_eq!(v.consecutive_same(), 0);
    }

    #[test]
    fn same_failure_three_times_fires() {
        let mut v = VerifyStuckTracker::new();
        assert_eq!(v.observe(false, "error A"), VerifySignal::Continue);
        assert_eq!(v.observe(false, "error A"), VerifySignal::Continue);
        let sig = v.observe(false, "error A");
        assert!(matches!(sig, VerifySignal::Kill { hash_repeats: 3 }));
    }

    #[test]
    fn distinct_failures_reset_counter() {
        let mut v = VerifyStuckTracker::new();
        v.observe(false, "error A");
        v.observe(false, "error B");
        assert_eq!(v.consecutive_same(), 1);
    }

    #[test]
    fn alternating_distinct_failures_never_fire() {
        // Genuine convergent fixing: each fix changes the error.
        let mut v = VerifyStuckTracker::new();
        for i in 0..20 {
            let msg = format!("error {i}");
            assert_eq!(v.observe(false, &msg), VerifySignal::Continue);
        }
    }
}

#[cfg(test)]
mod cycle_tests {
    use super::*;

    #[test]
    fn under_cap_continues() {
        let mut c = HandoffCycleCounter::new();
        for _ in 0..(HANDOFF_CYCLE_CAP - 1) {
            assert_eq!(c.observe_handoff_to_implementer(), CycleSignal::Continue);
        }
    }

    #[test]
    fn at_cap_fires() {
        let mut c = HandoffCycleCounter::new();
        for _ in 0..(HANDOFF_CYCLE_CAP - 1) {
            let _ = c.observe_handoff_to_implementer();
        }
        let sig = c.observe_handoff_to_implementer();
        assert!(matches!(
            sig,
            CycleSignal::Kill {
                cycles: HANDOFF_CYCLE_CAP,
            }
        ));
    }
}
