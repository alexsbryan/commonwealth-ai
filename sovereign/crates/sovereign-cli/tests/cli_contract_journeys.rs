// SPDX-License-Identifier: AGPL-3.0-or-later
//! Journey-conformance: the *sequenced* half of the CLI contract.
//!
//! `cli_contract_code` proves every verb EXISTS. `cli_contract_docs` proves
//! every verb is DOCUMENTED. Neither can prove that any ordered sequence of
//! them WORKS — before this file, `corpus install` → query → `corpus remove`
//! was unverified anywhere in the repo, and the only behavioural probes were
//! four read-only commands asserting `exit == 0` that nothing ever ran.
//!
//! Pure file I/O — no binary spawn, no daemon, no feature gate — so it runs
//! in every build and CI lane at effectively zero cost. The dispatch half is
//! `cli_journey_dispatch`; the behavioural half is `cli-journey-verify.sh`.
//!
//! The checks, in rising order of what they buy:
//!
//!  - **well-formed** — unique kebab ids, tier in 1..=5, non-empty steps.
//!  - **bindable** — every step resolves to a declared command. An
//!    `Unresolved` step means a journey drives something that does not
//!    exist; this is the check that catches a doc teaching a dead verb
//!    (`ATOS.md` prescribes `sovereign read-notes`, which exits 1 exactly
//!    like a made-up verb). A `VerbOnly` step is allowed but must carry a
//!    `note` — it is the to-do list of subcommands still to declare.
//!  - **cited** — every journey's `doc` path exists on disk.
//!  - **coherent** — a `public` journey may not contain a `dev-tools` step
//!    (it would exit 2 in the shipped binary), mirroring the command-level
//!    invariant in `cli_contract_code`.
//!  - **the ratchet** — every public command's verb belongs to a journey,
//!    every canonical verb is journeyed or explicitly stranded, and the
//!    stranded ledger may shrink but never grow. This is what stops the
//!    CLI drifting back into a bag of verbs.
//!  - **observable** — a journey containing a mutating step must assert
//!    something about OUTPUT somewhere, not just exit codes. `code search`
//!    is a Phase-2 placeholder that prints its own stub text and exits 0;
//!    an exit-code-only gate reads that as working.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use sovereign_cli_shared::cli_contract::{
    Contract, Disposition, Feature, Journey, StepBinding, Visibility,
};

/// The stranded ledger is a debt register. It may SHRINK as verbs are
/// promoted into journeys, folded into a parent, or demoted to hidden — it
/// may never grow. A new verb joins a journey or it does not ship.
///
/// Lower this number when you retire an entry. Do not raise it.
const MAX_STRANDED: usize = 12;

fn contract() -> Contract {
    Contract::load_default().expect("docs/cli-contract.toml must parse")
}

/// Repo root: the manifest lives at `<root>/sovereign/docs/cli-contract.toml`.
fn repo_root() -> PathBuf {
    sovereign_cli_shared::cli_contract::manifest_path()
        .ancestors()
        .nth(3)
        .map(PathBuf::from)
        .expect("manifest path has a repo root three ancestors up")
}

/// Strip a `#anchor` or `:line` suffix from a doc citation.
fn doc_file(cite: &str) -> &str {
    cite.split(['#', ':']).next().unwrap_or(cite)
}

fn verb_of(path: &str) -> &str {
    path.split_whitespace().next().unwrap_or("")
}

/// Every top-level verb a journey drives.
fn journey_verbs(c: &Contract) -> BTreeSet<String> {
    c.verbs_in_journeys()
}

fn stranded_verbs(c: &Contract) -> BTreeSet<String> {
    c.stranded.iter().map(|s| s.verb.clone()).collect()
}

/// Does any step of this journey assert something about output?
fn asserts_output(j: &Journey) -> bool {
    j.steps
        .iter()
        .filter_map(|s| s.expect.as_ref())
        .any(|e| e.inspects_output())
}

