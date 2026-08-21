// SPDX-License-Identifier: AGPL-3.0-or-later
//! Flag parser for `svrn tools`. Accepts `--k=v` and `--k v` alike.
//!
//! This used to claim it was "the same shape as `crate::atos_cmd::args`".
//! It was not: until 2026-08-21 this was the ONLY splitter in the family
//! with the `=` branch, and `atos_cmd` / `awareness_cmd` / `inner_chaos` /
//! `voice_eval` silently dropped that form. The four have been converged
//! onto this contract. The structural fix is still outstanding — all five
//! should call `sovereign_cli_shared::args::parse`, which has handled
//! `--k=v` since it landed and today has zero adopters.

/// Split `args` into `(positional, flag_pairs)`. Boolean flags (listed
/// below) stand alone; value-taking flags consume the following token
/// OR parse as `--key=value` in a single token.
pub(super) const BOOLEAN_FLAGS: &[&str] = &["help"];

pub(super) fn split_args(args: &[String]) -> (Vec<String>, Vec<(String, String)>) {
    let mut positional = Vec::new();
    let mut flags = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if let Some(name_eq_val) = arg.strip_prefix("--") {
            // Support `--key=value` in one token — useful for tool
            // params whose values contain spaces.
            if let Some((k, v)) = name_eq_val.split_once('=') {
                flags.push((k.to_string(), v.to_string()));
                i += 1;
                continue;
            }
            if BOOLEAN_FLAGS.contains(&name_eq_val) {
                flags.push((name_eq_val.to_string(), String::new()));
                i += 1;
            } else {
                let value = args.get(i + 1).cloned().unwrap_or_default();
                flags.push((name_eq_val.to_string(), value));
                i += 2;
            }
        } else {
            positional.push(arg.clone());
            i += 1;
        }
    }
    (positional, flags)
}

pub(super) fn get_flag(flags: &[(String, String)], name: &str) -> Option<String> {
    let key = name.strip_prefix("--").unwrap_or(name);
    flags.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
}
