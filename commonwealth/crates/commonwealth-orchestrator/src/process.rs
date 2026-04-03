use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::process::{Child, Command};
use tracing::{info, warn};

use commonwealth_core::capabilities::ProcessKind;
use commonwealth_core::ids::ProcessId;
use commonwealth_core::scheduler::{ShardAssignment, ShardPlan};
use commonwealth_core::Error;

/// A process managed by the orchestrator.
pub struct ManagedProcess {
    pub id: ProcessId,
    pub kind: ProcessKind,
    pub state: ProcessState,
    pub listen_address: SocketAddr,
    pub spawned_at: u64,
    child: Option<Child>,
    /// The OS PID, cached after spawn.
    pub pid: Option<u32>,
}

/// State of a managed process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    Starting,
    Running,
    Unhealthy,
    Failed,
    Stopped,
}

/// Configuration for spawning inference processes.
#[derive(Debug, Clone)]
pub struct SpawnConfig {
    /// Path to the llama-server binary.
    pub llama_server_path: String,
    /// Path to the rpc-server binary.
    pub rpc_server_path: String,
    /// Path to the model file on disk.
    pub model_path: String,
    /// Timeout for graceful stop (SIGTERM) before SIGKILL.
    pub stop_timeout: Duration,
}

impl Default for SpawnConfig {
    fn default() -> Self {
        Self {
            llama_server_path: "llama-server".into(),
            rpc_server_path: "rpc-server".into(),
            model_path: String::new(),
            stop_timeout: Duration::from_secs(10),
        }
    }
}

impl ManagedProcess {
    /// Check if the process is still alive by checking the child handle.
    pub fn is_alive(&mut self) -> bool {
        if let Some(child) = &mut self.child {
            match child.try_wait() {
                Ok(Some(_status)) => {
                    // Process has exited.
                    self.state = ProcessState::Failed;
                    false
                }
                Ok(None) => true, // Still running.
                Err(_) => {
                    self.state = ProcessState::Failed;
                    false
                }
            }
        } else {
            false
        }
    }

    /// Stop the process gracefully, then force-kill if needed.
    pub async fn stop(&mut self) -> Result<(), Error> {
        self.state = ProcessState::Stopped;

        if let Some(mut child) = self.child.take() {
            // Start an async kill (sends SIGKILL on unix, TerminateProcess on Windows).
            // For a more graceful approach, the llama-server should handle SIGTERM via
            // its own signal handler; here we give it a brief window then force-kill.
            let _ = child.start_kill();

            // Wait briefly for exit.
            match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
                Ok(Ok(_)) => {
                    info!(id = %self.id, "process stopped gracefully");
                }
                _ => {
                    // Force kill.
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    warn!(id = %self.id, "process force-killed");
                }
            }
        }

        Ok(())
    }

    /// Get the OS PID if available.
    pub fn os_pid(&self) -> Option<u32> {
        self.pid
    }
}

/// Spawn an rpc-server process for a shard assignment.
///
/// The rpc-server hosts a subset of model layers on a specific GPU,
/// making them accessible to the entry node's llama-server via RPC.
pub async fn spawn_rpc_server(
    assignment: &ShardAssignment,
    model_path: &str,
    config: &SpawnConfig,
) -> Result<ManagedProcess, Error> {
    let process_id = ProcessId::generate();
    let addr = assignment.rpc_address;

    let mut cmd = Command::new(&config.rpc_server_path);
    cmd.args([
        "--host",
        &addr.ip().to_string(),
        "--port",
        &addr.port().to_string(),
    ]);

    // GPU binding.
    cmd.env("CUDA_VISIBLE_DEVICES", assignment.gpu_index.to_string());

    // Model and layer range.
    cmd.args(["--model", model_path]);
    cmd.args([
        "--layers",
        &format!("{}-{}", assignment.layers.start, assignment.layers.end),
    ]);

    // Capture output for logging.
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    info!(
        id = %process_id,
        address = %addr,
        layers = %format!("{}-{}", assignment.layers.start, assignment.layers.end),
        gpu = assignment.gpu_index,
        "spawning rpc-server"
    );

    let child = cmd.spawn().map_err(|e| {
        Error::Orchestrator(format!(
            "failed to spawn rpc-server at {}: {}",
            config.rpc_server_path, e
        ))
    })?;

    let pid = child.id();

    Ok(ManagedProcess {
        id: process_id,
        kind: ProcessKind::RpcServer,
        state: ProcessState::Starting,
        listen_address: addr,
        spawned_at: now_secs(),
        pid,
        child: Some(child),
    })
}

