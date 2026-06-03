//! Multi-pod coordinator — fan one job out across N ephemeral worker pods.
//!
//! Spec: `sovereign/docs/EPHEMERAL_WORKER_PODS.md` §"Multi-pod jobs".
//!
//! ## Design
//!
//! Single-pod primitives ([`WorkerController::create_and_run`],
//! [`poll_completed`], [`destroy`]) are pod-local. The coordinator
//! is the only piece that grows for multi-pod:
//!
//! 1. **Partition** — round-robin the unit manifest across N pods.
//!    Each pod gets a derived [`JobSpec`] with a unique `job_id`
//!    (so per-pod `WorkerToken`s don't collide) and the same
//!    upload manifest (every pod needs the same models).
//! 2. **Fan-out create** — call `create_and_run_with_blob` N times
//!    in parallel via `tokio::join`. Failures during launch tear
//!    down the successful siblings to avoid orphaned billing.
//! 3. **Fan-in poll** — a single async loop walks every pod's
//!    `/completed` endpoint, advancing per-pod cursors. The
//!    callback receives `(pod_index, CompletedUnit)` as units land
//!    so the caller can write results in any aggregation shape they
//!    like (file, Lance fragment merge, in-memory vec).
//! 4. **Fan-out destroy** — explicit `destroy_all()` sends DELETE +
//!    provider-destroy to every pod. Errors are collected but
//!    don't short-circuit — a stuck pod is acceptable (Vast will
//!    bill it past budget, the user gets the report at the end).
//!
//! ## What is intentionally NOT here
//!
//! - **Reshuffling on failure** — if pod K's queue stalls, we
//!   don't pull its remaining units back to pods 0..K-1, K+1..N.
//!   That's a re-balancer feature; MVP just surfaces the stall via
//!   the poll loop's timeout and lets the operator decide.
//! - **Shared trust fate** — each pod has its own seed-derived
//!   cert and its own owner-signed token. One pod compromised
//!   doesn't compromise siblings.
//! - **Cost ceiling enforcement** — `cost_per_hour × N × elapsed`
//!   is straightforward but separable; the coordinator exposes
//!   the per-pod cost so a future cost-aware destroyer can plug in.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use reqwest::Client;
use tokio::sync::Mutex;

use crate::worker_controller::{
    ControllerError, ControllerResult, JobSpec, ProviderInstance, WorkerController, WorkerProvider,
};
use crate::worker_http::{CompletedUnit, WorkUnit};
use crate::worker_pod::{BootstrapBlob, WorkerHandle};

/// One pod inside a [`PoolHandle`]. Carries everything needed to keep
/// polling (the pinned client) and tear down (the provider instance
/// id). Kept in an `Arc` so per-pod tasks can clone the handle without
/// shuffling the whole pool around.
pub struct PoolPod {
    pub handle: WorkerHandle,
    pub instance: ProviderInstance,
    /// Blob retained so callers can re-derive client / cert later if
    /// needed (e.g. resume after restart). The seed inside is
    /// privileged — don't log it.
    pub blob: BootstrapBlob,
    pub client: Client,
    /// How many units this pod was given. The poll loop uses this to
    /// detect "we've drained this pod" without an extra HTTP round
    /// trip.
    pub assigned_units: usize,
    /// Running tally of completed units returned by `/completed`.
    /// Mutated by the poll loop under a lock.
    pub received_units: Mutex<usize>,
}

impl PoolPod {
    pub fn pod_index(&self) -> Option<u32> {
        None
    }
    pub fn cost_per_hour(&self) -> f64 {
        self.instance.cost_per_hour
    }
    /// Snapshot of how many units have landed from this pod so far.
    /// Cheap (just a lock + copy); fine to call inside operator
    /// progress prints.
    pub async fn received(&self) -> usize {
        *self.received_units.lock().await
    }
}

/// Tunable knobs for the coordinator. All have reasonable defaults.
#[derive(Debug, Clone)]
pub struct CoordinatorConfig {
    /// Time between full sweeps over all pods' `/completed` endpoints.
    /// Lower = lower latency-to-first-result, higher = less HTTP load.
    /// Default 1 s — matches the polling cadence Vast clients use for
    /// instance state, so the rate-limiter doesn't notice.
    pub poll_interval: Duration,
    /// If no progress is observed across ANY pod for this long, abort
    /// the poll loop with a stall error. Default 10 min — covers
    /// cold-loading a 36B GGUF plus the longest atom-enrichment
    /// prompts we run today.
    pub stall_timeout: Duration,
    /// Max wall-clock the entire poll loop is allowed to run. Default
    /// 6 h — well past any single batch we'd realistically dispatch.
    pub total_timeout: Duration,
}

