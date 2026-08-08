// SPDX-License-Identifier: AGPL-3.0-or-later
use std::process::Command;

use tracing::{debug, warn};

use commonwealth_core::capabilities::{ComputeType, GpuInfo, HardwareProfile};

/// Detect the full hardware profile of this machine.
pub fn detect_hardware() -> HardwareProfile {
    let sys = sysinfo::System::new_all();

    let gpus = detect_gpus();

    let total_storage_gb;
    let free_storage_gb;
    {
        let disks = sysinfo::Disks::new_with_refreshed_list();
        let total_bytes: u64 = disks.list().iter().map(|d| d.total_space()).sum();
        let free_bytes: u64 = disks.list().iter().map(|d| d.available_space()).sum();
        total_storage_gb = (total_bytes / 1_073_741_824) as u32;
        free_storage_gb = (free_bytes / 1_073_741_824) as u32;
    }

    HardwareProfile {
        gpus,
        system_ram_gb: (sys.total_memory() / 1_073_741_824) as u32,
        cpu_cores: sys.cpus().len() as u32,
        total_storage_gb,
        free_storage_gb,
        network_bandwidth_mbps: None, // Detected via latency probes, not static.
    }
}

/// Detect GPUs on this machine. Tries nvidia-smi, rocm-smi, then Metal.
fn detect_gpus() -> Vec<GpuInfo> {
    let mut gpus = Vec::new();

    // Try NVIDIA (CUDA).
    gpus.extend(detect_nvidia_gpus());

    // Try AMD (ROCm).
    if gpus.is_empty() {
        gpus.extend(detect_rocm_gpus());
    }

    // Try macOS Metal.
    if gpus.is_empty() {
        gpus.extend(detect_metal_gpus());
    }

    if gpus.is_empty() {
        debug!("no discrete GPUs detected");
    }

    gpus
}

/// Parse nvidia-smi output to detect CUDA GPUs.
fn detect_nvidia_gpus() -> Vec<GpuInfo> {
    let output = Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output();

    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return vec![],
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut gpus = Vec::new();

    for line in stdout.lines() {
        let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if parts.len() >= 2 {
            let name = parts[0].to_string();
            let vram_mb: u32 = parts[1].parse().unwrap_or(0);
            let vram_gb = (vram_mb + 512) / 1024; // Round to nearest GB.

            // Rough TFLOPS estimate based on VRAM class.
            let estimated_tflops = estimate_nvidia_tflops(&name, vram_gb);

            gpus.push(GpuInfo {
                name,
                vram_gb,
                compute_type: ComputeType::Cuda,
                estimated_tflops,
            });
        }
    }

    if !gpus.is_empty() {
        debug!(count = gpus.len(), "detected NVIDIA GPUs");
    }
    gpus
}

/// Parse rocm-smi output to detect ROCm GPUs.
fn detect_rocm_gpus() -> Vec<GpuInfo> {
    let output = Command::new("rocm-smi")
        .args(["--showproductname", "--showmeminfo", "vram", "--csv"])
        .output();

    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return vec![],
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut gpus = Vec::new();

    // rocm-smi CSV output varies by version; parse best-effort.
    for line in stdout.lines().skip(1) {
        let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if parts.len() >= 2 {
            let name = parts[0].to_string();
            let vram_bytes: u64 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            let vram_gb = (vram_bytes / 1_073_741_824) as u32;

            gpus.push(GpuInfo {
                name,
                vram_gb,
                compute_type: ComputeType::Rocm,
                estimated_tflops: 20.0, // Conservative default for ROCm GPUs.
            });
        }
    }

    if !gpus.is_empty() {
        debug!(count = gpus.len(), "detected ROCm GPUs");
    }
    gpus
}

