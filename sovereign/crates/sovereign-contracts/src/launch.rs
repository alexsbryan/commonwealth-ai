// SPDX-License-Identifier: AGPL-3.0-or-later
//! What this process becomes — decided once, at the entry point.
//!
//! # Why this type exists
//!
//! Before it, "which process am I" was an emergent property of argv checks
//! scattered across two crates. `sovereign-desktop/src-tauri/src/main.rs`
//! tested `--smoketest`, then `--daemon-child`, then `--compute-child`, then
//! fell through to Tauri. `sovereign-cli-daemon::run_with_args` tested
//! `--compute-child`, then string-matched five verbs. The two lists overlapped,
//! disagreed on ordering, and neither could be enumerated without reading both
//! files — so a reader could not answer "what can this binary become?" and a
//! reviewer could not tell whether a new arm was exhaustive.
//!
//! That is the concrete cost recorded in `quality/TOPOLOGY.md §1`: one seat
//! spent three subagents and ~2h deriving the top level and still concluded
//! that binaries were the unit of topology. They are not — **modes are**. The
//! desktop binary is four processes, and `--daemon-child` *is* the daemon
//! (`sovereign_cli_daemon::daemon_child_main`).
//!
//! # What it buys
//!
//! One closed set (ARCH §2), parsed by one decider (ARCH §10.6), matched
//! exhaustively at every entry point. A new launch mode cannot be added
//! without the compiler visiting every entry that dispatches on one. This is
//! the *completeness* half of the topology claim: no binary reaches a runtime
//! posture that no variant names.
//!
//! # What it deliberately does NOT do
//!
//! It does not carry configuration, and it does not decide policy. A `Launch`
//! answers "which process is this" and nothing else — port numbers, wiring and
//! capability sets belong to whatever the launch constructs. Keeping it that
//! narrow is what lets it live in Tier 0 with no dependencies.

/// The closed set of things a first-party binary can become.
///
/// Ordering of the parse is significant and is fixed here rather than at each
/// call site: the child modes are tested before the verbs, because a child
/// re-exec must never be mistaken for a CLI invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    /// The long-lived daemon: binds the client port and the peer port, owns
    /// models and mesh identity. Reached by `sovereign-cli-daemon daemon run`
    /// and, identically, by the desktop's `--daemon-child` re-exec.
    Daemon {
        /// Remaining args after the `daemon` verb.
        args: Vec<String>,
    },

    /// The container entrypoint: `daemon run --worker-mode`. A different
    /// server on a different socket (`0.0.0.0` + peer port, seed-derived TLS)
    /// with its own router, so it is a distinct launch rather than a flag on
    /// [`Launch::Daemon`].
    Worker {
        /// Remaining args after the `daemon` verb, `--worker-mode` included.
        args: Vec<String>,
    },

    /// An inference child owning the weights for one slot. Binds an ephemeral
    /// loopback port and announces it on stdout. Spawned as
    /// `current_exe() --compute-child …`, so ANY binary that can be
    /// `current_exe` must handle this variant.
    ComputeChild {
        /// Args after the `--compute-child` flag.
        args: Vec<String>,
    },

    /// A crash probe: load one model, decode one token, exit. Spawned by the
    /// desktop before it loads a model into the user-facing slot.
    Smoketest {
        /// The full argv, which `smoketest` parses itself.
        argv: Vec<String>,
    },

    /// The desktop GUI shell. Owns no domain state of its own; reaches a
    /// daemon over HTTP, and may supervise one as a child process.
    Desktop,

    /// The multi-tenant HTTP server (`sovereign-server --config <path>`).
    ///
    /// Like [`Launch::Desktop`] this is a binary's `default_ui` rather than a
    /// flag: `sovereign-server` parses its own `--config`, which is required.
    /// Two paths reach it — the desktop supervises one as the opt-in mobile
    /// access host (`mobile_host_setup::start`), and `svrn mobile` **`exec`s**
    /// it, replacing its own process image (`mobile_cmd.rs:152`).
    ///
    /// It is named here because it is **resident**: it binds a long-lived
    /// listener and owns tenant state. Its absence from this set is why an
    /// orphaned instance was found squatting `0.0.0.0:8080` for six days with
    /// no crash reporting and no refusal (`quality/TOPOLOGY.md` hazards 4,
    /// 10) — nothing that keys on [`Launch::is_resident`] could see it.
    ///
    /// In the target it is a *surface*, not an assembler: it speaks the turn
    /// protocol to the daemon rather than building a `Runtime`. Being resident
    /// and being an assembler are different questions, and this variant is the
    /// place that distinction is written down.
    Server,

    /// A run-once command that dispatches and exits.
    Verb {
        /// The verb name, e.g. `setup`, `doctor`, `model`.
        name: String,
        /// Args after the verb.
        args: Vec<String>,
    },

    /// No verb given. Distinct from [`Launch::Desktop`] because a CLI binary
    /// invoked bare must print usage, not start a GUI — collapsing the two is
    /// how a headless host ends up trying to open a window.
    Bare,
}

