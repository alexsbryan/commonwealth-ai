use std::collections::HashMap;

use tracing::{info, warn};

use crate::inference_plan::{InferencePlan, ShardPlan};
use commonwealth_core::capabilities::ProcessKind;
use commonwealth_core::ids::{ModelId, ProcessId};
use commonwealth_core::Error;

use super::health::{HealthCheckConfig, HealthStatus, HealthTracker};
use super::process::{ManagedProcess, ProcessState, SpawnConfig};

/// Events emitted by the orchestrator when process state changes.
#[derive(Debug, Clone)]
pub enum OrchestratorEvent {
    ProcessStarted {
        process_id: ProcessId,
        kind: ProcessKind,
        model: ModelId,
    },
    ProcessFailed {
        process_id: ProcessId,
        kind: ProcessKind,
        reason: String,
    },
    ProcessStopped {
        process_id: ProcessId,
        kind: ProcessKind,
    },
    HealthChanged {
        process_id: ProcessId,
        old_status: HealthStatus,
        new_status: HealthStatus,
    },
}

/// Manages the lifecycle of inference processes on this node.
///
/// The orchestrator translates scheduling decisions (shard plans) into
/// running `llama-server` and `rpc-server` processes, monitors their health,
/// and reports failures.
pub struct Orchestrator {
    processes: HashMap<ProcessId, ManagedProcess>,
    health_trackers: HashMap<ProcessId, HealthTracker>,
    /// Maps model_id → list of ProcessIds serving that model.
    model_processes: HashMap<ModelId, Vec<ProcessId>>,
    /// Next port to assign for llama-server instances.
    next_llama_port: u16,
    spawn_config: SpawnConfig,
    health_config: HealthCheckConfig,
    events: Vec<OrchestratorEvent>,
}

impl Orchestrator {
    pub fn new(spawn_config: SpawnConfig, health_config: HealthCheckConfig) -> Self {
        Self {
            processes: HashMap::new(),
            health_trackers: HashMap::new(),
            model_processes: HashMap::new(),
            next_llama_port: 8081, // llama-server instances start here.
            spawn_config,
            health_config,
            events: Vec::new(),
        }
    }

    /// Apply an inference plan for a single model. Spawns necessary processes.
    ///
    /// This is called on the entry node. The entry node spawns:
    /// - One `llama-server` for the model
    /// - (Remote nodes spawn their own `rpc-server` processes via their local orchestrators)
    pub async fn apply_shard_plan(
        &mut self,
        plan: &ShardPlan,
        model_path: &str,
    ) -> Result<(), Error> {
        // Check if we already have processes for this model.
        if self.model_processes.contains_key(&plan.model) {
            // Model already loaded — skip.
            info!(model = %plan.model, "model already has active processes, skipping");
            return Ok(());
        }

        let port = self.next_llama_port;
        self.next_llama_port += 1;

        let mut config = self.spawn_config.clone();
        config.model_path = model_path.to_string();

        match super::process::spawn_llama_server(plan, port, model_path, &config).await {
            Ok(proc) => {
                let proc_id = proc.id;
                let addr = proc.listen_address;

                self.events.push(OrchestratorEvent::ProcessStarted {
                    process_id: proc_id,
                    kind: ProcessKind::LlamaServer,
                    model: plan.model,
                });

                let tracker =
                    HealthTracker::new(addr, ProcessKind::LlamaServer, self.health_config.clone());
                self.health_trackers.insert(proc_id, tracker);
                self.processes.insert(proc_id, proc);

                self.model_processes
                    .entry(plan.model)
                    .or_default()
                    .push(proc_id);

                info!(
                    model = %plan.model,
                    process_id = %proc_id,
                    port = port,
                    "llama-server spawned for model"
                );
            }
            Err(e) => {
                warn!(model = %plan.model, error = %e, "failed to spawn llama-server");
                return Err(e);
            }
        }

        Ok(())
    }

    /// Apply a full multi-model inference plan.
    pub async fn apply_inference_plan(
        &mut self,
        plan: &InferencePlan,
        model_paths: &HashMap<ModelId, String>,
    ) -> Result<(), Error> {
        for shard_plan in &plan.model_plans {
            let model_path = model_paths.get(&shard_plan.model).ok_or_else(|| {
                Error::Orchestrator(format!("no model path for {}", shard_plan.model))
            })?;
            self.apply_shard_plan(shard_plan, model_path).await?;
        }
        Ok(())
    }

    /// Stop all processes for a specific model.
    pub async fn stop_model(&mut self, model_id: ModelId) -> Result<(), Error> {
        let proc_ids = self.model_processes.remove(&model_id).unwrap_or_default();

        for proc_id in proc_ids {
            if let Some(mut proc) = self.processes.remove(&proc_id) {
                proc.stop().await?;
                self.health_trackers.remove(&proc_id);
                self.events.push(OrchestratorEvent::ProcessStopped {
                    process_id: proc_id,
                    kind: proc.kind,
                });
            }
        }

        Ok(())
    }

