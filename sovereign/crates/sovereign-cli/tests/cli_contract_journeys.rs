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
    Contract, Disposition, Evidence, Feature, Journey, StepBinding, Visibility,
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

// ── the experience axis ─────────────────────────────────────────────────

/// Experiences that no journey serves yet. A declared gap is a DEBT with a
/// name — `code-intel-chat` is the honest instance: the flagship developer
/// experience the repo talks about most and covers least.
///
/// Lower this number when you write the journey. Do not raise it: a new
/// promise ships with a sequence that proves it, or it is not a promise.
const MAX_UNSERVED_EXPERIENCES: usize = 1;

#[test]
fn experiences_are_well_formed_and_cited() {
    let c = contract();
    let root = repo_root();
    assert!(!c.experiences.is_empty(), "manifest declares no experiences");
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    let mut fails = Vec::new();
    for e in &c.experiences {
        *seen.entry(e.id.as_str()).or_default() += 1;
        if e.id.is_empty() || !e.id.chars().all(|ch| ch.is_ascii_lowercase() || ch == '-') {
            fails.push(format!("experience id `{}` is not kebab-case", e.id));
        }
        if e.title.trim().is_empty() {
            fails.push(format!("experience `{}` has an empty title", e.id));
        }
        // REQUIRED, unlike a journey's: an undocumented experience is not a
        // promise, it is an intention.
        if !root.join(doc_file(&e.doc)).is_file() {
            fails.push(format!(
                "experience `{}` cites `{}`, which does not exist",
                e.id, e.doc
            ));
        }
    }
    for (id, n) in seen.iter().filter(|(_, n)| **n > 1) {
        fails.push(format!("experience id `{id}` declared {n} times"));
    }
    assert!(
        fails.is_empty(),
        "malformed experiences:\n  {}",
        fails.join("\n  ")
    );
}

#[test]
fn every_journey_serves_a_declared_experience() {
    // The other direction of the ratchet below. A journey pointing at an
    // experience nobody declared is a typo that would silently orphan both.
    let c = contract();
    let declared: BTreeSet<&str> = c.experiences.iter().map(|e| e.id.as_str()).collect();
    let dangling: Vec<String> = c
        .journeys
        .iter()
        .filter(|j| !declared.contains(j.experience.as_str()))
        .map(|j| format!("{} serves `{}`, which no [[experience]] declares", j.id, j.experience))
        .collect();
    assert!(
        dangling.is_empty(),
        "journeys pointing at undeclared experiences:\n  {}",
        dangling.join("\n  ")
    );
}

#[test]
fn an_unserved_experience_is_declared_as_a_gap() {
    // The check that makes a hole VISIBLE. An experience with no journey must
    // say so in `gap`; the count is capped and shrink-only. Before this axis,
    // "CODE_INTEL_CHAT.md has no journey" was findable only by
    // cross-referencing the docs against this manifest by hand.
    let c = contract();
    let mut silent = Vec::new();
    let mut declared_gaps = Vec::new();
    for e in &c.experiences {
        let served = !c.journeys_for(&e.id).is_empty();
        match (&e.gap, served) {
            (None, false) => silent.push(format!(
                "`{}` has no journey and no `gap` — either write the journey \
                 or say why it is missing",
                e.id
            )),
            (Some(_), false) => declared_gaps.push(e.id.as_str()),
            (Some(why), true) => silent.push(format!(
                "`{}` declares a gap (\"{}\") but IS served by {} journey(s) — \
                 retire the gap",
                e.id,
                why.chars().take(40).collect::<String>(),
                c.journeys_for(&e.id).len()
            )),
            (None, true) => {}
        }
    }
    assert!(silent.is_empty(), "experience gaps:\n  {}", silent.join("\n  "));
    let n = declared_gaps.len();
    assert!(
        n <= MAX_UNSERVED_EXPERIENCES,
        "unserved experiences grew to {n} (cap {MAX_UNSERVED_EXPERIENCES}): \
         {declared_gaps:?}. A new promise ships with a journey that proves it."
    );
    eprintln!("cli_contract_journeys: unserved experiences at {n}/{MAX_UNSERVED_EXPERIENCES} {declared_gaps:?}");
}

