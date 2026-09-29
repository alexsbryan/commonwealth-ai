// SPDX-License-Identifier: AGPL-3.0-or-later
//! Boot admission for the DISTRIBUTED primary: is ggml's RPC client allowed to
//! live in the daemon's own address space?
//!
//! ## Why this is a boot guard and not a runtime fix
//!
//! When a model is sharded across mesh RPC workers and one of those workers
//! leaves, the daemon's discovery loop reacts by reloading the primary. That
//! reload's teardown has to free the old sharded model's buffers **on the worker
//! that has already gone** — and ggml's RPC client has no error path for a dead
//! endpoint. It calls `GGML_ABORT` (`ggml-rpc.cpp:386`), which is a `SIGABRT` of
//! the whole process: gossip, `/status`, the client API, the desktop bridge, all
//! of it. That is not a hypothetical; it killed this daemon live on 2026-07-27
//! (note c4ef6fa0, systemd exit 134), and the mitigation built to protect the
//! host from a departed worker — shrink-fast-prune — turned out to be a
//! guaranteed instance of it.
//!
//! There is no safe runtime action available once a sharded worker has
//! departed: the reload aborts, and so does the teardown. Since the failure
//! cannot be handled where it happens, the only place left to intervene is
//! admission — refuse to enter the configuration at all.
//!
//! The containment that DOES work is `[compute] distributed_primary`: the model
//! loads in a supervised child process, so the same abort kills the child and
//! the daemon observes an exit it can re-plan around. That was proven live on
//! 2026-07-28 — worker `kill -9` mid-decode, daemon survived with
//! `NRestarts=0`, and a real request was served from the re-formed cluster
//! fourteen minutes later.
//!
//! ## Shape
//!
//! [`classify_containment`] is pure and takes every input as an argument, so the
//! whole decision table reads as one expression, the incident is replayable in a
//! unit test, and `sovereign doctor` can render the same verdict the boot guard
//! enforces without duplicating the rule.

use sovereign_contracts::setup_config::{SetupConfig, SharedModelRole};

/// The pure decider — [`classify_containment`], its verdict, the override
/// name and the one `SOVEREIGN_RPC_DISCOVER` reader — is the shared contract's
/// (`sovereign_contracts::containment`, pb-serve-distributes): serve's boot
/// guard below and svrn's doctor read one table.
pub use sovereign_contracts::containment::{
    classify_containment, rpc_discovery_armed, ContainmentVerdict, OVERRIDE_ENV,
};