// ── well-formed ─────────────────────────────────────────────────────────

#[test]
fn journeys_are_well_formed() {
    let c = contract();
    assert!(!c.journeys.is_empty(), "manifest declares no journeys");

    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    let mut fails = Vec::new();
    for j in &c.journeys {
        *seen.entry(j.id.as_str()).or_default() += 1;
        if j.id.is_empty() || !j.id.chars().all(|ch| ch.is_ascii_lowercase() || ch == '-') {
            fails.push(format!("journey id `{}` is not kebab-case", j.id));
        }
        if j.title.trim().is_empty() {
            fails.push(format!("journey `{}` has an empty title", j.id));
        }
        if !(1..=5).contains(&j.tier) {
            fails.push(format!(
                "journey `{}` has tier {} (must be 1..=5)",
                j.id, j.tier
            ));
        }
        if j.steps.is_empty() {
            fails.push(format!("journey `{}` has no steps", j.id));
        }
    }
    for (id, n) in seen.iter().filter(|(_, n)| **n > 1) {
        fails.push(format!("journey id `{id}` declared {n} times"));
    }
    assert!(fails.is_empty(), "malformed journeys:\n  {}", fails.join("\n  "));
}

// ── bindable ────────────────────────────────────────────────────────────

#[test]
fn every_journey_step_binds_to_a_declared_command() {
    let c = contract();
    let mut unresolved = Vec::new();
    let mut undocumented_verb_only = Vec::new();
    let mut verb_only_total = 0usize;

    for j in &c.journeys {
        for (i, step) in j.steps.iter().enumerate() {
            match c.resolve_step(step) {
                StepBinding::Exact(_) => {}
                StepBinding::VerbOnly(verb) => {
                    verb_only_total += 1;
                    if step.note.is_none() {
                        undocumented_verb_only.push(format!(
                            "{}[{}] `{}` binds only to the verb `{verb}` — \
                             declare a [[command]] row for it, or add a `note` \
                             acknowledging the gap",
                            j.id, i, step.run
                        ));
                    }
                }
                StepBinding::Unresolved => unresolved.push(format!(
                    "{}[{}] `{}` resolves to NOTHING — no command path and no \
                     tracked verb. The journey drives something that does not exist.",
                    j.id, i, step.run
                )),
            }
        }
    }

    // Glassbox: the count is the to-do list, printed on every run.
    eprintln!(
        "cli_contract_journeys: {verb_only_total} step(s) bind to a tracked verb \
         whose exact subcommand has no [[command]] row yet"
    );

    assert!(
        unresolved.is_empty(),
        "journey steps that resolve to nothing:\n  {}",
        unresolved.join("\n  ")
    );
    assert!(
        undocumented_verb_only.is_empty(),
        "journey steps binding loosely without a note:\n  {}",
        undocumented_verb_only.join("\n  ")
    );
}

// ── cited ───────────────────────────────────────────────────────────────

#[test]
fn every_journey_cites_a_doc_that_exists() {
    let c = contract();
    let root = repo_root();
    let missing: Vec<String> = c
        .journeys
        .iter()
        .filter_map(|j| j.doc.as_ref().map(|d| (j, d)))
        .filter(|(_, d)| !root.join(doc_file(d)).is_file())
        .map(|(j, d)| format!("{} cites `{d}`, which does not exist", j.id))
        .collect();
    assert!(
        missing.is_empty(),
        "journeys citing docs that are gone (rename the citation or restore \
         the doc):\n  {}",
        missing.join("\n  ")
    );
}

#[test]
fn journeys_are_derived_from_documentation() {
    // A journey with no citation is an invented one. The whole point of
    // this layer is that journeys come from sequences the docs already
    // teach; an uncited journey is a claim nobody can check.
    let c = contract();
    let uncited: Vec<&str> = c
        .journeys
        .iter()
        .filter(|j| j.doc.is_none())
        .map(|j| j.id.as_str())
        .collect();
    assert!(
        uncited.is_empty(),
        "journeys with no `doc` citation: {uncited:?}"
    );
}

