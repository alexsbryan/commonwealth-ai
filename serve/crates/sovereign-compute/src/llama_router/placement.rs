// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where a distributed router puts a model (ENGINE_SWAP_PLAN F, f2): the
//! peer endpoints `[compute] rpc_peers` names and the preset key that sends
//! them to llama.cpp (`rpc`, the static endpoints as one comma list).
//!
//! The placement is upstream's, verbatim. The peers go in each CHAT section's
//! `rpc` key, written before anything that indexes a device — llama.cpp keeps
//! a preset's options in a `std::map` keyed by each option's first spelling,
//! so `--rpc` registers the devices before `--tensor-split` reads them, the
//! order `arg.cpp:2598` requires for `-mmdev`, which sorts between them. With
//! no `-ts` written, llama.cpp divides the model by the free memory each
//! device advertises — the peer's queried over RPC, this card's from the
//! shipped binary — and puts the RPC devices first in the device order
//! (`llama-model.cpp`: "add RPC servers at the front of the list to minimize
//! network transfers"). No planner and no `-ot` here: that is D4's question,
//! and its answer decides whether the planner earns a place beside this.
//!
//! # What a peer loss looks like, and what this module does about it
//!
//! Upstream never reconnects: any later RPC touch of a dead peer is
//! `GGML_ABORT` (`ggml-rpc.cpp:30`, `RPC_STATUS_ASSERT`; measured: the model
//! child dies at `ggml-rpc.cpp:647`, "Remote RPC server crashed", and
//! `/models` reports it `failed (exit 1)`). What dies is the MODEL child, not
//! the router: the router loads no tensors itself, so the daemon and
//! `/status` survive, and `live::LiveRouter`'s repair loop re-issues the load
//! with backoff (`REPAIR_BACKOFF`) until the peer answers — the head's half
//! of the restart, the peer's half being its own supervisor restarting
//! `ggml-rpc-server` (`rpc_peer::RpcPeerChild`). This module only names the
//! peers on that line ([`RouterPlacement::peer_loss`]); it holds no probe.
//!
//! No reachability probe gates the spawn, and this is deliberate: a peer's
//! `ggml-rpc-server` accepts ONE client (`transport.cpp:677`, `listen(sock,
//! 1)`, a single accept loop), so the head's own model holds the only slot
//! and a probe by anyone else sits in an empty backlog and times out. A
//! connect is therefore not an instrument for "is the peer there" — measured
//! 2026-10-24: every probe timed out while the head was serving across that
//! very peer.

use std::fmt;
use std::net::SocketAddr;

use serde::Serialize;
use sovereign_contracts::setup_config::SetupConfig;

/// Where a router's models run: this card alone, or across peers too.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RouterPlacement {
    /// No `[compute] rpc_peers`: one server, one device, as before.
    Local,
    /// The peers the head named, in the order they are written to `rpc`.
    Peers { peers: Vec<SocketAddr> },
}

impl Default for RouterPlacement {
    fn default() -> Self {
        Self::Local
    }
}

impl fmt::Display for RouterPlacement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local => f.write_str("this card alone"),
            Self::Peers { peers } => write!(f, "across {}", peer_list(peers)),
        }
    }
}

impl RouterPlacement {
    /// The placement `endpoints` (raw `ip:port` strings, `[compute]
    /// rpc_peers`) names. Each is refused by the same rule a peer's own
    /// `[compute] rpc_peer` bind is (`rpc_peer::parse_bind`): rpc-server
    /// authenticates nothing, so loopback and every-interface addresses are
    /// not endpoints a head can reach, and the refusal names the key.
    pub fn from_endpoints<'a>(
        endpoints: impl IntoIterator<Item = &'a str>,
    ) -> Result<Self, String> {
        let mut peers = Vec::new();
        for raw in endpoints {
            let raw = raw.trim();
            if raw.is_empty() {
                continue;
            }
            let addr = parse_peer(raw)?;
            if peers.contains(&addr) {
                return Err(format!(
                    "[compute] rpc_peers names {addr} twice; one peer is one device"
                ));
            }
            peers.push(addr);
        }
        match peers.is_empty() {
            true => Ok(Self::Local),
            false => Ok(Self::Peers { peers }),
        }
    }

    /// `[compute] rpc_peers` as it is written in `config`.
    pub fn from_config(config: &SetupConfig) -> Result<Self, String> {
        Self::from_endpoints(config.compute.rpc_peers.iter().map(String::as_str))
    }

    /// The peers, in `rpc` order; empty for [`Self::Local`].
    pub fn peers(&self) -> &[SocketAddr] {
        match self {
            Self::Local => &[],
            Self::Peers { peers } => peers,
        }
    }

    /// The section's `rpc` value: the static endpoints as llama.cpp parses
    /// them (`--rpc`, a comma-separated `host:port` list), or `None` when
    /// there are none.
    pub fn rpc_list(&self) -> Option<String> {
        match self.peers().is_empty() {
            true => None,
            false => Some(peer_list(self.peers())),
        }
    }

    /// The reason `/status` gives for a distributed model that is not
    /// `loaded`: `reason` followed by the peers it is split across, and what
    /// happens next. `None` for a single-card router, which adds nothing.
    ///
    /// This only FORMATS. Whether a peer is actually gone is the caller's,
    /// from the router's own log (`live::LiveRouter::peer_loss_from_log`):
    /// a probe is not an instrument here (see the module doc), and this
    /// function never blocks.
    pub fn peer_loss(&self, model: &str, reason: &str) -> Option<String> {
        match self.peers() {
            [] => None,
            peers => Some(format!(
                "{reason}; peer loss: {model} is distributed across {peers:?}, and an RPC touch \
                 of a peer that has stopped answering aborts this model's process \
                 (ggml-rpc.cpp:30, upstream never reconnects); the router reloads it with \
                 backoff once the peer's supervisor has restarted ggml-rpc-server"
            )),
        }
    }
}

/// The peers as llama.cpp's comma list (and as a log line names them).
fn peer_list(peers: &[SocketAddr]) -> String {
    peers
        .iter()
        .map(SocketAddr::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// `ip:port` a head can reach: refused when it names loopback (no peer
/// listens there) or every interface (a head needs one address). A hostname
/// is refused for the same reason a peer's own bind refuses one: the endpoint
/// is written into a preset llama.cpp parses verbatim.
fn parse_peer(raw: &str) -> Result<SocketAddr, String> {
    let hint =
        "hint: name the peer's LAN or tunnel ip:port, as its own [compute] rpc_peer names it";
    let addr: SocketAddr = raw
        .trim()
        .parse()
        .map_err(|e| format!("[compute] rpc_peers: {raw:?} is not an ip:port ({e})\n{hint}"))?;
    if addr.ip().is_unspecified() {
        return Err(format!(
            "[compute] rpc_peers: {raw:?} names every interface, and a head needs one address\n{hint}"
        ));
    }
    if addr.ip().is_loopback() {
        return Err(format!(
            "[compute] rpc_peers: {raw:?} is loopback, where no peer listens\n{hint}"
        ));
    }
    if addr.port() == 0 {
        return Err(format!(
            "[compute] rpc_peers: {raw:?} names port 0, which no peer listens on\n{hint}"
        ));
    }
    Ok(addr)
}

#[cfg(test)]
#[path = "placement_tests.rs"]
mod tests;
