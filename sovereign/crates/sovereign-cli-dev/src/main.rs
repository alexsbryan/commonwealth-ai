// SPDX-License-Identifier: AGPL-3.0-or-later
//! Thin shim over the `sovereign_cli_dev` library.
//!
//! Everything that used to live here — the module tree, the runtime setup and
//! the verb table — moved into `src/lib.rs` on 2026-08-21 so the crate has a
//! `[lib]` target other crates can link. `sovereign-cli` now serves
//! `sovereign_cli_dev::InProcessCodeVerb` arms in its own process instead of
//! `exec`ing this binary for them. See the crate docs in `lib.rs` for why.

fn main() -> ! {
    sovereign_cli_dev::bin_main()
}
