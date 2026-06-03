//! Shared watcher-liveness assessment for `lint_status`, `test_status`,
//! and `build`.
//!
//! ## The problem this closes
//!
//! These tools read a SQLite store the daemon's watcher writes to. For a
//! long time their only liveness signal was a one-shot `AtomicBool` the
//! daemon set to `true` once the coordinator started and never cleared.
//! That couldn't distinguish three very different states that all look
//! the same to a caller:
//!
//! 1. **Not configured** — no `[lint_runner]`/`[test_runner]` in
//!    `sovereign.toml`, so the coordinator never had a runner to start.
//! 2. **Configured but dead** — the coordinator started, then its loop
//!    task panicked or the OS watch was lost. The bool stayed `true`.
//! 3. **Live** — actually watching.
//!
//! In states 1 and 2 the store still holds the *last* completed run,
//! possibly days old. The tools labeled it `fresh_failing`/`fresh_passing`
//! — the word "fresh" actively lying — and the caller had to infer death
//! from `watcher_active: false` + a large `age_seconds`, then re-poll, then
//! give up and fall back to a direct `cargo` invocation. That inference
//! dance is the cost we're removing.
//!
//! ## The fix
//!
//! The coordinator now stamps a [`WatcherHeartbeat`](corpus_engine::WatcherHeartbeat)
//! every loop iteration. This module turns (heartbeat, configured?,
//! run-in-progress?, last-run-age) into a single [`WatcherReason`] that:
//!
//! - says *why* the watcher is or isn't live, with an actionable `hint`,
//!   and
//! - drives a status demotion: when the watcher isn't live, a completed
//!   run is reported as [`WATCHER_DOWN`] instead of `fresh_*`/`stale`, so
//!   the only correct action — fall back to a direct `cargo` run — is
//!   unambiguous from a single call.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use corpus_engine::WatcherHeartbeat;
use serde_json::{json, Value};

/// The coordinator stamps its heartbeat every `debounce/2` (sub-second
/// with the default 800ms debounce). 30s is comfortably longer than any
/// single loop iteration — including a flush that briefly awaits the
/// per-watcher `on_files_changed` dispatch — yet short enough that a
/// dead loop is flagged within half a minute.
pub const HEARTBEAT_LIVENESS_WINDOW_SECS: u64 = 30;

/// CLI-mode fallback window. With no heartbeat handle (the CLI process
/// reads the daemon's store but is not itself the daemon), a completed
/// run newer than this is weak evidence the watcher is probably live.
/// Only used when neither a heartbeat nor a legacy bool is wired.
pub const CLI_FRESH_SECS: u64 = 600;

/// The status string a completed-but-orphaned run is demoted to when the
/// watcher isn't live. Deliberately *not* `fresh_*`/`stale` — those imply
/// a watcher that will rerun on the next save.
pub const WATCHER_DOWN: &str = "watcher_down";

/// Why the watcher is (or isn't) considered live, with enough specificity
/// that the caller knows what to do next without cross-referencing docs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WatcherReason {
    /// Heartbeat fresh (or a run is in progress) — results are trustworthy.
    Live,
    /// No runner configured for this tool's scope. The shared coordinator
    /// may be alive for the *other* watcher, but there's nothing producing
    /// results here.
    NotConfigured,
    /// A runner is configured but the coordinator loop is not alive — it
    /// either failed to start or died after starting. Stored results are
    /// orphaned.
    WatcherDead,
    /// Legacy bool says live (no heartbeat handle wired). Kept for the
    /// `project serve` path that hasn't migrated to the heartbeat.
    LegacyActive,
    /// CLI mode: liveness inferred from a recent last-run age. Can't see
    /// the daemon's heartbeat directly.
    InferredFromAge,
    /// CLI mode and the last run is old or absent — can't confirm a live
    /// watcher at all.
    Unknown,
}

