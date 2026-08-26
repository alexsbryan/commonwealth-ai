// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared declarative argument parser — the missing half of
//! help-as-data. `help.rs` models a command's help *as data*; this
//! models its *arguments* as data, so a command declares one
//! `&'static [ArgSpec]` (built alongside its `Help`) and gets parsing
//! for free instead of hand-rolling yet another `while i < args.len()`
//! loop. Roughly ~144 of those bespoke loops exist across the CLI crates
//! today; each is a place where the parser and the advertised help can
//! silently disagree.
//!
//! The CLI is deliberately, uniformly zero-clap (no proc-macro build
//! tax, no 50-command rewrite, no reversing that documented decision),
//! so this is a small hand-written parser, not a clap wrapper. It
//! handles exactly the surface the existing loops use:
//!
//! - `--flag` / `-f`            boolean presence flags (+ short alias)
//! - `--opt value` / `--opt=v`  flags that take a value (+ short alias)
//! - `-h` / `--help` / `help`   recognised everywhere → `Parsed::wants_help`
//! - anything else without a leading `-` is a positional
//! - an unknown `--flag` / `-f`, or a value flag with no value, is a
//!   hard error (matches the loops' `return Err("unknown flag …")`)
//!
//! **Adoption (nc-25, 2026-08-21): 33 call sites across four command
//! families** — `atos_cmd` (18), `awareness_cmd` (13), `inner_chaos`,
//! `voice_eval` — replacing five byte-identical hand-rolled splitters.
//! Before that this parser had ZERO adopters for its whole life, and the
//! bug it was written to prevent shipped anyway: four of the five
//! splitters dropped `--key=value` (`ae0ec58c`), and every one of them
//! treated an UNDECLARED `--x` as value-taking, so a typo ate the
//! following token and the command ran on defaults. `atos run --accept
//! --workdir /tmp/x` lost `--workdir` outright.
//!
//! Pair each `SPECS` with a §7.2 consistency test — see
//! [`advertised_flags`] — so help can't advertise a flag the spec omits.
//!
//! **Where it does NOT fit, and why.** `tools_cmd` (`svrn tools call
//! <id> --key=value …`) forwards arbitrary flags as a tool's JSON
//! params. Its flag set is the union of every registered tool's
//! parameter schema, discovered at runtime from the `ToolRegistry`
//! manifest. `ArgSpec` is a CLOSED set by construction and expressing
//! that would mean "accept anything", which destroys the guarantee that
//! makes adopting it worth doing. Closed sets are enums, open sets are
//! registries (§2, §4): `tools call` is the registry side and keeps its
//! own splitter, deliberately.

use std::collections::{BTreeSet, HashMap, HashSet};

/// One argument a command accepts. Construct as a `&'static [ArgSpec]`
/// next to the command's `Help` so the flag list has a single source of
/// truth. `long` is the dash-free long form (`"data-dir"` for
/// `--data-dir`); `short` is the optional one-char alias (`'y'` for
/// `-y`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArgSpec {
    pub long: &'static str,
    pub short: Option<char>,
    pub takes_value: bool,
}

impl ArgSpec {
    /// A boolean presence flag: `--<long>`.
    pub const fn flag(long: &'static str) -> ArgSpec {
        ArgSpec {
            long,
            short: None,
            takes_value: false,
        }
    }

    /// A boolean presence flag with a short alias: `--<long>` / `-<short>`.
    pub const fn flag_short(long: &'static str, short: char) -> ArgSpec {
        ArgSpec {
            long,
            short: Some(short),
            takes_value: false,
        }
    }

    /// A flag that takes a value: `--<long> <v>` or `--<long>=<v>`.
    pub const fn value(long: &'static str) -> ArgSpec {
        ArgSpec {
            long,
            short: None,
            takes_value: true,
        }
    }

    /// A value flag with a short alias.
    pub const fn value_short(long: &'static str, short: char) -> ArgSpec {
        ArgSpec {
            long,
            short: Some(short),
            takes_value: true,
        }
    }
}

/// Why parsing stopped. Rendered with `Display` so a command can
/// `eprintln!("error: {e}")` then print its usage, exactly as the
/// hand-rolled loops do today.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgError {
    /// A `--flag` / `-f` not present in the spec.
    UnknownFlag(String),
    /// A value flag (`--opt`) given with no following value.
    MissingValue(String),
    /// A boolean flag given an inline value (`--flag=x`).
    UnexpectedValue(String),
}

