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
        assert_eq!(
            t.observe_write(Some("src/lib.rs")),
            ThrashSignal::Continue
        );
        assert_eq!(t.same_path_writes(), 1);
    }

    #[test]
    fn same_path_three_kills_at_threshold() {
        let mut t = ThrashTracker::new();
        assert_eq!(
            t.observe_write(Some("src/lib.rs")),
            ThrashSignal::Continue
        );
        assert_eq!(
            t.observe_write(Some("src/lib.rs")),
            ThrashSignal::Continue
        );
        let sig = t.observe_write(Some("src/lib.rs"));
        assert!(matches!(sig, ThrashSignal::Kill { same_path_writes: 3 }));
    }

    #[test]
    fn two_same_path_writes_does_not_kill() {
        let mut t = ThrashTracker::new();
        assert_eq!(
            t.observe_write(Some("src/lib.rs")),
            ThrashSignal::Continue
        );
        assert_eq!(
            t.observe_write(Some("src/lib.rs")),
            ThrashSignal::Continue
        );
        assert_eq!(t.same_path_writes(), 2);
    }

    #[test]
    fn verify_resets_between_writes() {
        let mut t = ThrashTracker::new();
        t.observe_write(Some("src/lib.rs"));
        t.observe_verify();
        assert_eq!(
            t.observe_write(Some("src/lib.rs")),
            ThrashSignal::Continue
        );
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
        assert!(matches!(sig, ThrashSignal::Kill { same_path_writes: 3 }));
    }

    #[test]
    fn different_paths_do_not_kill() {
        // FromScratch tier: model legitimately scaffolds multiple
        // files before its first verify. Cross-file writes reset
        // the counter.
        let mut t = ThrashTracker::new();
        assert_eq!(
            t.observe_write(Some("Cargo.toml")),
            ThrashSignal::Continue
        );
        assert_eq!(
            t.observe_write(Some("src/lib.rs")),
            ThrashSignal::Continue
        );
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
