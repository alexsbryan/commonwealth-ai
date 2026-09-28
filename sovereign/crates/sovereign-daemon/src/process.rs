// SPDX-License-Identifier: AGPL-3.0-or-later
//! The svrn daemon's ONE process entry: the launch decision, the rebrand
//! migration, the panic hook, the tracing subscriber and the runtime. Moved
//! out of `bin/sovereign-daemon.rs` (pb-stock-binary) so every binary that
//! runs svrn calls the same entry rather than a copy of it (principle 8).
//! The argv contract is the bin's doc.

use crate::daemon_cmd;
pub use crate::serve_client::HostedServe;
use sovereign_contracts::launch::Launch;

/// The daemon-verb slice of the old dispatcher. Returns the process
/// exit code — `main` exits with it.
///
/// `hosted` is the distribution's composition when this process hosts serve
/// too (the stock binary); `None` for svrn alone, which dials a configured
/// serve (phase-b-29 Q1, Q2).
pub fn run(raw_args: &[String], hosted: Option<HostedServe>) -> i32 {
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        std::env::set_var("RUST_BACKTRACE", "full");
    }
    if std::env::var_os("RUST_MIN_STACK").is_none() {
        std::env::set_var("RUST_MIN_STACK", "8388608");
    }

    // ONE decision about what this process becomes (ARCH §2.1, §10.6),
    // made by the same parser the old dispatcher used. The verb is
    // re-prepended because the shim (and a unit file) passes only what
    // FOLLOWED it — and because `Launch::parse` finds the child re-exec
    // flags at any argv position, the `--compute-child` / `--rpc-worker`
    // re-execs of this very binary land in their arms either way.
    let mut full_args = vec!["daemon".to_string()];
    full_args.extend_from_slice(raw_args);
    let launch = Launch::parse(&full_args, Launch::Bare);

    // Compute-child re-exec: a distinct inference process with its OWN
    // runtime; it must skip the rebrand migration / panic hook / 8 MiB
    // runtime below (it inherits the stack env vars set above).
    if let Launch::ComputeChild { args } = &launch {
        return sovereign_compute::child_main::run(args);
    }

    // RPC-worker re-exec: same rule — owns no data root, needs no tokio.
    if let Launch::RpcWorker { args } = &launch {
        return sovereign_inference::rpc_worker_main::run(args);
    }

    // The setup wizard's join child: the admin assembly, never the full
    // serving one, and no canonical config required. No rebrand migration
    // or panic hook — it owns only the caller-named config's data dir. Warn
    // by default, as the wizard's in-process join logged; RUST_LOG overrides.
    if let Launch::AdminJoin { config, node_name } = &launch {
        init_tracing("warn");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("sovereign-admin-join-rt")
            .build()
            .expect("failed to build tokio runtime");
        return runtime.block_on(daemon_cmd::admin_join::run(
            &launch,
            config.as_deref(),
            node_name.as_deref(),
        ));
    }

    // Not a launch this binary serves (a `--smoketest` argv, the
    // desktop/server defaults…). Same refusal the old dispatcher made.
    let Launch::Daemon { args } = &launch else {
        eprintln!(
            "sovereign-daemon: {} is not a launch this binary serves",
            launch.as_str()
        );
        return 2;
    };

    // Rebrand back-compat (see sovereign_core::rebrand): idempotent,
    // non-destructive. The daemon is the migration authority — it runs
    // before binding the API port.
    sovereign_core::rebrand::promote_legacy_env();
    sovereign_core::rebrand::run_startup_migration();

    // Panic hook for the long-running daemon only. Installed after the
    // rebrand migration so the crash dir lands in the post-migration
    // data dir; before the runtime so even a runtime-build panic leaves
    // a record (DAEMON_RESILIENCE.md P0.4).
    if launch.is_resident() {
        let data_dir = sovereign_contracts::rebrand::svrnmesh_root();
        daemon_cmd::install_panic_hook(data_dir);
    }

    // Structured tracing for launchd/systemd operators tailing logs —
    // the same filter the `daemon` verb used pre-split (twin of
    // `sovereign-cli-daemon/src/lib.rs`; the pins below travel with it).
    let iroh_debug = std::env::var_os("SOVEREIGN_IROH_LOG").is_some();
    // One process, two programs' allowlists: svrn's and, when it hosts serve,
    // serve's, each declared once by its program (pb-stock-binary).
    let svrn_filter = daemon_tracing_filter(iroh_debug, llama_debug_requested());
    match &hosted {
        Some(h) => init_tracing(&format!("{svrn_filter},{}", h.filter())),
        None => init_tracing(&svrn_filter),
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(8 * 1024 * 1024)
        .thread_name("sovereign-daemon-rt")
        .build()
        .expect("failed to build tokio runtime");
    runtime.block_on(daemon_cmd::run(&launch, args, hosted))
}