    /// Stop all managed processes (graceful departure).
    pub async fn stop_all(&mut self) -> Result<(), Error> {
        let proc_ids: Vec<ProcessId> = self.processes.keys().copied().collect();

        for proc_id in proc_ids {
            if let Some(mut proc) = self.processes.remove(&proc_id) {
                proc.stop().await?;
                self.events.push(OrchestratorEvent::ProcessStopped {
                    process_id: proc_id,
                    kind: proc.kind,
                });
            }
        }

        self.health_trackers.clear();
        self.model_processes.clear();

        info!("all processes stopped");
        Ok(())
    }

    /// Run a single health check cycle across all managed processes.
    /// Returns events for any state changes.
    pub async fn check_health(&mut self) -> Vec<OrchestratorEvent> {
        let mut events = Vec::new();

        let proc_ids: Vec<ProcessId> = self.processes.keys().copied().collect();

        for proc_id in proc_ids {
            // First check if process is still alive.
            let is_alive = self
                .processes
                .get_mut(&proc_id)
                .map(|p| p.is_alive())
                .unwrap_or(false);

            if !is_alive {
                if let Some(proc) = self.processes.get(&proc_id) {
                    if proc.state == ProcessState::Failed {
                        let event = OrchestratorEvent::ProcessFailed {
                            process_id: proc_id,
                            kind: proc.kind,
                            reason: "process exited unexpectedly".into(),
                        };
                        events.push(event);
                    }
                }
                continue;
            }

            // Run health check probe.
            if let Some(tracker) = self.health_trackers.get_mut(&proc_id) {
                let old_status = tracker.current_status.clone();
                let result = tracker.check().await;

                if old_status != result.status {
                    events.push(OrchestratorEvent::HealthChanged {
                        process_id: proc_id,
                        old_status,
                        new_status: result.status.clone(),
                    });

                    // Update process state based on health.
                    if let Some(proc) = self.processes.get_mut(&proc_id) {
                        proc.state = match &result.status {
                            HealthStatus::Healthy => ProcessState::Running,
                            HealthStatus::Degraded { .. } => ProcessState::Unhealthy,
                            HealthStatus::Unresponsive => ProcessState::Failed,
                            HealthStatus::Dead => ProcessState::Failed,
                            HealthStatus::Unknown => proc.state,
                        };
                    }
                }
            }
        }

        self.events.extend(events.clone());
        events
    }

    /// Drain and return all pending events.
    pub fn drain_events(&mut self) -> Vec<OrchestratorEvent> {
        std::mem::take(&mut self.events)
    }

    /// Get the number of managed processes.
    pub fn process_count(&self) -> usize {
        self.processes.len()
    }

    /// Get the number of models currently loaded.
    pub fn loaded_model_count(&self) -> usize {
        self.model_processes.len()
    }

