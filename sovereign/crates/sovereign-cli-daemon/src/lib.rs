// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign-cli-daemon` — long-running daemon host + setup + service
//! install + doctor, as BOTH a binary and a library.
//!
//! The library face exists for the desktop's supervised child-process
//! mode (DAEMON_RESILIENCE.md P0.1): the desktop binary detects a
//! `--daemon-child` argv and calls [`daemon_child_main`], so ONE daemon
//! bootstrap serves the CLI binary and the desktop child alike — no
//! ~241 MB sidecar duplicated into the installer, and the child
//! inherits every daemon defense (panic hook, supervised background
//! tasks, RAM-derived OOM limits, run lock, listener watchdog).

pub(crate) mod corpus_maintenance;
mod daemon_cmd;
mod doctor_cmd;
mod install_service_cmd;
mod listener_watch;
pub(crate) mod log_rotation;
mod memory_watch;
mod model_cmd;
mod panic_hook;
mod service_install;
mod setup_cmd;
mod setup_config;
pub(crate) mod supervise;
mod watcher_supervisor;

/// Keep the mesh self-manifest in step with the distributed primary's
/// lifecycle. Re-exported so the acceptance test can drive the REAL wiring
/// rather than a copy of it — the bug this closes was a missing subscription,
/// which a reimplementation in the test would silently paper over.
pub use daemon_cmd::bootstrap::spawn_self_manifest_refresh;

use sovereign_contracts::launch::Launch;
use sovereign_cli_shared::tracing_init::init_tracing;

/// Default tracing allowlist for `sovereign-cli-daemon daemon run`.
///
/// This is an ALLOWLIST WITH NO DEFAULT LEVEL: an event whose target matches
/// nothing here is dropped. Custom-target events (`tracing::info!(target: "…")`)
/// therefore need their target listed explicitly — a module-scoped directive
/// like `sovereign_core=info` does NOT catch them, because their target is the
/// literal string, not a module path. This bit us three times: `prefix_state`
/// and `post_stream` went silent for a whole A/B session (2026-07-12), then the
/// entire grounding/synthesis observability surface (the trust gate, the
/// agentic evidence loop, the retrieval audit, and the synthesis lifecycle —
/// all named targets) was dark in the deployed daemon until 2026-07-13. These
/// carry the load-bearing trust decisions an operator needs to see:
/// abstain/verify/hold verdicts, entity-anchor decisions, truncation and
/// continuation events, citation stripping. Keeping named targets (not module
/// paths) means `RUST_LOG=grounding_gate=debug` can still crank one subsystem
/// without drowning in the rest. `tests::daemon_filter_lists_grounding_targets`
/// pins this list so the surface cannot silently go dark a fourth time.
const DAEMON_TRACING_FILTER: &str = "sovereign_cli_daemon=info,\
     sovereign_core=info,\
     sovereign_mesh=info,\
     sovereign_inference=info,\
     corpus_engine=info,\
     commonwealth_discovery=info,\
     commonwealth_api=info,\
     prefix_state=info,\
     capability=info,\
     post_stream=info,\
     grounding_gate=info,\
     gate.call=info,\
     gate.lifecycle=info,\
     agentic_kq=info,\
     retrieval_audit=info,\
     synth.lifecycle=info,\
     synth.truncation=info,\
     synth.continue=info,\
     synth.refusal_retry=info,\
     synth.citation=info,\
     synth.budget=info,\
     placement=info,\
     mesh.decision=info,\
     compute_child=info,\
     sovereign_compute=info,\
     fim=info,\
     next_edit=info,\
     admission=info,\
     corpus_maintenance=info,\
     sec_edgar=info,\
     sec_facts=info,\
     sec_facts_render=info";

