// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn mobile` — a named absence.
//!
//! This verb ran `sovereign-server`, the phone-facing API. That binary was
//! deleted and no replacement mobile host ships, so every subcommand
//! (`serve`, `status`, `pair`) answers the absence by name — never a
//! dispatcher miss, and never a "binary not found" that reads like a build
//! problem the user could fix (ARCH principle 6).

use sovereign_contracts::MOBILE_HOST_ABSENT;

/// Run a `mobile` subcommand. Returns the process exit code: always 1.
pub async fn run_mobile(args: &[String]) -> i32 {
    tracing::info!(?args, "mobile: absent — {MOBILE_HOST_ABSENT}");
    eprintln!("svrn mobile: {MOBILE_HOST_ABSENT}");
    1
}