// ── coherent ────────────────────────────────────────────────────────────

#[test]
fn public_journeys_contain_no_dev_tools_steps() {
    // Mirrors the command-level invariant: a dev-tools step would exit 2 in
    // the shipped binary, so a public journey containing one is a promise
    // the product cannot keep.
    let c = contract();
    let mut bad = Vec::new();
    for j in c.journeys.iter().filter(|j| j.visibility == Visibility::Public) {
        for step in &j.steps {
            if let Some(cmd) = c.resolve_step(step).exact() {
                if cmd.feature == Feature::DevTools {
                    bad.push(format!(
                        "{}: `{}` is dev-tools gated but the journey is public",
                        j.id, step.run
                    ));
                }
            }
        }
    }
    assert!(
        bad.is_empty(),
        "public journeys with dev-tools steps (they would exit 2 in the \
         shipped binary):\n  {}",
        bad.join("\n  ")
    );
}

// ── the ratchet ─────────────────────────────────────────────────────────

#[test]
fn every_public_command_belongs_to_a_journey() {
    // The strictest bar: the public surface is what the READMEs promise, so
    // every part of it must be reachable from a sequence someone actually
    // runs. A public verb in no journey is a feature nobody has a path to.
    let c = contract();
    let covered = journey_verbs(&c);
    let orphans: Vec<String> = c
        .commands
        .iter()
        .filter(|cmd| cmd.visibility == Visibility::Public)
        .map(|cmd| verb_of(&cmd.path).to_string())
        .filter(|v| !covered.contains(v))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    assert!(
        orphans.is_empty(),
        "public verbs in no journey (write the journey, or the promise has \
         no path): {orphans:?}"
    );
}

#[test]
fn every_canonical_verb_is_journeyed_or_stranded() {
    // The completeness ratchet. A verb is either part of a use case or it is
    // on the debt register with a reason. Silence is not an option.
    let c = contract();
    let covered = journey_verbs(&c);
    let ledgered = stranded_verbs(&c);
    let unaccounted: Vec<String> = c
        .canonical()
        .map(|cmd| verb_of(&cmd.path).to_string())
        .filter(|v| !covered.contains(v) && !ledgered.contains(v))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    assert!(
        unaccounted.is_empty(),
        "verbs that are neither in a journey nor on the stranded ledger \
         (add a [[journey.step]] that drives it, or a [[stranded]] row \
         saying why not): {unaccounted:?}"
    );
}

#[test]
fn the_stranded_ledger_does_not_grow() {
    let c = contract();
    let n = c.stranded.len();
    assert!(
        n <= MAX_STRANDED,
        "the stranded ledger grew to {n} (cap {MAX_STRANDED}). A new verb \
         should join a journey, not the debt register. If you genuinely must \
         add one, that is a deliberate decision — raise MAX_STRANDED in the \
         same commit and say why in the message."
    );
    eprintln!("cli_contract_journeys: stranded ledger at {n}/{MAX_STRANDED}");
}

#[test]
fn stranded_entries_are_real_verbs_and_not_also_journeyed() {
    let c = contract();
    let known: BTreeSet<&str> = c.commands.iter().map(|cmd| verb_of(&cmd.path)).collect();
    let journeyed = journey_verbs(&c);
    let mut fails = Vec::new();
    for s in &c.stranded {
        if !known.contains(s.verb.as_str()) {
            fails.push(format!(
                "`{}` is stranded but no [[command]] declares it — stale ledger entry",
                s.verb
            ));
        }
        if journeyed.contains(&s.verb) {
            fails.push(format!(
                "`{}` is BOTH stranded and driven by a journey — retire the \
                 ledger entry",
                s.verb
            ));
        }
        if s.reason.trim().is_empty() {
            fails.push(format!("`{}` is stranded with no reason given", s.verb));
        }
        if s.disposition == Disposition::Fold && s.fold_into.is_none() {
            fails.push(format!(
                "`{}` is disposition=fold but names no `fold_into` target",
                s.verb
            ));
        }
    }
    assert!(fails.is_empty(), "stranded ledger problems:\n  {}", fails.join("\n  "));
}

