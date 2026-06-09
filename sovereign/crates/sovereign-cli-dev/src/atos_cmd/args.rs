// SPDX-License-Identifier: AGPL-3.0-or-later
//! Flag parsing shared by every `sovereign atos` subcommand.
//!
//! The CLI sidesteps `clap` here because each subcommand has its own
//! flag shape and a clap-derived parser for the union would be more
//! machinery than payoff. Instead we split `args` into positional
//! arguments and `(name, value)` flag pairs once, then subcommands
//! pluck what they need via [`get_flag`].
//!
//! Boolean flags that don't consume the next token are listed in
//! [`BOOLEAN_FLAGS`]; everything else is treated as a value-taking
//! flag that consumes the following token.

use std::path::Path;

/// Boolean flags that do not consume the next token as their value.
pub(super) const BOOLEAN_FLAGS: &[&str] = &[
    "no-driver",
    "reuse-last-milestone",
    "yes",
    "y",
    "red-team",
    "auto",
    "dry-run",
    "fresh-plan",
];

/// Split `args` into `(positional, flag_pairs)`. Value-taking flags
/// (e.g. `--title "foo"`) consume the following token. Boolean flags
/// listed in [`BOOLEAN_FLAGS`] stand alone and are recorded with an
/// empty value.
pub(super) fn split_args(args: &[String]) -> (Vec<String>, Vec<(String, String)>) {
    let mut positional = Vec::new();
    let mut flags = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if let Some(name) = arg.strip_prefix("--") {
            if BOOLEAN_FLAGS.contains(&name) {
                flags.push((name.to_string(), String::new()));
                i += 1;
            } else {
                let value = args.get(i + 1).cloned().unwrap_or_default();
                flags.push((name.to_string(), value));
                i += 2;
            }
        } else {
            positional.push(arg.clone());
            i += 1;
        }
    }
    (positional, flags)
}

/// Look up a flag's value. Accepts both `"--title"` and `"title"` for
/// the `name` argument so callers can stay readable.
pub(super) fn get_flag(flags: &[(String, String)], name: &str) -> Option<String> {
    let key = name.strip_prefix("--").unwrap_or(name);
    flags.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
}

// Path helper kept for the stop-condition runner's future use (emit a
// diagnostic when the feature's stop cmd references a path that does
// not resolve relative to CWD).
#[allow(dead_code)]
pub(super) fn exists_on_path(p: &str) -> bool {
    Path::new(p).exists()
}
