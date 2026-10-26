// SPDX-License-Identifier: AGPL-3.0-or-later
//! The peer side of a distributed model: this node lends its GPU to another
//! node's llama-server through the shipped `ggml-rpc-server -c`, a supervised
//! child at the static `[compute] rpc_peer` address (ENGINE_SWAP_PLAN F, f1).
//! No discovery and no mesh registration: the head names this address in its
//! own config.
//!
//! Started once per serving process, on the first build, and kept across
//! reloads (`assembly::ReloadFactory`); it dies with the process, as the
//! router does. The child is the pinned build ([`build_pin::verify`]), so a
//! head on the same `PIN.toml` speaks its protocol; a head on another build
//! is refused by upstream's HELLO (`ggml-rpc.cpp:350`: a major that differs,
//! or a server minor newer than the client's) before it loads anything.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use sovereign_contracts::setup_config::SetupConfig;
use tokio::sync::broadcast::error::RecvError;

use crate::llama_router::build_pin::{self, BuildPin};
use crate::llama_router::live::BinarySource;
use crate::supervisor::{HealthTarget, Supervisor, SupervisorConfig, SupervisorState};

/// The server a peer runs, installed beside `llama-server` (`PIN.toml`).
pub const RPC_SERVER_BINARY: &str = "ggml-rpc-server";

/// The key every refusal names.
const KEY: &str = "[compute] rpc_peer";

/// Spawn to listening: rpc-server initialises its devices and binds; it loads
/// nothing until a head connects.
const READY: Duration = Duration::from_secs(60);

/// The address `[compute] rpc_peer` names, or `None` when this node lends
/// nothing. Refused unless another host could reach it ([`parse_bind`]).
pub fn configured_bind(config: &SetupConfig) -> Result<Option<SocketAddr>, String> {
    config
        .compute
        .rpc_peer
        .as_deref()
        .map(parse_bind)
        .transpose()
}

/// An `ip:port` another host can reach. rpc-server authenticates nothing and
/// prints "Never expose the RPC server to an open network" on any host but
/// 127.0.0.1, so the bind is one LAN or tunnel address: never every interface
/// (`0.0.0.0`, `::`), and never loopback, which no other host can reach.
pub fn parse_bind(raw: &str) -> Result<SocketAddr, String> {
    let hint = "hint: name this host's LAN or tunnel address, e.g. 192.168.1.20:50052";
    let addr: SocketAddr = raw
        .trim()
        .parse()
        .map_err(|e| format!("{KEY} = {raw:?} is not an ip:port ({e})\n{hint}"))?;
    let refusal = endpoint_refusal(&addr).map(|why| match why {
        EndpointRefusal::EveryInterface => {
            "binds every interface, and rpc-server authenticates nothing"
        }
        EndpointRefusal::Loopback => "is loopback, which no other host can reach",
        EndpointRefusal::PortZero => "names port 0, which a head cannot be told",
    });
    match refusal {
        Some(why) => Err(format!("{KEY} = {raw:?} {why}\n{hint}")),
        None => Ok(addr),
    }
}

/// Why `addr` is no endpoint between a head and a peer. One rule for both
/// ends: a peer's own bind ([`parse_bind`]) and the peers a head names
/// (`llama_router::placement`), each refusing in its own words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointRefusal {
    EveryInterface,
    Loopback,
    PortZero,
}

/// `None` when `addr` is one address another host can reach.
pub fn endpoint_refusal(addr: &SocketAddr) -> Option<EndpointRefusal> {
    if addr.ip().is_unspecified() {
        Some(EndpointRefusal::EveryInterface)
    } else if addr.ip().is_loopback() {
        Some(EndpointRefusal::Loopback)
    } else if addr.port() == 0 {
        Some(EndpointRefusal::PortZero)
    } else {
        None
    }
}

/// `ggml-rpc-server` beside the server this node would run: the
/// `[engine] server_binary` override's directory, else the daemon's.
fn resolve_binary(config: &SetupConfig) -> Result<(PathBuf, BinarySource), String> {
    let (server, source) = match &config.engine.server_binary {
        Some(path) => (path.clone(), BinarySource::Override),
        None => (
            sovereign_inference::gpu_probe::shipped_server_binary()?,
            BinarySource::Shipped,
        ),
    };
    let binary = server.with_file_name(RPC_SERVER_BINARY);
    match binary.is_file() {
        true => Ok((binary, source)),
        false => Err(format!(
            "{KEY} needs {RPC_SERVER_BINARY} at {}, and it is not there.\nhint: {}/build.sh \
             installs it beside llama-server",
            binary.display(),
            build_pin::SERVER_SOURCE_DIR
        )),
    }
}

/// The supervised `ggml-rpc-server` this process started.
pub struct RpcPeerChild {
    bind: SocketAddr,
    supervisor: Arc<Supervisor>,
}