impl Default for CoordinatorConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(1),
            stall_timeout: Duration::from_secs(10 * 60),
            total_timeout: Duration::from_secs(6 * 60 * 60),
        }
    }
}

/// Live pool of pods. Returned from [`MultiPodCoordinator::launch`].
/// Caller drives the lifecycle via [`Self::poll_until_complete`] +
/// [`Self::destroy_all`].
pub struct PoolHandle {
    pub pods: Vec<Arc<PoolPod>>,
    pub expected_total_units: usize,
    pub config: CoordinatorConfig,
    /// The job id passed in by the caller. Per-pod job ids are
    /// `<base>-p<index>` — see [`derive_pod_spec`].
    pub base_job_id: String,
}

impl PoolHandle {
    /// Poll all pods until every assigned unit has been observed, the
    /// stall timeout elapses, or the total timeout elapses. The
    /// callback is invoked once per completed unit, in roughly the
    /// order they finish (no strict ordering — different pods finish
    /// at different rates).
    pub async fn poll_until_complete<F>(
        &self,
        controller: &WorkerController,
        mut on_unit: F,
    ) -> ControllerResult<PollSummary>
    where
        F: FnMut(usize, CompletedUnit),
    {
        let started = Instant::now();
        let mut last_progress = started;
        let mut total_received = 0usize;
        let mut total_errors = 0usize;

        loop {
            let mut sweep_received = 0usize;
            for (pod_idx, pod) in self.pods.iter().enumerate() {
                let prior_received = *pod.received_units.lock().await;
                if prior_received >= pod.assigned_units {
                    continue;
                }
                match controller.poll_completed(&pod.handle, &pod.client).await {
                    Ok(batch) => {
                        for unit in batch.units {
                            on_unit(pod_idx, unit);
                            sweep_received += 1;
                            total_received += 1;
                            *pod.received_units.lock().await += 1;
                        }
                    }
                    Err(e) => {
                        total_errors += 1;
                        tracing::warn!(
                            pod_idx,
                            error = %e,
                            "multi-pod: poll error (will retry next sweep)"
                        );
                    }
                }
            }
            if sweep_received > 0 {
                last_progress = Instant::now();
            }
            if total_received >= self.expected_total_units {
                return Ok(PollSummary {
                    elapsed: started.elapsed(),
                    total_received,
                    total_errors,
                    timed_out: false,
                });
            }
            if last_progress.elapsed() >= self.config.stall_timeout {
                return Err(ControllerError::Timeout {
                    what: "multi-pod poll stalled — no units received across any pod",
                    elapsed_secs: last_progress.elapsed().as_secs(),
                });
            }
            if started.elapsed() >= self.config.total_timeout {
                return Ok(PollSummary {
                    elapsed: started.elapsed(),
                    total_received,
                    total_errors,
                    timed_out: true,
                });
            }
            tokio::time::sleep(self.config.poll_interval).await;
        }
    }

    /// Fan-out destroy. Sends DELETE to each pod's worker daemon and
    /// asks the provider to destroy each instance. Errors are
    /// collected, not raised — a stuck pod doesn't block destruction
    /// of its siblings.
    pub async fn destroy_all(
        &self,
        controller: &WorkerController,
    ) -> Vec<(usize, Result<(), ControllerError>)> {
        let mut results = Vec::with_capacity(self.pods.len());
        for (i, pod) in self.pods.iter().enumerate() {
            let r = controller
                .destroy(&pod.handle, &pod.client, &pod.instance.instance_id)
                .await;
            results.push((i, r));
        }
        results
    }

    /// Per-pod summary suitable for operator logs / dashboards. Cheap
    /// — does not hit the network.
    pub async fn snapshot(&self) -> Vec<PodSnapshot> {
        let mut out = Vec::with_capacity(self.pods.len());
        for (i, pod) in self.pods.iter().enumerate() {
            out.push(PodSnapshot {
                pod_index: i,
                instance_id: pod.instance.instance_id.clone(),
                gpu_name: pod.instance.gpu_name.clone(),
                cost_per_hour: pod.instance.cost_per_hour,
                worker_address: pod.handle.base_url(),
                pod_job_id: pod.handle.job_id().to_string(),
                assigned_units: pod.assigned_units,
                received_units: *pod.received_units.lock().await,
            });
        }
        out
    }
}