#[test]
fn every_capability_is_exercised_by_a_serving_journey() {
    // THE POINT OF THE WHOLE AXIS. A promise is made of capabilities; each
    // must be driven by a step that asserts OUTPUT (a read inline, a mutation
    // by a later step). Exit codes cannot carry this: every code-intelligence
    // tool in this repo exits 0 when it finds nothing, so a step that checks
    // only status is satisfied by a tool that answered nothing at all.
    //
    // Gapped experiences are skipped — they have no journeys by definition,
    // and their capability list is the spec for the journey somebody owes.
    let c = contract();
    let mut fails = Vec::new();
    for e in c.experiences.iter().filter(|e| e.gap.is_none()) {
        for (cap, mentioned) in c.unproven_capabilities(e) {
            // Two different repairs, and the second is the dangerous one —
            // it already LOOKS covered.
            if mentioned {
                fails.push(format!(
                    "{}: `{cap}` is driven by a step that asserts nothing about \
                     output — add stdout_contains/stdout_non_empty, or (for a \
                     mutation) a later step that proves the effect",
                    e.id
                ));
            } else {
                fails.push(format!(
                    "{}: `{cap}` is named by NO step of any journey serving it \
                     — write the step, or drop the capability if it is not part \
                     of the promise",
                    e.id
                ));
            }
        }
    }
    assert!(
        fails.is_empty(),
        "capabilities declared but not exercised:\n  {}",
        fails.join("\n  ")
    );
}

// ── what can actually fail ──────────────────────────────────────────────
//
// THE MEASUREMENT THAT MOTIVATED THIS BLOCK (2026-07-29/30). The first count
// was one number: "63 of 133 steps declare no assertion at all", capped and
// shrink-only. That number mixed two unrelated defects, and the mix was the
// problem — it made the cheap repair satisfy a ratchet aimed at the expensive
// one. Split by whether any lane RUNS the step:
//
//   live      73 steps, 19 asserting nothing  ← FALSE GREEN. A lane invokes
//             these and prints a tick whatever happens.
//   never-run 62 steps, 44 asserting nothing  ← NOT RUN BY ANYTHING. Whatever
//             they declare, no lane can catch it. 14 journeys carry a
//             journey-level `skip_live`: a second machine, a paid GPU pod, a
//             multi-minute benchmark.
//
// Sprinkling `exit = 0` over the 44 would have taken the headline number from
// 63 to 19 and changed nothing about what this repo can detect. So the live
// half is a HARD ZERO with no cap to negotiate, and the never-run half is a
// separate, named debt that shrinks only by making a journey runnable.
//
// The rules for the live half, and why they are not the same rule:
//
//   * every live step asserts SOMETHING (`live_steps_all_assert_something`)
//   * every live READ asserts OUTPUT (`live_read_steps_assert_output`) —
//     because in this repo an exit code is not evidence for a read: `symbols`
//     on a name that does not exist exits 0, and so does `doctor` on a sick
//     system, by design.
//   * a MUTATION may assert only its exit code, since it usually cannot see
//     its own effect (`corpus install` returns before the ingest lands) — but
//     its journey must prove the effect downstream, which is
//     `every_live_journey_asserts_output_somewhere` plus the capability rule.

#[test]
fn live_steps_all_assert_something() {
    // HARD ZERO, deliberately not a cap. A step some lane executes and nobody
    // checks is not a weak test, it is a demonstration reported as a test — and
    // it misattributes the sequence's failure to the next step that DOES assert
    // something (`enrich init` wrote no enrichment directory at all, ticked
    // green, and `enrich status` two steps later took the blame).
    //
    // There is no third option by design. Either the step declares what it
    // expects, or it declares `skip_live = "why"` and joins the never-run debt
    // below, where it is counted as unproven rather than as coverage.
    let c = contract();
    let census = c.assertion_census();
    assert!(
        census.live_unfalsifiable.is_empty(),
        "{} step(s) a lane RUNS declare no assertion whatever — they are invoked \
         and reported as passing no matter what happens:\n  {}\n\nGive each an \
         `expect` block (a read: `stdout_contains`/`stdout_non_empty`; a \
         mutation: at least `exit = 0`, with a later step proving the effect), \
         or mark the step `skip_live = \"why\"` so it is counted as unproven \
         instead of as evidence.",
        census.live_unfalsifiable.len(),
        census.live_unfalsifiable.join("\n  ")
    );
}

