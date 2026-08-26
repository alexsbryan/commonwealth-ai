// SPDX-License-Identifier: AGPL-3.0-or-later
//! The flag surface of `svrn awareness`, as data.
//!
//! Until 2026-08-21 this file held a hand-rolled `while i < args.len()`
//! loop — one of five byte-identical copies across the CLI crates, and
//! one of roughly 144 such loops in total. nc-22b converged their
//! BEHAVIOUR (four of the five silently dropped `--key=value`, so
//! `--kind=person` landed as the flag NAME `"kind=person"` and
//! `entities.rs` listed every kind); nc-25 removes the copies. Parsing
//! happens once, in [`sovereign_cli_shared::args::parse`], and this
//! module supplies only what is genuinely local: which flags
//! `awareness` accepts.
//!
//! **This module is NOT gated on `awareness`, and that is deliberate.**
//! A spec is data plus the shared parser — no `sovereign-tools`, no
//! knowledge-view surface, nothing the heavy feature drags in. Gating it
//! with the subcommands that read it meant its tests never ran in any
//! build (`--features awareness` does not compile at all; see
//! `mod.rs`), so the `--key=value` regression it exists to catch was
//! unwatched. Data with no heavy dependency should not inherit a heavy
//! dependency's gate.

#![cfg_attr(not(feature = "awareness"), allow(dead_code))]

use sovereign_cli_shared::args::{parse, ArgError, ArgSpec, Parsed};

/// Every flag any `svrn awareness` subcommand accepts. One union rather
/// than one spec per subcommand, because that is what the old
/// `BOOLEAN_FLAGS` list already was.
///
/// Declaring the VALUE flags — not just the booleans that list carried —
/// is what closes the hole: the splitter treated every UNDECLARED `--x`
/// as value-taking, so a typo silently ate the following token and the
/// command ran on defaults.
pub(super) const SPECS: &[ArgSpec] = &[
    // booleans
    ArgSpec::flag("entities-only"),
    ArgSpec::flag("full"),
    ArgSpec::flag("include-chunks"),
    ArgSpec::flag("json"),
    ArgSpec::flag_short("yes", 'y'),
    // `--y` was accepted by the old splitter as a long flag and is read
    // as one; kept so the spelling does not regress. `-y` is NEW — the
    // splitter never handled single-dash forms at all.
    ArgSpec::flag("y"),
    ArgSpec::flag("verbose"),
    ArgSpec::flag("dry-run"),
    ArgSpec::flag("mock"),
    ArgSpec::flag("show-scores"),
    ArgSpec::flag("show-rejected"),
    ArgSpec::flag("all-turns"),
    ArgSpec::flag("show-entity-linked"),
    ArgSpec::flag("interactive"),
    // Declared by the old boolean list and read nowhere yet. Kept
    // declared so it stays accepted rather than starting to error.
    ArgSpec::flag("use-cached"),
    // value-taking
    ArgSpec::value("budget"),
    ArgSpec::value("context"),
    ArgSpec::value("daemon-url"),
    ArgSpec::value("db-path"),
    ArgSpec::value("from-file"),
    ArgSpec::value("from-template"),
    ArgSpec::value("golden"),
    ArgSpec::value("kind"),
    ArgSpec::value("limit"),
    ArgSpec::value("max-tokens"),
    ArgSpec::value("model"),
    ArgSpec::value("months"),
    ArgSpec::value("output"),
    ArgSpec::value("phase"),
    ArgSpec::value("rate"),
    ArgSpec::value("report"),
    ArgSpec::value("sort"),
    ArgSpec::value("threshold"),
    ArgSpec::value("window"),
];

/// Parse a subcommand's own argument slice against [`SPECS`].
pub(super) fn parse_args(args: &[String]) -> Result<Parsed, ArgError> {
    parse(SPECS, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn split_separates_positional_and_flag_pairs() {
        let p = parse_args(&s(&["timeline", "Sarah Chen", "--window", "90"])).unwrap();
        assert_eq!(
            p.positionals(),
            &["timeline".to_string(), "Sarah Chen".to_string()]
        );
        assert_eq!(p.value("window"), Some("90"));
    }

    #[test]
    fn boolean_flag_does_not_consume_next_token() {
        let p = parse_args(&s(&[
            "timeline",
            "Sarah",
            "--include-chunks",
            "--window",
            "30",
        ]))
        .unwrap();
        assert_eq!(
            p.positionals(),
            &["timeline".to_string(), "Sarah".to_string()]
        );
        assert!(p.has("include-chunks"));
        assert_eq!(p.value("window"), Some("30"));
    }

    #[test]
    fn missing_flag_returns_none() {
        let p = parse_args(&s(&["entities"])).unwrap();
        assert_eq!(p.value("kind"), None);
        assert!(!p.has("json"));
    }

    /// `--key=value` must mean what `--key value` means.
    ///
    /// nc-22b wrote this test and it had NEVER RUN: the module was gated
    /// behind `awareness`, which does not compile, and no gate builds
    /// that feature. It runs now because the spec is no longer gated
    /// with the subcommands that read it — see the module doc.
    ///
    /// The bug it pins: the whole token became the flag NAME, so the
    /// lookup missed, the caller silently fell back to its default, and
    /// the NEXT token was swallowed. `--kind=person` (entities.rs) then
    /// listed every kind and exited 0 — a silent substitution, which is
    /// the one thing a flag must never do.
    #[test]
    fn equals_form_is_the_same_as_the_space_form() {
        let eq = parse_args(&s(&["entities", "--kind=person"])).unwrap();
        let sp = parse_args(&s(&["entities", "--kind", "person"])).unwrap();
        assert_eq!(eq, sp);
        assert_eq!(eq.value("kind"), Some("person"));
    }

    /// A value containing `=` survives: only the FIRST `=` splits.
    #[test]
    fn equals_form_keeps_the_rest_of_the_value() {
        let p = parse_args(&s(&["--kind=a=b=c"])).unwrap();
        assert_eq!(p.value("kind"), Some("a=b=c"));
    }

    /// BEHAVIOUR CHANGE (nc-25). The hand-rolled splitter accepted
    /// `--json=whatever` and recorded bare presence. The canonical parser
    /// refuses it and says so rather than guessing. The half that
    /// mattered is preserved either way: the following token is never
    /// swallowed.
    #[test]
    fn inline_value_on_a_boolean_is_refused_not_guessed() {
        let err = parse_args(&s(&["--json=whatever", "--kind", "person"])).unwrap_err();
        assert_eq!(err.to_string(), "--json does not take a value");
    }

    /// BEHAVIOUR CHANGE (nc-25). An undeclared flag was value-taking, so
    /// a typo silently consumed the next token and the command ran on
    /// defaults. It is now a hard error naming the flag.
    #[test]
    fn an_undeclared_flag_is_refused_instead_of_eating_the_next_token() {
        let err = parse_args(&s(&["entities", "--kidn", "person"])).unwrap_err();
        assert_eq!(err.to_string(), "unknown flag '--kidn'");
    }
}