/// Spawn a llama-server process as the entry node for a shard plan.
///
/// The llama-server receives inference requests and delegates layer
/// computation to rpc-servers across the mesh.
pub async fn spawn_llama_server(
    plan: &ShardPlan,
    listen_port: u16,
    model_path: &str,
    config: &SpawnConfig,
) -> Result<ManagedProcess, Error> {
    let process_id = ProcessId::generate();
    let listen_addr: SocketAddr = format!("127.0.0.1:{listen_port}").parse().unwrap();

    let mut cmd = Command::new(&config.llama_server_path);
    cmd.args([
        "--host",
        "127.0.0.1",
        "--port",
        &listen_port.to_string(),
        "--model",
        model_path,
    ]);

    // Build RPC server list for distributed inference.
    if plan.assignments.len() > 1 {
        let rpc_servers: Vec<String> = plan
            .assignments
            .iter()
            .filter(|a| a.node_id != plan.entry_node)
            .map(|a| a.rpc_address.to_string())
            .collect();

        if !rpc_servers.is_empty() {
            cmd.args(["--rpc", &rpc_servers.join(",")]);
        }
    }

    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    info!(
        id = %process_id,
        port = listen_port,
        model = %plan.model,
        rpc_nodes = plan.assignments.len() - 1,
        "spawning llama-server"
    );

    let child = cmd.spawn().map_err(|e| {
        Error::Orchestrator(format!(
            "failed to spawn llama-server at {}: {}",
            config.llama_server_path, e
        ))
    })?;

    let pid = child.id();

    Ok(ManagedProcess {
        id: process_id,
        kind: ProcessKind::LlamaServer,
        state: ProcessState::Starting,
        listen_address: listen_addr,
        spawned_at: now_secs(),
        pid,
        child: Some(child),
    })
}

/// Create a ManagedProcess for testing without actually spawning a process.
#[cfg(test)]
pub fn mock_process(kind: ProcessKind, addr: SocketAddr) -> ManagedProcess {
    ManagedProcess {
        id: ProcessId::generate(),
        kind,
        state: ProcessState::Running,
        listen_address: addr,
        spawned_at: now_secs(),
        pid: Some(99999),
        child: None,
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonwealth_core::scheduler::LayerRange;

    #[test]
    fn process_state_serde_roundtrip() {
        for state in [
            ProcessState::Starting,
            ProcessState::Running,
            ProcessState::Unhealthy,
            ProcessState::Failed,
            ProcessState::Stopped,
        ] {
            let json = serde_json::to_string(&state).unwrap();
            let back: ProcessState = serde_json::from_str(&json).unwrap();
            assert_eq!(state, back);
        }
    }

    #[test]
    fn mock_process_is_running() {
        let proc = mock_process(ProcessKind::LlamaServer, "127.0.0.1:8080".parse().unwrap());
        assert_eq!(proc.state, ProcessState::Running);
        assert_eq!(proc.kind, ProcessKind::LlamaServer);
    }

    #[tokio::test]
    async fn spawn_rpc_server_fails_with_missing_binary() {
        let assignment = ShardAssignment {
            node_id: commonwealth_core::NodeId::from_u128(1),
            layers: LayerRange::new(0, 32),
            gpu_index: 0,
            rpc_address: "127.0.0.1:50051".parse().unwrap(),
        };
        let config = SpawnConfig {
            rpc_server_path: "/nonexistent/rpc-server".into(),
            ..Default::default()
        };
        let result = spawn_rpc_server(&assignment, "/tmp/model.gguf", &config).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn spawn_llama_server_fails_with_missing_binary() {
        let plan = ShardPlan {
            model: commonwealth_core::ModelId::from_u128(1),
            entry_node: commonwealth_core::NodeId::from_u128(1),
            assignments: vec![ShardAssignment {
                node_id: commonwealth_core::NodeId::from_u128(1),
                layers: LayerRange::new(0, 64),
                gpu_index: 0,
                rpc_address: "127.0.0.1:50051".parse().unwrap(),
            }],
            estimated_tokens_per_sec: 40.0,
            estimated_ttft_ms: 1000,
        };
        let config = SpawnConfig {
            llama_server_path: "/nonexistent/llama-server".into(),
            ..Default::default()
        };
        let result = spawn_llama_server(&plan, 8080, "/tmp/model.gguf", &config).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn stop_process_without_child_is_ok() {
        let mut proc = mock_process(ProcessKind::RpcServer, "127.0.0.1:50051".parse().unwrap());
        // mock_process has no child — stop should succeed.
        proc.stop().await.unwrap();
        assert_eq!(proc.state, ProcessState::Stopped);
    }
}
