// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where a distributed router puts a model (ENGINE_SWAP_PLAN F, f2): the
//! peer endpoints `[compute] rpc_peers` names, the preset key that sends them
//! to llama.cpp (`rpc`, the static endpoints as one comma list), and the
//! split upstream computes from what each device reports.
//!
//! The placement is upstream's, verbatim: the peers go in the section's
//! `rpc` key, written before anything that indexes a device (llama.cpp keeps
//! a preset's options in a `std::map` keyed by each option's first spelling,
//! so `--rpc` registers the devices before `--tensor-split` reads them —
//! the order `arg.cpp:2598` requires for `-mmdev`, which sorts between
//! them), and with no `-ts` llama.cpp divides the model by the free memory
//! each device advertises — the peer's over RPC, this card's from the
//! shipped binary. No planner and no `-ot` here: that is D4's question, and
//! its answer decides whether the planner earns a place beside this.
//!
//! # Restart on peer loss
//!
//! Upstream never reconnects: any later RPC touch of a dead peer is
//! `GGML_ABORT` (`ggml-rpc.cpp:30`, `RPC_STATUS_ASSERT`), so a peer that
//! dies while its tensors are loaded aborts the router child. That abort is
//! contained here — the router is the only process that holds the peer's
//! buffers (`llama_router::live::LiveRouter`, started by serving assembly on
//! `spawn_blocking`), so the daemon and its `/status` survive. The router's
//! supervisor restarts the child; [`RouterPlacement::wait_for_peers`] holds
//! that restart until every peer answers, so a peer that is down is waited
//! out rather than spinning the child against a socket that is not there,
//! and [`RouterPlacement::peer_loss`] names which peer is gone on `/status`.
//! Never a fall back to this card alone: this node's config says the model is
//! distributed, and loading it whole would be the host-starving load the
//! split exists to prevent (ARCH 6).

use std::fmt;
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use serde::Serialize;
use sovereign_contracts::setup_config::SetupConfig;

/// Connecting time for one peer probe: a peer is on a LAN or a tunnel, and a
/// route that takes longer than this is not answering.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// How often the supervisor re-probes a peer that is not answering, between
/// the backoff schedule's attempts.
pub const PROBE_BACKOFF: [Duration; 4] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(10),
    Duration::from_secs(30),
];

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

    /// Wait until every peer accepts a TCP connection, at most `budget`, and
    /// say which did not. Called before the router's child is spawned for a
    /// distributed section: a peer that is coming back up (its own
    /// supervisor restarted `ggml-rpc-server`) is waited out here instead of
    /// spawning a child that loads nothing and dies, spending the crash
    /// budget on a socket that is not there yet.
    ///
    /// `Ok(())` for [`Self::Local`] at once — nothing to wait on.
    pub fn wait_for_peers(&self, budget: Duration) -> Result<(), String> {
        let deadline = Instant::now() + budget;
        let mut attempt = 0usize;
        loop {
            let unready = self.unanswered();
            if unready.is_empty() {
                tracing::info!(
                    target: "llama_router",
                    peers = ?self.peers(),
                    "placement: every peer answers"
                );
                return Ok(());
            }
            if Instant::now() >= deadline {
                tracing::warn!(
                    target: "llama_router",
                    unready = ?unready,
                    "placement: a peer never answered; the router is not spawned against it"
                );
                return Err(format!(
                    "the peers this node's router loads across are not answering: {unready:?}. \
                     A distributed section is never loaded on this card alone — the peers run \
                     the shipped ggml-rpc-server at the addresses [compute] rpc_peers names \
                     (es-f1's supervised child), and a head waits for them with backoff rather \
                     than spin a model load against a socket that is not there."
                ));
            }
            let backoff = PROBE_BACKOFF[attempt.min(PROBE_BACKOFF.len() - 1)];
            attempt += 1;
            tracing::debug!(
                target: "llama_router",
                unready = ?unready,
                ?backoff,
                "placement: waiting for a peer"
            );
            std::thread::sleep(backoff.min(deadline.saturating_duration_since(Instant::now())));
        }
    }

    /// The peers that do not accept a TCP connection now, in `rpc` order.
    pub fn unanswered(&self) -> Vec<SocketAddr> {
        self.peers()
            .iter()
            .copied()
            .filter(|addr| TcpStream::connect_timeout(addr, PROBE_TIMEOUT).is_err())
            .collect()
    }

    /// The router line `/status` reports when a peer is gone: the supervisor's
    /// own reason, then the peers that are not answering, named. `None` while
    /// every peer answers, so a healthy distributed router adds nothing.
    ///
    /// The abort needs no probe to be seen — a dead peer's next touch is
    /// `GGML_ABORT`, the child exits, and its `failed` status is the
    /// listing's — but the probe names WHICH peer, and puts it on the
    /// router's line rather than in the child's stderr ring.
    pub fn peer_loss(&self, what: &str, reason: &str) -> Option<String> {
        let unready = self.unanswered();
        match unready.is_empty() {
            true => None,
            false => Some(format!(
                "{reason}; peer loss: the split device(s) {unready:?} behind {what} are not \
                 answering, so an RPC touch of one aborts this child (ggml-rpc.cpp:30, upstream \
                 never reconnects); its supervisor restarts it once they answer"
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
    if addr.ip().is_unspecified() || addr.ip() == IpAddr::from([0, 0, 0, 0]) {
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
