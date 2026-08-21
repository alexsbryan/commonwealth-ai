// SPDX-License-Identifier: AGPL-3.0-or-later
//! Flag parsing shared by every `svrn atos` subcommand.
//!
//! The CLI sidesteps `clap` here because each subcommand has its own
//! flag shape and a clap-derived parser for the union would be more
//! machinery than payoff. Instead we split `args` into positional
//! arguments and `(name, value)` flag pairs once, then subcommands
//! pluck what they need via [`get_flag`].
//!
//! Boolean flags that don't consume the next token are listed in
//! [`BOOLEAN_FLAGS`]; everything else is treated as a value-taking
//! flag that consumes the following token, or carries its value inline
//! as `--flag=value`.
//!
//! The inline form is not optional garnish: `sovereign_cli_shared::args`
//! accepts it, `tools_cmd::args` accepts it, and AGENTS.md teaches
//! `--key=value` as the form to type. This copy did not accept it until
//! 2026-08-21, and the flags that went missing were silently defaulted
//! (`--driver-model`, `--synth-model`, `--daemon-url`, `--design`).

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
/// (e.g. `--title "foo"`) consume the following token, or carry the value
/// inline as `--title=foo`. Boolean flags listed in [`BOOLEAN_FLAGS`]
/// stand alone and are recorded with an empty value.
pub(super) fn split_args(args: &[String]) -> (Vec<String>, Vec<(String, String)>) {
    let mut positional = Vec::new();
    let mut flags = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if let Some(name_eq_val) = arg.strip_prefix("--") {
            // `--key=value` in one token. The declarative parser this
            // family is migrating to (`sovereign_cli_shared::args::parse`)
            // accepts it, `tools_cmd`'s splitter accepts it, and AGENTS.md
            // teaches `--key=value` as THE form — so an operator or agent
            // types it here too. Without this branch the whole token became
            // the flag NAME ("driver-model=x"), `get_flag("driver-model")`
            // returned None, the caller silently took its default, AND the
            // following token was eaten as the phantom flag's value.
            if let Some((k, v)) = name_eq_val.split_once('=') {
                // A boolean carries no inline value — the canonical parser
                // rejects `--flag=x` outright. Record presence and, crucially,
                // do not consume the next token.
                if BOOLEAN_FLAGS.contains(&k) {
                    flags.push((k.to_string(), String::new()));
                } else {
                    flags.push((k.to_string(), v.to_string()));
                }
                i += 1;
                continue;
            }
            let name = name_eq_val;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    /// `--key=value` must mean what `--key value` means.
    ///
    /// This splitter dropped the `=` form entirely: the whole token became
    /// the flag NAME, so the lookup missed, the caller silently fell back
    /// to its default, and the NEXT token was swallowed as the phantom
    /// flag's value. Proven end-to-end on 2026-08-21 against the shipped
    /// dispatcher — `svrn atos run --workdir=/tmp/x` printed "missing
    /// --workdir <path>" while `--workdir /tmp/x` was honoured. The silent
    /// half is worse: `--driver-model=X` (run.rs:165), `--synth-model=X`
    /// (replay.rs:67), `--daemon-url=X` (run.rs:159) and `--design=<path>`
    /// (run.rs:184) all fall through to a default and exit 0.
    ///
    /// `tools_cmd::args` — the same crate, one directory over — and
    /// `sovereign_cli_shared::args::parse` both accepted `=` the whole
    /// time. Only this copy did not.
    #[test]
    fn equals_form_is_the_same_as_the_space_form() {
        let (pos_eq, flags_eq) = split_args(&s(&["run", "--driver-model=qwen3-30b"]));
        let (pos_sp, flags_sp) = split_args(&s(&["run", "--driver-model", "qwen3-30b"]));
        assert_eq!(pos_eq, pos_sp);
        assert_eq!(flags_eq, flags_sp);
        assert_eq!(
            get_flag(&flags_eq, "--driver-model").as_deref(),
            Some("qwen3-30b")
        );
    }

    /// A value containing `=` survives: only the FIRST `=` splits. URLs and
    /// query strings arrive through `--daemon-url=` and must not be cut.
    #[test]
    fn equals_form_keeps_the_rest_of_the_value() {
        let (_pos, flags) = split_args(&s(&["--daemon-url=http://h:9741/?a=b"]));
        assert_eq!(
            get_flag(&flags, "daemon-url").as_deref(),
            Some("http://h:9741/?a=b")
        );
    }

    /// A boolean given an inline value records presence and — the part that
    /// actually bites — does NOT eat the following token.
    #[test]
    fn equals_form_on_a_boolean_consumes_no_following_token() {
        let (_pos, flags) = split_args(&s(&["--dry-run=yes", "--workdir", "/tmp/x"]));
        assert!(flags.iter().any(|(k, _)| k == "dry-run"));
        assert_eq!(get_flag(&flags, "workdir").as_deref(), Some("/tmp/x"));
    }

    /// The pre-existing space form is untouched.
    #[test]
    fn space_form_and_booleans_still_behave() {
        let (pos, flags) = split_args(&s(&["run", "--dry-run", "--workdir", "/tmp/x"]));
        assert_eq!(pos, vec!["run"]);
        assert!(flags.iter().any(|(k, v)| k == "dry-run" && v.is_empty()));
        assert_eq!(get_flag(&flags, "workdir").as_deref(), Some("/tmp/x"));
    }
}
