// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign-serve` — see the library's docs.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(sovereign_serve::run(&args));
}