/// The daemon tracing filter plus the always-on iroh observability layer:
/// `commonwealth_transport` (endpoint egress posture) at info, and `iroh` /
/// `iroh_relay` at `warn` — so relay/discovery ERRORS are always visible in a
/// deployed daemon — cranked to `debug` when `iroh_debug` (env
/// `SOVEREIGN_IROH_LOG`, mirroring the `SOVEREIGN_IROH` kill-switch) for
/// diagnosing a reachability wedge. Built as one `iroh=<level>` directive so
/// there is no override ambiguity. `RUST_LOG`, if set, still overrides all.
///
/// Also carries `llama_cpp`, the LITERAL target every ggml/llama.cpp log line
/// rides (`sovereign_inference::llama::ggml_log_cb`). Its absence was the
/// fourth instance of the allowlist trap above, and the most expensive: it
/// silently defeated BOTH the model-load-failure surface that
/// `install_log_tracing_errors_only` exists to provide (a failed load reaching
/// the operator as a bare "null result from llama cpp") AND every
/// `GGML_RPC_DEBUG=1` investigation — the documented llama.cpp knob for
/// debugging an RPC worker emitted `GGML_LOG_DEBUG` lines that this filter
/// then dropped on the floor, so a worker-side probe returned a null result
/// from a structurally dead instrument (2026-07-27 distributed-inference
/// crash hunt). `llama_debug` cranks it to `debug` so `GGML_RPC_DEBUG` /
/// `SOVEREIGN_LLAMA_LOGS=1` reach the log; otherwise `info` keeps routine
/// load chatter out while WARN/ERROR still surface.
fn daemon_tracing_filter(iroh_debug: bool, llama_debug: bool) -> String {
    let lvl = if iroh_debug { "debug" } else { "warn" };
    let llama_lvl = if llama_debug { "debug" } else { "info" };
    // `transport=info`: the bridge/tunnel layer logs under the LITERAL target
    // "transport" (not the crate path), so without this token every bridge
    // dial failure is invisible — the 2026-07-19 mesh-heal investigation was
    // blind for exactly this reason. P0.5 observability requirement.
    format!(
        "{DAEMON_TRACING_FILTER},commonwealth_transport=info,transport=info,\
         iroh={lvl},iroh_relay={lvl},llama_cpp={llama_lvl}"
    )
}

/// True when the operator has asked for verbose ggml/llama.cpp output, by
/// either our own knob (`SOVEREIGN_LLAMA_LOGS=1`) or llama.cpp's own
/// documented RPC knob (`GGML_RPC_DEBUG`). Honouring the latter here is what
/// makes `GGML_RPC_DEBUG=1 sovereign daemon run` behave the way its upstream
/// documentation promises: the var alone gates `LOG_DBG` inside ggml-rpc.cpp,
/// but those lines are `GGML_LOG_DEBUG` and would still die at our callback
/// and again at this filter. One env var, all three gates.
fn llama_debug_requested() -> bool {
    sovereign_inference::llama_logs::LlamaLogs::from_env().is_verbose()
}

/// Process-level entry shared by the `sovereign-cli-daemon` binary and
/// the desktop `--daemon-child` arm. Sets the diagnostic env defaults,
/// runs the rebrand migration, installs the daemon panic hook (daemon
/// verb only), builds the 8 MiB-stack tokio runtime, and dispatches.
/// Returns the process exit code — the caller exits.
pub fn run_with_args(raw_args: Vec<String>) -> i32 {
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        std::env::set_var("RUST_BACKTRACE", "full");
    }
    if std::env::var_os("RUST_MIN_STACK").is_none() {
        std::env::set_var("RUST_MIN_STACK", "8388608");
    }

    // ONE decision about what this process becomes (ARCH §2.1, §10.6). Before
    // this, `run_with_args` re-derived it three times — `first() ==
    // Some("--compute-child")` here, `first() == Some("daemon")` for the panic
    // hook, and `match cmd` in `dispatch` — and the desktop's `main.rs` kept a
    // FOURTH list that disagreed with this one on ordering.
    let launch = Launch::parse(&raw_args, Launch::Bare);

    // Compute-child re-exec (DISTRIBUTED_PILOT_READINESS.md P1): the daemon's
    // ComputeChildManager spawns `current_exe() --compute-child …`. This is a
    // distinct inference process with its OWN runtime; it must skip the
    // daemon's rebrand migration / panic hook / 8 MiB runtime below (it
    // inherits the stack env vars set above). The success path never returns
    // — the child `fast_exit`s on SIGTERM.
    //
    // DELIBERATE BEHAVIOURAL DELTA (2026-08-24): this used to match only
    // `args[0]`, while the desktop matched the flag at ANY position. A child
    // re-exec carries `current_exe`'s argv, so the flag can legitimately sit
    // behind other arguments — the first-only form silently fell through to
    // verb dispatch and printed "unknown subcommand". `Launch::parse` finds it
    // at any position, so the two entry points now agree by construction.
    if let Launch::ComputeChild { args } = &launch {
        return sovereign_compute::child_main::run(args);
    }

    // Rebrand back-compat (see sovereign_core::rebrand): idempotent, non-destructive.
    // The daemon is the migration authority — it runs before binding the API port.
    sovereign_core::rebrand::promote_legacy_env();
    sovereign_core::rebrand::run_startup_migration();

    // Panic hook for the long-running daemon verb only (setup/doctor/
    // install-service are interactive CLI runs where the std default
    // suffices). Installed after the rebrand migration so the crash dir
    // lands in the post-migration data dir; before the runtime so even a
    // runtime-build panic leaves a record. (DAEMON_RESILIENCE.md P0.4 —
    // without this, a tokio worker-task panic was swallowed with no log
    // line and no artifact.)
    // `is_resident()` is the one implementation of "binds a long-lived
    // listener": it covers `daemon run` and `daemon run --worker-mode`, which
    // is exactly what the `first() == Some("daemon")` test covered.
    //
    // It is NOT what the run lock keys on, and an earlier version of this
    // comment said it was. Residency and data-root ownership are different
    // questions: `Launch::Worker` is resident and owns no persistent state at
    // all, so it has nothing to lock. The lock is keyed on the data root by
    // whoever is about to write it (`sovereign_contracts::run_lock`).
    if launch.is_resident() {
        let data_dir = sovereign_contracts::rebrand::svrnmesh_root();
        panic_hook::install(data_dir);
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(8 * 1024 * 1024)
        .thread_name("sovereign-cli-daemon-rt")
        .build()
        .expect("failed to build tokio runtime");
    runtime.block_on(dispatch(launch, &raw_args))
}