/// Detect Apple Silicon GPU via system_profiler (macOS only).
fn detect_metal_gpus() -> Vec<GpuInfo> {
    if !cfg!(target_os = "macos") {
        return vec![];
    }

    // On Apple Silicon, unified memory is shared with GPU.
    // Use system_profiler to get the chip name.
    let output = Command::new("system_profiler")
        .args(["SPHardwareDataType", "-json"])
        .output();

    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return vec![],
    };

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse JSON to extract chip name and memory.
    let json: serde_json::Value = match serde_json::from_str(&stdout) {
        Ok(v) => v,
        Err(e) => {
            warn!("failed to parse system_profiler output: {e}");
            return vec![];
        }
    };

    let hardware = json
        .get("SPHardwareDataType")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first());

    let hardware = match hardware {
        Some(h) => h,
        None => return vec![],
    };

    let chip_name = hardware
        .get("chip_type")
        .and_then(|v| v.as_str())
        .unwrap_or("Apple Silicon");

    // physical_memory is like "32 GB".
    let memory_str = hardware
        .get("physical_memory")
        .and_then(|v| v.as_str())
        .unwrap_or("0 GB");
    let memory_gb: u32 = memory_str
        .split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    // On Apple Silicon, ~75% of unified memory is typically available for GPU.
    let gpu_vram_gb = (memory_gb as f32 * 0.75) as u32;

    let estimated_tflops = estimate_apple_tflops(chip_name);

    if gpu_vram_gb > 0 {
        debug!(
            chip = chip_name,
            vram_gb = gpu_vram_gb,
            "detected Metal GPU"
        );
        vec![GpuInfo {
            name: chip_name.to_string(),
            vram_gb: gpu_vram_gb,
            compute_type: ComputeType::Metal,
            estimated_tflops,
        }]
    } else {
        vec![]
    }
}

/// Rough FP16 TFLOPS estimate for NVIDIA GPUs based on name/VRAM.
fn estimate_nvidia_tflops(name: &str, vram_gb: u32) -> f32 {
    let name_lower = name.to_lowercase();
    if name_lower.contains("4090") {
        82.6
    } else if name_lower.contains("4080") {
        48.7
    } else if name_lower.contains("4070") {
        29.1
    } else if name_lower.contains("3090") {
        35.6
    } else if name_lower.contains("3080") {
        29.8
    } else if name_lower.contains("a100") {
        77.9
    } else if name_lower.contains("h100") {
        267.6
    } else {
        // Rough fallback: ~2 TFLOPS per GB VRAM.
        vram_gb as f32 * 2.0
    }
}

/// Rough FP16 TFLOPS estimate for Apple Silicon chips.
fn estimate_apple_tflops(chip_name: &str) -> f32 {
    let name_lower = chip_name.to_lowercase();
    if name_lower.contains("m4 ultra") {
        54.0
    } else if name_lower.contains("m4 max") {
        27.0
    } else if name_lower.contains("m4 pro") {
        15.0
    } else if name_lower.contains("m4") {
        8.0
    } else if name_lower.contains("m3 ultra") {
        42.0
    } else if name_lower.contains("m3 max") {
        21.0
    } else if name_lower.contains("m3 pro") {
        11.0
    } else if name_lower.contains("m3") {
        7.0
    } else if name_lower.contains("m2 ultra") {
        27.2
    } else if name_lower.contains("m2 max") {
        13.6
    } else if name_lower.contains("m2") {
        7.0
    } else if name_lower.contains("m1 ultra") {
        21.0
    } else if name_lower.contains("m1 max") {
        10.6
    } else if name_lower.contains("m1") {
        5.5
    } else {
        5.0 // Conservative default.
    }
}

/// Read current GPU utilization and free VRAM from nvidia-smi.
/// Returns (utilization 0.0-1.0, free_vram_gb) per GPU.
pub fn read_nvidia_gpu_state() -> Vec<(f32, f32)> {
    let output = Command::new("nvidia-smi")
        .args([
            "--query-gpu=utilization.gpu,memory.free",
            "--format=csv,noheader,nounits",
        ])
        .output();

    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return vec![],
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut results = Vec::new();

    for line in stdout.lines() {
        let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if parts.len() >= 2 {
            let util_pct: f32 = parts[0].parse().unwrap_or(0.0);
            let free_mb: f32 = parts[1].parse().unwrap_or(0.0);
            results.push((util_pct / 100.0, free_mb / 1024.0));
        }
    }
    results
}

