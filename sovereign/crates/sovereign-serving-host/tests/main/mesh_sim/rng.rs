// SPDX-License-Identifier: AGPL-3.0-or-later
//! A seeded PRNG with no external dependency.
//!
//! Deliberately hand-rolled rather than pulled from `rand`: a Tier-1
//! run has to be reproducible across machines *and across dependency
//! upgrades*. `rand`'s distribution internals are explicitly not
//! covered by semver-stable output guarantees, so a `cargo update`
//! could silently move every number in the scoreboard. SplitMix64 is
//! twenty lines and its output is pinned by this file.

/// SplitMix64. Small, fast, and adequate for arrival jitter and
/// two-choices sampling — this is not cryptography.
#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`.
    pub fn next_f64(&mut self) -> f64 {
        // 53 significant bits, the most an f64 can hold exactly.
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform integer in `[0, n)`. Returns 0 for `n == 0`.
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        (self.next_u64() % n as u64) as usize
    }

    /// Uniform in `[lo, hi]`, inclusive.
    pub fn range_u32(&mut self, lo: u32, hi: u32) -> u32 {
        if hi <= lo {
            return lo;
        }
        lo + (self.next_u64() % (hi - lo + 1) as u64) as u32
    }

    /// Exponential inter-arrival with the given mean, in the same
    /// unit as `mean`. The arrival process a household actually
    /// generates is bursty; an exponential gap is the standard
    /// memoryless approximation and keeps the seed the only input.
    pub fn exponential(&mut self, mean: f64) -> f64 {
        let u = 1.0 - self.next_f64(); // (0, 1]
        -mean * u.ln()
    }
}

#[cfg(test)]
mod tests {
    use super::Rng;

    #[test]
    fn the_same_seed_produces_the_same_stream() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..64 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = Rng::new(1);
        let mut b = Rng::new(2);
        assert_ne!(a.next_u64(), b.next_u64());
    }

    #[test]
    fn uniforms_stay_in_range() {
        let mut r = Rng::new(7);
        for _ in 0..1000 {
            let f = r.next_f64();
            assert!((0.0..1.0).contains(&f));
            assert!(r.below(5) < 5);
            let v = r.range_u32(10, 20);
            assert!((10..=20).contains(&v));
        }
    }

    #[test]
    fn exponential_is_positive_and_roughly_centred() {
        let mut r = Rng::new(99);
        let n = 20_000;
        let sum: f64 = (0..n).map(|_| r.exponential(100.0)).sum();
        let mean = sum / n as f64;
        assert!(mean > 90.0 && mean < 110.0, "mean was {mean}");
    }
}
