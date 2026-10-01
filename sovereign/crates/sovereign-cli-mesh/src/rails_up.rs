// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`ensure_rails`]: `svrn mesh up` makes cw-rails reachable (fp-solo-clients).
//! It moved here from sovereign-daemon's `rails_client` when svrn stopped
//! bringing cw-rails up (pb-rails-untether, phase-b-31): the mesh program owns
//! the one opt-in bring-up, and svrn only dials.

/// How long `refuse_n0_posture` waits for cw-rails' status. Loopback answers
/// or refuses in milliseconds; the bound turns a HUNG cw-rails into a named
/// refusal.
const STATUS_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// How long [`ensure_rails`] waits for cw-rails to answer, bring-up included.
/// cw-rails loads no model: a start is a lock, a bind, and projecting its
/// store from the journals before it serves (phase-b-5) — 1.74-1.88 s on a
/// copy of the operator's 12.9k-line store with the signature stack
/// optimized (root Cargo.toml `[profile.dev.package]`).
const RAILS_BRING_UP_WINDOW: std::time::Duration = std::time::Duration::from_secs(10);

/// `svrn mesh up`: the one opt-in bring-up of cw-rails (pb-rails-untether,
/// phase-b-31). Nothing starts cw-rails by default, the distribution
/// included, so a node that wants it across reboots runs it under its own
/// service unit. Reads the node's config the way svrn does: `[daemon]
/// rails_base` through its one reader, local-only through
/// `LocalOnlyProfile`, and `[data] dir` for the handover. Exit 0 once
/// cw-rails answers; 1 with the absence named.
pub async fn cmd_up(args: &[String]) -> i32 {
    if let Some(arg) = args.first() {
        if matches!(arg.as_str(), "--help" | "-h" | "help") {
            println!(
                "svrn mesh up — hand an upgraded node's rings to cw-rails, then bring cw-rails \
                 up on [daemon] rails_base.\nsvrn never starts cw-rails; this verb is the one \
                 that does."
            );
            return 0;
        }
        eprintln!("svrn mesh up: takes no arguments, got '{arg}'");
        return 2;
    }
    // The handover's warn (namespaces left waiting under a live cw-rails) is
    // this verb's report to the operator, so it reaches stderr.
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
    match up().await {
        Ok(_) => 0,
        Err(e) => {
            eprintln!("svrn mesh up: {e}");
            1
        }
    }
}

/// The bring-up `svrn mesh up` runs, and `svrn mesh create|join` run first
/// since the join and the key are cw-rails' (pb-mesh-exit-transport): the
/// handover, then cw-rails reachable at `[daemon] rails_base`, its unit
/// enabled. `Ok` is the base it answers at; `Err` names the absence.
pub(crate) async fn up() -> Result<String, String> {
    use sovereign_contracts::setup_config::SetupConfig;
    let config_path = SetupConfig::default_path();
    // No config file is a first run: the defaults. A config that exists and
    // does not load is refused — a handover from a guessed data dir is the
    // substitution principle 6 forbids.
    let config = match SetupConfig::load() {
        Ok(c) => c,
        Err(e) if config_path.exists() => {
            return Err(format!("{} does not load: {e}", config_path.display()));
        }
        Err(e) => {
            tracing::warn!(error = %e, "mesh up: no setup config; the rails base and data dir are the defaults");
            SetupConfig::unconfigured()
        }
    };
    let base = sovereign_turn_client::rails_kv::resolve_rails_base(&config.daemon);
    let local_only =
        sovereign_contracts::local_only::LocalOnlyProfile::resolve(config.daemon.local_only);
    let data_dir = config.data.dir.clone();
    let mdns = mdns_effective(local_only.is_local_only(), config.discovery.mdns);
    tracing::debug!(
        rails_base = %base,
        local_only = local_only.label(),
        local_only_source = local_only.source().as_str(),
        mdns,
        data_dir = %data_dir.display(),
        config = %config_path.display(),
        "mesh up: resolved"
    );
    let reached = {
        let base = base.clone();
        tokio::task::spawn_blocking(move || {
            ensure_rails(
                &base,
                local_only.is_local_only(),
                mdns,
                &data_dir,
                &config_path,
            )
        })
        .await
        .unwrap_or_else(|e| Err(format!("the bring-up thread failed: {e}")))
    };
    match reached {
        Ok(sovereign_turn_client::reach::Reached::BroughtUp { pid, .. }) => {
            println!("cw-rails is up at {base} (started, pid {pid})");
            crate::rails_unit::install_after_bring_up(&base, local_only.is_local_only(), mdns);
            Ok(base)
        }
        Ok(sovereign_turn_client::reach::Reached::AlreadyServing { .. }) => {
            println!("cw-rails is up at {base} (already running)");
            crate::rails_unit::install_after_bring_up(&base, local_only.is_local_only(), mdns);
            Ok(base)
        }
        Err(e) => Err(format!("cw-rails is not reachable at {base}: {e}")),
    }
}

