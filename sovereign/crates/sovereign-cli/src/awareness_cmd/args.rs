//! Flag parsing shared by every `sovereign awareness` subcommand.
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
/// (e.g. `--window 90`) consume the following token. Boolean flags
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
        let (pos, flags) = split_args(&s(&["timeline", "Sarah", "--include-chunks", "--window", "30"]));
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
}
