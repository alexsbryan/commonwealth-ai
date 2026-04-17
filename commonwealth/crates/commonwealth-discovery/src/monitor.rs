use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::RwLock;
use tracing::{debug, info};

use commonwealth_core::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};

use crate::hardware;
use crate::threshold::SignificanceThresholds;

/// Configuration for the resource monitor.
#[derive(Debug, Clone)]
pub struct MonitorConfig {
    /// How often to poll GPU state. Default: 5 seconds.
    pub gpu_poll_interval: Duration,
    /// How often to poll storage state. Default: 30 seconds.
    pub storage_poll_interval: Duration,
    /// Significance thresholds for gossip propagation.
    pub thresholds: SignificanceThresholds,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            gpu_poll_interval: Duration::from_secs(5),
            storage_poll_interval: Duration::from_secs(30),
            thresholds: SignificanceThresholds::default(),
        }
    }
}

/// Snapshot of the current node capabilities, updated by the monitor.
#[derive(Debug, Clone)]
pub struct CapabilitySnapshot {
    pub capabilities: NodeCapabilities,
    /// Incremented each time a significant change is detected.
    pub version: u64,
}

/// The resource monitor periodically reads hardware state and tracks
/// whether changes are significant enough to gossip.
pub struct ResourceMonitor {
    hardware_profile: HardwareProfile,
    config: MonitorConfig,
    snapshot: Arc<RwLock<CapabilitySnapshot>>,
    available_for_mesh: Arc<RwLock<bool>>,
}

impl ResourceMonitor {
    /// Create a new monitor. Performs initial hardware detection.
    pub fn new(config: MonitorConfig) -> Self {
        let hardware_profile = hardware::detect_hardware();

        info!(
            ram_gb = hardware_profile.system_ram_gb,
            cpu_cores = hardware_profile.cpu_cores,
            gpus = hardware_profile.gpus.len(),
            storage_gb = hardware_profile.total_storage_gb,
            "hardware profile detected"
        );

        let initial_available = AvailableResources {
            free_vram_gb: hardware_profile.gpus.iter().map(|g| g.vram_gb as f32).sum(),
            free_ram_gb: hardware_profile.system_ram_gb as f32,
            free_storage_gb: hardware_profile.free_storage_gb as f32,
            gpu_utilization: 0.0,
            cpu_utilization: 0.0,
            available_for_mesh: true,
        };

        let snapshot = CapabilitySnapshot {
            capabilities: NodeCapabilities {
                hardware: hardware_profile.clone(),
                available: initial_available,
                active_processes: vec![],
                hosted_corpora: vec![],
                reported_at: now_secs(),
                inference_availability: 1.0,
            },
            version: 0,
        };

        Self {
            hardware_profile,
            config,
            snapshot: Arc::new(RwLock::new(snapshot)),
            available_for_mesh: Arc::new(RwLock::new(true)),
        }
    }

    /// Get the current capability snapshot.
    pub async fn snapshot(&self) -> CapabilitySnapshot {
        self.snapshot.read().await.clone()
    }

    /// Get a reference to the shared snapshot (for external consumers).
    pub fn snapshot_handle(&self) -> Arc<RwLock<CapabilitySnapshot>> {
        Arc::clone(&self.snapshot)
    }

    /// Set whether this node is available for mesh work.
    pub async fn set_available_for_mesh(&self, available: bool) {
        *self.available_for_mesh.write().await = available;
    }

    /// Get the hardware profile (static, doesn't change).
    pub fn hardware_profile(&self) -> &HardwareProfile {
        &self.hardware_profile
    }

    /// Perform a single poll cycle: read current resources and check significance.
    /// Returns true if a significant change was detected.
    pub async fn poll_once(&self) -> bool {
        let new_available = self.read_current_resources().await;

        let mut snap = self.snapshot.write().await;
        let previous = &snap.capabilities.available;

        let significant = self
            .config
            .thresholds
            .is_significant(previous, &new_available);

        if significant {
            snap.version += 1;
            debug!(
                version = snap.version,
                "significant resource change detected"
            );
        }

        snap.capabilities.available = new_available;
        snap.capabilities.reported_at = now_secs();

        significant
    }

