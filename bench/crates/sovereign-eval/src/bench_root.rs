// SPDX-License-Identifier: AGPL-3.0-or-later
//! One decider for "where is the bench tree".

use std::path::PathBuf;

/// Env knob naming the bank/baseline root. Set for in-repo builds by the
/// workspace `.cargo/config.toml`; a lifted crate must set it itself.
pub const BENCH_ROOT_ENV: &str = "SOVEREIGN_BENCH_ROOT";

/// The bench tree, or `None` when the knob is unset.
pub fn bench_root() -> Option<PathBuf> {
    std::env::var(BENCH_ROOT_ENV).ok().map(PathBuf::from)
}

/// The bench tree, or a message naming what to set. Absence is reported.
pub fn require_bench_root() -> Result<PathBuf, String> {
    bench_root().ok_or_else(|| format!("{BENCH_ROOT_ENV} is unset — point it at the bench tree"))
}