/// The verbs that dispatch and exit. Adding one is an edit here, not a string
/// literal at a call site (ARCH §2.1: a `match` on string ids with more than
/// three arms is a closed set wearing the wrong clothes).
const ONE_SHOT_VERBS: &[&str] = &["model", "setup", "install-service", "doctor"];

/// Re-exec into the daemon. The desktop passes this to its own binary.
///
/// The tokens below are public because a **spawner** must name the same string
/// the **parser** reads. Six spawn sites held bare literals as of 2026-08-24
/// (`supervisor_setup.rs`, `sovereign-compute/src/manager.rs`, a mesh test);
/// a literal at the spawn site can drift from this parser silently, and the
/// process then falls through to verb dispatch instead of becoming a child.
pub const DAEMON_CHILD_FLAG: &str = "--daemon-child";

/// Re-exec into an inference compute child. May appear at ANY argv position:
/// the child carries `current_exe`'s argv, so matching only `args[0]` misses
/// it (see `a_child_flag_is_found_at_any_position`).
pub const COMPUTE_CHILD_FLAG: &str = "--compute-child";

/// Turns `daemon run` into [`Launch::Worker`] — a different server on a
/// different socket, not a flag on the daemon.
pub const WORKER_MODE_FLAG: &str = "--worker-mode";

// NOTE — the smoketest token is deliberately NOT declared here.
// `sovereign_inference::smoketest::SMOKETEST_FLAG` already owns it, next to
// the smoketest implementation, and the desktop re-exports it from there.
// Declaring a second copy would be the §10.6 smell this module exists to
// remove. `sovereign-contracts` sits BELOW `sovereign-inference`, so it cannot
// name that constant; the two are pinned equal by a test in a crate that can
// see both (`sovereign-cli-daemon`, `launch_smoketest_flag_matches_owner`).

impl Launch {
    /// Decide what this process is, from argv **excluding** `argv[0]`.
    ///
    /// Total: every input yields a variant. `default_ui` selects what a bare
    /// invocation means for this binary — the desktop passes
    /// [`Launch::Desktop`], every headless binary passes [`Launch::Bare`].
    /// Passing it explicitly is what stops a GUI default leaking into a
    /// headless host by omission.
    pub fn parse(args: &[String], default_ui: Launch) -> Launch {
        // Child re-execs first: these are spawned by us, with a flag that can
        // appear at any position, and must never fall through to verb
        // matching.
        if let Some(i) = args.iter().position(|a| a == COMPUTE_CHILD_FLAG) {
            return Launch::ComputeChild {
                args: args[i + 1..].to_vec(),
            };
        }
        if args.iter().any(|a| a == DAEMON_CHILD_FLAG) {
            return Launch::Daemon { args: vec![] };
        }
        if args.iter().any(|a| a == "--smoketest") {
            return Launch::Smoketest {
                argv: args.to_vec(),
            };
        }

        let Some(first) = args.first().map(String::as_str) else {
            return default_ui;
        };
        let rest = args[1..].to_vec();

        if first == "daemon" {
            return if rest.iter().any(|a| a == WORKER_MODE_FLAG) {
                Launch::Worker { args: rest }
            } else {
                Launch::Daemon { args: rest }
            };
        }
        if ONE_SHOT_VERBS.contains(&first) {
            return Launch::Verb {
                name: first.to_string(),
                args: rest,
            };
        }
        default_ui
    }

