// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for the RPC worker flag — see `bootstrap.rs`.
//!
//! Their own file because keeping them inline put that file past its
//! arch-gate slack (ARCH §3.1). `#[path]`, so the names are unchanged.

use super::{rpc_worker_flag, DEFAULT_RPC_BIND};

fn args(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_string()).collect()
}

/// Clause (a) of `tg-rpc-port-not-on-lan`: a node that sets NOTHING — no
/// env, no bind on the flag, only `role = "anchor"` — must not offer its
/// tensor port to the network. This is the default every other path in
/// this module falls back to, so it is asserted on the constant itself
/// rather than on one caller.
#[test]
fn the_default_bind_is_loopback_only() {
    let serve = sovereign_contracts::launch::RpcServe::resolve(Some(DEFAULT_RPC_BIND), false);
    assert!(
        serve.is_serving(),
        "the default must SERVE with no acknowledgement, not be refused: \
             {DEFAULT_RPC_BIND}"
    );
    assert!(
        DEFAULT_RPC_BIND.starts_with("127.0.0.1:"),
        "the default RPC bind must be loopback — the ggml worker \
             authenticates nothing, so {DEFAULT_RPC_BIND} would hand this \
             node's GPU to every host on its network"
    );
}

#[test]
fn absent_means_this_node_lends_nothing() {
    assert_eq!(rpc_worker_flag(&args(&["run"])), None);
    assert_eq!(rpc_worker_flag(&args(&[])), None);
}

#[test]
fn bare_flag_takes_the_documented_default() {
    assert_eq!(
        rpc_worker_flag(&args(&["run", "--rpc-worker"])).as_deref(),
        Some(DEFAULT_RPC_BIND)
    );
}

#[test]
fn an_explicit_bind_is_honoured_in_both_spellings() {
    for a in [
        args(&["--rpc-worker=10.0.0.4:9999"]),
        args(&["--rpc-worker", "10.0.0.4:9999"]),
    ] {
        assert_eq!(rpc_worker_flag(&a).as_deref(), Some("10.0.0.4:9999"));
    }
}

/// The space form must not swallow the next flag as a bind address.
/// `serve_rpc_worker_if_configured` would then try to bind `--setup-only`,
/// fail, and leave the operator with a daemon that quietly lends nothing.
#[test]
fn a_following_flag_is_not_mistaken_for_a_bind() {
    assert_eq!(
        rpc_worker_flag(&args(&["--rpc-worker", "--setup-only"])).as_deref(),
        Some(DEFAULT_RPC_BIND)
    );
}

/// `--rpc-worker=` is a typo. Serving on the empty string is silently a
/// no-op inside ggml, so treat it as the default rather than as consent to
/// do nothing.
#[test]
fn an_empty_bind_falls_back_rather_than_serving_nothing() {
    assert_eq!(
        rpc_worker_flag(&args(&["--rpc-worker="])).as_deref(),
        Some(DEFAULT_RPC_BIND)
    );
}

/// The flag has to survive the trip to the detached child `daemon start`
/// spawns, which re-parses it from the `--rpc-worker=<bind>` form.
#[test]
fn the_forwarded_form_round_trips() {
    let bind = rpc_worker_flag(&args(&["--rpc-worker", "192.168.1.2:50052"])).expect("parsed once");
    let forwarded = args(&["run", &format!("--rpc-worker={bind}")]);
    assert_eq!(rpc_worker_flag(&forwarded), Some(bind));
}
