// SPDX-License-Identifier: AGPL-3.0-or-later
//! One truthiness rule for the workspace's operator switches.
//!
//! A switch is on for `1`, `true`, `yes` or `on` — case-insensitive,
//! surrounding whitespace ignored; unset, empty or anything else is off.
//!
//! It lives here, beside the rest of the daemon↔package contract, because its
//! two readers may not name each other: the daemon's job drivers
//! (`sovereign-mesh`'s `auto_resume`, bound for `sovereign-daemon` by
//! `quality/DAEMON_CORE.md` §3.2/§4.3) and the corpus reindexer (Workbench,
//! bound for a `corpus-engine` crate). A second spelling of the match would be
//! the ARCH 8 duplicate this helper was created to prevent
//! (`ralph/DECISIONS.md` 2026-09-16, `REVIEW-build-mesh-host-decouple`).

/// True when `name` is set to `1`, `true`, `yes` or `on` — case-insensitive,
/// surrounding whitespace ignored. Unset, empty or any other value is false.
pub fn truthy(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => {
            let v = v.trim().to_ascii_lowercase();
            matches!(v.as_str(), "1" | "true" | "yes" | "on")
        }
        Err(_) => false,
    }
}
