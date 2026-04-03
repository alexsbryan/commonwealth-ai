use serde::{Deserialize, Serialize};
use sysinfo::System;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareProfile {
    pub system_ram_bytes: u64,
    pub gpu_available: bool,
    pub gpu_name: Option<String>,
    pub gpu_memory_bytes: Option<u64>,
    pub recommended_gpu_layers: u32,
}

impl HardwareProfile {
    pub fn detect() -> Self {
        let sys = System::new_all();
        let system_ram_bytes = sys.total_memory();

        let (gpu_available, gpu_name, gpu_memory_bytes) = detect_gpu();

        let recommended_gpu_layers = if gpu_available { 999 } else { 0 };

        eprintln!(
            "Hardware: {:.1} GB RAM, GPU: {} (layers: {})",
            system_ram_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
            gpu_name.as_deref().unwrap_or("none"),
            recommended_gpu_layers,
        );

        Self {
            system_ram_bytes,
            gpu_available,
            gpu_name,
            gpu_memory_bytes,
            recommended_gpu_layers,
        }
    }

    pub fn system_ram_gb(&self) -> f64 {
        self.system_ram_bytes as f64 / (1024.0 * 1024.0 * 1024.0)
    }
}

fn detect_gpu() -> (bool, Option<String>, Option<u64>) {
    // On macOS Intel, Metal + llama.cpp on discrete AMD GPUs produces
    // garbage output. Only trust GPU on Apple Silicon.
    #[cfg(all(target_os = "macos", not(target_arch = "aarch64")))]
    return (false, None, None);

    #[cfg(not(all(target_os = "macos", not(target_arch = "aarch64"))))]
    {
        // Check llama.cpp backend devices for a non-CPU device.
        let devices = llama_cpp_2::list_llama_ggml_backend_devices();
        for dev in &devices {
            if dev.memory_total > 0 && dev.name.to_lowercase() != "cpu" {
                return (
                    true,
                    Some(dev.name.clone()),
                    Some(dev.memory_total as u64),
                );
            }
        }

        // On Apple Silicon, unified memory means Metal is always available.
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            let sys = System::new_all();
            return (
                true,
                Some("Apple Metal (unified memory)".to_string()),
                Some(sys.total_memory()),
            );
        }

        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        (false, None, None)
    }
}
