// SPDX-License-Identifier: AGPL-3.0-or-later
//! Thin shim over the `sovereign_cli_llm` library.
//!
//! Everything that used to live here — the module tree, the runtime setup, the
//! tracing table and the verb table — moved into `src/lib.rs` on 2026-08-21
//! (nc-26) so the crate has a `[lib]` target other crates can link: the stock
//! distribution's `sovereign-cli-llm-stock` enters through `bin_main_with`.
//! See the crate docs in `lib.rs` for why.

fn main() {
    sovereign_cli_llm::bin_main()
}