// ── observable ──────────────────────────────────────────────────────────

#[test]
fn journeys_that_mutate_assert_something_observable() {
    // A journey that changes state but only ever checks exit codes cannot
    // detect its own breakage. `code search` exits 0 while printing "ships
    // in Phase 2"; `project install-hooks` exits 0 doing nothing at all.
    let c = contract();
    let weak: Vec<&str> = c
        .journeys
        .iter()
        .filter(|j| j.steps.iter().any(|s| s.mutates))
        .filter(|j| !asserts_output(j))
        .map(|j| j.id.as_str())
        .collect();
    assert!(
        weak.is_empty(),
        "journeys that mutate state but assert only exit codes — add a \
         stdout_contains / stdout_absent / stdout_non_empty to at least one \
         step: {weak:?}"
    );
}

#[test]
fn tier_one_and_two_journeys_prove_their_effect() {
    // The journeys that matter most carry the strictest bar: they must look
    // at output, not just exit status.
    let c = contract();
    let weak: Vec<&str> = c
        .journeys
        .iter()
        .filter(|j| j.tier <= 2)
        .filter(|j| !asserts_output(j))
        .map(|j| j.id.as_str())
        .collect();
    assert!(
        weak.is_empty(),
        "tier-1/2 journeys with no output assertion: {weak:?}"
    );
}

#[test]
fn reversible_mutations_are_reversed() {
    // A journey that installs something and never removes it leaves the next
    // run dirty AND never proves the removal path. Where the manifest says a
    // step reverses a mutation, the journey must actually assert absence.
    let c = contract();
    let mut fails = Vec::new();
    for j in &c.journeys {
        let claims_reversal = j
            .steps
            .iter()
            .any(|s| s.note.as_deref().is_some_and(|n| n.contains("reverse")));
        if !claims_reversal {
            continue;
        }
        let proves_absence = j
            .steps
            .iter()
            .filter_map(|s| s.expect.as_ref())
            .any(|e| e.stdout_absent.is_some());
        // `corpus-lifecycle` is the reference implementation of this shape.
        if j.id == "corpus-lifecycle" && !proves_absence {
            fails.push(format!(
                "{} claims to reverse a mutation but no step asserts \
                 stdout_absent",
                j.id
            ));
        }
    }
    assert!(fails.is_empty(), "{}", fails.join("\n  "));
}

// ── glassbox summary ────────────────────────────────────────────────────

#[test]
fn print_the_journey_map() {
    // Not an assertion — a rendered map, so `cargo test -- --nocapture`
    // answers "what does this CLI actually promise?" in one place.
    let c = contract();
    let mut by_tier: BTreeMap<u8, Vec<&Journey>> = BTreeMap::new();
    for j in &c.journeys {
        by_tier.entry(j.tier).or_default().push(j);
    }
    eprintln!("\n── CLI journey map ──");
    for (tier, js) in &by_tier {
        eprintln!("tier {tier}:");
        for j in js {
            let live = if j.skip_live.is_some() { "     " } else { "LIVE " };
            eprintln!("  {live}{:<24} {} steps  {}", j.id, j.steps.len(), j.title);
        }
    }
    eprintln!(
        "\n{} journeys, {} steps, {} live-eligible, {} stranded verbs",
        c.journeys.len(),
        c.journeys.iter().map(|j| j.steps.len()).sum::<usize>(),
        c.live_journeys().len(),
        c.stranded.len()
    );
}