impl std::fmt::Display for ArgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArgError::UnknownFlag(flag) => write!(f, "unknown flag '{flag}'"),
            ArgError::MissingValue(flag) => write!(f, "{flag} needs a value"),
            ArgError::UnexpectedValue(flag) => {
                write!(f, "{flag} does not take a value")
            }
        }
    }
}

impl std::error::Error for ArgError {}

/// The result of a successful `parse`. Query it with `has` / `value` /
/// `positionals`; `wants_help` is true when `-h`/`--help`/`help` appeared
/// anywhere (commands typically check it first and print usage).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    flags: HashSet<String>,
    values: HashMap<String, String>,
    positionals: Vec<String>,
    help: bool,
}

impl Parsed {
    /// Was the boolean flag `<long>` present?
    pub fn has(&self, long: &str) -> bool {
        self.flags.contains(long)
    }

    /// The value supplied for `--<long>`, if any. Last write wins when a
    /// flag is repeated.
    pub fn value(&self, long: &str) -> Option<&str> {
        self.values.get(long).map(|s| s.as_str())
    }

    /// Positional (non-flag) arguments, in order.
    pub fn positionals(&self) -> &[String] {
        &self.positionals
    }

    /// True when help was requested (`-h` / `--help` / `help`).
    pub fn wants_help(&self) -> bool {
        self.help
    }
}

/// Parse `args` against `specs`. `args` is the command's own argument
/// slice (the dispatcher has already stripped the verb). Help tokens are
/// recognised regardless of position; an unknown flag or a value flag
/// missing its value is a hard error.
pub fn parse(specs: &[ArgSpec], args: &[String]) -> Result<Parsed, ArgError> {
    let mut parsed = Parsed::default();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();

        if arg == "-h" || arg == "--help" || arg == "help" {
            parsed.help = true;
            i += 1;
            continue;
        }

        if let Some(rest) = arg.strip_prefix("--") {
            // `--opt=value` (inline) or `--opt` (value follows / boolean).
            let (name, inline) = match rest.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (rest, None),
            };
            let spec = specs
                .iter()
                .find(|s| s.long == name)
                .ok_or_else(|| ArgError::UnknownFlag(format!("--{name}")))?;
            if spec.takes_value {
                let val = match inline {
                    Some(v) => v,
                    None => {
                        i += 1;
                        args.get(i)
                            .cloned()
                            .ok_or_else(|| ArgError::MissingValue(format!("--{name}")))?
                    }
                };
                parsed.values.insert(spec.long.to_string(), val);
            } else if inline.is_some() {
                return Err(ArgError::UnexpectedValue(format!("--{name}")));
            } else {
                parsed.flags.insert(spec.long.to_string());
            }
        } else if arg.len() == 2 && arg.starts_with('-') && arg != "--" {
            // `-y` short form (single char after the dash).
            let c = arg.chars().nth(1).unwrap();
            let spec = specs
                .iter()
                .find(|s| s.short == Some(c))
                .ok_or_else(|| ArgError::UnknownFlag(format!("-{c}")))?;
            if spec.takes_value {
                i += 1;
                let val = args
                    .get(i)
                    .cloned()
                    .ok_or_else(|| ArgError::MissingValue(format!("-{c}")))?;
                parsed.values.insert(spec.long.to_string(), val);
            } else {
                parsed.flags.insert(spec.long.to_string());
            }
        } else {
            parsed.positionals.push(arg.to_string());
        }

        i += 1;
    }
    Ok(parsed)
}

