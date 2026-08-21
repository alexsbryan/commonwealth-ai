// SPDX-License-Identifier: AGPL-3.0-or-later
//! Flag parsing shared by every `svrn awareness` subcommand.
//!
//! Mirrors `atos_cmd/args.rs` — manual split into positional + flag
//! pairs, then subcommands look up flags by name. Boolean flags that
//! don't consume the next token are listed below.
//!
//! Lifted as a private copy rather than re-exporting `atos_cmd::args`
//! because the boolean-flag set differs (we have `--entities-only`,
//! `--full`, `--include-chunks`, `--show-scores`, etc. that atos
//! doesn't), and `atos_cmd::args` is module-private.

/// Boolean flags that do not consume the next token as their value.
pub(super) const BOOLEAN_FLAGS: &[&str] = &[
    // Phase 1
    "entities-only",
    "full",
    "include-chunks",
    "json",
    "yes",
    "y",
    // Phase 2+ (declared early so the module's flag splitter is
    // forward-compatible — unused booleans don't cost anything).
    "verbose",
    "dry-run",
    "mock",
    "show-scores",
    "show-rejected",
    "all-turns",
    "show-entity-linked",
    "interactive",
    "use-cached",
];

/// Split `args` into `(positional, flag_pairs)`. Value-taking flags
/// (e.g. `--window 90`) consume the following token, or carry the value
/// inline as `--window=90`. Boolean flags listed in [`BOOLEAN_FLAGS`]
/// stand alone and are recorded with an empty value.
///
/// The inline form was missing here until 2026-08-21, which made
/// `--kind=person` land as the flag NAME `"kind=person"` — so
/// `entities.rs:50` saw no `--kind` at all and silently listed every
/// kind. Same for `--sort=` (entities.rs:61), `--phase=` (extract.rs:48)
/// and `--context=` (digest.rs:48).
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

/// Look up a flag's value. Accepts both `"--window"` and `"window"`.
pub(super) fn get_flag(flags: &[(String, String)], name: &str) -> Option<String> {
    let key = name.strip_prefix("--").unwrap_or(name);
    flags.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
}

/// Boolean-flag presence check.
pub(super) fn has_flag(flags: &[(String, String)], name: &str) -> bool {
    let key = name.strip_prefix("--").unwrap_or(name);
    flags.iter().any(|(k, _)| k == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn split_separates_positional_and_flag_pairs() {
        let (pos, flags) = split_args(&s(&["timeline", "Sarah Chen", "--window", "90"]));
        assert_eq!(pos, vec!["timeline", "Sarah Chen"]);
        assert_eq!(flags, vec![("window".into(), "90".into())]);
    }

    #[test]
    fn boolean_flag_does_not_consume_next_token() {
        let (pos, flags) = split_args(&s(&[
            "timeline",
            "Sarah",
            "--include-chunks",
            "--window",
            "30",
        ]));
        assert_eq!(pos, vec!["timeline", "Sarah"]);
        assert!(has_flag(&flags, "include-chunks"));
        assert_eq!(get_flag(&flags, "window"), Some("30".into()));
    }

    #[test]
    fn get_flag_accepts_bare_or_dashed_name() {
        let (_pos, flags) = split_args(&s(&["entities", "--kind", "person"]));
        assert_eq!(get_flag(&flags, "kind"), Some("person".into()));
        assert_eq!(get_flag(&flags, "--kind"), Some("person".into()));
    }

    #[test]
    fn missing_flag_returns_none() {
        let (_pos, flags) = split_args(&s(&["entities"]));
        assert_eq!(get_flag(&flags, "kind"), None);
        assert!(!has_flag(&flags, "json"));
    }

    /// `--key=value` must mean what `--key value` means.
    ///
    /// This half of the splitter fork dropped the `=` form entirely: the
    /// whole token became the flag NAME, so the lookup missed, the caller
    /// silently fell back to its default, and the NEXT token was swallowed
    /// as the phantom flag's value. `svrn atos run --driver-model=X` ran
    /// on DEFAULT_DRIVER_MODEL and exited 0 (atos_cmd/run.rs:165) — a
    /// silent substitution, which is the one thing a flag must never do.
    /// `tools_cmd::args` and `sovereign_cli_shared::args::parse` both
    /// accepted `=` the whole time; only these copies did not.
    #[test]
    fn equals_form_is_the_same_as_the_space_form() {
        let (pos_eq, flags_eq) = split_args(&s(&["entities", "--kind=person"]));
        let (pos_sp, flags_sp) = split_args(&s(&["entities", "--kind", "person"]));
        assert_eq!(pos_eq, pos_sp);
        assert_eq!(flags_eq, flags_sp);
        assert_eq!(get_flag(&flags_eq, "kind").as_deref(), Some("person"));
    }

    /// A value containing `=` survives: only the FIRST `=` splits.
    #[test]
    fn equals_form_keeps_the_rest_of_the_value() {
        let (_pos, flags) = split_args(&s(&["--kind=a=b=c"]));
        assert_eq!(get_flag(&flags, "kind").as_deref(), Some("a=b=c"));
    }

    /// A boolean given an inline value records presence and — the part
    /// that actually bites — does NOT eat the following token.
    #[test]
    fn equals_form_on_a_boolean_consumes_no_following_token() {
        let (_pos, flags) = split_args(&s(&["--json=whatever", "--kind", "person"]));
        assert!(has_flag(&flags, "json"));
        assert_eq!(get_flag(&flags, "kind").as_deref(), Some("person"));
    }
}
