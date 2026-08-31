// SPDX-License-Identifier: AGPL-3.0-or-later
//! Whether the flags the manifest promises are flags the binaries accept.
//!
//! `cli_contract_code` reconciles VERBS against the binaries in both
//! directions. Flags had no such check: `cli_contract_docs` only asks
//! whether a declared flag appears as a substring somewhere in
//! `CLI_REFERENCE.md`. A flag can be renamed in the parser, or never
//! have existed, and both the manifest and the reference stay green —
//! because the reference is where the promise is written, not where it
//! is kept.
//!
//! Measured when this module was written: six declared flags were
//! parsed by nothing at all.
//!
//!   atos run-ab --drivers   the parser builds `--driver` (ab.rs:317)
//!   atos report --section   cmd_report reads positionals only
//!   atos report --out       likewise
//!   atos teardown --auto    the shared parser knows only --dry-run
//!   recipe test --offline   belongs to cmd_validate / cmd_list
//!   recipe test --verbose   never existed
//!
//! Every one was documented in `CLI_REFERENCE.md`, so a user could read
//! the reference, type the flag, and watch it be silently dropped —
//! these parsers ignore what they do not recognise rather than
//! rejecting it. That is the end-user shape of this failure: exit 0,
//! no message, and the flag did nothing.
//!
//! Two tiers, because the evidence available is not uniform:
//!
//!   HELP    the command's own `--help` names the flag. Exact and
//!           per-command; follows forwarding for free, since a verb
//!           that execs a sibling prints that sibling's help.
//!   SOURCE  a `"--flag"` literal exists somewhere in the four CLI
//!           crates. Weak — it says the token is parsed SOMEWHERE, not
//!           that it is wired to this command — but it is the only
//!           evidence available for verbs that cannot be help-probed.
//!
//! [`no_promised_flag_is_parsed_by_nothing`] is the hard gate and takes
//! either tier. [`flags_missing_from_their_own_help_do_not_grow`] is a
//! shrink-only ratchet over the weaker tier, so the gap can be paid
//! down but never widened.

use sovereign_cli_shared::cli_contract::{Feature, Probe};

use crate::cli_contract_code::{
    combined, contract, help_argv, refs, run, siblings_built, verb_of, UNPROBED_VERBS,
};

/// Is the cargo feature this command lives behind compiled into the
/// binary under test?
///
/// Without this the survey measures the build, not the contract. Under
/// `--features dev-tools` alone, `project init` (feature =
/// "code-intel") is not compiled in, so `project init --help` answers
/// "Unknown project subcommand: init" and all six of its flags look
/// undocumented. That is a true statement about a binary nobody ships
/// and a false one about the promise — ARCH §18.4, validate the
/// instrument before the result.
fn feature_compiled_in(feature: Feature) -> bool {
    match feature {
        Feature::Default => true,
        Feature::DevTools => cfg!(feature = "dev-tools"),
        Feature::Awareness => cfg!(feature = "awareness"),
        Feature::CodeIntel => cfg!(feature = "code-intel"),
    }
}

/// Flags that are declared but absent from their command's own `--help`.
///
/// Every one is a discoverability defect: the flag works, but the only
/// place a user is told about it is a markdown file they may never
/// open. Recorded as a baseline rather than fixed in one pass because
/// each needs its own help text written by someone who knows what the
/// flag does.
///
/// SHRINK-ONLY. Lower it when you fix one; never raise it. To
/// re-measure, run this test — the failure message prints the list.
const HELP_GAP_BASELINE: usize = 11;

/// Concatenated sources of the four CLI crates, for the SOURCE tier.
///
/// All four, not just the declaring command's own binary: `drift
/// accept` forwards to the `atos-spec-accept` sibling via
/// `dev_bin::exec`, so its `--reason` is parsed in a crate the manifest
/// does not name. Scoping this per-binary reported that as missing —
/// a false positive that would have taught the next reader to distrust
/// the gate.
fn cli_crate_sources() -> String {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("sovereign-cli has a parent crates dir");
    let mut all = String::new();
    for crate_name in [
        "sovereign-cli",
        "sovereign-cli-dev",
        "sovereign-cli-llm",
        "sovereign-cli-daemon",
    ] {
        let src = root.join(crate_name).join("src");
        let mut stack = vec![src];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in entries.filter_map(|e| e.ok()) {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    if let Ok(t) = std::fs::read_to_string(&p) {
                        all.push_str(&t);
                        all.push('\n');
                    }
                }
            }
        }
    }
    assert!(
        all.len() > 100_000,
        "read only {} bytes of CLI source; the SOURCE tier would pass everything \
         vacuously (ARCH §18.1)",
        all.len()
    );
    all
}

/// Can this command be safely probed with `--help` offline?
///
/// Mirrors `cli_contract_code`'s Direction-1 skip set rather than
/// restating it: `Probe::Skip` opts out explicitly, and `atos`
/// subcommands route into handlers that mutate, which is why they are
/// in `UNPROBED_VERBS` there.
fn help_probeable(path: &str, probe: Probe) -> bool {
    probe != Probe::Skip && !UNPROBED_VERBS.contains(&verb_of(path))
}

