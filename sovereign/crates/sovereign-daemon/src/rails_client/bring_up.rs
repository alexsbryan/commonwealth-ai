// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`ensure_rails`]: a client makes cw-rails reachable at a user-action moment
//! (fp-solo-clients), split out of `rails_client.rs` at its arch-gate band.

/// How long [`ensure_rails`] waits for cw-rails to answer, bring-up included.
/// cw-rails loads no model: a start is a lock, a bind, and projecting its
/// store from the journals before it serves (phase-b-5) — 1.74-1.88 s on a
/// copy of the operator's 12.9k-line store with the signature stack
/// optimized (root Cargo.toml `[profile.dev.package]`).
const RAILS_BRING_UP_WINDOW: std::time::Duration = std::time::Duration::from_secs(10);

/// Make cw-rails reachable at `base`, bringing it up if nothing answers there
/// (five-programs-63/-65: cw-rails owns its root, a client owns reaching it).
///
/// Call it ONLY at a user-action moment — daemon boot, a `svrn portfolio` or
/// `svrn newsworthy` run — and never on a refused dial: the settled bar
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
/// `data_dir` is the daemon's (`[data] dir`): its one-time journal handover
/// runs FIRST, before any bring-up, because cw-rails reads its journals once
/// at start (phase-b-3, [`crate::rail_migration::hand_over`]).
///
/// Sync and safe from any thread: the probe runs on its own thread and
/// runtime. A non-loopback base, a base with no port, or an absent binary
/// is a traced, named absence (principle 6).
///
/// [`ServingHost::ensure_reachable`]: sovereign_turn_client::reach::ServingHost::ensure_reachable
pub fn ensure_rails(
    base: &str,
    local_only: bool,
    data_dir: &std::path::Path,
) -> Result<sovereign_turn_client::reach::Reached, String> {
    use sovereign_turn_client::reach::{locate_sibling, BundledBackend, Reached, ServingHost};

    hand_over_first(base, data_dir);
    let absent = |why: String| {
        tracing::warn!(rails_base = base, reason = %why, "ensure_rails: cw-rails is not reachable");
        why
    };
    let url = reqwest::Url::parse(base)
        .map_err(|e| absent(format!("the rails base {base} is not a URL: {e}")))?;
    let host = url
        .host_str()
        .map(|h| h.trim_matches(['[', ']']).to_string())
        .unwrap_or_default();
    let loopback = host
        .parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(host == "localhost");
    if !loopback {
        return Err(absent(format!(
            "the rails base {base} is not loopback; only a local cw-rails is brought up"
        )));
    }
    let Some(port) = url.port() else {
        return Err(absent(format!(
            "the rails base {base} names no port; a cw-rails is brought up on the port \
             its client probes, so the base must name one"
        )));
    };
    let Some(bin) = locate_sibling("cw-rails", "CW_RAILS_BIN") else {
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
    let mut backend = BundledBackend::at(bin)
        .arg("run")
        .arg("--listen")
        .arg(port.to_string());
    if local_only {
        backend = backend.arg("--local-only");
    }
    tracing::debug!(
        rails_base = base,
        port,
        local_only,
        "ensure_rails: bring-up argv resolved"
    );
    let serving = ServingHost::at(base)
        .ready_at("/v1/mesh/status")
        .bringing_up(backend.log_to(data_dir.join("rails.log")));
    let outcome = std::thread::scope(|s| {
        s.spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("no runtime to reach cw-rails with: {e}"))
                .and_then(|rt| {
                    rt.block_on(async {
                        let reached = serving
                            .ensure_reachable(RAILS_BRING_UP_WINDOW)
                            .await
                            .map_err(|e| e.to_string())?;
                        if local_only && matches!(reached, Reached::AlreadyServing { .. }) {
                            refuse_n0_posture(base).await?;
                        }
                        Ok::<_, String>(reached)
                    })
                })
        })
        .join()
        .unwrap_or_else(|_| Err("the ensure_rails probe thread panicked".into()))
    });
    match outcome {
        Ok(reached) => {
            tracing::info!(rails_base = base, reached = ?reached, "ensure_rails: cw-rails is reachable");
            Ok(reached)
        }
        Err(e) => Err(absent(e)),
    }
}

/// One probe of `base`, then the handover: moved when nothing answers, left
/// in place and named when a cw-rails already does. A probe that cannot run
/// moves nothing — a journal under a live store is the loss this prevents.
fn hand_over_first(base: &str, data_dir: &std::path::Path) {
    let host = sovereign_turn_client::reach::ServingHost::at(base).ready_at("/v1/mesh/status");
    let answering = std::thread::scope(|s| {
        s.spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map(|rt| rt.block_on(host.is_serving()))
                .map_err(|e| e.to_string())
        })
        .join()
        .unwrap_or_else(|_| Err("the handover probe thread panicked".into()))
    });
    let answering = answering.unwrap_or_else(|e| {
        tracing::warn!(rails_base = base, error = %e, "ensure_rails: could not probe cw-rails before the handover; treated as answering, so nothing moves");
        true
    });
    tracing::debug!(rails_base = base, answering, "ensure_rails: handover probe");
    crate::rail_migration::hand_over(
        data_dir,
        &sovereign_core::setup_config::SetupConfig::default_path(),
        answering,
    );
}

/// A local-only node found a cw-rails it did not start: `Ok` only when that
/// cw-rails' `/v1/mesh/status` says n0 services are off. On, unreported, or
/// unreadable is a named Err — the node never rides an n0-homed cw-rails, and
/// never starts a second one beside it.
async fn refuse_n0_posture(base: &str) -> Result<(), String> {
    let url = format!("{}/v1/mesh/status", base.trim_end_matches('/'));
    let status: serde_json::Value = reqwest::Client::builder()
        .timeout(super::DIAL_TIMEOUT)
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
