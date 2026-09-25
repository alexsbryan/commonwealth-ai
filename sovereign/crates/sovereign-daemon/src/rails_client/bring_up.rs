// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`ensure_rails`]: a client makes cw-rails reachable at a user-action moment
//! (fp-solo-clients), split out of `rails_client.rs` at its arch-gate band.

/// How long [`ensure_rails`] waits for cw-rails to answer, bring-up included.
/// cw-rails loads no model: a start is a lock, a bind and a store open.
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
/// Sync and safe from any thread: the probe runs on its own thread and
/// runtime. A non-loopback base or an absent binary is a traced, named
/// absence (principle 6).
///
/// [`ServingHost::ensure_reachable`]: sovereign_turn_client::reach::ServingHost::ensure_reachable
pub fn ensure_rails(base: &str) -> Result<sovereign_turn_client::reach::Reached, String> {
    use sovereign_turn_client::reach::{locate_sibling, BundledBackend, ServingHost};

    let absent = |why: String| {
        tracing::warn!(rails_base = base, reason = %why, "ensure_rails: cw-rails is not reachable");
        why
    };
    let host = reqwest::Url::parse(base)
        .map_err(|e| absent(format!("the rails base {base} is not a URL: {e}")))?
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
    let serving = ServingHost::at(base)
        .ready_at("/v1/mesh/status")
        .bringing_up(
            BundledBackend::at(bin)
                .arg("run")
                .log_to(data_dir.join("rails.log")),
        );
    let outcome = std::thread::scope(|s| {
        s.spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("no runtime to reach cw-rails with: {e}"))
                .and_then(|rt| {
                    rt.block_on(serving.ensure_reachable(RAILS_BRING_UP_WINDOW))
                        .map_err(|e| e.to_string())
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
