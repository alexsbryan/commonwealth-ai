// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::HashMap;

use tracing::{info, warn};

use crate::inference_plan::{InferencePlan, ShardPlan};
use commonwealth_core::capabilities::ProcessKind;
use commonwealth_core::ids::{ModelId, ProcessId};
use commonwealth_core::Error;

use super::departure::{DepartureState, GracefulDeparture, DEFAULT_COUNTDOWN};
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
    /// This node advanced one step through its graceful departure. Emitted
    /// once per transition so the whole sequence is observable from outside —
    /// a departure that skipped a state would otherwise look identical to one
    /// that ran it, since both end with the processes gone.
    DepartureAdvanced {
        node_id: commonwealth_core::NodeId,
        state: DepartureState,
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
    /// The departure in progress, if this node is leaving.
    ///
    /// `Some` from the moment a departure is announced until its processes
    /// are stopped. Its presence is load-bearing, not decorative:
    /// `apply_shard_plan` refuses while it is set, so the scheduler cannot
    /// place new work on a node that is on its way out — which is the whole
    /// point of announcing before stopping.
    departure: Option<GracefulDeparture>,
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
            departure: None,
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
        // A departing node takes no new work. This is what the Announced →
        // Rebalancing states MEAN: the scheduler is moving plans off this
        // node, and accepting one back mid-countdown would put a shard on a
        // process that is about to be stopped.
        if let Some(dep) = &self.departure {
            return Err(Error::Orchestrator(format!(
                "node is departing ({:?}) — refusing to place {} ",
                dep.state(),
                plan.model
            )));
        }
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

    /// Stop all managed processes IMMEDIATELY.
    ///
    /// This is the abrupt path — every process is terminated as fast as its
    /// own stop window allows, with no announcement and no drain. Correct for
    /// a host shutting down; wrong for a node leaving a mesh that is still
    /// serving, which is what [`Self::depart_gracefully`] is for.
    ///
    /// Its doc comment used to say "(graceful departure)" while doing exactly
    /// this, which is how `GracefulDeparture` came to exist, be unit-tested,
    /// and never be constructed by anything.
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

    /// Leave the mesh without dropping anyone's request: announce, let the
    /// scheduler rebalance, drain what is in flight, then stop.
    ///
    /// Walks [`GracefulDeparture`] through `Announced → Rebalancing →
    /// Draining → Complete`, emitting a [`OrchestratorEvent::DepartureAdvanced`]
    /// at each step, and only then calls [`Self::stop_all`]. The countdown is
    /// the total budget for the whole sequence; `Draining` holds whatever is
    /// left of it, because that is the state during which in-flight requests
    /// on the old plan are still being served.
    ///
    /// From the announcement onward the node takes no new work — see the
    /// `departure` field. That is the difference between a state machine and
    /// a log line: `apply_shard_plan` fails while this runs.
    ///
    /// Returns the state sequence actually observed, so a caller (and a test)
    /// can assert the departure ran rather than inferring it from the
    /// processes being gone — which an abrupt kill also produces.
    pub async fn depart_gracefully(
        &mut self,
        node_id: commonwealth_core::NodeId,
        countdown: std::time::Duration,
    ) -> Result<Vec<DepartureState>, Error> {
        let announced = self.announce_departure(node_id, countdown)?;
        let rest = self.complete_departure().await?;
        Ok(std::iter::once(announced).chain(rest).collect())
    }

    /// Announce a departure WITHOUT draining or stopping.
    ///
    /// Split from [`Self::depart_gracefully`] because the announcement is the
    /// half with an immediate effect — from here the node takes no new shard
    /// plans — while the drain is a wait. A caller that wants to tell the mesh
    /// now and stop on its own schedule uses this pair; one that just wants to
    /// leave calls `depart_gracefully`.
    ///
    /// Returns the state entered ([`DepartureState::Announced`]).
    pub fn announce_departure(
        &mut self,
        node_id: commonwealth_core::NodeId,
        countdown: std::time::Duration,
    ) -> Result<DepartureState, Error> {
        if self.departure.is_some() {
            return Err(Error::Orchestrator(
                "a graceful departure is already in progress".into(),
            ));
        }
        // ONE departure, held on `self`, so the clock the countdown reads and
        // the state peers observe are the same object. Set BEFORE any wait:
        // from here on `apply_shard_plan` refuses, which is what makes
        // "announced" mean something to the rest of the system.
        self.departure = Some(GracefulDeparture::with_countdown(node_id, countdown));
        self.events.push(OrchestratorEvent::DepartureAdvanced {
            node_id,
            state: DepartureState::Announced,
        });
        info!(%node_id, ?countdown, "graceful departure announced");
        Ok(DepartureState::Announced)
    }

    /// Finish an announced departure: rebalance, drain what is left of the
    /// countdown, then stop every process.
    ///
    /// Returns the states entered AFTER the announcement, so the caller can
    /// assert the departure ran rather than inferring it from the processes
    /// being gone — which an abrupt kill also produces.
    pub async fn complete_departure(&mut self) -> Result<Vec<DepartureState>, Error> {
        let node_id = match &self.departure {
            Some(d) => d.node_id,
            None => {
                return Err(Error::Orchestrator(
                    "no departure has been announced".into(),
                ))
            }
        };
        let mut observed = Vec::new();

        loop {
            let (state, remaining) = {
                let dep = self
                    .departure
                    .as_mut()
                    .expect("the departure was just installed");
                (dep.advance(), dep.remaining())
            };
            observed.push(state);
            self.events
                .push(OrchestratorEvent::DepartureAdvanced { node_id, state });
            match state {
                // In-flight requests on the old plan are still being served.
                // This is the only point in the sequence where waiting buys
                // anything, so the whole remaining budget is spent here.
                DepartureState::Draining if !remaining.is_zero() => {
                    tokio::time::sleep(remaining).await;
                }
                DepartureState::Complete => break,
                _ => {}
            }
        }

        let ready = self
            .departure
            .as_ref()
            .is_some_and(|d| d.is_ready_to_stop());
        if !ready {
            self.departure = None;
            return Err(Error::Orchestrator(
                "departure reached its last state without being ready to stop".into(),
            ));
        }

        let result = self.stop_all().await;
        self.departure = None;
        info!(%node_id, "graceful departure complete — processes stopped");
        result.map(|()| observed)
    }

    /// Whether a graceful departure is in progress. While true this node
    /// accepts no new shard plans.
    pub fn is_departing(&self) -> bool {
        self.departure.is_some()
    }

    /// The departure's current state, if one is in progress.
    pub fn departure_state(&self) -> Option<DepartureState> {
        self.departure.as_ref().map(|d| d.state())
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
            // Node is standby-only — this is a DEPARTURE, not a shutdown: the
            // mesh is still serving and the scheduler is moving our plans
            // elsewhere. Announce and drain rather than killing mid-request,
            // which is what this call did until 2026-09-02.
            info!("Node assigned Standby — departing gracefully");
            // `apply_mesh_plan` is best-effort by design (per the
            // docstring above): individual process failures are
            // logged, never propagated. Surface the error at `warn`
            // so the operator can grep `orchestrator: depart` for
            // a stuck process — silently dropping the Result was
            // hiding genuine "process refused to stop" cases from
            // logs.
            if let Err(e) = self.depart_gracefully(my_node_id, DEFAULT_COUNTDOWN).await {
                warn!(
                    error = %e,
                    "orchestrator: graceful departure returned error during standby transition"
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
