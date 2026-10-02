// SPDX-License-Identifier: AGPL-3.0-or-later
use commonwealth_core::capabilities::{ComputeType, GpuInfo};

use super::*;
use crate::gossip::minimal_capabilities;

fn gpu(vram_gb: u32) -> GpuInfo {
    GpuInfo {
        name: "GPU".to_string(),
        vram_gb,
        compute_type: ComputeType::Vulkan,
        estimated_tflops: 0.0,
    }
}

fn measured(gpus: Vec<GpuInfo>) -> SelfMeasurement {
    SelfMeasurement {
        hardware: HardwareProfile {
            gpus,
            system_ram_gb: 128,
            cpu_cores: 16,
            total_storage_gb: 2000,
            free_storage_gb: 900,
            network_bandwidth_mbps: None,
        },
        cpu_utilization: 0.1,
        free_ram_gb: 64.0,
        free_disk_gb: 900.5,
        nvidia: None,
    }
}

fn declaring(gpus: Vec<GpuInfo>, storage_remaining_bytes: Option<u64>) -> NodeCapabilities {
    let mut d = minimal_capabilities(0, &[], None);
    d.hardware.gpus = gpus;
    d.storage_remaining_bytes = storage_remaining_bytes;
    d
}

fn merged(m: &SelfMeasurement, declared: &[NodeCapabilities]) -> NodeCapabilities {
    let mut caps = minimal_capabilities(0, &[], None);
    apply(&mut caps, m, declared);
    caps
}

fn vram(caps: &NodeCapabilities) -> Vec<u32> {
    caps.hardware.gpus.iter().map(|g| g.vram_gb).collect()
}

/// The four VRAM cases: a shortfall goes to the first measured GPU; with no
/// GPU measured the declared entry stands in; a declaration below the
/// measurement, and no declaration, leave the measurement. Free VRAM
/// follows the result. Failing input: take the declared hardware wholesale.
#[test]
fn a_declared_vram_figure_adjusts_the_measured_one() {
    let shortfall = merged(
        &measured(vec![gpu(1), gpu(2)]),
        &[declaring(vec![gpu(124)], None)],
    );
    assert_eq!(vram(&shortfall), vec![122, 2]);
    assert_eq!(shortfall.available.free_vram_gb, 122.0);

    let none_measured = merged(&measured(vec![]), &[declaring(vec![gpu(124)], None)]);
    assert_eq!(vram(&none_measured), vec![124]);
    assert_eq!(none_measured.available.free_vram_gb, 124.0);

    let below = merged(&measured(vec![gpu(24)]), &[declaring(vec![gpu(8)], None)]);
    assert_eq!(vram(&below), vec![24]);

    let undeclared = merged(&measured(vec![gpu(24)]), &[declaring(vec![], None)]);
    assert_eq!(vram(&undeclared), vec![24]);
    assert_eq!(undeclared.hardware.system_ram_gb, 128);
    assert!(undeclared.available.available_for_mesh);
}

/// Free storage, static and live, is clamped to the smallest declared
/// budget, floored to whole GiB; `Some(0)` advertises zero; no declared
/// budget leaves the measurement; the self row never carries the field.
/// Failing input: drop the clamp.
#[test]
fn a_declared_storage_budget_clamps_free_storage() {
    let m = measured(vec![]);
    let clamped = merged(
        &m,
        &[
            declaring(vec![], Some(50 * GIB + 7)),
            declaring(vec![], Some(10 * GIB + GIB / 2)),
        ],
    );
    assert_eq!(clamped.hardware.free_storage_gb, 10);
    assert_eq!(clamped.available.free_storage_gb, 10.0);
    assert_eq!(clamped.storage_remaining_bytes, None);

    let exhausted = merged(&m, &[declaring(vec![], Some(0))]);
    assert_eq!(exhausted.hardware.free_storage_gb, 0);
    assert_eq!(exhausted.available.free_storage_gb, 0.0);

    let unbudgeted = merged(&m, &[declaring(vec![], None)]);
    assert_eq!(unbudgeted.hardware.free_storage_gb, 900);
    assert_eq!(unbudgeted.available.free_storage_gb, 900.5);
    assert_eq!(unbudgeted.storage_remaining_bytes, None);
}