/// Make cw-rails reachable at `base`, bringing it up if nothing answers there
/// (five-programs-63/-65: cw-rails owns its root, a client owns reaching it).
///
/// Call it ONLY at a user-action moment — `svrn mesh up` is the one — and
/// never on a refused dial: the settled bar
/// forbids bring-up on a timer or a health signal (`bring_up_decider` in
/// quality/ARCH_LAYERS.toml), so a refused dial reports absence. The decision
/// is [`ServingHost::ensure_reachable`]'s; this holds no child, and two
/// racing callers are cw-rails' question — its `rails.lock` turns the loser
/// away, and the refusal lands in `rails.log`.
///
/// A cw-rails it brings up serves the port `base` names (`--listen`), and a
/// `local_only` node's runs with n0 severed (`--local-only`). One already
/// answering on a local-only node must report n0 services off on
/// `/v1/mesh/status`, or this is a named Err — never a second cw-rails
/// (five-programs-66).
///
/// `data_dir` is the daemon's (`[data] dir`) and `config_path` its config
/// file: the one-time journal and media handover runs FIRST, before any
/// bring-up, because cw-rails reads its journals once at start (phase-b-3,
/// [`crate::rail_migration::hand_over`]).
///
/// Sync and safe from any thread: the probe runs on its own thread and
/// runtime. A non-loopback base, a base with no port, or an absent binary
/// is a traced, named absence (principle 6).
///
/// [`ServingHost::ensure_reachable`]: sovereign_turn_client::reach::ServingHost::ensure_reachable
pub fn ensure_rails(
    base: &str,
    local_only: bool,
    mdns: bool,
    data_dir: &std::path::Path,
    config_path: &std::path::Path,
) -> Result<sovereign_turn_client::reach::Reached, String> {
    use sovereign_turn_client::reach::{BundledBackend, Reached, ServingHost};

    let absent = |why: String| {
        tracing::warn!(rails_base = base, reason = %why, "ensure_rails: cw-rails is not reachable");
        why
    };
    hand_over_first(base, data_dir, config_path).map_err(absent)?;
    let port = loopback_port(base).map_err(absent)?;
    let Some(bin) = locate_rails() else {
        return Err(absent(
            "no cw-rails binary: set CW_RAILS_BIN, or install it beside this program or on PATH"
                .into(),
        ));
    };
    // cw-rails resolves this same dir itself (no --data-dir is passed); the
    // log is the bring-up's output, so the client makes room for it.
    let data_dir = commonwealth_media::rails_data_dir();
    std::fs::create_dir_all(&data_dir).map_err(|e| {
        absent(format!(
            "the cw-rails data dir {} cannot be created: {e}",
            data_dir.display()
        ))
    })?;
    let mut backend = BundledBackend::at(bin);
    for arg in run_args(port, local_only, mdns) {
        backend = backend.arg(arg);
    }
    tracing::debug!(
        rails_base = base,
        port,
        local_only,
        mdns,
        "ensure_rails: bring-up argv resolved"
    );
    let serving = ServingHost::at(base)
        .ready_at("/v1/mesh/status")
        .bringing_up(backend.log_to(data_dir.join("rails.log")));
    let outcome = on_own_runtime("ensure_rails probe", || async {
        let reached = serving
            .ensure_reachable(RAILS_BRING_UP_WINDOW)
            .await
            .map_err(|e| e.to_string())?;
        if local_only && matches!(reached, Reached::AlreadyServing { .. }) {
            refuse_n0_posture(base).await?;
        }
        Ok(reached)
    });
    match outcome {
        Ok(reached) => {
            tracing::info!(rails_base = base, reached = ?reached, "ensure_rails: cw-rails is reachable");
            Ok(reached)
        }
        Err(e) => Err(absent(e)),
    }
}

/// The port a local cw-rails is brought up on: `base`'s, when `base` is a
/// loopback URL that names one. `Err` says which of the three it is not.
pub(crate) fn loopback_port(base: &str) -> Result<u16, String> {
    let url = reqwest::Url::parse(base)
        .map_err(|e| format!("the rails base {base} is not a URL: {e}"))?;
    let host = url
        .host_str()
        .map(|h| h.trim_matches(['[', ']']).to_string())
        .unwrap_or_default();
    let loopback = host
        .parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(host == "localhost");
    if !loopback {
        return Err(format!(
            "the rails base {base} is not loopback; only a local cw-rails is brought up"
        ));
    }
    url.port().ok_or_else(|| {
        format!(
            "the rails base {base} names no port; a cw-rails is brought up on the port \
             its client probes, so the base must name one"
        )
    })
}

/// The cw-rails binary the bring-up and the boot unit both run: `CW_RAILS_BIN`,
/// else beside this program, else on PATH.
pub(crate) fn locate_rails() -> Option<std::path::PathBuf> {
    sovereign_turn_client::reach::locate_sibling("cw-rails", "CW_RAILS_BIN")
}

