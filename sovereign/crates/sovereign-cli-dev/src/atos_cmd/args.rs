// SPDX-License-Identifier: AGPL-3.0-or-later
//! The flag surface of `svrn atos`, as data.
//!
//! Until 2026-08-21 this file held a hand-rolled `while i < args.len()`
//! loop — one of five byte-identical copies across the CLI crates, and
//! one of roughly 144 such loops in total. nc-22b converged their
//! BEHAVIOUR (four of the five silently dropped `--key=value`); nc-25
//! removes the copies. Parsing now happens exactly once, in
//! [`sovereign_cli_shared::args::parse`], and this module supplies only
//! the thing that is genuinely local: which flags `atos` accepts.
//!
//! Declaring the VALUE flags — not just the booleans the old
//! `BOOLEAN_FLAGS` list carried — is the part that closes the hole. The
//! splitter treated every UNDECLARED `--x` as value-taking, so it ate
//! the following token and the command ran on defaults. `--accept`
//! (read at `run.rs`) was never in the boolean list, so
//! `atos run --accept --workdir /tmp/x` lost `--workdir` outright.

use sovereign_cli_shared::args::{parse, ArgError, ArgSpec, Parsed};

/// Every flag any `svrn atos` subcommand accepts. One union rather than
/// one spec per subcommand, because that is what the old `BOOLEAN_FLAGS`
/// list already was — narrowing it per subcommand is a separate,
/// behaviour-visible change (a flag meant for `run` would start erroring
/// under `status`), not part of removing the copies.
pub(super) const SPECS: &[ArgSpec] = &[
    // booleans
    ArgSpec::flag("no-driver"),
    ArgSpec::flag("reuse-last-milestone"),
    ArgSpec::flag_short("yes", 'y'),
    // `--y` was accepted by the old splitter as a long flag and is read
    // as one at `milestone.rs`; kept so the spelling does not regress.
    // `-y` is NEW — the splitter never handled single-dash forms at all.
    ArgSpec::flag("y"),
    ArgSpec::flag("red-team"),
    ArgSpec::flag("auto"),
    ArgSpec::flag("dry-run"),
    ArgSpec::flag("fresh-plan"),
    // Read at `run.rs` but absent from the old boolean list, so it ate
    // the next token. See the module doc.
    ArgSpec::flag("accept"),
    // value-taking
    ArgSpec::value("branch-name"),
    ArgSpec::value("brief"),
    ArgSpec::value("charter"),
    ArgSpec::value("commit"),
    ArgSpec::value("content"),
    ArgSpec::value("daemon-url"),
    ArgSpec::value("design"),
    ArgSpec::value("done-marker"),
    ArgSpec::value("driver"),
    ArgSpec::value("driver-model"),
    ArgSpec::value("drivers"),
    ArgSpec::value("feature-id"),
    ArgSpec::value("max-iters"),
    ArgSpec::value("milestone"),
    ArgSpec::value("milestone-id"),
    ArgSpec::value("ordinal"),
    ArgSpec::value("out"),
    ArgSpec::value("plan"),
    ArgSpec::value("reason"),
    ArgSpec::value("reviewer-model"),
    ArgSpec::value("section"),
    ArgSpec::value("synth-model"),
    ArgSpec::value("to"),
    ArgSpec::value("url"),
    ArgSpec::value("workdir"),
];

