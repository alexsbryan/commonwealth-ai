// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reaching `serve`, the model server (FIVE_PROGRAMS §2), from the svrn
//! daemon (pb-svrn-dials-serve): where it listens.

use sovereign_contracts::setup_config::{EntryBinding, NodeSection};

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