/// The subscriber over [`compose_filter`], on stderr so machine-readable
/// stdout stays clean.
fn init_tracing(default_filter: &str) {
    let rust_log = std::env::var("RUST_LOG").ok();
    let filter = compose_filter(default_filter, rust_log.as_deref());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init();
}

/// The filter this process logs through: `default_filter`, then every
/// directive of a host `RUST_LOG` ADDED on top of it (a directive for a target
/// the list already names replaces that one), then lance's silencer.
///
/// `RUST_LOG` used to REPLACE the list (pb-stock-binary). An allowlist with no
/// default level drops every target it does not name, so a host override
/// written for one subsystem silently took every other one dark: this host's
/// unit override names neither `serving_path` nor any of serve's targets, so
/// the stock process's serving decisions would not have reached its log
/// (principle 1). An unparsable directive is reported on stderr and skipped,
/// never dropped in silence.
///
/// lance's per-`Dataset::open` INFO event is silenced because the daemon
/// re-opens every installed corpus's index repeatedly; WARN still surfaces.
pub(crate) fn compose_filter(
    default_filter: &str,
    rust_log: Option<&str>,
) -> tracing_subscriber::EnvFilter {
    let mut filter = tracing_subscriber::EnvFilter::new(default_filter);
    for directive in rust_log
        .into_iter()
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        match directive.parse() {
            Ok(d) => filter = filter.add_directive(d),
            Err(e) => eprintln!("RUST_LOG: skipping directive `{directive}`: {e}"),
        }
    }
    filter.add_directive(
        "lance::dataset_events=warn"
            .parse()
            .expect("static lance directive parses"),
    )
}

/// svrn's tracing allowlist: THE one list (pb-stock-binary collapsed
/// cli-daemon's twin onto it; the stock distribution composes it with
/// serve's `DEFAULT_FILTER`).
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
pub const DAEMON_TRACING_FILTER: &str = "sovereign_cli_daemon=info,\
     sovereign_core=info,\
     sovereign_mesh=info,\
     sovereign_inference=info,\
     corpus_engine=info,\
     commonwealth_discovery=info,\
     sovereign_daemon=info,\
     host_kit=info,\
     commonwealth_core=info,\
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
     served_kind=info,\
     corpus_maintenance=info,\
     sec_edgar=info,\
     sec_facts=info,\
     sec_facts_render=info,\
     serving_path=info";