/// Operator-facing per-pod summary. Stable, serializable — embed in
/// pool-status command output, ledger rows, web UI, etc.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PodSnapshot {
    pub pod_index: usize,
    pub instance_id: String,
    pub gpu_name: String,
    pub cost_per_hour: f64,
    pub worker_address: String,
    pub pod_job_id: String,
    pub assigned_units: usize,
    pub received_units: usize,
}

/// Result of a poll loop, surfaced once `poll_until_complete` returns.
#[derive(Debug, Clone)]
pub struct PollSummary {
    pub elapsed: Duration,
    pub total_received: usize,
    pub total_errors: usize,
    /// True if the total_timeout elapsed before every unit was
    /// received. Caller should still inspect the pool snapshot to
    /// learn which partitions stalled.
    pub timed_out: bool,
}

/// Coordinator entry point. Holds an `Arc<WorkerController>` so the
/// same provider/owner credentials drive every pod in the pool.
pub struct MultiPodCoordinator {
    controller: Arc<WorkerController>,
    config: CoordinatorConfig,
}

impl MultiPodCoordinator {
    pub fn new(
        provider: Arc<dyn WorkerProvider>,
        owner_signing: SigningKey,
        controller_config: crate::worker_controller::ControllerConfig,
        coordinator_config: CoordinatorConfig,
    ) -> Self {
        let controller = Arc::new(WorkerController::new(
            provider,
            owner_signing,
            controller_config,
        ));
        Self {
            controller,
            config: coordinator_config,
        }
    }

    /// Build a coordinator that shares an existing controller. Useful
    /// when the caller already constructed one (e.g. the CLI built a
    /// controller for the single-pod path and wants to reuse the
    /// provider plumbing).
    pub fn with_controller(controller: Arc<WorkerController>, config: CoordinatorConfig) -> Self {
        Self { controller, config }
    }

    pub fn controller(&self) -> &WorkerController {
        &self.controller
    }

    /// Spin up `pod_count` pods in parallel, partitioning `spec.units`
    /// across them. Returns once every pod is in "dispatching" state
    /// (uploads complete, manifest accepted, runner running). The
    /// caller then drives [`PoolHandle::poll_until_complete`].
    ///
    /// If `pod_count == 0` returns an error — empty pools don't make
    /// sense and would silently complete with `total_received == 0`.
    pub async fn launch(&self, spec: JobSpec, pod_count: usize) -> ControllerResult<PoolHandle> {
        if pod_count == 0 {
            return Err(ControllerError::InvalidArgument(
                "pod_count must be ≥ 1".into(),
            ));
        }
        let base_job_id = spec.job_id.clone();
        let total_units = spec.units.len();
        let partitions = partition_units(spec.units.clone(), pod_count);

        // Fan out create_and_run_with_blob. Each derived spec gets a
        // unique job_id and its slice of units; everything else stays
        // identical.
        let mut launches = Vec::with_capacity(pod_count);
        for (i, part) in partitions.into_iter().enumerate() {
            let derived = derive_pod_spec(&spec, i, part);
            let ctrl = self.controller.clone();
            launches.push(tokio::spawn(async move {
                ctrl.create_and_run_with_blob(&derived).await.map(|t| {
                    let (handle, instance, blob, client) = t;
                    PoolPod {
                        handle,
                        instance,
                        blob,
                        client,
                        assigned_units: derived.units.len(),
                        received_units: Mutex::new(0),
                    }
                })
            }));
        }

        // Collect results. If any pod failed, destroy the survivors.
        let mut successes: Vec<Arc<PoolPod>> = Vec::with_capacity(pod_count);
        let mut errors: Vec<(usize, ControllerError)> = Vec::new();
        for (i, jh) in launches.into_iter().enumerate() {
            match jh.await {
                Ok(Ok(p)) => successes.push(Arc::new(p)),
                Ok(Err(e)) => errors.push((i, e)),
                Err(join_err) => errors.push((
                    i,
                    ControllerError::InvalidArgument(format!("join error: {join_err}")),
                )),
            }
        }
        if !errors.is_empty() {
            tracing::error!(
                failed = errors.len(),
                ok = successes.len(),
                "multi-pod launch had failures — tearing down successful siblings"
            );
            for pod in &successes {
                let _ = self
                    .controller
                    .destroy(&pod.handle, &pod.client, &pod.instance.instance_id)
                    .await;
            }
            let summary = errors
                .iter()
                .map(|(i, e)| format!("pod {i}: {e}"))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(ControllerError::InvalidArgument(format!(
                "multi-pod launch failed: {summary}"
            )));
        }

        Ok(PoolHandle {
            pods: successes,
            expected_total_units: total_units,
            config: self.config.clone(),
            base_job_id,
        })
    }
}