/// The desktop `--daemon-child` entry: exactly `daemon run`, nothing
/// else reachable. The desktop binary calls this BEFORE any Tauri
/// initialization, so the child is a plain headless daemon process
/// (DAEMON_RESILIENCE.md P0.1).
pub fn daemon_child_main() -> i32 {
    run_with_args(vec!["daemon".into(), "run".into()])
}

/// Dispatch on what this process became. Exhaustive over [`Launch`] on
/// purpose: adding a launch mode must not compile until this binary has
/// decided what it means here, including deciding it is unreachable.
///
/// `raw_args` is carried only for the `Bare` diagnostic — `Launch` answers
/// "what is this process", and "no launch matched" is one answer whether the
/// argv was empty or held a word this binary does not know. The two still
/// deserve different messages.
async fn dispatch(launch: Launch, raw_args: &[String]) -> i32 {
    // The daemon needs structured tracing for launchd / systemd
    // operators tailing logs. Match the filter sovereign-cli used
    // pre-split.
    match &launch {
        Launch::Daemon { .. } | Launch::Worker { .. } => {
            // Track W: `SOVEREIGN_IROH_LOG` cranks iroh/relay/transport internals to
            // debug for diagnosing a reachability wedge; off, they stay at warn
            // (errors still visible) so the log isn't flooded.
            let iroh_debug = std::env::var_os("SOVEREIGN_IROH_LOG").is_some();
            init_tracing(&daemon_tracing_filter(iroh_debug, llama_debug_requested()));
        }
        Launch::Verb { name, .. } if name == "setup" => {
            init_tracing("sovereign_cli_daemon=info");
        }
        _ => {}
    }

    match launch {
        Launch::Daemon { ref args } | Launch::Worker { ref args } => {
            daemon_cmd::run(&launch, &args.clone()).await
        }
        Launch::Verb { name, args } => match name.as_str() {
            "model" => model_cmd::run(&args).await,
            "setup" => setup_cmd::run_setup(&args).await,
            "install-service" => install_service_cmd::run(&args).await,
            "doctor" => doctor_cmd::run_doctor(&args).await,
            // `ONE_SHOT_VERBS` is the closed set `Launch::parse` matches on;
            // a name reaching here means that list and this one disagree.
            other => {
                eprintln!("sovereign-cli-daemon: verb '{other}' is declared in Launch but not wired here");
                2
            }
        },
        Launch::Bare => match raw_args.first() {
            None => {
                eprintln!(
                    "sovereign-cli-daemon: usage: sovereign-cli-daemon <subcommand> [args...]"
                );
                2
            }
            Some(other) => {
                eprintln!("sovereign-cli-daemon: unknown subcommand '{other}'");
                2
            }
        },
        // Handled before the runtime is built; listed so the match stays
        // exhaustive rather than falling through a wildcard.
        Launch::ComputeChild { .. } => unreachable!("compute-child returns in run_with_args"),
        // Other binaries' launches. Named explicitly so that adding a variant
        // forces a decision here instead of silently landing in a `_` arm.
        Launch::Desktop | Launch::Server | Launch::Smoketest { .. } => {
            eprintln!(
                "sovereign-cli-daemon: {} is not a launch this binary serves",
                launch.as_str()
            );
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DAEMON_TRACING_FILTER;
    use sovereign_contracts::launch::Launch;

    /// The smoketest token has ONE owner — `sovereign_inference::smoketest::
    /// SMOKETEST_FLAG`, next to the implementation. `sovereign-contracts` sits
    /// below `sovereign-inference` and cannot name it, so `Launch::parse`
    /// matches the literal. This test is the seam: it feeds the OWNER's
    /// constant into the parser and fails if either side drifts. Lives here
    /// because this is a crate that can see both.
    #[test]
    fn launch_smoketest_flag_matches_owner() {
        let argv = vec![sovereign_inference::smoketest::SMOKETEST_FLAG.to_string()];
        assert_eq!(
            Launch::parse(&argv, Launch::Bare).as_str(),
            "smoketest",
            "sovereign-inference renamed SMOKETEST_FLAG without updating Launch::parse"
        );
    }

    /// The three tokens `launch.rs` owns must be what spawn sites send. Pins
    /// the round trip a bare literal at a spawn site would silently break.
    #[test]
    fn every_owned_launch_flag_round_trips_through_parse() {
        use sovereign_contracts::launch::{
            COMPUTE_CHILD_FLAG, DAEMON_CHILD_FLAG, WORKER_MODE_FLAG,
        };
        let one = |a: &str| Launch::parse(&[a.to_string()], Launch::Bare);
        assert_eq!(one(DAEMON_CHILD_FLAG).as_str(), "daemon");
        assert_eq!(one(COMPUTE_CHILD_FLAG).as_str(), "compute-child");
        // Worker is reached only via the `daemon` verb, never bare.
        let worker = Launch::parse(
            &["daemon".into(), "run".into(), WORKER_MODE_FLAG.to_string()],
            Launch::Bare,
        );
        assert_eq!(worker.as_str(), "worker");
        assert!(worker.is_resident());
    }

    /// The daemon tracing filter is an allowlist with NO default level, so every
    /// custom-target observability event must be listed by name or it goes dark
    /// in the deployed daemon (see the `DAEMON_TRACING_FILTER` doc comment for
    /// the three times this bit us). This pins the grounding/synthesis
    /// trust-decision targets. It fails if:
    ///   - a directive stops parsing — e.g. a dotted target like
    ///     `synth.truncation` were rejected by `EnvFilter` (`parse`, unlike
    ///     `parse_lossy`, returns `Err` on ANY malformed directive), or
    ///   - one of the required targets is dropped from the list (the `Display`
    ///     round-trip only contains directives that actually survived parsing).
    #[test]
    fn daemon_filter_lists_grounding_targets() {
        let filter = tracing_subscriber::EnvFilter::builder()
            .parse(DAEMON_TRACING_FILTER)
            .expect("daemon tracing filter must parse (dotted targets included)");
        let rendered = filter.to_string();
        for target in [
            // Grounding trust gate + synthesis lifecycle: the load-bearing
            // decisions an operator must be able to see in the deployed daemon.
            "grounding_gate",
            "gate.call",
            "gate.lifecycle",
            "agentic_kq",
            "retrieval_audit",
            // Distributed-inference placement: distributed-vs-local + the
            // per-device split. An operator must see this in the deployed
            // daemon, never infer it from `free`.
            "placement",
            // Compute-child lifecycle transitions (P1). The glassbox source
            // for "distributed across N children / warming / recovering".
            "compute_child",
            // Routing decision records (SCHEDULER_QUALITY.md Phase 0 P1).
            // The surface that answers "why did this request go to the hub,
            // and was that right in hindsight" from a deployed daemon's
            // logs. Dark without this entry, which would leave the whole
            // scheduler-quality loop blind in exactly the deployment it
            // exists to measure.
            "mesh.decision",
            // FIM inline completion (INLINE_COMPLETION.md): slot install,
            // alias-mode decisions, per-request stop outcomes — the
            // "why did my ghost text do that" surface.
            "fim",
            // Next-edit rule lane (NEXT_EDIT.md §9): per-settle
            // fired/silent decisions with support/sites/reason — the
            // "why did/didn't it suggest" surface.
            "next_edit",
            // Measured model-capability verdicts + probe/ladder
            // disagreement (embedded::capabilities). This is the
            // shadow-phase collection surface: without it the whole
            // point of probing — finding out whether the arch ladder
            // ever lies — is invisible in the deployed daemon.
            "capability",
            "synth.lifecycle",
            "synth.truncation",
            "synth.continue",
            "synth.refusal_retry",
            "synth.citation",
            "synth.budget",
            // Self-healing corpus maintenance (`corpus_maintenance.rs`). This
            // was instance FIVE of the trap this test exists to prevent: the
            // sweep shipped in 92602386 emitting every line under the literal
            // target `corpus_maintenance`, which `sovereign_cli_daemon=info`
            // does not match. Caught 2026-08-05 by restarting the daemon and
            // watching for the "sweep armed" line that never came. Because the
            // allowlist has no default level, this was not merely "the debug
            // detail is missing" — the arm line, the per-corpus maintenance
            // results, and BOTH `warn!` failure paths were all dropped, so a
            // sweep that never ran and one failing on every corpus looked
            // identical to a healthy one from the deployed daemon's logs.
            "corpus_maintenance",
            // Client + peer admission decisions (order `serve50-identity`,
            // MESH_SCALE_100_USERS_1000_CORPORA.md §9.3). Names the principal
            // a turn was charged to, how that principal was identified, and
            // the equal share it was measured against. Without this entry the
            // refusal that makes the node FAIR is indistinguishable, from the
            // logs, from the node being broken — and the per-request decision
            // at `debug` would be unreachable even with the directive raised.
            "admission",
            // SEC filings install + figure answering (FINANCIAL_CORPORA.md
            // §7). This was the NEXT instance of the trap, and it cost a
            // 20.5-minute e2e run to find: `sec_edgar` names every install
            // decision — ticker -> CIK, which 10-K was selected, which were
            // SKIPPED and why, First/Refresh/Replaces — and `sec_facts` names
            // every refusal. All of it rode literal targets that this
            // allowlist did not carry, so an install by ticker was TOTALLY
            // SILENT in the deployed daemon: the acquire phase logged
            // nothing, and because the allowlist has no default level the
            // `warn!` in `install_fact_sidecar` (the one that says figures
            // will refuse until a store is rendered) was dropped too. A
            // 20-minute acquire and a wedged one looked identical.
            "sec_edgar",
            "sec_facts",
            "sec_facts_render",
            // Pre-existing custom targets, guarded against accidental removal.
            "prefix_state",
            "post_stream",
        ] {
            assert!(
                rendered.contains(target),
                "daemon tracing filter is missing custom target `{target}` — events \
                 under it would be silently dropped by the allowlist. Rendered: {rendered}"
            );
        }
    }

    /// The load-bearing check: does the daemon filter actually ENABLE events at
    /// these custom targets? Listing a target in the string is necessary but not
    /// obviously sufficient — this pins the real EnvFilter matching behaviour so
    /// we stop guessing about it. Captures the target of every event the filter
    /// lets through and asserts the grounding/synthesis targets survive while an
    /// unlisted one is dropped.
    #[test]
    fn daemon_filter_enables_custom_target_events() {
        use std::sync::{Arc, Mutex};
        use tracing_subscriber::layer::{Context, SubscriberExt};
        use tracing_subscriber::Layer;

        #[derive(Clone)]
        struct Capture(Arc<Mutex<Vec<String>>>);
        impl<S: tracing::Subscriber> Layer<S> for Capture {
            fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
                self.0
                    .lock()
                    .unwrap()
                    .push(event.metadata().target().to_string());
            }
        }

        let seen = Arc::new(Mutex::new(Vec::new()));
        // Build the filter EXACTLY as `init_tracing` does on the RUST_LOG-unset
        // path: `default_filter.into()` → `EnvFilter::new` (which sets an ERROR
        // default directive for unmatched targets) plus the lance silencer. Using
        // `builder().parse()` here would NOT reproduce the daemon and could mask a
        // real matching difference.
        let filter = tracing_subscriber::EnvFilter::new(DAEMON_TRACING_FILTER).add_directive(
            "lance::dataset_events=warn"
                .parse()
                .expect("lance directive parses"),
        );
        let subscriber = tracing_subscriber::registry()
            .with(filter)
            .with(Capture(seen.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "grounding_gate", "probe");
            tracing::info!(target: "gate.call", "probe");
            tracing::info!(target: "synth.truncation", "probe");
            tracing::info!(target: "fim", "probe");
            tracing::info!(target: "mesh.decision", "probe");
            tracing::info!(target: "corpus_maintenance", "probe");
            // The sweep's failure paths are `warn!`, and an allowlist with no
            // default level drops unmatched targets at ERROR — so "the target
            // is listed" must also mean "its warnings arrive".
            tracing::warn!(target: "corpus_maintenance", "probe-warn");
            tracing::info!(target: "sovereign_core", "probe"); // module control: enabled
            tracing::info!(target: "definitely_unlisted_zzz", "probe"); // control: dropped
        });
        let seen = seen.lock().unwrap().clone();
        for want in [
            "grounding_gate",
            "gate.call",
            "synth.truncation",
            "fim",
            "mesh.decision",
            "corpus_maintenance",
            "sovereign_core",
        ] {
            assert!(
                seen.iter().any(|t| t == want),
                "filter dropped an event at target `{want}` — the allowlist does NOT \
                 enable it. seen={seen:?}"
            );
        }
        assert!(
            !seen.iter().any(|t| t == "definitely_unlisted_zzz"),
            "filter leaked an UNLISTED target — allowlist is not actually restricting. \
             seen={seen:?}"
        );
    }

    /// Track W: the iroh observability layer must parse in both postures and
    /// flip iroh/relay warn↔debug with the `SOVEREIGN_IROH_LOG` toggle, without
    /// disturbing the base allowlist.
    #[test]
    fn daemon_filter_iroh_toggle() {
        for iroh in [false, true] {
            for llama in [false, true] {
                let f = super::daemon_tracing_filter(iroh, llama);
                tracing_subscriber::EnvFilter::builder()
                    .parse(&f)
                    .expect("daemon tracing filter (with iroh layer) must parse");
            }
        }
        let off = super::daemon_tracing_filter(false, false);
        let on = super::daemon_tracing_filter(true, false);
        assert!(off.contains("commonwealth_transport=info"));
        assert!(off.contains("iroh=warn") && off.contains("iroh_relay=warn"));
        assert!(on.contains("iroh=debug") && on.contains("iroh_relay=debug"));
    }

    /// The `llama_cpp` target must be in the deployed daemon's filter at
    /// BOTH postures, and must reach `debug` when the operator asks for
    /// verbose ggml output.
    ///
    /// Absent, two surfaces go dark with no error anywhere: a failed model
    /// load loses the ggml text that explains it (the whole reason
    /// `install_log_tracing_errors_only` replaced `void_logs`), and
    /// `GGML_RPC_DEBUG=1` — llama.cpp's own documented knob for debugging an
    /// RPC worker — produces a log containing not one `LOG_DBG` line, which
    /// reads as "the worker received no traffic" rather than "the instrument
    /// was never connected". That misread cost a round-trip of the
    /// distributed-inference crash hunt on 2026-07-27.
    #[test]
    fn daemon_filter_carries_llama_cpp_target() {
        let quiet = super::daemon_tracing_filter(false, false);
        let verbose = super::daemon_tracing_filter(false, true);
        assert!(
            quiet.contains("llama_cpp=info"),
            "llama_cpp must be allowlisted even when quiet, or a failed model \
             load reaches the operator as a bare null result: {quiet}"
        );
        assert!(
            verbose.contains("llama_cpp=debug"),
            "GGML_RPC_DEBUG / SOVEREIGN_LLAMA_LOGS=1 must lift llama_cpp to \
             debug, or ggml LOG_DBG output is dropped by the subscriber: {verbose}"
        );
        // The rendered filter must actually admit a DEBUG event on that
        // target — `contains` alone would pass on a directive EnvFilter
        // silently dropped as malformed.
        let rendered = tracing_subscriber::EnvFilter::builder()
            .parse(&verbose)
            .expect("verbose daemon filter must parse")
            .to_string();
        assert!(
            rendered.contains("llama_cpp=debug"),
            "llama_cpp=debug did not survive EnvFilter parsing: {rendered}"
        );
    }
}