/// cw-rails' argv after its binary: the ONE spelling the bring-up and the
/// boot unit both run (`crate::rails_unit`), so the unit cannot start a
/// cw-rails on another port or posture than `svrn mesh up` did. `mdns` is
/// [`mdns_effective`]'s answer.
pub(crate) fn run_args(port: u16, local_only: bool, mdns: bool) -> Vec<String> {
    let mut args = vec!["run".to_string(), "--listen".to_string(), port.to_string()];
    if local_only {
        args.push("--local-only".to_string());
    }
    if mdns {
        args.push("--mdns".to_string());
    }
    args
}

/// Whether this node's cw-rails advertises and browses mDNS: `[discovery]
/// mdns`, never on a local-only node, and off under `SOVEREIGN_DISABLE_MDNS`.
/// The daemon's `mdns_enabled_effective`, moved here when mDNS left the
/// daemon for cw-rails (pb-mesh-exit-transport): the one decider, now read
/// by the one verb that starts cw-rails.
pub(crate) fn mdns_effective(local_only: bool, cfg_mdns: bool) -> bool {
    if local_only {
        return false;
    }
    let env_force_off = std::env::var("SOVEREIGN_DISABLE_MDNS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let on = cfg_mdns && !env_force_off;
    tracing::debug!(cfg_mdns, env_force_off, on, "mesh up: mdns posture");
    on
}

/// Run `probe` on its own thread and current-thread runtime, so the caller
/// is safe from any thread, a runtime's included. `what` names the probe
/// when the thread panics.
fn on_own_runtime<T: Send, F: std::future::Future<Output = Result<T, String>>>(
    what: &str,
    probe: impl FnOnce() -> F + Send,
) -> Result<T, String> {
    std::thread::scope(|s| {
        s.spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("no runtime to reach cw-rails with: {e}"))
                .and_then(|rt| rt.block_on(probe()))
        })
        .join()
        .unwrap_or_else(|_| Err(format!("the {what} thread panicked")))
    })
}

/// One probe of `base`, then the handover: moved when nothing answers, left
/// in place and named when a cw-rails already does. A probe that cannot run
/// moves nothing — a journal under a live store is the loss this prevents.
///
/// The identity handover (`crate::identity_handover`) runs on the same probe:
/// a key, node id and meshes that fail to move are an `Err`, and nothing is
/// brought up over a half-moved store.
fn hand_over_first(
    base: &str,
    data_dir: &std::path::Path,
    config_path: &std::path::Path,
) -> Result<(), String> {
    let host = sovereign_turn_client::reach::ServingHost::at(base).ready_at("/v1/mesh/status");
    let answering = on_own_runtime("handover probe", || async { Ok(host.is_serving().await) });
    let answering = answering.unwrap_or_else(|e| {
        tracing::warn!(rails_base = base, error = %e, "ensure_rails: could not probe cw-rails before the handover; treated as answering, so nothing moves");
        true
    });
    tracing::debug!(rails_base = base, answering, "ensure_rails: handover probe");
    let identity = crate::identity_handover::hand_over(
        data_dir,
        &commonwealth_media::rails_data_dir(),
        answering,
    )
    .map_err(|e| format!("the daemon's identity did not hand over to cw-rails: {e}"))?;
    tracing::info!(rails_base = base, outcome = ?identity, "ensure_rails: identity handover");
    crate::rail_migration::hand_over(data_dir, config_path, answering);
    Ok(())
}

/// A local-only node found a cw-rails it did not start: `Ok` only when that
/// cw-rails' `/v1/mesh/status` says n0 services are off. On, unreported, or
/// unreadable is a named Err — the node never rides an n0-homed cw-rails, and
/// never starts a second one beside it.
async fn refuse_n0_posture(base: &str) -> Result<(), String> {
    let url = format!("{}/v1/mesh/status", base.trim_end_matches('/'));
    let status: serde_json::Value = reqwest::Client::builder()
        .timeout(STATUS_READ_TIMEOUT)
        .build()
        .map_err(|e| format!("no HTTP client to read cw-rails' posture with: {e}"))?
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("the cw-rails at {base} did not answer its status: {e}"))?
        .json()
        .await
        .map_err(|e| format!("the cw-rails at {base} answered an unreadable status: {e}"))?;
    match status["relay"]["n0_services"].as_bool() {
        Some(false) => {
            tracing::info!(
                rails_base = base,
                "ensure_rails: the running cw-rails is local-only"
            );
            Ok(())
        }
        Some(true) => Err(format!(
            "this node is local-only, but the cw-rails already serving {base} uses n0 services \
             (relay + DNS); stop it, or run it with `cw-rails run --local-only` — a second one \
             is not started beside it"
        )),
        None => Err(format!(
            "this node is local-only, and the cw-rails serving {base} does not report its relay \
             posture on /v1/mesh/status; it cannot be judged local-only"
        )),
    }
}