/// The daemon tracing filter plus the always-on iroh observability layer:
/// `commonwealth_transport` (endpoint egress posture) at info, and `iroh` /
/// `iroh_relay` at `warn` — so relay/discovery ERRORS are always visible in a
/// deployed daemon — cranked to `debug` when `iroh_debug` (env
/// `SOVEREIGN_IROH_LOG`, mirroring the `SOVEREIGN_IROH` kill-switch) for
/// diagnosing a reachability wedge. Built as one `iroh=<level>` directive so
/// there is no override ambiguity. A host `RUST_LOG` adds to it
/// ([`compose_filter`]).
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
pub fn daemon_tracing_filter(iroh_debug: bool, llama_debug: bool) -> String {
    let lvl = if iroh_debug { "debug" } else { "warn" };
    let llama_lvl = if llama_debug { "debug" } else { "info" };
    // `mesh.peer_path`: the watchdog's per-poll peer-path census — one line
    // per peer per 20s saying what the endpoint holds and what membership
    // believes. Its own literal target, and it rides `iroh_debug` because
    // that knob's whole stated purpose is diagnosing a reachability wedge and
    // this is the continuous record of one. At `info` the census (a `debug!`)
    // stays quiet and only the TRANSITIONS reach the log — `peer path LOST`
    // with `path_at_death`, `established`, `migrated` — which is the right
    // default: the alarm is free, the tape costs an env var.
    let peer_path_lvl = if iroh_debug { "debug" } else { "info" };
    // `transport=info`: the bridge/tunnel layer logs under the LITERAL target
    // "transport" (not the crate path), so without this token every bridge
    // dial failure is invisible — the 2026-07-19 mesh-heal investigation was
    // blind for exactly this reason. P0.5 observability requirement.
    format!(
        "{DAEMON_TRACING_FILTER},commonwealth_transport=info,transport=info,\
         mesh.peer_path={peer_path_lvl},iroh={lvl},iroh_relay={lvl},llama_cpp={llama_lvl}"
    )
}