/// Read current disk state (free storage).
pub fn read_disk_state() -> f32 {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let free_bytes: u64 = disks.list().iter().map(|d| d.available_space()).sum();
    free_bytes as f32 / 1_073_741_824.0
}

/// Aggregate free space across all mounted disks, in bytes. Mirrors
/// the disk-aggregation logic of [`detect_hardware`] and
/// [`read_disk_state`] without the GB-truncation, for callers that
/// need to compare against byte-precise budgets.
pub fn read_disk_free_bytes() -> u64 {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    disks.list().iter().map(|d| d.available_space()).sum()
}

/// Where the free-RAM figure in [`read_cpu_ram_state`] came from.
///
/// Exists so the fallback below is *named* rather than silent: a caller
/// advertising RAM to the mesh should be able to tell a measured number from a
/// derived one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RamSource {
    /// `sysinfo::System::available_memory()` returned a usable figure.
    Reported,
    /// `available_memory()` reported an impossible 0; derived `total - used`.
    DerivedFromUsed,
    /// Neither figure was usable — the instrument, not the machine, is at fault.
    Unmeasurable,
}

/// Resolve free RAM in GiB from raw byte counters, naming which source won.
///
/// Split out from [`read_cpu_ram_state`] so the fallback has a failing input we
/// can actually name in a test (ARCH §18.1) — `available == 0` cannot be
/// provoked on demand from a live `sysinfo` handle.
///
/// **Why a fallback is needed at all.** On macOS, sysinfo computes available as
/// `(free + inactive + purgeable - compressor_pages) * page_size`, subtracting
/// on the *page count* before scaling. When the memory compressor is saturated
/// — routinely true right after a large build or test run — that expression
/// saturates to exactly 0, and this host then advertised 0GiB free RAM to the
/// mesh while 30+GiB was genuinely available. Measured on BeefyMac 2026-08-08
/// (64GiB host, `available_memory() == 0`, `total - used == 31.4GiB`).
///
/// `total - used` is the durable cross-check because sysinfo counts compressor
/// pages as *used*, so the compressor is accounted for once rather than twice.
/// It can only reach 0 when memory genuinely is exhausted.
pub fn resolve_free_ram_gb(available: u64, total: u64, used: u64) -> (f32, RamSource) {
    const BYTES_PER_GIB: f32 = 1_073_741_824.0;

    if available > 0 {
        return (available as f32 / BYTES_PER_GIB, RamSource::Reported);
    }
    // A machine executing this code has non-zero available memory by
    // definition, so `available == 0` is an instrument failure, not a reading.
    match total.checked_sub(used) {
        Some(derived) if derived > 0 && total > 0 => {
            (derived as f32 / BYTES_PER_GIB, RamSource::DerivedFromUsed)
        }
        _ => (0.0, RamSource::Unmeasurable),
    }
}

