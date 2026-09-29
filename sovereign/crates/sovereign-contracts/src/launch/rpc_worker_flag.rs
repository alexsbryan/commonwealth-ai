// SPDX-License-Identifier: AGPL-3.0-or-later
//! `--rpc-worker[=<bind>]`: svrn's CLI forwards it and the loader reads it,
//! so the parse is the launch contract's (moved from the daemon's bootstrap,
//! pb-serve-distributes).

#[cfg(test)]
mod tests;

/// Default bind for an anchor's in-process RPC worker. Applied only when a
/// `[shared_model]` role asks to serve but no explicit `SOVEREIGN_RPC_SERVE`
/// is set.
///
/// LOOPBACK, not `0.0.0.0`. The ggml rpc-server authenticates nothing and
/// encrypts nothing, so a node that only set `role = "anchor"` used to offer
/// its GPU and the tensors crossing it to every host on its network. Members
/// reach this worker through the encrypted mesh tunnel instead — the
/// `RPC_ALPN` splice in `sovereign_mesh::iroh_access`, which admits members
/// only — and that path needs no LAN bind. An operator who genuinely wants the
/// plaintext LAN bind sets it and acknowledges it; see
/// [`super::RpcServe`].
pub const DEFAULT_RPC_BIND: &str = "127.0.0.1:50052";

/// `--rpc-worker[=<bind>]` → the address this node should serve its GPU on.
///
/// Lending a GPU to the mesh was previously reachable only by editing
/// `[shared_model] role` in a TOML file or by knowing the name of an
/// undocumented environment variable. Both work; neither is something an
/// operator can discover from `--help`, and "turn this box into a worker" is a
/// one-line intention that deserves a one-line spelling.
///
/// Accepts `--rpc-worker`, `--rpc-worker=<bind>` and `--rpc-worker <bind>`. A
/// following token is taken as the bind only when it is not itself a flag, so
/// `--rpc-worker --setup-only` means the default bind, not a bind of
/// `--setup-only`.
///
/// This is deliberately NOT the same lever as `role = "anchor"`. The role also
/// turns on peer *discovery* (`SOVEREIGN_RPC_DISCOVER`) and enters this node in
/// the host election; this flag only offers the GPU. On a node whose daemon
/// predates the 2026-07-29 containment fix that distinction matters, because
/// the discovery flag is what the boot gate reads back — see
/// `sovereign_compute::containment`.
pub fn rpc_worker_flag(args: &[String]) -> Option<String> {
    let mut it = args.iter().enumerate();
    let (i, a) =
        it.find(|(_, a)| a.as_str() == "--rpc-worker" || a.starts_with("--rpc-worker="))?;
    if let Some(bind) = a.strip_prefix("--rpc-worker=") {
        let bind = bind.trim();
        // `--rpc-worker=` with nothing after it is a typo, not a request to
        // serve on the empty string (which `serve_rpc_worker_if_configured`
        // would silently ignore, leaving the operator with no worker and no
        // explanation).
        return Some(if bind.is_empty() {
            DEFAULT_RPC_BIND.to_string()
        } else {
            bind.to_string()
        });
    }
    let next = args
        .get(i + 1)
        .map(String::as_str)
        .filter(|n| !n.starts_with('-') && !n.is_empty());
    Some(next.unwrap_or(DEFAULT_RPC_BIND).to_string())
}