impl WatcherReason {
    pub fn as_str(self) -> &'static str {
        match self {
            WatcherReason::Live => "live",
            WatcherReason::NotConfigured => "not_configured",
            WatcherReason::WatcherDead => "watcher_dead",
            WatcherReason::LegacyActive => "legacy_active",
            WatcherReason::InferredFromAge => "inferred_from_age",
            WatcherReason::Unknown => "unknown",
        }
    }

    /// Whether results from the store should be trusted as reflecting the
    /// current file state. `InferredFromAge`/`LegacyActive` count as live
    /// — they're the best signal available without a heartbeat.
    pub fn is_live(self) -> bool {
        matches!(
            self,
            WatcherReason::Live | WatcherReason::LegacyActive | WatcherReason::InferredFromAge
        )
    }

    /// Actionable next step for the caller. `None` when live (nothing to do).
    ///
    /// When the watcher is down, the hint deliberately steers to the
    /// repo's **full-workspace scripts** — not an ad-hoc `cargo` call.
    /// A scoped `cargo -p <crate>` / `--test <name>` fallback under-covers
    /// the workspace, so regressions in untouched crates slip through and
    /// bugs accrete. `scripts/sovereign-lint.sh` / `scripts/sovereign-test.sh`
    /// run the same `cargo --workspace` surface the watcher does, so the
    /// fallback has equal coverage.
    pub fn hint(self) -> Option<&'static str> {
        match self {
            WatcherReason::Live | WatcherReason::LegacyActive => None,
            WatcherReason::NotConfigured => Some(
                "No lint/test runner is configured for this scope. Restore \
                 `.sovereign/sovereign.toml.with-watchers` (or add the \
                 [lint_runner]/[test_runner] section) and restart the daemon. \
                 Until then, get EQUAL coverage by running the full-workspace \
                 scripts `scripts/sovereign-lint.sh --human` and \
                 `scripts/sovereign-test.sh --human` — do NOT substitute a \
                 narrow `cargo -p <crate>` / `--test <name>` call, which \
                 under-covers the workspace and lets bugs accrete. \
                 `sovereign doctor` confirms the config state.",
            ),
            WatcherReason::WatcherDead => Some(
                "A runner is configured but the watcher coordinator is not \
                 alive — the result below is orphaned; do NOT trust it as \
                 current. The daemon's supervisor should restart it shortly; \
                 if it persists, `sovereign daemon restart`. Meanwhile run the \
                 full-workspace scripts `scripts/sovereign-lint.sh --human` / \
                 `scripts/sovereign-test.sh --human` (they cover every crate) \
                 — not a scoped `cargo -p <crate>` call, which under-covers \
                 and lets regressions in untouched crates slip through.",
            ),
            WatcherReason::InferredFromAge => Some(
                "CLI mode: liveness inferred from the last-run age; the \
                 daemon's heartbeat isn't visible from here.",
            ),
            WatcherReason::Unknown => Some(
                "CLI mode and a live watcher can't be confirmed (last run old \
                 or absent). Run the full-workspace scripts \
                 `scripts/sovereign-lint.sh --human` / \
                 `scripts/sovereign-test.sh --human` for full coverage — avoid \
                 a narrow `cargo -p <crate>` call, which under-covers the \
                 workspace and lets bugs accrete.",
            ),
        }
    }
}

/// Everything the assessment needs — all of which the tools already have
/// at the point they build their response.
pub struct WatcherHealthInputs<'a> {
    /// The shared heartbeat, when the daemon wired one (preferred signal).
    pub heartbeat: Option<&'a Arc<WatcherHeartbeat>>,
    /// Legacy one-shot bool, for callers not yet migrated to the heartbeat.
    pub legacy_active: Option<bool>,
    /// True when a `[lint_runner]`/`[test_runner]` is configured for THIS
    /// tool — derived from `watched_scope.is_some()`.
    pub configured: bool,
    /// A run is executing right now.
    pub run_in_progress: bool,
    /// Age of the last completed run, if any.
    pub last_run_age_secs: Option<u64>,
}

/// Decide the watcher's liveness reason. `configured` is checked first:
/// the heartbeat is shared across the lint + test watchers, so a live
/// coordinator does NOT mean *this* tool's runner exists.
pub fn assess(inputs: &WatcherHealthInputs) -> WatcherReason {
    // Daemon mode: a heartbeat handle is the authoritative signal.
    if let Some(hb) = inputs.heartbeat {
        if !inputs.configured {
            return WatcherReason::NotConfigured;
        }
        if inputs.run_in_progress {
            return WatcherReason::Live;
        }
        if hb.is_live(HEARTBEAT_LIVENESS_WINDOW_SECS) {
            return WatcherReason::Live;
        }
        // Configured + heartbeat stale or never-stamped == the loop is not
        // running. Failed-to-start and died-after-start are the same
        // actionable state from the caller's seat.
        return WatcherReason::WatcherDead;
    }

    // Legacy bool path (e.g. `project serve`, pre-heartbeat).
    if let Some(active) = inputs.legacy_active {
        if !inputs.configured {
            return WatcherReason::NotConfigured;
        }
        return if active {
            WatcherReason::LegacyActive
        } else {
            WatcherReason::WatcherDead
        };
    }

    // CLI mode: no liveness handle at all.
    if !inputs.configured {
        return WatcherReason::NotConfigured;
    }
    if inputs.run_in_progress {
        return WatcherReason::InferredFromAge;
    }
    match inputs.last_run_age_secs {
        Some(age) if age < CLI_FRESH_SECS => WatcherReason::InferredFromAge,
        _ => WatcherReason::Unknown,
    }
}

/// The `watcher` health object embedded in every status response. This is
/// the single field a caller reads to know whether to trust the rest of
/// the response.
pub fn watcher_json(
    reason: WatcherReason,
    heartbeat: Option<&Arc<WatcherHeartbeat>>,
    configured: bool,
) -> Value {
    json!({
        "live": reason.is_live(),
        "reason": reason.as_str(),
        "configured": configured,
        "heartbeat_age_secs": heartbeat.and_then(|h| h.age_secs()),
        "hint": reason.hint(),
    })
}

