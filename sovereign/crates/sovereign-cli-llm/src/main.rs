// SPDX-License-Identifier: AGPL-3.0-or-later
//! Thin shim over the `sovereign_cli_llm` library.
//!
//! Everything that used to live here — the module tree, the runtime setup, the
//! tracing table and the verb table — moved into `src/lib.rs` on 2026-08-21
//! (nc-26) so the crate has a `[lib]` target other crates can link.
//! `sovereign-cli` now serves `svrn awareness` in its own process by calling
//! `sovereign_cli_llm::awareness_cmd::run_awareness`, instead of that module
//! sitting in the dispatcher and reaching across a crate boundary into
//! `enrich_cmd` — an import that had not compiled since 2026-05-22. See the
//! crate docs in `lib.rs` for why.

fn main() {
    sovereign_cli_llm::bin_main()
}