/// Read current CPU and RAM state.
///
/// Returns `(cpu_utilisation_0_to_1, free_ram_gib)`. See
/// [`resolve_free_ram_gb`] for why the RAM figure is not read straight from
/// `available_memory()`.
pub fn read_cpu_ram_state() -> (f32, f32) {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    sys.refresh_cpu_usage();

    // sysinfo needs a brief pause to measure CPU delta — on first call returns 0.
    // Callers should call this periodically; the first reading may be inaccurate.
    let cpu_util = if sys.cpus().is_empty() {
        0.0
    } else {
        sys.cpus().iter().map(|c| c.cpu_usage()).sum::<f32>() / sys.cpus().len() as f32 / 100.0
    };

    let (free_ram, source) =
        resolve_free_ram_gb(sys.available_memory(), sys.total_memory(), sys.used_memory());
    match source {
        RamSource::Reported => {
            debug!(free_ram_gb = free_ram, "read free RAM from available_memory");
        }
        RamSource::DerivedFromUsed => {
            // Named, not silent (ARCH §18.3): downstream mesh advertisement
            // would otherwise publish a substituted figure indistinguishable
            // from a measured one.
            warn!(
                free_ram_gb = free_ram,
                total_gb = sys.total_memory() as f32 / 1_073_741_824.0,
                "available_memory() reported 0 on a running host; derived free RAM from total - used"
            );
        }
        RamSource::Unmeasurable => {
            warn!(
                total_bytes = sys.total_memory(),
                used_bytes = sys.used_memory(),
                "could not measure free RAM: both available_memory() and total - used are unusable"
            );
        }
    }
    (cpu_util, free_ram)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_hardware_returns_valid_profile() {
        let profile = detect_hardware();
        // These should always be nonzero on any real machine.
        assert!(profile.system_ram_gb > 0, "expected nonzero RAM");
        assert!(profile.cpu_cores > 0, "expected nonzero CPU cores");
        assert!(profile.total_storage_gb > 0, "expected nonzero storage");
    }

    #[test]
    fn nvidia_tflops_estimates() {
        assert!((estimate_nvidia_tflops("NVIDIA GeForce RTX 4090", 24) - 82.6).abs() < 0.1);
        assert!((estimate_nvidia_tflops("NVIDIA A100", 80) - 77.9).abs() < 0.1);
        // Unknown GPU: fallback is ~2 * vram_gb.
        assert!((estimate_nvidia_tflops("Unknown GPU", 16) - 32.0).abs() < 0.1);
    }

    #[test]
    fn apple_tflops_estimates() {
        assert!((estimate_apple_tflops("Apple M3 Ultra") - 42.0).abs() < 0.1);
        assert!((estimate_apple_tflops("Apple M4 Max") - 27.0).abs() < 0.1);
        assert!((estimate_apple_tflops("Apple M1") - 5.5).abs() < 0.1);
    }

    #[test]
    fn read_cpu_ram_state_returns_values() {
        let (cpu, ram) = read_cpu_ram_state();
        // CPU may be 0.0 on first call, but RAM should be > 0.
        assert!((0.0..=1.0).contains(&cpu));
        assert!(ram > 0.0, "expected nonzero free RAM");
    }

    #[test]
    fn free_ram_prefers_the_reported_figure() {
        let (gb, source) = resolve_free_ram_gb(8 * 1_073_741_824, 64 * 1_073_741_824, 0);
        assert_eq!(source, RamSource::Reported);
        assert!((gb - 8.0).abs() < 0.001, "got {gb}");
    }

    /// The regression this fix exists for, using the counters measured on
    /// BeefyMac 2026-08-08: a 64GiB host where the saturated memory compressor
    /// drove sysinfo's macOS `available_memory()` to exactly 0. Before the fix
    /// this host advertised 0GiB free RAM to the mesh.
    #[test]
    fn free_ram_falls_back_when_available_is_zero_under_compressor_pressure() {
        let total = 68_719_476_736; // 64GiB
        let used = 37_069_896_704; // ~34.5GiB (active + wired + compressor + speculative)

        let (gb, source) = resolve_free_ram_gb(0, total, used);

        assert_eq!(
            source,
            RamSource::DerivedFromUsed,
            "available_memory() == 0 on a running host must trigger the derivation, not be believed"
        );
        assert!(
            gb > 29.0 && gb < 30.0,
            "expected ~29.5GiB derived from total - used, got {gb}"
        );
    }

    #[test]
    fn free_ram_is_unmeasurable_rather_than_zero_when_both_sources_fail() {
        // An all-zero read is a dead instrument, not a full machine. It must be
        // reported as such rather than defaulted to a plausible number.
        assert_eq!(resolve_free_ram_gb(0, 0, 0).1, RamSource::Unmeasurable);
        // used > total is incoherent; saturating that into a number would hide it.
        assert_eq!(
            resolve_free_ram_gb(0, 1_073_741_824, 2_147_483_648).1,
            RamSource::Unmeasurable
        );
    }

    #[test]
    fn read_disk_state_returns_positive() {
        let free = read_disk_state();
        assert!(free > 0.0, "expected nonzero free disk space");
    }
}
