// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign-cli-daemon` — long-running daemon host + setup + service
//! install + doctor. Parent `sovereign` shim execs into this binary
//! for `daemon`, `setup`, `install-service`, `doctor` verbs.

mod daemon_cmd;
mod doctor_cmd;
mod install_service_cmd;
pub(crate) mod log_rotation;
mod memory_watch;
mod service_install;
mod setup_cmd;
mod setup_config;
mod watcher_supervisor;

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
     synth.budget=info";

/// The daemon tracing filter plus the always-on iroh observability layer:
/// `commonwealth_transport` (endpoint egress posture) at info, and `iroh` /
/// `iroh_relay` at `warn` — so relay/discovery ERRORS are always visible in a
/// deployed daemon — cranked to `debug` when `iroh_debug` (env
/// `SOVEREIGN_IROH_LOG`, mirroring the `SOVEREIGN_IROH` kill-switch) for
/// diagnosing a reachability wedge. Built as one `iroh=<level>` directive so
/// there is no override ambiguity. `RUST_LOG`, if set, still overrides all.
fn daemon_tracing_filter(iroh_debug: bool) -> String {
    let lvl = if iroh_debug { "debug" } else { "warn" };
    format!("{DAEMON_TRACING_FILTER},commonwealth_transport=info,iroh={lvl},iroh_relay={lvl}")
}

fn main() {
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        std::env::set_var("RUST_BACKTRACE", "full");
    }
    if std::env::var_os("RUST_MIN_STACK").is_none() {
        std::env::set_var("RUST_MIN_STACK", "8388608");
    }
    // Rebrand back-compat (see sovereign_core::rebrand): idempotent, non-destructive.
    // The daemon is the migration authority — it runs before binding the API port.
    sovereign_core::rebrand::promote_legacy_env();
    sovereign_core::rebrand::run_startup_migration();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(8 * 1024 * 1024)
        .thread_name("sovereign-cli-daemon-rt")
        .build()
        .expect("failed to build tokio runtime");
    runtime.block_on(async_main());
}

async fn async_main() {
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = raw_args.first().map(|s| s.as_str()).unwrap_or("");
    let rest: &[String] = if raw_args.is_empty() {
        &[]
    } else {
        &raw_args[1..]
    };

    // The daemon needs structured tracing for launchd / systemd
    // operators tailing logs. Match the filter sovereign-cli used
    // pre-split.
    if cmd == "daemon" {
        // Track W: `SOVEREIGN_IROH_LOG` cranks iroh/relay/transport internals to
        // debug for diagnosing a reachability wedge; off, they stay at warn
        // (errors still visible) so the log isn't flooded.
        let iroh_debug = std::env::var_os("SOVEREIGN_IROH_LOG").is_some();
        init_tracing(&daemon_tracing_filter(iroh_debug));
    } else if cmd == "setup" {
        init_tracing("sovereign_cli_daemon=info");
    }

    let code: i32 = match cmd {
        "daemon" => daemon_cmd::run(rest).await,
        "setup" => setup_cmd::run_setup(rest).await,
        "install-service" => install_service_cmd::run(rest).await,
        "doctor" => doctor_cmd::run_doctor(rest).await,
        "" => {
            eprintln!("sovereign-cli-daemon: usage: sovereign-cli-daemon <subcommand> [args...]");
            2
        }
        other => {
            eprintln!("sovereign-cli-daemon: unknown subcommand '{other}'");
            2
        }
    };

    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::DAEMON_TRACING_FILTER;

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
            "synth.lifecycle",
            "synth.truncation",
            "synth.continue",
            "synth.refusal_retry",
            "synth.citation",
            "synth.budget",
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
            tracing::info!(target: "sovereign_core", "probe"); // module control: enabled
            tracing::info!(target: "definitely_unlisted_zzz", "probe"); // control: dropped
        });
        let seen = seen.lock().unwrap().clone();
        for want in ["grounding_gate", "gate.call", "synth.truncation", "sovereign_core"] {
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
        for f in [
            super::daemon_tracing_filter(false),
            super::daemon_tracing_filter(true),
        ] {
            tracing_subscriber::EnvFilter::builder()
                .parse(&f)
                .expect("daemon tracing filter (with iroh layer) must parse");
        }
        let off = super::daemon_tracing_filter(false);
        let on = super::daemon_tracing_filter(true);
        assert!(off.contains("commonwealth_transport=info"));
        assert!(off.contains("iroh=warn") && off.contains("iroh_relay=warn"));
        assert!(on.contains("iroh=debug") && on.contains("iroh_relay=debug"));
    }
}
