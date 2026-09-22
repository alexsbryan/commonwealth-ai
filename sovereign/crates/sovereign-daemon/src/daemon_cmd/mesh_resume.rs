// SPDX-License-Identifier: AGPL-3.0-or-later
//! Split from daemon_cmd/mod.rs for the §3.2 size ceiling (behaviour-preserving move).

use std::sync::Arc;

use sovereign_core::setup_config::SetupConfig;

/// Resumes the persisted mesh, or bootstraps the first-boot one (solo node or
/// configured fleet joiner). `Some(exit_code)` aborts daemon boot.
pub(super) async fn resume_or_bootstrap_mesh(
    daemon: &Arc<crate::EmbeddedDaemon>,
    config: &SetupConfig,
) -> Option<i32> {
    // ── Resume or bootstrap a solo mesh ───────────────────────────
    match daemon.try_resume().await {
        Ok(true) => {
            tracing::info!("mesh resumed from persisted state");
        }
        Ok(false) => {
            // First boot, no persisted mesh. A fleet JOINER (config carries a
            // `[discovery] join_key`) joins an existing mesh through its static
            // seed addresses; a founder / standalone node (no join_key) creates
            // a silent solo mesh so the listener comes up.
            let hostname = hostname::get()
                .ok()
                .and_then(|h| h.into_string().ok())
                .unwrap_or_else(|| "sovereign".to_string());
            let disc = &config.discovery;
            // A local-only daemon that dials a seed is not local-only. Refused
            // here, loudly and by name, rather than letting the profile
            // quietly turn a fleet joiner into a split-brained solo node
            // (ARCH §18.3 — refuse, never silently substitute).
            let profile = crate::LocalOnlyProfile::resolve(config.daemon.local_only);
            if profile.is_local_only() && disc.join_key.is_some() {
                eprintln!(
                    "error: [daemon] local_only is set (source: {}) but [discovery] \
                     join_key names a mesh to join. A local-only daemon never dials a \
                     peer. Unset one of them: drop join_key to run a solo node, or \
                     unset local_only / {}=0 to join the fleet.",
                    profile.source().as_str(),
                    crate::local_only::ENV_VAR,
                );
                return Some(1);
            }
            match disc.join_key.as_deref() {
                // Configured fleet joiner: try each static seed as a direct
                // `/internal/join` target (no mDNS needed) until one accepts.
                // Hard-fail rather than fall back to a solo mesh — that would
                // split-brain the fleet.
                Some(join_key) if !disc.seed_addrs.is_empty() => {
                    let mut joined = false;
                    for seed in &disc.seed_addrs {
                        let link = sovereign_mesh::DeepLink::Join {
                            join_key: join_key.to_string(),
                            relay_hint: Some(seed.clone()),
                            mesh_name: None,
                            iroh_dial: None,
                            encrypted: false,
                            expires_at: None,
                        };
                        match daemon.join_mesh(&link, &hostname).await {
                            Ok(_) => {
                                tracing::info!(seed = %seed, "joined fleet via configured seed");
                                joined = true;
                                break;
                            }
                            Err(e) => {
                                tracing::warn!(
                                    seed = %seed,
                                    error = %e,
                                    "seed join failed; trying next seed"
                                );
                            }
                        }
                    }
                    if !joined {
                        eprintln!(
                            "error: could not join the mesh via any of the {} configured \
                             [discovery] seed_addrs — check the addresses are reachable and \
                             the join_key matches the founder's mesh",
                            disc.seed_addrs.len()
                        );
                        return Some(1);
                    }
                }
                // join_key set but nowhere to send it — a joiner with no way in.
                Some(_) => {
                    eprintln!(
                        "error: [discovery] join_key is set but seed_addrs is empty — \
                         a fleet joiner needs at least one reachable seed address"
                    );
                    return Some(1);
                }
                // No join credential: founder / standalone node.
                //
                // This mint is the campaign's CLASS 3 blocker, and under the
                // local-only profile it is closed WITHOUT a branch here. Its
                // `return 1` fired on `start_daemon`'s mDNS register/browse
                // failures — a real solo-node failure on a host whose network
                // namespace cannot bind the multicast socket. The profile
                // turns mDNS off inside `start_daemon`, so there is no
                // multicast bind to fail: the N=1 answer is TOTAL, and an
                // `if local { skip }` here would be a second decider for a
                // question the profile already answers (ARCH §10.6). What
                // remains fatal — AlreadyRunning, "no node in mesh", a client
                // listener that will not bind — is local and honestly fatal.
                None => {
                    let mesh_name = format!("{hostname}'s Mesh");
                    match daemon.create_mesh(&mesh_name, &hostname).await {
                        Ok(_result) => {
                            tracing::info!(%mesh_name, "solo mesh created");
                        }
                        Err(e) => {
                            eprintln!("error: could not create initial mesh: {e}");
                            return Some(1);
                        }
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("error: mesh resume failed: {e}");
            return Some(1);
        }
    }
    None
}