/// Round-robin partition. Yields `n` vectors whose lengths differ by
/// at most one — the natural balance when `units.len()` isn't an
/// integer multiple of `n`.
///
/// Round-robin (vs sequential chunks) keeps adjacent unit_ids on
/// different pods. For the atom-enrichment workload that's a feature:
/// related-topic units (which often hit overlapping atlas neighbours
/// and therefore similar cache lines) get spread out instead of
/// concentrated.
pub fn partition_units(units: Vec<WorkUnit>, n: usize) -> Vec<Vec<WorkUnit>> {
    let mut parts: Vec<Vec<WorkUnit>> = (0..n).map(|_| Vec::new()).collect();
    if n == 0 {
        return parts;
    }
    for (i, u) in units.into_iter().enumerate() {
        parts[i % n].push(u);
    }
    parts
}

/// Derive a per-pod JobSpec: unique job_id, the partition's units,
/// everything else cloned from the base spec.
///
/// `job_id = "<base>-p<index>"` is short and stable — the dispatcher
/// uses it for log breadcrumbs, the operator can grep ledger rows by
/// `<base>-p` to find pool members.
pub fn derive_pod_spec(base: &JobSpec, pod_index: usize, units: Vec<WorkUnit>) -> JobSpec {
    JobSpec {
        job_id: format!("{}-p{}", base.job_id, pod_index),
        image: base.image.clone(),
        disk_gb: base.disk_gb,
        gpu_name: base.gpu_name.clone(),
        max_price_per_hour: base.max_price_per_hour,
        label: format!("{}-p{}", base.label, pod_index),
        uploads: base.uploads.clone(),
        units,
        runner_config: base.runner_config.clone(),
    }
}

