// SPDX-License-Identifier: AGPL-3.0-or-later
use commonwealth_core::capabilities::AvailableResources;

/// Thresholds for deciding when to propagate capability updates via gossip.
/// Updates only propagate when they cross significance thresholds,
/// keeping background gossip traffic negligible.
#[derive(Debug, Clone)]
pub struct SignificanceThresholds {
    /// Minimum change in free VRAM (as fraction of total) to trigger update.
    /// Architecture specifies >10% change.
    pub vram_fraction: f32,
    /// GPU utilization crossing these boundaries triggers update.
    /// Architecture specifies 0.5 and 0.9.
    pub gpu_util_boundaries: Vec<f32>,
    /// Minimum change in free storage (GB) to trigger update.
    pub storage_change_gb: f32,
    /// Minimum change in CPU utilization to trigger update.
    pub cpu_util_change: f32,
}

impl Default for SignificanceThresholds {
    fn default() -> Self {
        Self {
            vram_fraction: 0.10,
            gpu_util_boundaries: vec![0.5, 0.9],
            storage_change_gb: 5.0,
            cpu_util_change: 0.2,
        }
    }
}

impl SignificanceThresholds {
    /// Check if a resource change is significant enough to warrant a gossip update.
    pub fn is_significant(
        &self,
        previous: &AvailableResources,
        current: &AvailableResources,
    ) -> bool {
        // VRAM change exceeds threshold.
        let vram_total = previous.free_vram_gb.max(current.free_vram_gb);
        if vram_total > 0.0 {
            let vram_delta = (current.free_vram_gb - previous.free_vram_gb).abs();
            if vram_delta / vram_total > self.vram_fraction {
                return true;
            }
        }

        // GPU utilization crosses a boundary.
        if self.crosses_boundary(previous.gpu_utilization, current.gpu_utilization) {
            return true;
        }

        // Storage change exceeds threshold.
        if (current.free_storage_gb - previous.free_storage_gb).abs() > self.storage_change_gb {
            return true;
        }

        // CPU utilization change exceeds threshold.
        if (current.cpu_utilization - previous.cpu_utilization).abs() > self.cpu_util_change {
            return true;
        }

        // Availability toggle always propagates.
        if current.available_for_mesh != previous.available_for_mesh {
            return true;
        }

        false
    }

    /// Check if a value crosses any of the configured boundaries.
    fn crosses_boundary(&self, old: f32, new: f32) -> bool {
        for &boundary in &self.gpu_util_boundaries {
            let old_side = old < boundary;
            let new_side = new < boundary;
            if old_side != new_side {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonwealth_core::capabilities::AvailableResources;

    fn make_resources(
        free_vram: f32,
        gpu_util: f32,
        free_storage: f32,
        cpu_util: f32,
        available: bool,
    ) -> AvailableResources {
        AvailableResources {
            free_vram_gb: free_vram,
            free_ram_gb: 16.0,
            free_storage_gb: free_storage,
            gpu_utilization: gpu_util,
            cpu_utilization: cpu_util,
            available_for_mesh: available,
        }
    }

    #[test]
    fn no_change_is_not_significant() {
        let t = SignificanceThresholds::default();
        let r = make_resources(10.0, 0.3, 200.0, 0.4, true);
        assert!(!t.is_significant(&r, &r));
    }

    #[test]
    fn small_vram_change_not_significant() {
        let t = SignificanceThresholds::default();
        let old = make_resources(10.0, 0.3, 200.0, 0.4, true);
        let new = make_resources(9.5, 0.3, 200.0, 0.4, true);
        // 0.5/10.0 = 5% < 10% threshold.
        assert!(!t.is_significant(&old, &new));
    }

    #[test]
    fn large_vram_change_is_significant() {
        let t = SignificanceThresholds::default();
        let old = make_resources(10.0, 0.3, 200.0, 0.4, true);
        let new = make_resources(8.0, 0.3, 200.0, 0.4, true);
        // 2.0/10.0 = 20% > 10% threshold.
        assert!(t.is_significant(&old, &new));
    }

    #[test]
    fn gpu_util_crossing_half_is_significant() {
        let t = SignificanceThresholds::default();
        let old = make_resources(10.0, 0.4, 200.0, 0.4, true);
        let new = make_resources(10.0, 0.6, 200.0, 0.4, true);
        // Crosses 0.5 boundary.
        assert!(t.is_significant(&old, &new));
    }

    #[test]
    fn gpu_util_crossing_ninety_is_significant() {
        let t = SignificanceThresholds::default();
        let old = make_resources(10.0, 0.85, 200.0, 0.4, true);
        let new = make_resources(10.0, 0.95, 200.0, 0.4, true);
        // Crosses 0.9 boundary.
        assert!(t.is_significant(&old, &new));
    }

    #[test]
    fn gpu_util_within_same_band_not_significant() {
        let t = SignificanceThresholds::default();
        let old = make_resources(10.0, 0.6, 200.0, 0.4, true);
        let new = make_resources(10.0, 0.7, 200.0, 0.4, true);
        // Both between 0.5 and 0.9 — no boundary crossing.
        assert!(!t.is_significant(&old, &new));
    }

    #[test]
    fn storage_change_significant() {
        let t = SignificanceThresholds::default();
        let old = make_resources(10.0, 0.3, 200.0, 0.4, true);
        let new = make_resources(10.0, 0.3, 190.0, 0.4, true);
        // 10 GB > 5 GB threshold.
        assert!(t.is_significant(&old, &new));
    }

    #[test]
    fn availability_toggle_always_significant() {
        let t = SignificanceThresholds::default();
        let old = make_resources(10.0, 0.3, 200.0, 0.4, true);
        let new = make_resources(10.0, 0.3, 200.0, 0.4, false);
        assert!(t.is_significant(&old, &new));
    }

    #[test]
    fn cpu_util_jump_significant() {
        let t = SignificanceThresholds::default();
        let old = make_resources(10.0, 0.3, 200.0, 0.3, true);
        let new = make_resources(10.0, 0.3, 200.0, 0.6, true);
        // 0.3 change > 0.2 threshold.
        assert!(t.is_significant(&old, &new));
    }
}