/// The long flags a command's help text advertises, for the §7.2 pin
/// that keeps [`parse`]'s spec and the help from drifting apart:
/// `assert_eq!(advertised_flags(HELP), SPECS.iter().map(|s| s.long)…)`.
///
/// One decider for "what does this help promise". Every adopter needs
/// the same answer, and the same three prose traps: a usage line writes
/// alternatives as `--minutes/--threads`, a body paragraph writes a
/// family as `--*-model`, and every entry is followed by punctuation.
/// So: split on whitespace AND `/`, strip surrounding punctuation, keep
/// only `--name` where `name` is alphanumeric-plus-dash.
pub fn advertised_flags(help: &str) -> BTreeSet<String> {
    help.split_whitespace()
        .flat_map(|t| t.split('/'))
        .map(|t| t.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-'))
        .filter_map(|t| t.strip_prefix("--"))
        .filter(|t| !t.is_empty() && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    // Mirrors a real command's spec (setup_cmd's parse_args): a few
    // booleans with short aliases + one value flag.
    const SPECS: &[ArgSpec] = &[
        ArgSpec::flag("reset"),
        ArgSpec::flag_short("yes", 'y'),
        ArgSpec::flag("repair"),
        ArgSpec::value("data-dir"),
    ];

    #[test]
    fn presence_flags_and_short_aliases() {
        let p = parse(SPECS, &argv(&["--reset", "-y"])).unwrap();
        assert!(p.has("reset"));
        assert!(p.has("yes")); // resolved via the short alias
        assert!(!p.has("repair"));
        assert!(!p.wants_help());
    }

    #[test]
    fn value_flag_space_and_equals_forms_are_equivalent() {
        let space = parse(SPECS, &argv(&["--data-dir", "/tmp/sov"])).unwrap();
        let equals = parse(SPECS, &argv(&["--data-dir=/tmp/sov"])).unwrap();
        assert_eq!(space.value("data-dir"), Some("/tmp/sov"));
        assert_eq!(equals.value("data-dir"), Some("/tmp/sov"));
        assert_eq!(space, equals);
    }

    #[test]
    fn help_is_recognised_in_any_form_and_position() {
        for form in [["--help"], ["-h"], ["help"]] {
            assert!(parse(SPECS, &argv(&form)).unwrap().wants_help());
        }
        let p = parse(SPECS, &argv(&["--reset", "help"])).unwrap();
        assert!(p.wants_help() && p.has("reset"));
    }

    #[test]
    fn positionals_are_collected_in_order() {
        let p = parse(SPECS, &argv(&["alpha", "--yes", "beta"])).unwrap();
        assert_eq!(p.positionals(), &["alpha".to_string(), "beta".to_string()]);
        assert!(p.has("yes"));
    }

    #[test]
    fn unknown_flag_is_an_error() {
        assert_eq!(
            parse(SPECS, &argv(&["--bogus"])),
            Err(ArgError::UnknownFlag("--bogus".to_string()))
        );
        assert_eq!(
            parse(SPECS, &argv(&["-z"])),
            Err(ArgError::UnknownFlag("-z".to_string()))
        );
    }

    #[test]
    fn value_flag_without_a_value_is_an_error() {
        assert_eq!(
            parse(SPECS, &argv(&["--data-dir"])),
            Err(ArgError::MissingValue("--data-dir".to_string()))
        );
    }

    #[test]
    fn boolean_flag_with_inline_value_is_an_error() {
        assert_eq!(
            parse(SPECS, &argv(&["--reset=1"])),
            Err(ArgError::UnexpectedValue("--reset".to_string()))
        );
    }

    #[test]
    fn repeated_value_flag_takes_the_last() {
        let p = parse(SPECS, &argv(&["--data-dir=/a", "--data-dir=/b"])).unwrap();
        assert_eq!(p.value("data-dir"), Some("/b"));
    }

    #[test]
    fn error_display_matches_the_legacy_loop_phrasing() {
        assert_eq!(
            ArgError::UnknownFlag("--bogus".to_string()).to_string(),
            "unknown flag '--bogus'"
        );
        assert_eq!(
            ArgError::MissingValue("--data-dir".to_string()).to_string(),
            "--data-dir needs a value"
        );
    }

    #[test]
    fn advertised_flags_reads_a_help_block() {
        let help = "USAGE\n  cmd [--minutes N | --threads N]\n\nFLAGS\n                      --minutes <N>   Budget.\n  --no-judge      No number.\n";
        let got = advertised_flags(help);
        assert_eq!(
            got,
            ["minutes", "threads", "no-judge"]
                .iter()
                .map(|s| s.to_string())
                .collect::<BTreeSet<_>>()
        );
    }

    /// The three prose traps, each of which produced a false flag name
    /// when this extractor was written inline in a command module.
    #[test]
    fn advertised_flags_ignores_prose_artifacts() {
        let help = "Reuses --minutes/--threads and --temperature/--daemon/--*-model.\n\
                    Trailing punctuation: --report, --diff.";
        let got = advertised_flags(help);
        assert_eq!(
            got,
            [
                "minutes",
                "threads",
                "temperature",
                "daemon",
                "report",
                "diff"
            ]
            .iter()
            .map(|s| s.to_string())
            .collect::<BTreeSet<_>>()
        );
    }
}