    /// Read the current available resources from the system.
    async fn read_current_resources(&self) -> AvailableResources {
        // GPU state.
        let gpu_states = hardware::read_nvidia_gpu_state();
        let (gpu_util, free_vram) = if gpu_states.is_empty() {
            // No NVIDIA GPUs or nvidia-smi unavailable.
            // For Metal, estimate from system memory.
            (
                0.0,
                self.hardware_profile
                    .gpus
                    .iter()
                    .map(|g| g.vram_gb as f32)
                    .sum(),
            )
        } else {
            let total_util: f32 = gpu_states.iter().map(|(u, _)| u).sum::<f32>();
            let avg_util = total_util / gpu_states.len() as f32;
            let total_free: f32 = gpu_states.iter().map(|(_, f)| f).sum();
            (avg_util, total_free)
        };

        // CPU + RAM.
        let (cpu_util, free_ram) = hardware::read_cpu_ram_state();

        // Disk.
        let free_storage = hardware::read_disk_state();

        let available_for_mesh = *self.available_for_mesh.read().await;

        AvailableResources {
            free_vram_gb: free_vram,
            free_ram_gb: free_ram,
            free_storage_gb: free_storage,
            gpu_utilization: gpu_util,
            cpu_utilization: cpu_util,
            available_for_mesh,
        }
    }

    /// Run the monitor loop. Calls `on_significant_change` each time a
    /// significant change is detected. This runs forever until the task is cancelled.
    pub async fn run<F>(&self, mut on_significant_change: F)
    where
        F: FnMut(NodeCapabilities) + Send,
    {
        let mut gpu_interval = tokio::time::interval(self.config.gpu_poll_interval);
        let mut storage_tick_count: u64 = 0;
        let storage_every_n = (self.config.storage_poll_interval.as_secs()
            / self.config.gpu_poll_interval.as_secs())
        .max(1);

        loop {
            gpu_interval.tick().await;
            storage_tick_count += 1;

            let significant = self.poll_once().await;

            if significant || storage_tick_count.is_multiple_of(storage_every_n) {
                let snap = self.snapshot.read().await;
                if significant {
                    on_significant_change(snap.capabilities.clone());
                }
            }
        }
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

    #[tokio::test]
    async fn monitor_creates_valid_snapshot() {
        let monitor = ResourceMonitor::new(MonitorConfig::default());
        let snap = monitor.snapshot().await;

        assert!(snap.capabilities.hardware.cpu_cores > 0);
        assert!(snap.capabilities.hardware.system_ram_gb > 0);
        assert_eq!(snap.version, 0);
    }

    #[tokio::test]
    async fn monitor_poll_once_runs_without_panic() {
        let monitor = ResourceMonitor::new(MonitorConfig::default());
        // poll_once should not panic even without GPUs.
        let _significant = monitor.poll_once().await;

        let snap = monitor.snapshot().await;
        assert!(snap.capabilities.reported_at > 0);
    }

    #[tokio::test]
    async fn monitor_set_available_for_mesh() {
        let monitor = ResourceMonitor::new(MonitorConfig::default());

        monitor.set_available_for_mesh(false).await;
        monitor.poll_once().await;

        let snap = monitor.snapshot().await;
        assert!(!snap.capabilities.available.available_for_mesh);

        monitor.set_available_for_mesh(true).await;
        monitor.poll_once().await;

        let snap = monitor.snapshot().await;
        assert!(snap.capabilities.available.available_for_mesh);
    }

    #[tokio::test]
    async fn monitor_availability_toggle_is_significant() {
        let monitor = ResourceMonitor::new(MonitorConfig::default());

        // First poll to establish baseline.
        monitor.poll_once().await;

        // Toggle availability — this should be significant.
        monitor.set_available_for_mesh(false).await;
        let significant = monitor.poll_once().await;
        assert!(significant, "availability toggle should be significant");
    }
}
