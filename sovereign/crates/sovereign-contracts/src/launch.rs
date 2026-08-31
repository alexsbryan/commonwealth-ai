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

    /// The ggml RPC worker behind a process boundary: serves this node's
    /// local GPU to mesh peers over the raw ggml RPC protocol, and owns no
    /// model, no data root and no mesh identity.
    ///
    /// It exists because that protocol is the one surface here that feeds
    /// PEER-supplied bytes into llama.cpp, and ggml enforces its bounds with
    /// `GGML_ASSERT` — an unconditional `abort()`, not a rejected message. In
    /// process, one malformed tensor takes down the daemon holding the mesh
    /// key and the conversation store; out of process it takes down a child
    /// the supervisor re-spawns. Spawned as `current_exe() --rpc-worker …`.
    RpcWorker {
        /// Args after the `--rpc-worker` flag.
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

/// Spawns the out-of-process ggml RPC worker ([`Launch::RpcWorker`]). Set by
/// `sovereign-inference`'s supervisor when `SOVEREIGN_RPC_WORKER_PROCESS=1`,
/// on a `current_exe()` re-exec — so, like [`COMPUTE_CHILD_FLAG`], ANY binary
/// that can be `current_exe` must route this variant instead of falling
/// through to verb matching.
pub const RPC_WORKER_FLAG: &str = "--rpc-worker";

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
        if let Some(i) = args.iter().position(|a| a == RPC_WORKER_FLAG) {
            return Launch::RpcWorker {
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
            Launch::RpcWorker { .. } => "rpc-worker",
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

/// The shared-model fleet this node belongs to — one reading of the two
/// values that decide it, instead of three.
///
/// ## The split this closes
///
/// `SOVEREIGN_SHARED_MODEL_ID` is not configuration a user sets; on the
/// daemon path it is written by `apply_shared_model_role_to_env` from
/// `[shared_model] model_id` and read back by three modules in two crates.
/// That makes the process environment an inter-module contract, which is
/// `TOPOLOGY.md` §2's point exactly: a value read at the point of use is not
/// configuration, it is *state*.
///
/// The three readers had also drifted apart on what counts as a value. Two
/// filtered on `!s.is_empty()`; the third on `!s.trim().is_empty()`. So a
/// configured id of `"  "` made this node **advertise a fleet it does not
/// join**: `capabilities` published `model_resident: Some("  ")` and
/// `/v1/mesh/status` reported a shared-model fleet, while the inference
/// provider declined to route into it. One accessor, one trim rule, and that
/// disagreement has nowhere to live (ARCH §10.6, principle 8).
///
/// ## Where it belongs
///
/// Beside [`DaemonHost`], for the same reason: it is a fact about what this
/// process IS, resolved once, and `launch.rs` is where those live. Phase 10's
/// falsifier is *no `env::var` read selects behaviour after construction* —
/// this is two more reads off that count, and the remaining work is to hold
/// the resolved value on the daemon rather than re-resolving it per request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SharedModelFleet {
    /// This node answers from its own weights. Nothing is advertised, and
    /// `/v1/mesh/status` carries no shared-model section.
    Solo,
    /// This node is in a fleet serving one model across pooled anchors.
    Member {
        /// The fleet's model id, trimmed and non-empty by construction.
        model_id: String,
        /// Eligible anchors required before the host will distribute.
        quorum_anchors: u32,
    },
}

/// Anchors required when nothing says otherwise — the historic
/// `unwrap_or(1)` at the status site, named.
pub const DEFAULT_QUORUM_ANCHORS: u32 = 1;

impl SharedModelFleet {
    /// Resolve from the environment. **Call this once, at construction** —
    /// same rule as [`DaemonHost::from_env`], and for the same reason.
    pub fn from_env() -> Self {
        Self::resolve(
            std::env::var("SOVEREIGN_SHARED_MODEL_ID").ok().as_deref(),
            std::env::var("SOVEREIGN_RPC_QUORUM_ANCHORS")
                .ok()
                .as_deref(),
        )
    }

    /// THE decider, separated from the environment so it can be tested
    /// against the inputs that produced the split — process env is global
    /// state and a test that sets it is a test that races its neighbours.
    pub fn resolve(model_id: Option<&str>, quorum_anchors: Option<&str>) -> Self {
        let Some(model_id) = model_id
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
        else {
            return Self::Solo;
        };
        Self::Member {
            model_id,
            quorum_anchors: quorum_anchors
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(DEFAULT_QUORUM_ANCHORS),
        }
    }

    /// The fleet's model id, or `None` when this node is [`Self::Solo`].
    pub fn model_id(&self) -> Option<&str> {
        match self {
            Self::Solo => None,
            Self::Member { model_id, .. } => Some(model_id),
        }
    }

    /// Anchors required before the host distributes. Defined for `Solo` too,
    /// because a caller asking the question has a number to use either way.
    pub fn quorum_anchors(&self) -> u32 {
        match self {
            Self::Solo => DEFAULT_QUORUM_ANCHORS,
            Self::Member { quorum_anchors, .. } => *quorum_anchors,
        }
    }
}

/// Whether this node lends its GPU into a shared-model layer-split, and where
/// it accepts — one reading of `SOVEREIGN_RPC_SERVE` instead of four.
///
/// ## The lie this closes
///
/// Four modules in four crates parsed this variable independently and did not
/// agree on what "set" means:
///
/// | site | rule |
/// |---|---|
/// | `sovereign-inference::rpc_distribution` (the one that BINDS) | trim, reject empty |
/// | `sovereign-mesh::iroh_access` | trim, reject empty, parse port |
/// | `commonwealth-api::routes_status` | parse port (empty fails it incidentally) |
/// | `sovereign-mesh::capabilities` | **`var_os(..).is_some()` — any value at all** |
///
/// So `SOVEREIGN_RPC_SERVE=""` made this node gossip `can_anchor: true` with
/// its full VRAM while nothing bound, nothing advertised a port, and the iroh
/// acceptor routed no `RPC_ALPN`. A host running discovery would count it as
/// an eligible anchor and reach a worker that does not exist — the anchor
/// equivalent of the whitespace-model-id split [`SharedModelFleet`] closes.
///
/// The rule kept is the binding site's, because that is the only one with
/// ground truth: if `serve_rpc_worker_if_configured` would not bind on it, no
/// surface may advertise it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcServe {
    /// Not an RPC worker. Advertise nothing.
    Off,
    /// Accepting ggml RPC on `bind`.
    On {
        /// The bind string as configured, trimmed (e.g. `0.0.0.0:50052`).
        bind: String,
    },
}

impl RpcServe {
    /// Resolve from the environment. **Call this once, at construction.**
    pub fn from_env() -> Self {
        Self::resolve(std::env::var("SOVEREIGN_RPC_SERVE").ok().as_deref())
    }

    /// THE decider, separated from the environment so the disagreement above
    /// can be tested without racing another test's `set_var`.
    pub fn resolve(raw: Option<&str>) -> Self {
        match raw.map(str::trim).filter(|s| !s.is_empty()) {
            Some(bind) => Self::On {
                bind: bind.to_string(),
            },
            None => Self::Off,
        }
    }

    /// The bind address, or `None` when this node serves nothing.
    pub fn bind(&self) -> Option<&str> {
        match self {
            Self::Off => None,
            Self::On { bind } => Some(bind),
        }
    }

    /// The TCP port from the bind address. `None` when off, or when the bind
    /// carries no parseable port — a malformed bind is not a worker, and
    /// reporting it as one is the failure this type exists to prevent.
    pub fn port(&self) -> Option<u16> {
        self.bind()?.rsplit(':').next()?.parse().ok()
    }

    /// True when this node lends a GPU shard to the layer-split.
    pub fn is_serving(&self) -> bool {
        matches!(self, Self::On { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The input the four readers disagreed on: an empty bind made
    /// `capabilities` gossip `can_anchor: true` while nothing bound.
    #[test]
    fn an_empty_bind_is_not_a_worker() {
        assert_eq!(RpcServe::resolve(Some("")), RpcServe::Off);
        assert_eq!(RpcServe::resolve(Some("   ")), RpcServe::Off);
        assert_eq!(RpcServe::resolve(None), RpcServe::Off);
        assert!(!RpcServe::resolve(Some("")).is_serving());
        assert_eq!(RpcServe::resolve(Some("")).port(), None);
    }

    #[test]
    fn a_configured_worker_reports_its_bind_and_port() {
        let serve = RpcServe::resolve(Some("  0.0.0.0:50052\n"));
        assert!(serve.is_serving());
        assert_eq!(serve.bind(), Some("0.0.0.0:50052"));
        assert_eq!(serve.port(), Some(50052));
    }

    /// A bind with no parseable port is serving-but-unadvertisable: the type
    /// says so rather than publishing a port nobody listens on.
    #[test]
    fn a_portless_bind_advertises_nothing() {
        let serve = RpcServe::resolve(Some("not-an-address"));
        assert!(serve.is_serving());
        assert_eq!(serve.port(), None);
    }

    /// The exact input the three divergent readers disagreed on. Two treated
    /// `"  "` as a model id and one did not, so the node advertised a fleet
    /// its own inference provider refused to join.
    #[test]
    fn a_whitespace_model_id_is_not_a_fleet() {
        assert_eq!(
            SharedModelFleet::resolve(Some("  "), None),
            SharedModelFleet::Solo
        );
        assert_eq!(
            SharedModelFleet::resolve(Some(""), None),
            SharedModelFleet::Solo
        );
        assert_eq!(
            SharedModelFleet::resolve(None, Some("3")),
            SharedModelFleet::Solo
        );
    }

    /// A named id is trimmed once, here, so no downstream reader has to.
    #[test]
    fn a_named_fleet_carries_a_trimmed_id_and_its_quorum() {
        assert_eq!(
            SharedModelFleet::resolve(Some(" qwen3.8-27b \n"), Some(" 3 ")),
            SharedModelFleet::Member {
                model_id: "qwen3.8-27b".to_string(),
                quorum_anchors: 3,
            }
        );
        // An unparseable quorum falls back rather than refusing the fleet —
        // matching the `unwrap_or(1)` the status site always had.
        assert_eq!(
            SharedModelFleet::resolve(Some("m"), Some("not-a-number")).quorum_anchors(),
            DEFAULT_QUORUM_ANCHORS
        );
    }

    /// `Solo` still answers the quorum question, so a caller never has to
    /// invent a number when there is no fleet.
    #[test]
    fn solo_has_no_model_and_the_default_quorum() {
        assert_eq!(SharedModelFleet::Solo.model_id(), None);
        assert_eq!(
            SharedModelFleet::Solo.quorum_anchors(),
            DEFAULT_QUORUM_ANCHORS
        );
    }

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

    /// The RPC worker is a child re-exec like `--compute-child`: it must keep
    /// only the args after its own flag, and must be found wherever the flag
    /// sits in argv.
    #[test]
    fn rpc_worker_keeps_only_the_args_after_its_flag() {
        let Launch::RpcWorker { args } = parse(&["--rpc-worker", "--bind", "127.0.0.1:50052"])
        else {
            panic!("expected RpcWorker");
        };
        assert_eq!(args, v(&["--bind", "127.0.0.1:50052"]));
        assert_eq!(
            parse(&["--quiet", "--rpc-worker", "--bind", "x:1"]).as_str(),
            "rpc-worker"
        );
    }

    /// The RPC worker owns no data root and binds nothing the run lock knows
    /// about, so it must NOT be resident — residency installs the daemon panic
    /// hook against a data dir this process does not own.
    #[test]
    fn rpc_worker_is_not_resident_and_is_not_a_daemon() {
        let w = parse(&["--rpc-worker", "--bind", "x:1"]);
        assert!(!w.is_resident());
        assert_ne!(w, parse(&["daemon", "run"]));
    }

    /// Same trap the compute child has: a worker spawned with a verb-shaped
    /// argument must not boot a second daemon.
    #[test]
    fn the_rpc_worker_flag_outranks_a_verb() {
        assert_eq!(parse(&["daemon", "--rpc-worker"]).as_str(), "rpc-worker");
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
        assert_eq!(
            parse(&["--compute-child", "--slot", "0"]).as_str(),
            "compute-child"
        );
        assert_eq!(
            parse(&["--quiet", "--compute-child", "--slot", "0"]).as_str(),
            "compute-child"
        );
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
        assert_eq!(
            parse(&["daemon", "--compute-child"]).as_str(),
            "compute-child"
        );
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
