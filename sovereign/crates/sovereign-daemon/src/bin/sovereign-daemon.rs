// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign-daemon` — the assembled-host process (docs/FIVE_PROGRAMS.md
//! §11 step 10): the cmnwlth binary's own main, holding what used to be
//! the `daemon` verb's half of `sovereign-cli-daemon`'s dispatcher.
//!
//! # argv contract
//!
//! `argv[1..]` is whatever followed the `daemon` verb in the old
//! dispatcher — the `svrn` CLI's shim `exec`s this bin with exactly
//! those args. Direct invocations (launchd/systemd units, a dev shell)
//! pass the same shape: `[run] [--config <path>] [--rpc-worker[=<bind>]]
//! …`. The verb is reconstructed below and handed to the ONE parser
//! (`Launch::parse`), so the decision this process makes about itself is
//! the same one the old dispatcher made:
//!
//! - `sovereign-daemon run …` → `Launch::Daemon` → `daemon_cmd::run`
//! - `sovereign-daemon run --worker-mode …` → `Launch::Worker`
//! - a `current_exe()` re-exec carrying `--compute-child` / `--rpc-worker`
//!   (the daemon's own supervisors spawn those with THIS binary's path)
//!   → the child mains, before any daemon bootstrap runs.
//!
//! Everything below the argv reconstruction is the daemon-verb slice of
//! `sovereign-cli-daemon/src/lib.rs::run_with_args`, mirrored line for
//! line; the twin is named so the two can be collapsed when the CLI
//! tree's copy retires.

use sovereign_contracts::launch::Launch;
use sovereign_daemon::daemon_cmd;

fn main() {
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(run(&raw_args));
}

/// The daemon-verb slice of the old dispatcher. Returns the process
/// exit code — `main` exits with it.
fn run(raw_args: &[String]) -> i32 {
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

    // Not a launch this binary serves (a `--smoketest` argv, the
    // desktop/server defaults…). Same refusal the old dispatcher made.
    let (Launch::Daemon { args } | Launch::Worker { args }) = &launch else {
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
    init_tracing(&daemon_tracing_filter(iroh_debug, llama_debug_requested()));

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(8 * 1024 * 1024)
        .thread_name("sovereign-daemon-rt")
        .build()
        .expect("failed to build tokio runtime");
    runtime.block_on(daemon_cmd::run(&launch, args))
}

/// Twin of `sovereign_cli_shared::tracing_init::init_tracing` — the
/// daemon crate may not take a cli-shared (svrn-package) edge, so the
/// subscriber bootstrap is reimplemented here verbatim.
fn init_tracing(default_filter: &str) {
    // Silence lance's per-`Dataset::open` INFO event: the daemon
    // re-opens every installed corpus's index repeatedly, so at INFO it
    // floods the log. WARN still surfaces real lance failures.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| default_filter.into())
        .add_directive(
            "lance::dataset_events=warn"
                .parse()
                .expect("static lance directive parses"),
        );
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        // Force stderr so machine-readable stdout stays clean.
        .with_writer(std::io::stderr)
        .try_init();
}

// ── The daemon tracing filter (twin of cli-daemon's lib.rs; copied
// whole with its pins) ───────────────────────────────────────────────
//
// This is an ALLOWLIST WITH NO DEFAULT LEVEL: an event whose target
// matches nothing here is dropped. Custom-target events therefore need
// their target listed explicitly — see the pins in `tests` below for
// the times this went dark before it was pinned.

const DAEMON_TRACING_FILTER: &str = "sovereign_cli_daemon=info,\
     sovereign_core=info,\
     sovereign_mesh=info,\
     sovereign_inference=info,\
     corpus_engine=info,\
     commonwealth_discovery=info,\
     sovereign_daemon=info,\
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
     corpus_maintenance=info,\
     sec_edgar=info,\
     sec_facts=info,\
     sec_facts_render=info";

/// The daemon tracing filter plus the always-on iroh observability
/// layer, cranked to `debug` when `iroh_debug` (env `SOVEREIGN_IROH_LOG`)
/// for diagnosing a reachability wedge, and the `llama_cpp` literal
/// target every ggml log line rides. `RUST_LOG`, if set, overrides all.
fn daemon_tracing_filter(iroh_debug: bool, llama_debug: bool) -> String {
    let lvl = if iroh_debug { "debug" } else { "warn" };
    let llama_lvl = if llama_debug { "debug" } else { "info" };
    let peer_path_lvl = if iroh_debug { "debug" } else { "info" };
    format!(
        "{DAEMON_TRACING_FILTER},commonwealth_transport=info,transport=info,\
         mesh.peer_path={peer_path_lvl},iroh={lvl},iroh_relay={lvl},llama_cpp={llama_lvl}"
    )
}

/// True when the operator has asked for verbose ggml/llama.cpp output,
/// by either our own knob (`SOVEREIGN_LLAMA_LOGS=1`) or llama.cpp's own
/// documented RPC knob (`GGML_RPC_DEBUG`). One env var, all three gates.
fn llama_debug_requested() -> bool {
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
            "sec_edgar",
            "sec_facts",
            "sec_facts_render",
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
}
