// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reaching `serve`, the model server (FIVE_PROGRAMS §2), from the svrn
//! daemon (pb-svrn-dials-serve): where it listens, and whether this daemon
//! dials it at all.

use sovereign_contracts::launch::RpcServe;
use sovereign_contracts::setup_config::{EntryBinding, NodeSection, SetupConfig};

/// Where this daemon's inference is served from — THE one decider, read once
/// at boot after `apply_shared_model_role_to_env`, so the env contract it
/// reads is the one bootstrap.rs already translated (phase-b-24).
///
/// Mesh-distributed inference (RPC-worker discovery, the distributed-primary
/// respawn, the auto-warm orchestrator, the worker role) still needs the
/// loading process beside the roster, so a config that opts into it keeps the
/// in-process path until pb-serve-distributes moves it into serve. Every
/// other config dials serve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServingPath {
    /// The in-process engine, kept for an opt-in to distributed inference.
    /// `chosen_by` names the input that chose it, because four inputs are
    /// not predictable from outside (principle 1).
    InProcess { chosen_by: &'static str },
    /// Serving lives in `serve`, which this daemon dials.
    DialsServe,
}

impl ServingPath {
    /// Decide from each input's ONE existing reader, never a fresh `var_os`
    /// (principle 8): the subsystems the in-process path keeps gate on the
    /// same readers, so they and this decider cannot disagree on a spelling.
    pub fn decide(config: &SetupConfig) -> Self {
        let path = Self::from_inputs(
            &RpcServe::from_env(),
            crate::startup::rpc_discovery_armed(),
            &sovereign_inference::embedded::rpc_workers_from_env(),
            sovereign_inference::engine_factory::child_owns_primary(config),
        );
        tracing::info!(
            target: "serving_path",
            serving = %path.status_line(),
            owner = "pb-serve-distributes moves the in-process path into serve",
            "serving path decided (pb-svrn-dials-serve)"
        );
        path
    }

    /// The decision over the four inputs. A `Refused` RPC bind keeps today's
    /// path, so its refusal is still reported where it is today; an EMPTY
    /// `SOVEREIGN_RPC_SERVE` is `Off` at its reader and dials serve.
    pub fn from_inputs(
        rpc_serve: &RpcServe,
        discovery_armed: bool,
        rpc_workers: &[String],
        child_owns_primary: bool,
    ) -> Self {
        let chosen_by = if !matches!(rpc_serve, RpcServe::Off) {
            "SOVEREIGN_RPC_SERVE"
        } else if discovery_armed {
            "SOVEREIGN_RPC_DISCOVER"
        } else if !rpc_workers.is_empty() {
            "SOVEREIGN_RPC_WORKERS"
        } else if child_owns_primary {
            "[compute] distributed_primary"
        } else {
            return Self::DialsServe;
        };
        Self::InProcess { chosen_by }
    }

    /// How `svrn daemon status` names the path: `in-process (<input>)`, or
    /// `serve`.
    pub fn status_line(&self) -> String {
        match self {
            Self::InProcess { chosen_by } => format!("in-process ({chosen_by})"),
            Self::DialsServe => "serve".to_string(),
        }
    }
}

/// Where `serve` listens when nothing says otherwise. Mirrors
/// `sovereign_serve::DEFAULT_LISTEN` — 9748, beside cw-rails' 9747 and
/// outside the 9741..9745 family this daemon binds (the two programs are
/// built and versioned separately, so the convention is mirrored and
/// documented on both sides rather than imported across the lift boundary,
/// as `DEFAULT_RAILS_BASE` is).
pub const DEFAULT_SERVE_BASE: &str = "http://127.0.0.1:9748";

/// The base this daemon dials `serve` at, without `/v1`: `[node] entry` when
/// it is set, else [`DEFAULT_SERVE_BASE`]. THE one reader of that answer.
///
/// An identity binding (`[node] entry_node`) names a mesh peer, not a
/// process on this host, so it never names `serve`; it answers the default
/// and says so.
pub fn resolve_serve_base(node: &NodeSection) -> String {
    match node.binding() {
        Some(EntryBinding::Address(url)) => {
            let base = url
                .trim_end_matches('/')
                .trim_end_matches("/v1")
                .to_string();
            tracing::debug!(serve_base = %base, source = "[node] entry", "serve base resolved");
            base
        }
        Some(EntryBinding::Node(id)) => {
            tracing::debug!(
                serve_base = DEFAULT_SERVE_BASE,
                source = "default",
                entry_node = %id,
                "serve base resolved: an identity binding names a peer, not serve"
            );
            DEFAULT_SERVE_BASE.to_string()
        }
        None => {
            tracing::debug!(
                serve_base = DEFAULT_SERVE_BASE,
                source = "default",
                "serve base resolved"
            );
            DEFAULT_SERVE_BASE.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(entry: Option<&str>, entry_node: Option<&str>) -> NodeSection {
        NodeSection {
            entry: entry.map(str::to_string),
            entry_node: entry_node.map(str::to_string),
            ..NodeSection::default()
        }
    }

    fn decide(serve: Option<&str>, discover: bool, workers: &[&str], primary: bool) -> ServingPath {
        let workers: Vec<String> = workers.iter().map(|w| w.to_string()).collect();
        ServingPath::from_inputs(
            &RpcServe::resolve(serve, false),
            discover,
            &workers,
            primary,
        )
    }

    #[test]
    fn a_default_config_dials_serve() {
        assert_eq!(decide(None, false, &[], false), ServingPath::DialsServe);
    }

    #[test]
    fn an_empty_rpc_serve_is_off_and_dials_serve() {
        assert_eq!(decide(Some(""), false, &[], false), ServingPath::DialsServe);
    }

    #[test]
    fn each_opt_in_keeps_the_in_process_path_and_names_itself() {
        let cases = [
            (
                decide(Some("127.0.0.1:50052"), false, &[], false),
                "SOVEREIGN_RPC_SERVE",
            ),
            // A refused plaintext-LAN bind keeps today's path and its refusal.
            (
                decide(Some("0.0.0.0:50052"), false, &[], false),
                "SOVEREIGN_RPC_SERVE",
            ),
            (decide(None, true, &[], false), "SOVEREIGN_RPC_DISCOVER"),
            (
                decide(None, false, &["10.0.0.2:50052"], false),
                "SOVEREIGN_RPC_WORKERS",
            ),
            (
                decide(None, false, &[], true),
                "[compute] distributed_primary",
            ),
        ];
        for (path, input) in cases {
            assert_eq!(path, ServingPath::InProcess { chosen_by: input });
            assert_eq!(path.status_line(), format!("in-process ({input})"));
        }
    }

    #[test]
    fn no_entry_dials_the_default_base() {
        assert_eq!(resolve_serve_base(&node(None, None)), DEFAULT_SERVE_BASE);
    }

    #[test]
    fn an_address_entry_is_the_base_without_its_v1() {
        for entry in ["http://127.0.0.1:18748/v1", "http://127.0.0.1:18748/v1/"] {
            assert_eq!(
                resolve_serve_base(&node(Some(entry), None)),
                "http://127.0.0.1:18748"
            );
        }
    }

    #[test]
    fn an_identity_binding_never_names_serve() {
        assert_eq!(
            resolve_serve_base(&node(None, Some("ab12"))),
            DEFAULT_SERVE_BASE
        );
    }
}
