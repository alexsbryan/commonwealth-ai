use serde::{Deserialize, Serialize};
use sysinfo::System;

/// Detected hardware capabilities used for model loading decisions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareProfile {
    pub system_ram_bytes: u64,
    pub gpu_available: bool,
    pub gpu_name: Option<String>,
    pub gpu_memory_bytes: Option<u64>,
    pub recommended_gpu_layers: u32,
    /// True on Apple Silicon (M-series) where GPU and CPU share the same
    /// unified memory pool. When true, `system_ram_bytes` is the effective
    /// VRAM for profile selection.
    pub is_unified_memory: bool,
}

/// Hardware-tier profile. Used to select the appropriate model sizes from
/// the models.toml manifest (cpu_only → low_mem → default → high → very_high).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProfileName {
    CpuOnly,
    LowMem,
    Default,
    High,
    VeryHigh,
}

impl HardwareProfile {
    pub fn detect() -> Self {
        let sys = System::new_all();
        let system_ram_bytes = sys.total_memory();

        let (gpu_available, gpu_name, gpu_memory_bytes, is_unified_memory) = detect_gpu();

        let recommended_gpu_layers = if gpu_available { 999 } else { 0 };

        eprintln!(
            "Hardware: {:.1} GB RAM, GPU: {} (layers: {}, unified: {})",
            system_ram_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
            gpu_name.as_deref().unwrap_or("none"),
            recommended_gpu_layers,
            is_unified_memory,
        );

        Self {
            system_ram_bytes,
            gpu_available,
            gpu_name,
            gpu_memory_bytes,
            recommended_gpu_layers,
            is_unified_memory,
        }
    }

    pub fn system_ram_gb(&self) -> f64 {
        self.system_ram_bytes as f64 / (1024.0 * 1024.0 * 1024.0)
    }

    /// Effective VRAM available for model loading.
    /// On unified memory systems (Apple Silicon) this is the full system RAM.
    /// On discrete GPU systems this is the GPU's VRAM.
    pub fn effective_vram_gb(&self) -> f32 {
        if self.is_unified_memory {
            self.system_ram_bytes as f32 / 1_073_741_824.0
        } else {
            self.gpu_memory_bytes.unwrap_or(0) as f32 / 1_073_741_824.0
        }
    }
}

/// Select the model-size profile for this hardware.
///
/// Thresholds match models.toml:
/// - `high` requires ≥20 GB so Qwen3.5-27B (~16.5 GB) fits with headroom
///   for the always-resident Fast (~2 GB) and Embed (~2.5 GB) slots.
pub fn select_profile(hw: &HardwareProfile) -> ProfileName {
    match hw.effective_vram_gb() as u32 {
        0       => ProfileName::CpuOnly,
        1..=7   => ProfileName::LowMem,
        8..=19  => ProfileName::Default,
        20..=23 => ProfileName::High,
        _       => ProfileName::VeryHigh,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hw(effective_gb: f32) -> HardwareProfile {
        HardwareProfile {
            system_ram_bytes: (effective_gb * 1_073_741_824.0) as u64,
            gpu_available: effective_gb > 0.0,
            gpu_name: None,
            gpu_memory_bytes: None,
            recommended_gpu_layers: if effective_gb > 0.0 { 999 } else { 0 },
            is_unified_memory: true, // simplest test: use unified
        }
    }

    #[test]
    fn profile_selection() {
        assert_eq!(select_profile(&hw(0.0)),  ProfileName::CpuOnly);
        assert_eq!(select_profile(&hw(4.0)),  ProfileName::LowMem);
        assert_eq!(select_profile(&hw(12.0)), ProfileName::Default);
        assert_eq!(select_profile(&hw(19.0)), ProfileName::Default);
        assert_eq!(select_profile(&hw(20.0)), ProfileName::High);
        assert_eq!(select_profile(&hw(23.0)), ProfileName::High);
        assert_eq!(select_profile(&hw(24.0)), ProfileName::VeryHigh);
        assert_eq!(select_profile(&hw(64.0)), ProfileName::VeryHigh);
    }
}

/// Returns `(gpu_available, gpu_name, gpu_memory_bytes, is_unified_memory)`.
fn detect_gpu() -> (bool, Option<String>, Option<u64>, bool) {
    // On macOS Intel, Metal + llama.cpp on discrete AMD GPUs produces
    // garbage output. Only trust GPU on Apple Silicon.
    #[cfg(all(target_os = "macos", not(target_arch = "aarch64")))]
    return (false, None, None, false);

    #[cfg(not(all(target_os = "macos", not(target_arch = "aarch64"))))]
    {
        // On Apple Silicon, unified memory means Metal is always available
        // and the full system RAM is usable for models.
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            let sys = System::new_all();
            return (
                true,
                Some("Apple Metal (unified memory)".to_string()),
                Some(sys.total_memory()),
                true, // is_unified_memory
            );
        }

        // Check llama.cpp backend devices for a non-CPU device.
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        {
            let devices = llama_cpp_2::list_llama_ggml_backend_devices();
            for dev in &devices {
                if dev.memory_total > 0 && dev.name.to_lowercase() != "cpu" {
                    return (
                        true,
                        Some(dev.name.clone()),
                        Some(dev.memory_total as u64),
                        false, // discrete GPU — not unified memory
                    );
                }
            }
            (false, None, None, false)
        }
    }
}
