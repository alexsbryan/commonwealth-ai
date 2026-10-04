// SPDX-License-Identifier: AGPL-3.0-or-later
//! One machine's hardware identity, derived from what it advertises.
//!
//! Moved here from `sovereign_mesh::mesh_measurements` by pb-serve-placement
//! (phase-b-22): the mesh stamps it on every member's status and serve keys
//! its placement measurements on it, so two programs derive it and one
//! function answers both (identity from essence, ARCH principle 8).

/// Stable hash of one machine's hardware.
///
/// Small on purpose — this needs equality against a previously recorded value,
/// not cryptographic uniqueness. `gpus` is `(name, vram_gb, backend)`, and the
/// backend string is part of the hash because the same silicon driven through
/// different backends is, for throughput purposes, different hardware.
///
/// Order-independent across GPUs: the same two cards enumerated in either order
/// hash identically, so a driver-order change does not invalidate a record.
pub fn hardware_fingerprint(
    cpu_cores: u32,
    system_ram_gb: u32,
    gpus: &[(String, u32, String)],
) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    // Hash each GPU independently, then combine with a commutative fold so
    // enumeration order cannot change the result.
    let mut gpu_mix: u64 = 0;
    for (name, vram_gb, backend) in gpus {
        let mut g = DefaultHasher::new();
        name.hash(&mut g);
        vram_gb.hash(&mut g);
        backend.hash(&mut g);
        gpu_mix ^= g.finish();
    }

    let mut h = DefaultHasher::new();
    cpu_cores.hash(&mut h);
    system_ram_gb.hash(&mut h);
    gpus.len().hash(&mut h);
    gpu_mix.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardware_fingerprint_is_gpu_order_independent() {
        let a = hardware_fingerprint(
            32,
            128,
            &[
                ("RTX 4090".into(), 24, "cuda".into()),
                ("Radeon 8060S".into(), 128, "vulkan".into()),
            ],
        );
        let b = hardware_fingerprint(
            32,
            128,
            &[
                ("Radeon 8060S".into(), 128, "vulkan".into()),
                ("RTX 4090".into(), 24, "cuda".into()),
            ],
        );
        assert_eq!(
            a, b,
            "driver enumeration order must not invalidate a record"
        );
    }
}