#[test]
fn live_read_steps_assert_output() {
    // The second layer, and the one that stops this whole axis from being
    // satisfied by `exit = 0` everywhere. A READ that checks only its status is
    // satisfied by the command answering NOTHING — measured on this repo's own
    // tools: `symbols`, `callers`, `callees` and `capability_map` all print a
    // helpful paragraph and exit 0 for a name that does not exist, and
    // `code search` shipped a Phase-2 stub that did the same.
    //
    // Mutations are exempt: `corpus install` POSTs and returns before its ingest
    // lands, so it cannot asssert its own effect and the proof is a later step.
    // That exemption is what `every_live_journey_asserts_output_somewhere` and
    // `every_capability_is_exercised_by_a_serving_journey` collect on.
    let c = contract();
    let weak: Vec<String> = c
        .journeys
        .iter()
        .flat_map(|j| {
            j.live_steps()
                .filter(|(_, s)| !s.mutates && s.evidence() != Evidence::Output)
                .map(move |(i, s)| format!("{}[{}] {}", j.id, i, s.run))
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(
        weak.is_empty(),
        "{} read-only step(s) a lane RUNS assert no output:\n  {}\n\nAn exit code \
         is not evidence for a read in this repo — every code-intelligence tool \
         here exits 0 when it finds nothing. Assert what the command should \
         actually say (`stdout_contains` for a fact, `stdout_non_empty` only \
         when the text is genuinely unpredictable), or mark the step `mutates` \
         if it is really a write.",
        weak.len(),
        weak.join("\n  ")
    );
}

#[test]
fn every_live_journey_asserts_output_somewhere() {
    // The static twin of the runner's ⊘ UNPROVEN verdict, and the check that
    // makes `code-intel-lifecycle` impossible to write again: six steps
    // (`project init | list | status | refresh | serve | stop`) that ran end to
    // end, printed ✓ 6/6, and never once asked the index a question.
    //
    // A journey with no output assertion anywhere cannot fail for any reason
    // except a crash. Whatever its tick says, it proves that the binary starts.
    let c = contract();
    let census = c.assertion_census();
    assert!(
        census.live_journeys_without_output.is_empty(),
        "live journey(s) with no output assertion anywhere: {:?}\n\nA sequence \
         that only checks exit codes proves the binary starts. Add the step that \
         asks the question the journey is named for — the runner reports this \
         same shape as ⊘ UNPROVEN at run time.",
        census.live_journeys_without_output
    );
}

/// Steps NO lane executes: a journey-level `skip_live` (needs a second
/// machine, a paid GPU pod, a multi-minute benchmark against real models) or a
/// step-level one. They are dispatch-replayed and statically checked, so a
/// renamed verb still breaks the build — but nothing ever runs them, so no
/// `expect` block they carry can catch a regression.
///
/// 62 of 135 steps when first counted (2026-07-30): 46% of the manifest is a
/// written intention. That is not automatically wrong — `pipeline-pods`
/// provisions paid cloud GPUs and should not run nightly — but it must be
/// COUNTED, because "133 steps" as a coverage claim is off by half.
///
/// The cap shrinks by making a journey runnable (usually: move `skip_live` off
/// the journey and onto the two expensive steps, so its cheap read-only prefix
/// becomes real evidence). It never grows: a new journey that no lane runs is
/// a doc comment with a TOML syntax.
const MAX_NEVER_RUN_STEPS: usize = 62;

#[test]
fn steps_no_lane_runs_do_not_grow() {
    let c = contract();
    let census = c.assertion_census();
    let n = census.never_run.total();
    assert!(
        n <= MAX_NEVER_RUN_STEPS,
        "steps no lane runs grew to {n} (cap {MAX_NEVER_RUN_STEPS}). A journey \
         nothing executes cannot detect a regression; it is a written \
         intention. Either make it runnable (move `skip_live` from the journey \
         onto the steps that are genuinely expensive, so the read-only prefix \
         runs) or lower the promise it claims to cover.\n\nnever-run journeys:\n  {}",
        census
            .never_run_journeys
            .iter()
            .map(|(id, why)| format!("{id} — {why}"))
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}

#[test]
fn print_the_assertion_census() {
    // Not an assertion — the answer to "how much of this manifest can actually
    // fail?", printed on every run so the ratio is visible rather than
    // reconstructed. The same census the `svrn contract` verb renders.
    let c = contract();
    eprintln!("{}", sovereign_cli_shared::cli_contract_report::render_census(&c));
}

// ── glassbox summary ────────────────────────────────────────────────────

#[test]
fn print_the_experience_map() {
    // Not an assertion — the answer to "what does this product PROMISE, and
    // how much of each promise is actually proven?" in one place.
    //
    // Rendered by `cli_contract_report`, which is also what `svrn contract`
    // prints. ONE renderer on purpose: this map used to live here as a wall of
    // `eprintln!`, reachable only by knowing the exact `cargo test … --nocapture`
    // incantation, and a second copy would have started drifting from the
    // numbers the ratchets above enforce the moment either was edited.
    let c = contract();
    eprintln!("\n{}", sovereign_cli_shared::cli_contract_report::render_experience_map(&c));
}

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