    /// Does this launch bind a long-lived listener?
    ///
    /// One implementation, because call sites previously each re-derived it
    /// from `args.first() == Some("daemon")` and the desktop's child arm
    /// silently disagreed with them.
    ///
    /// Deliberately NOT the predicate for the run lock. That keys on the DATA
    /// ROOT, which is a different question with a different answer:
    /// [`Launch::Worker`] is resident and owns no persistent state (an
    /// ephemeral pod boots from a bootstrap blob and exits), while
    /// [`Launch::Desktop`] is not resident yet owns a data root whenever it
    /// runs its own in-process daemon. See [`crate::run_lock`].
    pub fn is_resident(&self) -> bool {
        matches!(
            self,
            Launch::Daemon { .. } | Launch::Worker { .. } | Launch::Server
        )
    }

    /// A short, stable name for logs and diagnostics.
    pub fn as_str(&self) -> &'static str {
        match self {
            Launch::Daemon { .. } => "daemon",
            Launch::Worker { .. } => "worker",
            Launch::ComputeChild { .. } => "compute-child",
            Launch::Smoketest { .. } => "smoketest",
            Launch::Desktop => "desktop",
            Launch::Server => "server",
            Launch::Verb { .. } => "verb",
            Launch::Bare => "bare",
        }
    }
}

/// Where the desktop's daemon RUNS — the one structural question the two
/// launch-topology env flags answer, as a value resolved at construction.
///
/// ## Why this is a type and not two `env::var` calls
///
/// `quality/TOPOLOGY.md` §10 Phase 10: a flag whose value selects *which code
/// path runs* is structural and folds into a profile variant (§2.1 — a closed
/// set belongs in an enum); a flag that merely tunes a value the same path uses
/// is data and becomes a field read **once at construction**. These two are the
/// first kind, and they were being read at THREE points of use through
/// `supervisor_setup::is_enabled()`, which is invisible to go-to-definition:
/// a reader following "does the desktop supervise a child?" landed on a
/// predicate over the process environment rather than on a decision the
/// process made.
///
/// It lives beside [`Launch`] because it is the same family of question —
/// what shape this process takes — and `Launch::parse` is already the one
/// place that answers it. Phase 10's falsifier is *no `env::var` read selects
/// behaviour after construction*; this is one flag pair off that count.
///
/// ## What it is NOT
///
/// It is not a second topology. §10 decision 2: embedded versus supervised is
/// only *where* the daemon runs — same construction, same routes. That is why
/// this is a separate question from `Launch`, rather than two more `Launch`
/// variants: both shapes still assemble `DaemonServices::Desktop`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonHost {
    /// A supervised child process (`current_exe() --daemon-child`, the
    /// desktop binary re-entering as a headless daemon). The default since the
    /// W1 flip (DAEMON_RESILIENCE.md P0.1, 2026-07-18): a ggml/llama.cpp crash
    /// kills the child, not the window.
    SupervisedChild,
    /// An in-process `EmbeddedDaemon` — this process owns the data root and
    /// runs the weights. Carries WHY, because the two reasons have different
    /// operational meanings and a log line saying only "supervisor disabled"
    /// cannot tell an operator which one they are in (§18.3).
    InProcess(InProcessReason),
}

/// Why a desktop process hosts its own daemon instead of supervising one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InProcessReason {
    /// `SOVEREIGN_FORCE_LOCAL=1` — "THIS process runs the weights": the
    /// real-mode desktop harnesses and the run-local-while-a-daemon-is-up
    /// power case. Takes precedence, because a child daemon contradicts it.
    ForceLocal,
    /// `SOVEREIGN_USE_SUPERVISOR=0` (or `false`) — the kill-switch back to the
    /// pre-W1 shape. (`=1`/`true`, the old opt-IN spelling, is accepted and
    /// redundant.)
    KillSwitch,
}

impl DaemonHost {
    /// Resolve from the environment. **Call this once, at construction** —
    /// that is the entire point of the type, and a second call site is the
    /// defect it removes.
    pub fn from_env() -> Self {
        // Order is load-bearing and matches the predicate this replaced:
        // FORCE_LOCAL wins, because "this process runs the weights" is
        // incompatible with a child daemon however the other flag is set.
        if std::env::var("SOVEREIGN_FORCE_LOCAL").is_ok_and(|v| v == "1") {
            return Self::InProcess(InProcessReason::ForceLocal);
        }
        if std::env::var("SOVEREIGN_USE_SUPERVISOR")
            .is_ok_and(|v| v == "0" || v.eq_ignore_ascii_case("false"))
        {
            return Self::InProcess(InProcessReason::KillSwitch);
        }
        Self::SupervisedChild
    }