    /// Get process IDs for a specific model.
    pub fn processes_for_model(&self, model_id: ModelId) -> &[ProcessId] {
        self.model_processes
            .get(&model_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Get a reference to a managed process by ID.
    pub fn get_process(&self, id: ProcessId) -> Option<&ManagedProcess> {
        self.processes.get(&id)
    }

    /// Get the llama-server listen address for a model (if loaded).
    pub fn llama_server_address(&self, model_id: ModelId) -> Option<std::net::SocketAddr> {
        self.model_processes.get(&model_id).and_then(|proc_ids| {
            proc_ids.iter().find_map(|id| {
                self.processes.get(id).and_then(|p| {
                    if p.kind == ProcessKind::LlamaServer {
                        Some(p.listen_address)
                    } else {
                        None
                    }
                })
            })
        })
    }

    /// Apply a MeshPlan — stop processes for removed roles, start
    /// processes for new roles. Called by the orchestrator when it
    /// detects a new MeshPlan in MeshStore.
    ///
    /// This is a best-effort operation: individual process failures
    /// are logged but don't abort the overall plan application.
    pub async fn apply_mesh_plan(
        &mut self,
        plan: &crate::plan::MeshPlan,
        my_node_id: commonwealth_core::NodeId,
    ) {
        let my_roles = plan
            .node_roles
            .get(&my_node_id)
            .cloned()
            .unwrap_or_default();

        info!(
            plan_version = plan.version,
            my_roles = my_roles.len(),
            "Applying mesh plan"
        );

        // Check if any current processes should be stopped.
        // In the current architecture, model processes are tracked by ModelId.
        // The MeshPlan roles reference models by string name. For now, we log
        // the transition rather than forcefully stopping processes — the
        // existing apply_shard_plan handles spawning.
        let has_standby = my_roles
            .iter()
            .any(|r| matches!(r, crate::plan::NodeRole::Standby));
        if has_standby
            && !my_roles.iter().any(|r| {
                matches!(
                    r,
                    crate::plan::NodeRole::ThroughputInference { .. }
                        | crate::plan::NodeRole::QualityInference { .. }
                )
            })
        {
            // Node is standby-only — stop all inference processes.
            info!("Node assigned Standby — stopping all inference processes");
            // `apply_mesh_plan` is best-effort by design (per the
            // docstring above): individual process failures are
            // logged, never propagated. Surface the error at `warn`
            // so the operator can grep `orchestrator: stop_all` for
            // a stuck process — silently dropping the Result was
            // hiding genuine "process refused to stop" cases from
            // logs.
            if let Err(e) = self.stop_all().await {
                warn!(
                    error = %e,
                    "orchestrator: stop_all returned error during standby transition"
                );
            }
        }

        info!(
            plan_version = plan.version,
            roles = ?my_roles.iter().map(|r| format!("{r:?}")).collect::<Vec<_>>(),
            "Mesh plan applied"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::process::mock_process;
    use commonwealth_core::capabilities::ProcessKind;

    fn test_spawn_config() -> SpawnConfig {
        SpawnConfig {
            // Use nonexistent paths — we won't actually spawn in most tests.
            llama_server_path: "/nonexistent/llama-server".into(),
            rpc_server_path: "/nonexistent/rpc-server".into(),
            ..Default::default()
        }
    }

    #[test]
    fn orchestrator_new() {
        let orch = Orchestrator::new(test_spawn_config(), HealthCheckConfig::default());
        assert_eq!(orch.process_count(), 0);
        assert_eq!(orch.loaded_model_count(), 0);
    }

    #[tokio::test]
    async fn stop_all_empty_is_ok() {
        let mut orch = Orchestrator::new(test_spawn_config(), HealthCheckConfig::default());
        orch.stop_all().await.unwrap();
        assert_eq!(orch.process_count(), 0);
    }

    #[tokio::test]
    async fn stop_model_that_doesnt_exist_is_ok() {
        let mut orch = Orchestrator::new(test_spawn_config(), HealthCheckConfig::default());
        orch.stop_model(ModelId::from_u128(999)).await.unwrap();
    }

    #[test]
    fn orchestrator_with_mock_processes() {
        let mut orch = Orchestrator::new(test_spawn_config(), HealthCheckConfig::default());

        let model_id = ModelId::from_u128(1);
        let proc = mock_process(ProcessKind::LlamaServer, "127.0.0.1:8081".parse().unwrap());
        let proc_id = proc.id;

        orch.processes.insert(proc_id, proc);
        orch.model_processes
            .entry(model_id)
            .or_default()
            .push(proc_id);

        assert_eq!(orch.process_count(), 1);
        assert_eq!(orch.loaded_model_count(), 1);
        assert_eq!(orch.processes_for_model(model_id).len(), 1);

        let addr = orch.llama_server_address(model_id);
        assert_eq!(addr, Some("127.0.0.1:8081".parse().unwrap()));
    }

    #[tokio::test]
    async fn stop_model_with_mock_processes() {
        let mut orch = Orchestrator::new(test_spawn_config(), HealthCheckConfig::default());

        let model_id = ModelId::from_u128(1);
        let proc = mock_process(ProcessKind::LlamaServer, "127.0.0.1:8081".parse().unwrap());
        let proc_id = proc.id;

        orch.processes.insert(proc_id, proc);
        orch.model_processes
            .entry(model_id)
            .or_default()
            .push(proc_id);

        orch.stop_model(model_id).await.unwrap();

        assert_eq!(orch.process_count(), 0);
        assert_eq!(orch.loaded_model_count(), 0);
        assert!(orch.llama_server_address(model_id).is_none());

        let events = orch.drain_events();
        assert!(events
            .iter()
            .any(|e| matches!(e, OrchestratorEvent::ProcessStopped { .. })));
    }

    #[tokio::test]
    async fn health_check_cycle_with_no_processes() {
        let mut orch = Orchestrator::new(test_spawn_config(), HealthCheckConfig::default());
        let events = orch.check_health().await;
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn apply_shard_plan_fails_with_missing_binary() {
        let mut orch = Orchestrator::new(test_spawn_config(), HealthCheckConfig::default());

        let plan = ShardPlan {
            model: ModelId::from_u128(1),
            entry_node: commonwealth_core::NodeId::from_u128(1),
            assignments: vec![crate::inference_plan::ShardAssignment {
                node_id: commonwealth_core::NodeId::from_u128(1),
                layers: crate::inference_plan::LayerRange::new(0, 64),
                gpu_index: 0,
                rpc_address: "127.0.0.1:50051".parse().unwrap(),
            }],
            estimated_tokens_per_sec: 40.0,
            estimated_ttft_ms: 1000,
        };

        let result = orch.apply_shard_plan(&plan, "/tmp/model.gguf").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn drain_events_clears_buffer() {
        let mut orch = Orchestrator::new(test_spawn_config(), HealthCheckConfig::default());

        // Manually push an event.
        orch.events.push(OrchestratorEvent::ProcessStopped {
            process_id: ProcessId::generate(),
            kind: ProcessKind::LlamaServer,
        });

        let events = orch.drain_events();
        assert_eq!(events.len(), 1);

        let events = orch.drain_events();
        assert!(events.is_empty());
    }
}