/// Parse a subcommand's own argument slice against [`SPECS`].
pub(super) fn parse_args(args: &[String]) -> Result<Parsed, ArgError> {
    parse(SPECS, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    /// `--key=value` must mean what `--key value` means.
    ///
    /// This splitter dropped the `=` form entirely until 2026-08-21: the
    /// whole token became the flag NAME, so the lookup missed, the caller
    /// silently fell back to its default, and the NEXT token was
    /// swallowed. Proven end-to-end against the shipped dispatcher —
    /// `svrn atos run --workdir=/tmp/x` printed "missing --workdir
    /// <path>". The silent half was worse: `--driver-model=X`,
    /// `--synth-model=X`, `--daemon-url=X` and `--design=<path>` all fell
    /// through to a default and exited 0.
    #[test]
    fn equals_form_is_the_same_as_the_space_form() {
        let eq = parse_args(&s(&["run", "--driver-model=qwen3-30b"])).unwrap();
        let sp = parse_args(&s(&["run", "--driver-model", "qwen3-30b"])).unwrap();
        assert_eq!(eq, sp);
        assert_eq!(eq.value("driver-model"), Some("qwen3-30b"));
        assert_eq!(eq.positionals(), &["run".to_string()]);
    }

    /// A value containing `=` survives: only the FIRST `=` splits. URLs
    /// and query strings arrive through `--daemon-url=` and must not be
    /// cut.
    #[test]
    fn equals_form_keeps_the_rest_of_the_value() {
        let p = parse_args(&s(&["--daemon-url=http://h:9741/?a=b"])).unwrap();
        assert_eq!(p.value("daemon-url"), Some("http://h:9741/?a=b"));
    }

    /// BEHAVIOUR CHANGE (nc-25). The hand-rolled splitter accepted
    /// `--dry-run=yes` and recorded bare presence. The canonical parser
    /// refuses it and says so rather than guessing what was meant. The
    /// half that mattered is preserved either way: the following token
    /// is never swallowed.
    #[test]
    fn inline_value_on_a_boolean_is_refused_not_guessed() {
        let err = parse_args(&s(&["--dry-run=yes", "--workdir", "/tmp/x"])).unwrap_err();
        assert_eq!(err.to_string(), "--dry-run does not take a value");
    }

    /// The pre-existing space form is untouched.
    #[test]
    fn space_form_and_booleans_still_behave() {
        let p = parse_args(&s(&["run", "--dry-run", "--workdir", "/tmp/x"])).unwrap();
        assert_eq!(p.positionals(), &["run".to_string()]);
        assert!(p.has("dry-run"));
        assert_eq!(p.value("workdir"), Some("/tmp/x"));
    }

    /// THE BUG THE SPEC EXISTS TO CLOSE. `--accept` is read by
    /// `RunCfg::from_args` but was never in the old `BOOLEAN_FLAGS`
    /// list, so the splitter treated it as value-taking and ate
    /// `--workdir` — `atos run` then failed with "missing --workdir"
    /// while the operator was looking straight at it on the command
    /// line. Declared as a boolean, both flags survive.
    #[test]
    fn accept_is_a_boolean_and_no_longer_eats_the_next_flag() {
        let p = parse_args(&s(&["run", "--accept", "--workdir", "/tmp/x"])).unwrap();
        assert!(p.has("accept"));
        assert_eq!(p.value("workdir"), Some("/tmp/x"));
    }

    /// BEHAVIOUR CHANGE (nc-25). An undeclared flag was value-taking, so
    /// a typo silently consumed the next token and the run continued on
    /// defaults. It is now a hard error naming the flag.
    #[test]
    fn an_undeclared_flag_is_refused_instead_of_eating_the_next_token() {
        let err = parse_args(&s(&["run", "--wrkdir", "/tmp/x"])).unwrap_err();
        assert_eq!(err.to_string(), "unknown flag '--wrkdir'");
    }

    /// `--help` is recognised wherever it appears. The old splitter sent
    /// it to the FLAG list while `RunCfg::from_args` looked for it among
    /// the POSITIONALS, so `atos run --help` printed no help at all —
    /// only the bare `help` token worked.
    #[test]
    fn help_is_recognised_in_flag_position() {
        for form in [["run", "--help"], ["run", "-h"], ["run", "help"]] {
            assert!(parse_args(&s(&form)).unwrap().wants_help(), "{form:?}");
        }
    }
}
