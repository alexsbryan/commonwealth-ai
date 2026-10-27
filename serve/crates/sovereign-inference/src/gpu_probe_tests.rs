// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

/// The Halo's answer at llama.cpp 035e227, captured 2026-10-12.
const HALO: &str = "Available devices:\n  Vulkan0: AMD Radeon 8060S Graphics (RADV GFX1151) (127488 MiB, 89258 MiB free)\n";

#[test]
fn parse_reads_name_description_and_memory() {
    let devices = parse(HALO).unwrap();
    assert_eq!(
        devices,
        vec![GpuDevice {
            name: "Vulkan0".into(),
            description: "AMD Radeon 8060S Graphics (RADV GFX1151)".into(),
            total_bytes: 127488 * MIB,
            free_bytes: 89258 * MIB,
        }]
    );
}

#[test]
fn parse_reads_a_cpu_only_host_as_no_devices() {
    assert_eq!(parse("Available devices:\n").unwrap(), vec![]);
}

#[test]
fn parse_refuses_what_it_cannot_read() {
    assert!(parse("error: unknown argument --list-devices\n").is_err());
    assert!(parse("Available devices:\n  Vulkan0: no memory here\n").is_err());
}

#[test]
fn halo_reads_124_gb() {
    let seen = GpuObservation {
        at: sovereign_time::system_now(),
        binary: PathBuf::new(),
        devices: parse(HALO),
    };
    assert_eq!(seen.total_vram_gb(), Ok(Some(124)));
    assert_eq!(seen.first_gpu().unwrap().unwrap().name, "Vulkan0");
}

#[test]
fn a_missing_binary_reads_as_unknown_not_zero() {
    let seen = probe(Path::new("/nonexistent/llama-server"));
    assert!(seen.devices.is_err(), "{:?}", seen.devices);
    assert!(seen.first_gpu().is_err());
    assert!(seen.total_vram_gb().is_err(), "unknown, not 0 GB");
}

#[test]
fn no_gpu_reads_as_none_not_zero() {
    let seen = GpuObservation {
        at: sovereign_time::system_now(),
        binary: PathBuf::new(),
        devices: Ok(vec![]),
    };
    assert_eq!(seen.first_gpu(), Ok(None));
    assert_eq!(seen.total_vram_gb(), Ok(None));
}