/// Demote a store-derived completed status when the watcher isn't live.
///
/// `fresh_passing`/`fresh_failing`/`stale` all imply a watcher that ran
/// recently and will rerun on the next save. When nothing is watching,
/// that implication is false and the stored run is orphaned — return
/// [`WATCHER_DOWN`] so the caller falls back instead of trusting a
/// possibly-ancient result. `running` and `never_run` are passed through
/// unchanged (their meaning is already liveness-independent).
pub fn apply_liveness(raw_status: &str, reason: WatcherReason) -> &str {
    if reason.is_live() {
        return raw_status;
    }
    match raw_status {
        "fresh_passing" | "fresh_failing" | "stale" => WATCHER_DOWN,
        other => other,
    }
}

/// Back-compat scalar: the old `watcher_active` boolean some callers and
/// docs still read. Equals `watcher.live`.
pub fn legacy_watcher_active(reason: WatcherReason) -> bool {
    reason.is_live()
}

/// Helper for tools that hold the legacy `Option<Arc<AtomicBool>>`.
pub fn read_legacy(flag: &Option<Arc<AtomicBool>>) -> Option<bool> {
    flag.as_ref().map(|f| f.load(Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs<'a>(
        heartbeat: Option<&'a Arc<WatcherHeartbeat>>,
        legacy_active: Option<bool>,
        configured: bool,
        run_in_progress: bool,
        last_run_age_secs: Option<u64>,
    ) -> WatcherHealthInputs<'a> {
        WatcherHealthInputs {
            heartbeat,
            legacy_active,
            configured,
            run_in_progress,
            last_run_age_secs,
        }
    }

    #[test]
    fn daemon_live_heartbeat_is_live() {
        let hb = WatcherHeartbeat::new();
        hb.stamp();
        let r = assess(&inputs(Some(&hb), None, true, false, Some(5)));
        assert_eq!(r, WatcherReason::Live);
        assert!(r.is_live());
    }

    #[test]
    fn daemon_configured_but_dead_heartbeat() {
        // Never-stamped heartbeat + configured == failed to start / died.
        let hb = WatcherHeartbeat::new();
        let r = assess(&inputs(Some(&hb), None, true, false, Some(99999)));
        assert_eq!(r, WatcherReason::WatcherDead);
        assert!(!r.is_live());
        assert!(r.hint().is_some());
    }

    #[test]
    fn daemon_not_configured_even_when_coordinator_alive() {
        // Shared heartbeat is live (other watcher), but THIS tool has no
        // runner — must report not_configured, not live.
        let hb = WatcherHeartbeat::new();
        hb.stamp();
        let r = assess(&inputs(Some(&hb), None, false, false, None));
        assert_eq!(r, WatcherReason::NotConfigured);
        assert!(!r.is_live());
    }

    #[test]
    fn legacy_bool_paths() {
        assert_eq!(
            assess(&inputs(None, Some(true), true, false, Some(5))),
            WatcherReason::LegacyActive
        );
        assert_eq!(
            assess(&inputs(None, Some(false), true, false, Some(5))),
            WatcherReason::WatcherDead
        );
        assert_eq!(
            assess(&inputs(None, Some(true), false, false, None)),
            WatcherReason::NotConfigured
        );
    }

    #[test]
    fn cli_mode_age_heuristic() {
        assert_eq!(
            assess(&inputs(None, None, true, false, Some(10))),
            WatcherReason::InferredFromAge
        );
        assert_eq!(
            assess(&inputs(None, None, true, false, Some(99999))),
            WatcherReason::Unknown
        );
        assert_eq!(
            assess(&inputs(None, None, false, false, None)),
            WatcherReason::NotConfigured
        );
    }

    #[test]
    fn apply_liveness_demotes_only_when_not_live() {
        // The exact bug from this session: a days-old failing run behind a
        // dead/absent watcher must NOT read `fresh_failing`.
        assert_eq!(
            apply_liveness("fresh_failing", WatcherReason::WatcherDead),
            WATCHER_DOWN
        );
        assert_eq!(
            apply_liveness("fresh_passing", WatcherReason::NotConfigured),
            WATCHER_DOWN
        );
        assert_eq!(
            apply_liveness("stale", WatcherReason::Unknown),
            WATCHER_DOWN
        );
        // Live → untouched.
        assert_eq!(
            apply_liveness("fresh_failing", WatcherReason::Live),
            "fresh_failing"
        );
        // never_run / running are liveness-independent → untouched.
        assert_eq!(
            apply_liveness("never_run", WatcherReason::NotConfigured),
            "never_run"
        );
        assert_eq!(
            apply_liveness("running", WatcherReason::WatcherDead),
            "running"
        );
    }

    #[test]
    fn watcher_json_shape() {
        let hb = WatcherHeartbeat::new();
        hb.stamp();
        let v = watcher_json(WatcherReason::Live, Some(&hb), true);
        assert_eq!(v["live"], true);
        assert_eq!(v["reason"], "live");
        assert_eq!(v["configured"], true);
        assert!(v["heartbeat_age_secs"].as_u64().is_some());
        assert!(v["hint"].is_null());

        let v2 = watcher_json(WatcherReason::NotConfigured, None, false);
        assert_eq!(v2["live"], false);
        assert_eq!(v2["reason"], "not_configured");
        assert!(v2["heartbeat_age_secs"].is_null());
        assert!(v2["hint"].is_string());
    }
}
