// SPDX-License-Identifier: AGPL-3.0-or-later
//! The node's hardware and live load, measured by cw-rails itself, and the
//! two declarations that adjust the measurement (phase-b-83 (1): the node's
//! hardware is the node's, not a registrant's).
//!
//! A registrant still knows two things a sysfs read cannot: serve's loader
//! sees the real VRAM pool on a unified-memory APU (sysfs sees only the
//! carveout), and svrn holds a storage budget smaller than the disk. Both
//! arrive as declarations and are applied here over the measurement
//! (phase-b-90 (B), phase-b-91), before the live resources are read off the
//! result, so `available.free_vram_gb` follows the override.

use commonwealth_core::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};
use commonwealth_discovery::hardware;
use tracing::debug;

const GIB: u64 = 1_073_741_824;

/// One reading of this node through `commonwealth_discovery::hardware`, the
/// one detector.
#[derive(Debug, Clone)]
pub struct SelfMeasurement {
    pub hardware: HardwareProfile,
    pub cpu_utilization: f32,
    pub free_ram_gb: f32,
    pub free_disk_gb: f32,
    /// The first NVIDIA GPU's (utilization, free VRAM GB), when one answers.
    pub nvidia: Option<(f32, f32)>,
}

impl SelfMeasurement {
    /// Read the node now. Blocking: it walks disks and may spawn
    /// `nvidia-smi`.
    pub fn now() -> Self {
        let (cpu_utilization, free_ram_gb) = hardware::read_cpu_ram_state();
        Self {
            hardware: hardware::detect_hardware(),
            cpu_utilization,
            free_ram_gb,
            free_disk_gb: hardware::read_disk_state(),
            nvidia: hardware::read_nvidia_gpu_state().first().copied(),
        }
    }
}

/// Write `measured` into `caps`' hardware and live resources, adjusted by
/// what `declared` says about VRAM and storage budget.
///
/// VRAM: when the first declaration naming a GPU totals more than was
/// measured, the shortfall goes to the first measured GPU, or the declared
/// entry stands in when none was measured. Storage: free storage, static and
/// live, is clamped to the smallest declared `storage_remaining_bytes`,
/// floored to whole GiB. The field itself is never copied onto `caps`.
pub fn apply(caps: &mut NodeCapabilities, measured: &SelfMeasurement, declared: &[NodeCapabilities]) {
    let mut hw = measured.hardware.clone();
    let detected: u32 = hw.gpus.iter().map(|g| g.vram_gb).sum();
    match declared.iter().find(|d| !d.hardware.gpus.is_empty()) {
        Some(d) => {
            let declared_vram: u32 = d.hardware.gpus.iter().map(|g| g.vram_gb).sum();
            if declared_vram > detected {
                if let Some(g0) = hw.gpus.first_mut() {
                    g0.vram_gb += declared_vram - detected;
                } else {
                    hw.gpus.extend(d.hardware.gpus.iter().cloned());
                }
            }
            debug!(target: "gossip", detected, declared_vram,
                   "self measure: a registrant's VRAM figure applied over the measured one");
        }
        None => debug!(target: "gossip", detected,
                       "self measure: no registrant declares VRAM; the measured figure stands"),
    }

    let mut free_disk_gb = measured.free_disk_gb;
    let ceiling = declared.iter().filter_map(|d| d.storage_remaining_bytes).min();
    if let Some(remaining) = ceiling {
        let remaining_gb = (remaining / GIB) as u32;
        if remaining_gb < hw.free_storage_gb {
            hw.free_storage_gb = remaining_gb;
        }
        free_disk_gb = free_disk_gb.min((remaining / GIB) as f32);
        debug!(target: "gossip", budget_remaining_gb = remaining_gb,
               free_storage_gb = hw.free_storage_gb,
               "self measure: free storage clamped to the declared budget");
    }

    let (gpu_utilization, free_vram_gb) = measured
        .nvidia
        .unwrap_or((0.0, hw.gpus.first().map(|g| g.vram_gb as f32).unwrap_or(0.0)));
    caps.available = AvailableResources {
        free_vram_gb,
        free_ram_gb: measured.free_ram_gb,
        free_storage_gb: free_disk_gb,
        gpu_utilization,
        cpu_utilization: measured.cpu_utilization,
        available_for_mesh: true,
    };
    caps.hardware = hw;
}

#[cfg(test)]
#[path = "self_measure/tests.rs"]
mod tests;