/// Read the environment, classify, report, and say whether boot may continue.
///
/// `self_node_id` is `None` at this point in startup on the normal path (the
/// mesh identity is not loaded until later), in which case the `host_node_id`
/// pin is treated as NOT self and the decision falls through to the role term.
/// That is the right direction for a boot guard: refuse on a KNOWN hazard,
/// never on an unknown — the same posture the VRAM preflight takes when its
/// sensor is unreadable.
pub fn check_containment(config: &SetupConfig, self_node_id: Option<&str>) -> bool {
    let child_owns_primary = sovereign_inference::engine_factory::child_owns_primary(config);
    let pinned_host_is_self = match (config.shared_model.host_node_id.as_deref(), self_node_id) {
        (Some(pin), Some(me)) => pin.eq_ignore_ascii_case(me),
        _ => false,
    };
    let verdict = classify_containment(
        child_owns_primary,
        config.shared_model.role,
        pinned_host_is_self,
        rpc_discovery_armed(),
        std::env::var(OVERRIDE_ENV).is_ok(),
    );

    match verdict {
        ContainmentVerdict::NotApplicable => {}
        ContainmentVerdict::Armed => {
            tracing::info!(
                target: "compute_child",
                "distributed-primary containment ARMED — the primary loads in a supervised \
                 child, so a worker-loss abort kills the child, not the daemon"
            );
        }
        ContainmentVerdict::Warn => {
            tracing::warn!(
                target: "compute_child",
                "this node is a shared-model ANCHOR with no distributed-primary containment. \
                 If it wins the host election it will hold the split IN-PROCESS, where a \
                 worker leaving aborts the whole daemon (ggml-rpc.cpp:386, SIGABRT). \
                 Set `[compute] enabled = true` + `distributed_primary = true`."
            );
        }
        ContainmentVerdict::RefuseOverridden => {
            eprintln!(
                "warning: running a DISTRIBUTED primary IN-PROCESS because {OVERRIDE_ENV} is set."
            );
            eprintln!(
                "         A worker leaving the mesh will abort this daemon (SIGABRT, exit 134). \
                 You have been warned."
            );
            tracing::warn!(
                target: "compute_child",
                "in-process distributed primary permitted by {OVERRIDE_ENV}"
            );
        }
        ContainmentVerdict::Refuse => {
            tracing::error!(
                target: "compute_child",
                "containment refused boot: a shared-model HOST would run the DISTRIBUTED \
                 primary IN-PROCESS, where a worker leaving aborts the daemon; set \
                 `[compute] enabled = true` + `distributed_primary = true`"
            );
            eprintln!(
                "error: this node is a shared-model HOST but the DISTRIBUTED primary would run"
            );
            eprintln!(
                "       IN-PROCESS. A worker leaving the mesh then drives an in-place reload"
            );
            eprintln!(
                "       whose teardown frees buffers on the departed worker — ggml aborts the"
            );
            eprintln!("       whole daemon (SIGABRT, exit 134). Confirmed live 2026-07-27.");
            eprintln!();
            eprintln!("fix:   add to {} —", SetupConfig::default_path().display());
            eprintln!();
            eprintln!("           [compute]");
            eprintln!("           enabled = true");
            eprintln!("           distributed_primary = true");
            eprintln!();
            eprintln!(
                "       The primary then runs in a supervised child: the same abort kills the"
            );
            eprintln!(
                "       child, the daemon respawns it across the surviving workers, and gossip"
            );
            eprintln!("       / /status / the client API stay up. Proven live 2026-07-28.");
            eprintln!();
            eprintln!("alt:   set `[shared_model] role = \"consumer\"` to stop hosting the split,");
            eprintln!("       or point `[models] primary` at a model that fits one box.");
            eprintln!("       (Override at your own risk: {OVERRIDE_ENV}=1)");
        }
    }

    verdict.proceeds()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Replays the configuration that aborted the daemon on 2026-07-27: a
    /// declared host, a distributable primary, and no compute-child boundary.
    /// The abort was `ggml-rpc.cpp:386` during `reload_primary`'s teardown of a
    /// worker that had already left — uncatchable, hence admission-time refusal.
    /// The refusal reaches the daemon's log, not only stderr: a detached
    /// daemon discards eprintln, so an eprintln-only refusal is dark.
    #[test]
    fn a_refused_boot_is_logged_where_the_daemon_filter_admits_it() {
        assert!(
            std::env::var_os(OVERRIDE_ENV).is_none(),
            "the override turns the refusal into a warning"
        );
        let mut config = SetupConfig::unconfigured();
        config.shared_model.role = SharedModelRole::Host;
        let (proceeds, log) =
            crate::logged_under("compute_child=info", || check_containment(&config, None));
        assert!(!proceeds, "a declared host with no containment is refused");
        assert!(
            log.contains("containment refused boot"),
            "no refusal event at compute_child: {log:?}"
        );
    }

    #[test]
    fn classify_containment_replays_the_2026_07_27_abort() {
        assert_eq!(
            classify_containment(false, SharedModelRole::Host, true, false, false),
            ContainmentVerdict::Refuse
        );
    }

    /// The fixed configuration must never be refused, for any combination of
    /// the other inputs. This is the invariant that keeps the guard from
    /// blocking the very posture it is telling people to adopt.
    #[test]
    fn armed_containment_is_never_refused() {
        for role in [
            SharedModelRole::Consumer,
            SharedModelRole::Anchor,
            SharedModelRole::Host,
        ] {
            for pinned in [false, true] {
                for discover in [false, true] {
                    for over in [false, true] {
                        let v = classify_containment(true, role, pinned, discover, over);
                        assert_eq!(v, ContainmentVerdict::Armed, "role={role:?}");
                        assert!(v.proceeds());
                    }
                }
            }
        }
    }

    /// Back-compat guard: an ordinary single-node user (the default role) must
    /// not notice this guard exists.
    #[test]
    fn a_consumer_is_never_touched() {
        for pinned in [false, true] {
            // `pinned_host_is_self` true would mean the operator pinned THIS
            // node as the host, which is a host declaration by another name —
            // so only the unpinned case is "untouched".
            let v = classify_containment(false, SharedModelRole::Consumer, pinned, false, false);
            if pinned {
                assert_eq!(v, ContainmentVerdict::Refuse);
            } else {
                assert_eq!(v, ContainmentVerdict::NotApplicable);
                assert!(v.proceeds());
            }
        }
    }

    /// An anchor is one election away from the hazard, not in it. Warn, boot.
    /// Refusing here would strand a whole fleet on upgrade.
    #[test]
    fn an_anchor_warns_but_boots() {
        let v = classify_containment(false, SharedModelRole::Anchor, false, false, false);
        assert_eq!(v, ContainmentVerdict::Warn);
        assert!(v.proceeds());
    }

    #[test]
    fn the_override_downgrades_refusal_but_not_the_shape() {
        let v = classify_containment(false, SharedModelRole::Host, false, false, true);
        assert_eq!(v, ContainmentVerdict::RefuseOverridden);
        assert!(v.proceeds());
    }

    /// The verdict is SIZE-BLIND, and this test exists to stop anyone
    /// reintroducing a size threshold. `classify_containment` takes no size
    /// argument at all — by construction, a 4B host is refused exactly like a
    /// 122B host. The 2026-07-25 forced-tunnel E2E ran a 4B primary as
    /// `role = "host"`, and that configuration is exposed to the identical
    /// teardown abort.
    #[test]
    fn a_small_primary_is_refused_too() {
        assert_eq!(
            classify_containment(false, SharedModelRole::Host, false, false, false),
            ContainmentVerdict::Refuse
        );
    }

    /// The CLI power-user path: `SOVEREIGN_RPC_DISCOVER` by hand turns a
    /// consumer into a node that discovers workers and can win the election.
    #[test]
    fn an_env_forced_discover_is_treated_as_hosting() {
        assert_eq!(
            classify_containment(false, SharedModelRole::Consumer, false, true, false),
            ContainmentVerdict::Refuse
        );
    }

    /// The production call, which the anchor test above could not reach.
    ///
    /// `an_anchor_warns_but_boots` passes `discover_forced_by_env = false` — a
    /// value the real boot never produces. `apply_shared_model_role_to_env`
    /// (mod.rs:336) sets `SOVEREIGN_RPC_DISCOVER=1` for any serving role, and
    /// `check_containment` (inference.rs:76) reads it afterwards, so an anchor
    /// always arrives here with the flag ON. Before 2026-07-29 that reached the
    /// `declared_host || …` arm and refused the boot outright, telling the
    /// operator their anchor was "a shared-model HOST". BeefyMac hit exactly
    /// this with a config whose only shared-model line was `role = "anchor"`.
    ///
    /// The flag is the daemon's own output on this path, so it carries no
    /// information about operator intent and must not decide anything.
    #[test]
    fn an_anchor_still_boots_when_its_own_role_set_the_discover_flag() {
        let v = classify_containment(false, SharedModelRole::Anchor, false, true, false);
        assert_eq!(
            v,
            ContainmentVerdict::Warn,
            "the role set that flag; reading it back must not turn an anchor into a host"
        );
        assert!(v.proceeds());
    }

    /// …and the same self-set flag must not silently promote a HOST out of
    /// refusal either. Host refuses on the role term alone, flag or no flag.
    #[test]
    fn a_host_is_refused_whether_or_not_the_discover_flag_is_set() {
        for flag in [false, true] {
            assert_eq!(
                classify_containment(false, SharedModelRole::Host, false, flag, false),
                ContainmentVerdict::Refuse,
                "host refuses on its role, independent of the discover flag"
            );
        }
    }
}
