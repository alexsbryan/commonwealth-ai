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

/// The golden pin (P dec 5): on this host the shipped server's reading equals
/// the one the vendored ggml gave `hardware::detect_gpu` (first non-CPU
/// device with memory) and the mesh's VRAM figure read (non-CPU totals, whole
/// GB). The server prints whole MiB, so bytes compare at that grain.
#[test]
fn shipped_server_reading_equals_the_ggml_one() {
    // A test runs from `target/<profile>/deps`; the shipped server sits in
    // `target/<profile>`.
    let exe = std::env::current_exe().unwrap();
    let binary = exe.parent().unwrap().parent().unwrap().join(SERVER_BINARY);
    if !binary.is_file() {
        eprintln!(
            "COULD-NOT-JUDGE shipped_server_reading_equals_the_ggml_one: no {} \
             (build it with serve/crates/sovereign-compute/llama-server/build.sh)",
            binary.display()
        );
        return;
    }
    let seen = probe(&binary);
    let ggml: Vec<_> = crate::llama::list_llama_ggml_backend_devices()
        .into_iter()
        .filter(|d| d.name.to_lowercase() != "cpu")
        .collect();

    let ggml_first = ggml.iter().find(|d| d.memory_total > 0);
    let probe_first = seen.first_gpu().expect("the probe read the server");
    assert_eq!(
        probe_first.map(|d| (d.name.clone(), d.total_bytes)),
        ggml_first.map(|d| (d.name.clone(), d.memory_total / MIB * MIB)),
    );

    let ggml_total: u64 = ggml.iter().map(|d| d.memory_total).sum();
    let ggml_gb = (ggml_total > 0).then_some((ggml_total / (1024 * MIB)) as u32);
    assert_eq!(seen.total_vram_gb(), Ok(ggml_gb));
    eprintln!(
        "golden: probe {:?} GB, first {:?}; ggml {ggml_gb:?} GB",
        seen.total_vram_gb(),
        probe_first.map(|d| &d.name)
    );
}