/// True when the operator has asked for verbose ggml/llama.cpp output, by
/// either our own knob (`SOVEREIGN_LLAMA_LOGS=1`) or llama.cpp's own
/// documented RPC knob (`GGML_RPC_DEBUG`). Honouring the latter here is what
/// makes `GGML_RPC_DEBUG=1 sovereign daemon run` behave the way its upstream
/// documentation promises: the var alone gates `LOG_DBG` inside ggml-rpc.cpp,
/// but those lines are `GGML_LOG_DEBUG` and would still die at our callback
/// and again at this filter. One env var, all three gates.
pub fn llama_debug_requested() -> bool {
    sovereign_inference::llama_logs::LlamaLogs::from_env().is_verbose()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The daemon tracing filter is an allowlist with NO default level, so
    /// every custom-target observability event must be listed by name or it
    /// goes dark in the deployed daemon (twin of the pin that travels with
    /// cli-daemon's copy). Fails if a directive stops parsing or one of the
    /// required targets is dropped from the list.
    #[test]
    fn daemon_filter_lists_grounding_targets() {
        let filter = tracing_subscriber::EnvFilter::builder()
            .parse(DAEMON_TRACING_FILTER)
            .expect("daemon tracing filter must parse (dotted targets included)");
        let rendered = filter.to_string();
        for target in [
            "grounding_gate",
            "gate.call",
            "gate.lifecycle",
            "agentic_kq",
            "retrieval_audit",
            "commonwealth_core",
            "placement",
            "compute_child",
            "mesh.decision",
            "fim",
            "next_edit",
            "capability",
            "synth.lifecycle",
            "synth.truncation",
            "synth.continue",
            "synth.refusal_retry",
            "synth.citation",
            "synth.budget",
            "corpus_maintenance",
            "admission",
            "served_kind",
            "sec_edgar",
            "sec_facts",
            "sec_facts_render",
            "prefix_state",
            "post_stream",
            // The serving decision (`ServingPath::decide`, serve_client.rs):
            // which path this process booted on, and the input that chose it.
            // Dark at the default filter until pb-stock-binary (principle 1).
            "serving_path",
        ] {
            assert!(
                rendered.contains(target),
                "daemon tracing filter is missing custom target `{target}` — events \
                 under it would be silently dropped by the allowlist. Rendered: {rendered}"
            );
        }
    }

    /// The load-bearing check: does the filter actually ENABLE events at
    /// these custom targets? Built EXACTLY as `init_tracing` does on the
    /// RUST_LOG-unset path.
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

    /// The iroh observability layer must parse in both postures and flip
    /// iroh/relay warn↔debug with the `SOVEREIGN_IROH_LOG` toggle.
    #[test]
    fn daemon_filter_iroh_toggle() {
        for iroh in [false, true] {
            for llama in [false, true] {
                let f = daemon_tracing_filter(iroh, llama);
                tracing_subscriber::EnvFilter::builder()
                    .parse(&f)
                    .expect("daemon tracing filter (with iroh layer) must parse");
            }
        }
        let off = daemon_tracing_filter(false, false);
        let on = daemon_tracing_filter(true, false);
        assert!(off.contains("commonwealth_transport=info"));
        assert!(off.contains("iroh=warn") && off.contains("iroh_relay=warn"));
        assert!(on.contains("iroh=debug") && on.contains("iroh_relay=debug"));
    }

    /// The peer-path census rides a LITERAL target (`mesh.peer_path`), so the
    /// allowlist-with-no-default-level trap this module has hit five times
    /// applies to it in full: unlisted, the watchdog's continuous record of a
    /// decaying transport is dropped by the subscriber and a capture returns
    /// an empty log that reads like "nothing happened".
    ///
    /// It must be listed at BOTH postures — at `info` the transitions still
    /// reach the log and only the per-poll census is quiet — and must reach
    /// `debug` under `SOVEREIGN_IROH_LOG`, which is the knob whose stated
    /// purpose is diagnosing a reachability wedge.
    #[test]
    fn daemon_filter_carries_the_peer_path_census() {
        let quiet = super::daemon_tracing_filter(false, false);
        let verbose = super::daemon_tracing_filter(true, false);
        assert!(
            quiet.contains("mesh.peer_path=info"),
            "the target must be allowlisted even when quiet, or `peer path LOST` \
             — the one line naming the path at the moment it died — is dropped: {quiet}"
        );
        assert!(
            verbose.contains("mesh.peer_path=debug"),
            "SOVEREIGN_IROH_LOG must lift the census to debug, or a decay capture \
             records only transitions and not the ramp between them: {verbose}"
        );
        // Not spelling — ADMISSION. `contains` (and even a parse + `Display`
        // round-trip, which is as far as the `llama_cpp` test above goes)
        // asserts on a string this function itself produced; it cannot tell a
        // directive that admits an event from one that merely survives
        // parsing. So emit the real thing through a subscriber built from the
        // real filter and count what comes out the other side. Failing input:
        // drop `mesh.peer_path` from the allowlist and `quiet_out` loses the
        // warn as well as the debug.
        fn emitted(filter: &str) -> String {
            let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
            let sink = buf.clone();
            let sub = tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::builder()
                        .parse(filter)
                        .expect("daemon filter parses"),
                )
                .with_writer(move || CaptureWriter(sink.clone()))
                .finish();
            tracing::subscriber::with_default(sub, || {
                tracing::debug!(target: "mesh.peer_path", "CENSUS-LINE");
                tracing::warn!(target: "mesh.peer_path", "LOST-LINE");
            });
            let out = buf.lock().expect("not poisoned").clone();
            String::from_utf8_lossy(&out).into_owned()
        }

        let quiet_out = emitted(&quiet);
        assert!(
            quiet_out.contains("LOST-LINE"),
            "a WARN on the census target must reach the log at the DEFAULT posture — \
             `peer path LOST` is the whole alarm: {quiet_out:?}"
        );
        assert!(
            !quiet_out.contains("CENSUS-LINE"),
            "the per-poll census must stay quiet by default, or every daemon pays \
             7 lines per 20s for a capture nobody asked for: {quiet_out:?}"
        );
        let verbose_out = emitted(&verbose);
        assert!(
            verbose_out.contains("CENSUS-LINE") && verbose_out.contains("LOST-LINE"),
            "SOVEREIGN_IROH_LOG must admit the census itself, not merely name it: \
             {verbose_out:?}"
        );
    }

    /// Collects formatted events into a shared buffer so a test can assert on
    /// what a filter ADMITTED rather than on how it is spelled.
    struct CaptureWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CaptureWriter {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("not poisoned").extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// The `llama_cpp` target must be in the filter at BOTH postures, and
    /// must reach `debug` when the operator asks for verbose ggml output —
    /// or a failed model load reaches the operator as a bare null result.
    #[test]
    fn daemon_filter_carries_llama_cpp_target() {
        let quiet = daemon_tracing_filter(false, false);
        let verbose = daemon_tracing_filter(false, true);
        assert!(
            quiet.contains("llama_cpp=info"),
            "llama_cpp must be allowlisted even when quiet: {quiet}"
        );
        assert!(
            verbose.contains("llama_cpp=debug"),
            "GGML_RPC_DEBUG / SOVEREIGN_LLAMA_LOGS=1 must lift llama_cpp to debug: {verbose}"
        );
        let rendered = tracing_subscriber::EnvFilter::builder()
            .parse(&verbose)
            .expect("verbose daemon filter must parse")
            .to_string();
        assert!(
            rendered.contains("llama_cpp=debug"),
            "llama_cpp=debug did not survive EnvFilter parsing: {rendered}"
        );
    }

    /// The verb reconstruction must round-trip the shapes the shim sends:
    /// subargs → Daemon with those args; `--worker-mode` anywhere →
    /// Worker; a child re-exec flag → its own arm.
    #[test]
    fn launch_reconstruction_matches_old_dispatcher() {
        let full = |rest: &[&str]| {
            let mut v = vec!["daemon".to_string()];
            v.extend(rest.iter().map(|s| s.to_string()));
            v
        };
        assert_eq!(
            Launch::parse(&full(&["run"]), Launch::Bare),
            Launch::Daemon {
                args: vec!["run".to_string()]
            }
        );
        assert_eq!(
            Launch::parse(&full(&[]), Launch::Bare),
            Launch::Daemon { args: vec![] }
        );
        assert!(matches!(
            Launch::parse(&full(&["run", "--worker-mode"]), Launch::Bare),
            Launch::Worker { .. }
        ));
        // A compute-child re-exec of THIS binary: the flag may sit at any
        // position and carries its args after it.
        assert_eq!(
            Launch::parse(&full(&["--compute-child", "--fd", "3"]), Launch::Bare),
            Launch::ComputeChild {
                args: vec!["--fd".to_string(), "3".to_string()]
            }
        );
        assert!(matches!(
            Launch::parse(
                &full(&["--rpc-worker", "--bind", "127.0.0.1:50052"]),
                Launch::Bare
            ),
            Launch::RpcWorker { .. }
        ));
    }

    /// A host `RUST_LOG` ADDS to the allowlist: a directive for one subsystem
    /// no longer takes every other listed target dark. Failing input: build
    /// the filter from `RUST_LOG` alone (the pre-pb-stock-binary behaviour)
    /// and the `serving_path` and `grounding_gate` events are dropped.
    #[test]
    fn a_host_rust_log_adds_to_the_allowlist() {
        use std::sync::{Arc, Mutex};
        use tracing_subscriber::layer::{Context, SubscriberExt};
        use tracing_subscriber::Layer;

        #[derive(Clone)]
        struct Capture(Arc<Mutex<Vec<String>>>);
        impl<S: tracing::Subscriber> Layer<S> for Capture {
            fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
                let m = event.metadata();
                self.0
                    .lock()
                    .unwrap()
                    .push(format!("{}:{}", m.target(), m.level()));
            }
        }
        let seen = Arc::new(Mutex::new(Vec::new()));
        let filter = compose_filter(
            DAEMON_TRACING_FILTER,
            Some("sovereign_core=warn, grounding_gate=debug,not a directive=="),
        );
        let subscriber = tracing_subscriber::registry()
            .with(filter)
            .with(Capture(seen.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "serving_path", "probe");
            tracing::debug!(target: "grounding_gate", "probe");
            tracing::info!(target: "sovereign_core", "probe");
        });
        let seen = seen.lock().unwrap().clone();
        assert!(seen.contains(&"serving_path:INFO".to_string()), "{seen:?}");
        assert!(
            seen.contains(&"grounding_gate:DEBUG".to_string()),
            "{seen:?}"
        );
        // The added directive replaces the list's own for that target.
        assert!(
            !seen.iter().any(|t| t.starts_with("sovereign_core")),
            "{seen:?}"
        );
    }
}
