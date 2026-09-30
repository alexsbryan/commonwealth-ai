// SPDX-License-Identifier: AGPL-3.0-or-later
//! svrn speaks serving only through contracts ports and vocabulary
//! (pb-svrn-serving-ports). Serving's part, the admission layers and the
//! terminal's entry-node resolver are svrn's own; the admission wire,
//! `turn_admission` and the OpenAI/FIM rendering are the contracts leaf's.
//! What the daemon still reaches in `sovereign-serving-host` is the router
//! and the OpenAI adapter, which pb-serve-ranks takes.
//!
//! The daemon still links serving-host for those, and serving-host keeps
//! re-exports at the moved modules' old paths for serve, so the compiler would
//! let one of these paths come back. This reads the daemon's sources. Watched
//! red by `use sovereign_serving_host::state` back in the daemon.

use std::path::Path;

use crate::no_engine_census::rust_files;

/// The serving-host modules svrn no longer names, assembled so this file does
/// not match itself.
fn needles() -> Vec<String> {
    [
        "state",
        "admission",
        "turn_admission",
        "entry_endpoint",
        "openai_http",
        "fim_http",
    ]
    .iter()
    .map(|module| ["sovereign_serving_host", "::", module].concat())
    .collect()
}

/// `needle` followed by a path separator, a brace, a `;` or the end of the
/// line — so `...::admission` does not match `...::admission_wire`.
fn names_module(code: &str, needle: &str) -> bool {
    code.match_indices(needle).any(|(at, _)| {
        let rest = &code[at + needle.len()..];
        rest.is_empty() || rest.starts_with("::") || rest.starts_with(';') || rest.starts_with(' ')
    })
}

#[test]
fn the_daemon_names_no_moved_serving_host_module() {
    let krate = Path::new(env!("CARGO_MANIFEST_DIR"));
    let needles = needles();
    let mut found = Vec::new();
    let mut files = Vec::new();
    rust_files(&krate.join("src"), &mut files);
    for file in &files {
        let text = std::fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
        for (n, line) in text.lines().enumerate() {
            if let Some(needle) = needles.iter().find(|needle| names_module(line, needle)) {
                found.push(format!("{}:{}: `{needle}`", file.display(), n + 1));
            }
        }
    }
    assert!(
        files.len() > 50,
        "the census read only {} files — it is not looking",
        files.len()
    );
    assert!(
        found.is_empty(),
        "the daemon names a serving-host module that moved: Serving's part, \
         the admission layers and entry_endpoint are svrn's own \
         (crate::state::serving, crate::admission, crate::build::entry_endpoint); \
         the admission wire, turn_admission and the OpenAI/FIM rendering are \
         sovereign_contracts' (admission_wire, principal, turn_admission, \
         openai_http, fim_http):\n  {}",
        found.join("\n  ")
    );
}