    /// True when the desktop spawns and supervises a daemon child.
    pub fn is_supervised(self) -> bool {
        matches!(self, Self::SupervisedChild)
    }

    /// Stable name for logs. Closed set — ARCH §2.1.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SupervisedChild => "supervised-child",
            Self::InProcess(InProcessReason::ForceLocal) => "in-process (SOVEREIGN_FORCE_LOCAL=1)",
            Self::InProcess(InProcessReason::KillSwitch) => {
                "in-process (SOVEREIGN_USE_SUPERVISOR=0)"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_string()).collect()
    }

    fn parse(args: &[&str]) -> Launch {
        Launch::parse(&v(args), Launch::Bare)
    }

    #[test]
    fn a_bare_invocation_takes_the_caller_s_default_not_a_baked_in_one() {
        assert_eq!(Launch::parse(&[], Launch::Bare), Launch::Bare);
        assert_eq!(Launch::parse(&[], Launch::Desktop), Launch::Desktop);
    }

    #[test]
    fn the_daemon_verb_and_the_desktop_child_reach_the_same_variant() {
        // The load-bearing equivalence: `--daemon-child` IS `daemon run`.
        // A reader who does not know this concludes the desktop is a
        // separate runtime — which is exactly what happened (TOPOLOGY §1).
        assert!(parse(&["daemon", "run"]).is_resident());
        assert!(parse(&["--daemon-child"]).is_resident());
        assert_eq!(parse(&["--daemon-child"]).as_str(), "daemon");
    }

    /// Worker mode is a different server on a different socket, so it must not
    /// collapse into `Daemon` — the container entrypoint depends on it.
    #[test]
    fn worker_mode_is_its_own_launch_not_a_daemon_flag() {
        let w = parse(&["daemon", "run", "--worker-mode"]);
        assert_eq!(w.as_str(), "worker");
        assert!(w.is_resident());
        assert_ne!(w, parse(&["daemon", "run"]));
    }

    /// A child re-exec carries `current_exe`'s argv, so the flag can sit
    /// behind other args. Matching only on `args[0]` misses it, and the
    /// process then falls through to the GUI or to usage.
    #[test]
    fn a_child_flag_is_found_at_any_position() {
        assert_eq!(parse(&["--compute-child", "--slot", "0"]).as_str(), "compute-child");
        assert_eq!(parse(&["--quiet", "--compute-child", "--slot", "0"]).as_str(), "compute-child");
        assert_eq!(parse(&["--quiet", "--daemon-child"]).as_str(), "daemon");
    }

    #[test]
    fn compute_child_keeps_only_the_args_after_its_flag() {
        let Launch::ComputeChild { args } = parse(&["--compute-child", "--slot", "0"]) else {
            panic!("expected ComputeChild");
        };
        assert_eq!(args, v(&["--slot", "0"]));
    }

    /// A child re-exec must win over verb matching. Were the order reversed, a
    /// child spawned with a verb-shaped argument would boot a second daemon.
    #[test]
    fn a_child_re_exec_outranks_a_verb() {
        assert_eq!(parse(&["daemon", "--compute-child"]).as_str(), "compute-child");
    }

    #[test]
    fn one_shot_verbs_are_verbs_and_an_unknown_word_is_not() {
        for name in ONE_SHOT_VERBS {
            assert_eq!(parse(&[name]).as_str(), "verb", "{name}");
        }
        assert_eq!(parse(&["definitely-not-a-verb"]), Launch::Bare);
    }

    /// `is_resident` is the predicate three call sites used to re-derive. Pin
    /// both directions so a new variant has to decide deliberately.
    /// `Server` is reached the way `Desktop` is — as a binary's `default_ui`,
    /// never by a flag — because `sovereign-server` parses its own `--config`.
    #[test]
    fn the_server_is_a_default_ui_not_a_flag() {
        assert_eq!(Launch::parse(&[], Launch::Server), Launch::Server);
        assert_eq!(
            Launch::parse(&v(&["--config", "/x.toml"]), Launch::Server),
            Launch::Server
        );
        assert_eq!(Launch::Server.as_str(), "server");
    }

    #[test]
    fn only_the_three_resident_launches_are_resident() {
        assert!(parse(&["daemon", "run"]).is_resident());
        assert!(parse(&["daemon", "run", "--worker-mode"]).is_resident());
        assert!(!parse(&["--compute-child"]).is_resident());
        assert!(!parse(&["--smoketest", "--model", "x.gguf"]).is_resident());
        assert!(!parse(&["setup"]).is_resident());
        assert!(!Launch::Desktop.is_resident());
        assert!(!Launch::Bare.is_resident());
        // Resident: it binds a listener and owns tenant state. This is the
        // assertion whose absence left an orphaned server unlocked and
        // unreaped for six days.
        assert!(Launch::Server.is_resident());
    }
}

