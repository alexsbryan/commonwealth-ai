// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign-serve` — see the library's docs.

fn main() {
    // Rebrand back-compat, as every sibling runs it: a directly launched
    // serve honours SVRNMESH_* and warns on a removed var too.
    sovereign_contracts::rebrand::promote_legacy_env();
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(sovereign_serve::run(&args));
}