/// A declared flag, and whether anything backs it.
struct FlagEvidence {
    path: String,
    flag: String,
    in_help: bool,
    in_source: bool,
}

fn survey() -> Vec<FlagEvidence> {
    let c = contract();
    let sources = cli_crate_sources();
    let mut out = Vec::new();

    for cmd in c.canonical() {
        if cmd.flags.is_empty() || !feature_compiled_in(cmd.feature) {
            continue;
        }
        let help = if help_probeable(&cmd.path, cmd.probe) {
            let argv = help_argv(&cmd.path);
            Some(combined(&run(&refs(&argv))))
        } else {
            None
        };
        for f in &cmd.flags {
            out.push(FlagEvidence {
                path: cmd.path.clone(),
                flag: f.name.clone(),
                in_help: help.as_deref().is_some_and(|h| h.contains(&f.name)),
                // The parser spelling: `"--flag"` as a literal. `=`-joined
                // forms (`--flag=value`) are caught by the same prefix.
                in_source: sources.contains(&format!("\"{}", f.name)),
            });
        }
    }

    assert!(
        out.len() >= 60,
        "surveyed only {} declared flags; the manifest carries ~150 across all feature \
         gates, so something filtered the corpus away and every assertion below is \
         vacuous (ARCH §18.1)",
        out.len()
    );
    out
}

/// THE gate. A flag the manifest promises must be evidenced by
/// something the binary actually contains — its own help text, or a
/// parser literal. Neither means the promise cannot be kept.
#[test]
fn no_promised_flag_is_parsed_by_nothing() {
    if !siblings_built() {
        eprintln!("skip: sibling CLI bins not built — run `cargo build --bins`");
        return;
    }
    let phantom: Vec<String> = survey()
        .into_iter()
        .filter(|e| !e.in_help && !e.in_source)
        .map(|e| format!("svrn {} {}", e.path, e.flag))
        .collect();

    assert!(
        phantom.is_empty(),
        "the manifest promises {} flag(s) that no CLI binary parses or documents:\n  {}\n\
         These parsers ignore unrecognised flags, so a user who types one gets exit 0 and \
         no effect. Either wire the flag up, or stop promising it in cli-contract.toml and \
         CLI_REFERENCE.md.",
        phantom.len(),
        phantom.join("\n  ")
    );
}

/// The ratchet. Flags that exist but are missing from their own
/// `--help` are undiscoverable; the count may fall, never rise.
#[test]
fn flags_missing_from_their_own_help_do_not_grow() {
    if !siblings_built() {
        eprintln!("skip: sibling CLI bins not built — run `cargo build --bins`");
        return;
    }
    let c = contract();
    let probeable: Vec<String> = survey()
        .into_iter()
        .filter(|e| {
            !e.in_help
                && c.canonical().find(|cmd| cmd.path == e.path).is_some_and(|cmd| {
                    help_probeable(&cmd.path, cmd.probe) && feature_compiled_in(cmd.feature)
                })
        })
        .map(|e| format!("svrn {} {}", e.path, e.flag))
        .collect();

    assert!(
        probeable.len() <= HELP_GAP_BASELINE,
        "{} declared flags are missing from their command's own --help, above the \
         baseline of {HELP_GAP_BASELINE}. A new flag must be in the help text a user \
         actually reads, not only in CLI_REFERENCE.md:\n  {}",
        probeable.len(),
        probeable.join("\n  ")
    );

    // Shrink-only: when the gap is paid down, the baseline follows it
    // down in the same commit, or the ratchet stops ratcheting.
    assert!(
        probeable.len() >= HELP_GAP_BASELINE,
        "the help gap is down to {} from a baseline of {HELP_GAP_BASELINE} — good. \
         Lower HELP_GAP_BASELINE to {} in this commit so it cannot drift back up.",
        probeable.len(),
        probeable.len()
    );
}

// ── falsifiers ────────────────────────────────────────────────────

/// The hard gate must be able to see a flag nothing backs. Without
/// this, a survey that silently returned no evidence rows — a moved
/// source tree, a renamed crate — would read as "no phantom flags".
#[test]
fn the_gate_reports_a_flag_that_nothing_parses() {
    let sources = cli_crate_sources();
    let fabricated = "--definitely-not-a-real-sovereign-flag";
    assert!(
        !sources.contains(&format!("\"{fabricated}")),
        "the SOURCE tier claims to find a flag that does not exist, so it would \
         evidence anything"
    );
    // And the tier is not blind in the other direction: a flag that IS
    // parsed is found.
    assert!(
        sources.contains("\"--help"),
        "the SOURCE tier cannot find `--help`, so it is reading the wrong tree and \
         would report every flag as phantom"
    );
}

/// The help tier must distinguish a flag the help names from one it
/// does not — on real output from a real command, not a fixture.
#[test]
fn the_help_tier_distinguishes_named_from_unnamed_flags() {
    if !siblings_built() {
        eprintln!("skip: sibling CLI bins not built — run `cargo build --bins`");
        return;
    }
    let help = combined(&run(&["--help"]));
    assert!(
        !help.is_empty(),
        "`svrn --help` produced nothing; the help tier has no signal to read"
    );
    assert!(
        !help.contains("--definitely-not-a-real-sovereign-flag"),
        "the help tier matches a flag that is not there"
    );
}