#[cfg(test)]
mod daemon_host_tests {
    use super::*;

    /// The precedence is the whole content of `from_env`, and it is the half a
    /// reader is most likely to get wrong: both flags set is not a
    /// contradiction, it is FORCE_LOCAL winning.
    ///
    /// Env is process-global, so this drives the classification directly
    /// rather than mutating the environment — a test that sets vars races
    /// every other test in the binary, and the thing worth pinning is the
    /// ORDER, not `std::env`.
    fn classify(force_local: Option<&str>, use_supervisor: Option<&str>) -> DaemonHost {
        if force_local == Some("1") {
            return DaemonHost::InProcess(InProcessReason::ForceLocal);
        }
        if use_supervisor.is_some_and(|v| v == "0" || v.eq_ignore_ascii_case("false")) {
            return DaemonHost::InProcess(InProcessReason::KillSwitch);
        }
        DaemonHost::SupervisedChild
    }

    #[test]
    fn supervised_is_the_default_and_the_legacy_opt_in_is_redundant() {
        assert_eq!(classify(None, None), DaemonHost::SupervisedChild);
        assert_eq!(classify(None, Some("1")), DaemonHost::SupervisedChild);
        assert_eq!(classify(None, Some("true")), DaemonHost::SupervisedChild);
    }

    #[test]
    fn the_kill_switch_names_itself() {
        assert_eq!(
            classify(None, Some("0")),
            DaemonHost::InProcess(InProcessReason::KillSwitch)
        );
        assert_eq!(
            classify(None, Some("FALSE")),
            DaemonHost::InProcess(InProcessReason::KillSwitch)
        );
    }

    #[test]
    fn force_local_wins_over_the_supervisor_default_and_over_an_explicit_opt_in() {
        assert_eq!(
            classify(Some("1"), None),
            DaemonHost::InProcess(InProcessReason::ForceLocal)
        );
        assert_eq!(
            classify(Some("1"), Some("1")),
            DaemonHost::InProcess(InProcessReason::ForceLocal)
        );
    }

    /// Every shape says which one it is. A single "supervisor disabled" line
    /// cannot tell an operator whether a harness set FORCE_LOCAL or a user
    /// tripped the kill-switch (§18.3).
    #[test]
    fn every_shape_names_itself_distinctly() {
        let all = [
            DaemonHost::SupervisedChild,
            DaemonHost::InProcess(InProcessReason::ForceLocal),
            DaemonHost::InProcess(InProcessReason::KillSwitch),
        ];
        let mut seen = std::collections::HashSet::new();
        for h in all {
            assert!(seen.insert(h.as_str()), "duplicate label: {}", h.as_str());
        }
        assert!(!DaemonHost::InProcess(InProcessReason::KillSwitch).is_supervised());
        assert!(DaemonHost::SupervisedChild.is_supervised());
    }

    /// `from_env` must agree with the classification the tests above pin —
    /// otherwise they pin a function nothing calls (§18.4). Read whatever the
    /// ambient environment happens to be and require the two to match.
    #[test]
    fn from_env_agrees_with_the_pinned_classification() {
        let fl = std::env::var("SOVEREIGN_FORCE_LOCAL").ok();
        let us = std::env::var("SOVEREIGN_USE_SUPERVISOR").ok();
        assert_eq!(
            DaemonHost::from_env(),
            classify(fl.as_deref(), us.as_deref())
        );
    }
}