impl RpcPeerChild {
    /// Start the child at `bind` and return once it listens. Blocking: call
    /// it off the async workers (serving assembly runs on `spawn_blocking`).
    pub fn start(bind: SocketAddr, config: &SetupConfig) -> Result<Arc<Self>, String> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| format!("{KEY}: the peer child is started from a tokio runtime"))?;
        // Named before the spawn: an address of another host would only show
        // as a child that exits until the crash budget is spent.
        std::net::TcpListener::bind((bind.ip(), 0))
            .map_err(|e| format!("{KEY} = \"{bind}\" is not an address of this host ({e})"))?;
        let (binary, source) = resolve_binary(config)?;
        match source {
            BinarySource::Shipped => {
                let pin = BuildPin::compiled()?;
                build_pin::verify(&binary, &pin).map_err(|e| format!("{KEY}: {e}"))?;
                tracing::info!(target: "rpc_peer", %pin, "ggml-rpc-server build verified against the compiled pin");
            }
            BinarySource::Override => tracing::info!(
                target: "rpc_peer",
                binary = %binary.display(),
                "ggml-rpc-server build not checked: [engine] server_binary overrides the shipped one"
            ),
        }
        // rpc-server keeps its cache at `$LLAMA_CACHE/rpc/`; set here, so it
        // is a directory the daemon names and never the user's own cache.
        let dir = config.data.dir.join("rpc-peer");
        std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        let args = vec![
            "-H".to_string(),
            bind.ip().to_string(),
            "-p".to_string(),
            bind.port().to_string(),
            "-c".to_string(),
        ];
        tracing::info!(
            target: "rpc_peer",
            binary = %binary.display(),
            ?source,
            %bind,
            cache = %dir.join("rpc").display(),
            "starting the rpc peer child"
        );
        let supervisor = Arc::new(Supervisor::new(SupervisorConfig {
            binary_path: binary,
            args,
            working_dir: Some(dir.clone()),
            env: vec![("LLAMA_CACHE".into(), dir.to_string_lossy().into_owned())],
            health: HealthTarget::TcpListening(bind),
            crash_log_dir: config.data.dir.join("compute-crash-logs"),
            heartbeat_interval: Duration::from_secs(1),
            heartbeat_timeout: Duration::from_secs(2),
            heartbeat_failure_threshold: 3,
            ready_deadline: READY,
            backoff_schedule: [1, 2, 10, 60].map(Duration::from_secs).to_vec(),
            healthy_reset_after: Duration::from_secs(60),
            crash_loop_max: 5,
            stderr_ring_lines: 200,
            terminate_grace: Duration::from_secs(10),
        }));
        let mut states = supervisor.subscribe();
        runtime.spawn({
            let supervisor = Arc::clone(&supervisor);
            async move { supervisor.run().await }
        });
        let child = Arc::new(Self { bind, supervisor });
        if let Err(e) = runtime.block_on(child.await_listening(&mut states)) {
            child.terminate();
            return Err(e);
        }
        runtime.spawn(follow(bind, states));
        Ok(child)
    }

    /// The address this child serves.
    pub fn bind(&self) -> SocketAddr {
        self.bind
    }

    /// Stop the child.
    pub fn terminate(&self) {
        let stopped = self.supervisor.terminate();
        tracing::info!(target: "rpc_peer", bind = %self.bind, stopped, "rpc peer: terminate requested");
    }

    async fn await_listening(
        &self,
        states: &mut tokio::sync::broadcast::Receiver<SupervisorState>,
    ) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + READY;
        let tail = || self.supervisor.stderr_tail().join("\n");
        loop {
            let state = tokio::time::timeout_at(deadline, states.recv())
                .await
                .map_err(|_| {
                    format!(
                        "{KEY}: {RPC_SERVER_BINARY} did not listen at {} within {READY:?}; its \
                         last lines:\n{}",
                        self.bind,
                        tail()
                    )
                })?;
            match state {
                Ok(SupervisorState::Healthy { pid, .. }) => {
                    tracing::info!(target: "rpc_peer", pid, bind = %self.bind, "rpc peer: listening");
                    return Ok(());
                }
                Ok(SupervisorState::Failed { reason, .. }) => {
                    return Err(format!(
                        "{KEY}: {RPC_SERVER_BINARY} failed to start at {}: {reason}; its last \
                         lines:\n{}",
                        self.bind,
                        tail()
                    ))
                }
                Ok(s) => tracing::debug!(target: "rpc_peer", state = ?s, "rpc peer: starting"),
                Err(e) => {
                    tracing::debug!(target: "rpc_peer", error = %e, "supervisor state lagged")
                }
            }
        }
    }
}

/// Log every transition after start: a restart is a head's peer gone for a
/// while, and must be visible on this side too.
async fn follow(bind: SocketAddr, mut states: tokio::sync::broadcast::Receiver<SupervisorState>) {
    loop {
        match states.recv().await {
            Ok(s @ (SupervisorState::Restarting { .. } | SupervisorState::Failed { .. })) => {
                tracing::warn!(target: "rpc_peer", %bind, state = ?s, "rpc peer: lifecycle")
            }
            Ok(s) => tracing::info!(target: "rpc_peer", %bind, state = ?s, "rpc peer: lifecycle"),
            Err(RecvError::Lagged(n)) => {
                tracing::debug!(target: "rpc_peer", skipped = n, "supervisor state lagged")
            }
            Err(RecvError::Closed) => return,
        }
    }
}

#[cfg(test)]
#[path = "rpc_peer_tests.rs"]
mod tests;