// ───── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker_controller::{
        ControllerConfig, JobSpec, ProviderResult, PublicAddress, UploadFile, WorkerProvider,
    };
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn make_units(n: u64) -> Vec<WorkUnit> {
        (0..n)
            .map(|i| WorkUnit {
                unit_id: i,
                kind: "test".to_string(),
                payload: serde_json::json!({"i": i}),
            })
            .collect()
    }

    #[test]
    fn partition_round_robin_balances() {
        let units = make_units(10);
        let parts = partition_units(units, 4);
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0].len(), 3); // 0, 4, 8
        assert_eq!(parts[1].len(), 3); // 1, 5, 9
        assert_eq!(parts[2].len(), 2); // 2, 6
        assert_eq!(parts[3].len(), 2); // 3, 7
                                       // Original unit_ids preserved
        assert_eq!(parts[0][0].unit_id, 0);
        assert_eq!(parts[0][1].unit_id, 4);
        assert_eq!(parts[3][1].unit_id, 7);
    }

    #[test]
    fn partition_handles_fewer_units_than_pods() {
        let parts = partition_units(make_units(2), 5);
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[0].len(), 1);
        assert_eq!(parts[1].len(), 1);
        assert_eq!(parts[2].len(), 0);
        assert_eq!(parts[3].len(), 0);
        assert_eq!(parts[4].len(), 0);
    }

    #[test]
    fn derive_pod_spec_uses_unique_job_id() {
        let base = JobSpec {
            job_id: "atom-enrich".to_string(),
            image: "x".to_string(),
            disk_gb: 80,
            gpu_name: "L40S".to_string(),
            max_price_per_hour: 1.0,
            label: "lbl".to_string(),
            uploads: BTreeMap::new(),
            units: vec![],
            runner_config: serde_json::json!({}),
        };
        let d0 = derive_pod_spec(&base, 0, make_units(2));
        let d3 = derive_pod_spec(&base, 3, make_units(1));
        assert_eq!(d0.job_id, "atom-enrich-p0");
        assert_eq!(d3.job_id, "atom-enrich-p3");
        assert_eq!(d0.label, "lbl-p0");
        // Uploads should be byte-identical — every pod needs the same
        // models.
        assert_eq!(d0.uploads.len(), d3.uploads.len());
        assert_eq!(d0.image, base.image);
        assert_eq!(d0.units.len(), 2);
        assert_eq!(d3.units.len(), 1);
    }

    /// Provider mock that never actually binds a TLS listener. It
    /// pretends pods come up, hands back deterministic addresses, and
    /// `destroy` is a no-op. Used to test the partition / fan-out
    /// logic without needing a real worker daemon.
    struct StubProvider {
        create_count: AtomicUsize,
    }
    impl StubProvider {
        fn new() -> Self {
            Self {
                create_count: AtomicUsize::new(0),
            }
        }
    }
    impl WorkerProvider for StubProvider {
        fn create(&self, _bootstrap_b64: &str, spec: &JobSpec) -> ProviderResult<ProviderInstance> {
            let idx = self.create_count.fetch_add(1, Ordering::SeqCst);
            Ok(ProviderInstance {
                instance_id: format!("inst-{idx}-{}", spec.job_id),
                gpu_name: "Mock-L40S".to_string(),
                cost_per_hour: 0.50,
            })
        }
        fn address(&self, instance_id: &str) -> ProviderResult<Option<PublicAddress>> {
            // Return a port we'll never actually connect to (the test
            // doesn't drive the full lifecycle past `provider.create`).
            Ok(Some(PublicAddress {
                host: format!("127.0.0.1-{instance_id}"),
                port: 1,
            }))
        }
        fn destroy(&self, _instance_id: &str) -> ProviderResult<()> {
            Ok(())
        }
    }

    /// Partition + derive_pod_spec are deterministic; this test
    /// exercises that the *coordinator* preserves the partition shape
    /// in the spec it hands to the provider. Doesn't run the full
    /// launch (which would need real TLS) — checks the spec-derivation
    /// boundary instead.
    #[test]
    fn coordinator_partitions_and_derives_specs_per_pod() {
        let base = JobSpec {
            job_id: "test-job".to_string(),
            image: "x".to_string(),
            disk_gb: 0,
            gpu_name: "Mock".to_string(),
            max_price_per_hour: 0.0,
            label: "lbl".to_string(),
            uploads: BTreeMap::new(),
            units: make_units(7),
            runner_config: serde_json::json!({}),
        };
        let n = 3;
        let parts = partition_units(base.units.clone(), n);
        let total: usize = parts.iter().map(|p| p.len()).sum();
        assert_eq!(total, 7);
        let derived: Vec<JobSpec> = parts
            .into_iter()
            .enumerate()
            .map(|(i, p)| derive_pod_spec(&base, i, p))
            .collect();
        assert_eq!(derived.len(), 3);
        // Every job_id should be unique across the pool.
        let mut ids: Vec<String> = derived.iter().map(|d| d.job_id.clone()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 3);
        // Total units across the derived specs must equal the input.
        let derived_total: usize = derived.iter().map(|d| d.units.len()).sum();
        assert_eq!(derived_total, 7);
    }

    #[test]
    fn upload_files_are_shared_across_partitions() {
        let mut uploads = BTreeMap::new();
        uploads.insert(
            "primary.gguf".to_string(),
            UploadFile::local(std::path::PathBuf::from("/tmp/x"), [9u8; 32]),
        );
        let base = JobSpec {
            job_id: "j".to_string(),
            image: "x".to_string(),
            disk_gb: 0,
            gpu_name: "M".to_string(),
            max_price_per_hour: 0.0,
            label: "l".to_string(),
            uploads,
            units: make_units(4),
            runner_config: serde_json::json!({}),
        };
        let parts = partition_units(base.units.clone(), 2);
        let d0 = derive_pod_spec(&base, 0, parts[0].clone());
        let d1 = derive_pod_spec(&base, 1, parts[1].clone());
        // Both should reference the same upload entry. (Identity not
        // required — value equality is what matters for the wire.)
        assert_eq!(d0.uploads.len(), 1);
        assert_eq!(d1.uploads.len(), 1);
        assert_eq!(
            d0.uploads.keys().next().unwrap(),
            d1.uploads.keys().next().unwrap()
        );
    }

    #[tokio::test]
    async fn launch_rejects_zero_pod_count() {
        let provider = Arc::new(StubProvider::new());
        let owner = SigningKey::from_bytes(&[1u8; 32]);
        let coord = MultiPodCoordinator::new(
            provider,
            owner,
            ControllerConfig::default(),
            CoordinatorConfig::default(),
        );
        let spec = JobSpec {
            job_id: "j".to_string(),
            image: "x".to_string(),
            disk_gb: 0,
            gpu_name: "M".to_string(),
            max_price_per_hour: 0.0,
            label: "l".to_string(),
            uploads: BTreeMap::new(),
            units: make_units(3),
            runner_config: serde_json::json!({}),
        };
        let err = match coord.launch(spec, 0).await {
            Ok(_) => panic!("zero pod_count should have errored"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(msg.contains("pod_count"), "got: {msg}");
    }
}
