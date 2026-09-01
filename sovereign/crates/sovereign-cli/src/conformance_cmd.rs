// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn conformance` — which requirements of the specification are actually
//! proven, in four verdicts.
//!
//! It joins five artifacts and owns no judgement of its own:
//!
//! | artifact | answers |
//! |---|---|
//! | `quality/requirements.toml` | what the spec obliges (625 in scope) |
//! | `quality/conformance/*.toml` | which TEST claims each requirement |
//! | `target/nextest/*/junit.xml` | what that test actually did, last run |
//! | `sovereign/docs/cli-contract.toml` | which JOURNEY STEP claims each requirement |
//! | `~/.svrnmesh/journey-nightly/latest-steps.jsonl` | what that step actually did, last lane |
//!
//! # Two instruments, because one class of requirement cannot reach the other
//!
//! 260 of the 625 are classified `structural` in
//! `quality/requirements-enforceability.toml` — settled by a type, a lint, or
//! a source-scanning test, and a unit test is the right instrument. **311 are
//! classified `cli`: settleable by a command plus an assertion on its output,
//! and by nothing else.** Until the journey join existed those 311 had no
//! instrument at all, and the attempt to cover them with unit tests instead
//! produced 35 overclaims out of 74 adjudicated candidates — tests that
//! touched the area and asserted nothing the clause says.
//!
//! Both sources feed the same four verdicts and the same worst-wins rule. A
//! requirement claimed by both a test and a journey step is only as proven as
//! its weaker evidence.
//!
//! # `--scenarios` — the second axis, and why it is two columns
//!
//! `REQUIREMENTS.md §16` writes 19 acceptance scenarios and opens "a rebuild
//! that reproduces the feature list and fails the following is not a rebuild of
//! this system". They parse into the registry as `[[scenarios]]`, each citing
//! the requirement ids it covers.
//!
//! A scenario is reported as **demonstrated** (a journey declaring
//! `demonstrates`, resolved through the journey lane) *and* **cited** (the
//! roll-up over its cites) — never as one number. §16.1's A-1 requires the
//! demonstration to have been watched, so proving IN-10 and OP-7 by other means
//! does not mean anyone killed the numerical engine mid-decode (A-8). Folding
//! the two would let green cites stand in for a scenario nobody ran.
//!
//! A scenario no journey declares reads **not declared**, which is deliberately
//! not `never-ran`: never-ran means something claimed it and no lane executed
//! the claim, and collapsing the two hides which of the 19 nobody has taken on.
//!
//! WHY A VERB AND NOT A TEST. The registry gate is a test, because a test can
//! check a file. This cannot be: it reads the report of a test run, and a test
//! cannot read the report of the run it is inside. That is the whole reason
//! this is a separate surface rather than a ninth `cargo xtask quality` gate.
//!
//! # Four verdicts, and the two that usually go missing (ARCH §18.2)
//!
//! - **passed** — the claiming test is in the report and did not fail.
//! - **failed** — it is in the report and failed.
//! - **could-not-judge** — the report predates the test's own source file, so
//!   a recorded PASS describes code that may no longer exist. A recorded FAIL
//!   in the same state stays `failed`: the asymmetry is deliberate, and it runs
//!   in the direction that cannot hide a defect.
//! - **never-ran** — no claim at all, or the claiming test is absent from the
//!   report (a filtered run). A requirement with no manifest entry is
//!   `never-ran` BY CONSTRUCTION, so the denominator cannot be shrunk by
//!   omission (ARCH §18.3).
//!
//! **Four numbers, never one.** No headline percentage is printed here or
//! anywhere: "91% conformant" is the artifact most likely to outlive its
//! caveats, and is the thing this campaign exists to prevent.

use kernel_types::conformance::RequirementRegistry;
use kernel_types::Verdict;
use sovereign_cli_shared::cli_contract::Contract;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One requirement's resolved state.
struct Row {
    id: String,
    verdict: Verdict,
    detail: String,
    /// Does anything claim this requirement at all?
    ///
    /// The axis the verdict cannot carry. `never-ran` is the honest verdict for
    /// both "nothing claims it" — the starting state of most of the 625 — and
    /// "something claims it and no lane executed the claim". The second is the
    /// *declared and never run* shape: it reads as progress on every surface
    /// that counts claims, and proves exactly as much as silence. Folding it in
    /// with 600 unclaimed requirements is how it stays invisible, so the render
    /// names those rows separately.
    claimed: bool,
}

/// A `[[claim]]` from a per-crate conformance manifest.
#[derive(serde::Deserialize)]
struct Claim {
    requirement: String,
    test: String,
    file: String,
    line: u32,
    asserts: u32,
}

#[derive(serde::Deserialize)]
struct ClaimFile {
    #[serde(default)]
    claim: Vec<Claim>,
}

pub async fn run(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP);
        return 0;
    }
    let json = args.iter().any(|a| a == "--json");
    let scenarios = args.iter().any(|a| a == "--scenarios");
    // `--family` filters requirements and a scenario belongs to no family, so
    // the scenario view always resolves every in-scope requirement: its cites
    // reach across families by construction (A-8 names IN-10 and OP-7).
    let asked_family = args
        .iter()
        .position(|a| a == "--family")
        .and_then(|i| args.get(i + 1))
        .cloned();
    // `--family` filters requirements and a scenario belongs to no family: a
    // scenario's cites reach across families by construction (A-8 names IN-10
    // and OP-7). So the scenario view drops the filter — and SAYS SO. Dropping
    // it in silence is the substitution ARCH §18.3 forbids, and a code comment
    // is not the response; this module's own doc cites that principle twice.
    if scenarios && asked_family.is_some() {
        eprintln!(
            "conformance: --family {} IGNORED — a scenario belongs to no family, and its \
             cites cross families by construction. Showing all 19.",
            asked_family.as_deref().unwrap_or_default()
        );
    }
    let family = if scenarios { None } else { asked_family };

    let Some(root) = repo_root() else {
        eprintln!("conformance: not in a source checkout — no quality/requirements.toml found");
        return 2;
    };

    let registry: RequirementRegistry = match std::fs::read_to_string(root.join(REGISTRY))
        .map_err(|e| e.to_string())
        .and_then(|t| {
            strip_header(&t)
                .parse::<toml::Value>()
                .map_err(|e| e.to_string())
        })
        .and_then(|v| v.try_into().map_err(|e: toml::de::Error| e.to_string()))
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("conformance: cannot read {REGISTRY}: {e}");
            return 2;
        }
    };

    let claims = match load_claims(&root) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("conformance: {e}");
            return 2;
        }
    };

    let report = load_junit(&root);

    // The journey side. A missing manifest or a lane that has never run are
    // both real states, not errors: every journey claim reads never-ran, which
    // is the honest answer and the one the four-tuple exists to say.
    let contract = Contract::load(&root.join(MANIFEST)).ok();
    let steps = load_steps();

    // ── Resolve ────────────────────────────────────────────────────────────
    let mut rows: Vec<Row> = Vec::new();
    for req in registry.in_scope() {
        if let Some(f) = &family {
            if &req.family != f {
                continue;
            }
        }
        let mine: Vec<&Claim> = claims.iter().filter(|c| c.requirement == req.id).collect();
        let journeyed: Vec<_> = contract
            .as_ref()
            .map(|c| {
                c.requirement_claims()
                    .into_iter()
                    .filter(|jc| {
                        registry
                            .resolve(jc.requirement)
                            .is_some_and(|r| r.id == req.id)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if mine.is_empty() && journeyed.is_empty() {
            rows.push(Row {
                id: req.id.clone(),
                verdict: Verdict::NeverRan,
                detail: "nothing claims it".into(),
                claimed: false,
            });
            continue;
        }
        // Worst wins across every claim from EITHER instrument — Verdict::rank()
        // already ranks Failed < NeverRan < CouldNotJudge < Passed
        // (kernel-types). A requirement a test proves and a journey fails is
        // failed: the two sources are evidence about one obligation, not two
        // scores to pick the better of.
        let mut worst: Option<Row> = None;
        let mut consider = |row: Row| {
            if worst
                .as_ref()
                .is_none_or(|w| row.verdict.rank() < w.verdict.rank())
            {
                worst = Some(Row {
                    id: req.id.clone(),
                    ..row
                });
            }
        };
        for c in mine {
            consider(resolve(&root, c, report.as_ref()));
        }
        for jc in &journeyed {
            consider(resolve_journey(&root, jc, steps.as_ref()));
        }
        rows.push(worst.expect("at least one claim"));
    }

    if rows.is_empty() {
        // X-EH-3 applied to this runner: a selection that matched nothing
        // examined nothing, and must not read as success.
        eprintln!(
            "conformance: NEVER-RAN — no requirement matched{}. A zero-work run does not \
             report success.",
            family
                .as_deref()
                .map(|f| format!(" --family {f}"))
                .unwrap_or_default()
        );
        return 4;
    }

    if scenarios {
        return render_scenarios(&registry, &rows, contract.as_ref(), steps.as_ref(), json);
    }

    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for r in &rows {
        *counts.entry(r.verdict.as_str()).or_insert(0) += 1;
    }
    let n = |k: &str| counts.get(k).copied().unwrap_or(0);

    if json {
        let checks: Vec<String> = rows
            .iter()
            .map(|r| {
                format!(
                    "{{\"id\":{},\"verdict\":{},\"claimed\":{},\"detail\":{}}}",
                    esc(&r.id),
                    esc(r.verdict.as_str()),
                    r.claimed,
                    esc(&r.detail)
                )
            })
            .collect();
        println!(
            "{{\"passed\":{},\"failed\":{},\"could_not_judge\":{},\"never_ran\":{},\"checks\":[{}]}}",
            n("passed"),
            n("failed"),
            n("could-not-judge"),
            n("never-ran"),
            checks.join(",")
        );
    } else {
        // Both instruments name themselves, including when they are absent.
        // "no report" is the state most likely to be misread as "nothing to
        // prove", so it gets a line rather than a silence.
        match &report {
            Some(r) => println!(
                "tests    {} — {} testcase(s), run {}",
                rel(&r.path, &root),
                r.results.len(),
                &r.uuid
            ),
            None => println!(
                "tests    no report on disk — every test claim reads never-ran until something runs"
            ),
        }
        match &steps {
            Some(st) if st.status.is_empty() => println!(
                "journeys {} — ZERO step result(s). A lane that recorded nothing proves \
                 nothing;\n         every journey claim below reads never-ran for that \
                 reason, not because\n         the claim is absent. Re-run \
                 sovereign/scripts/cli-journey-nightly.sh.",
                rel(&st.path, &root)
            ),
            Some(st) => println!(
                "journeys {} — {} step result(s)",
                rel(&st.path, &root),
                st.status.len()
            ),
            None => println!(
                "journeys no lane report on this host — every journey claim reads never-ran \
                 (run sovereign/scripts/cli-journey-nightly.sh)"
            ),
        }
        println!(
            "\n  {} passed   {} failed   {} could-not-judge   {} never-ran   (of {})",
            n("passed"),
            n("failed"),
            n("could-not-judge"),
            n("never-ran"),
            rows.len()
        );
        // Lead with what is NOT proven. Sorted worst-first by rank.
        let mut shown = rows.iter().collect::<Vec<_>>();
        shown.sort_by_key(|r| (r.verdict.rank(), r.id.clone()));
        println!();
        for r in shown.iter().filter(|r| r.verdict != Verdict::NeverRan) {
            println!("  {:<16} {:<9} {}", r.verdict.as_str(), r.id, r.detail);
        }
        // DECLARED AND NEVER RUN, named row by row. This is the failure that
        // killed cli-contract-live-verify.sh — a lane gated on a variable
        // nothing set, reporting green for its entire life — restated at the
        // requirement level, and it is invisible unless it is printed apart
        // from the honestly-unclaimed.
        let stated: Vec<&Row> = rows
            .iter()
            .filter(|r| r.claimed && r.verdict == Verdict::NeverRan)
            .collect();
        if !stated.is_empty() {
            println!(
                "\n  {} requirement(s) are CLAIMED and were never run — a claim no lane \
                 executes proves exactly as much as no claim:",
                stated.len()
            );
            for r in &stated {
                println!("  {:<16} {:<9} {}", r.verdict.as_str(), r.id, r.detail);
            }
        }
        let unclaimed = rows.iter().filter(|r| !r.claimed).count();
        if unclaimed > 0 {
            println!("\n  {unclaimed} requirement(s) have nothing claiming them at all.");
        }
        // §16 is a second axis over the same registry and it was referenced by
        // nothing for as long as no view printed it. One line here, so the
        // default report cannot leave the reader unaware the axis exists.
        let declared: std::collections::BTreeSet<&str> = contract
            .as_ref()
            .map(Contract::scenario_claims)
            .unwrap_or_default()
            .iter()
            .map(|c| c.scenario)
            .collect::<Vec<_>>()
            .into_iter()
            .collect();
        println!(
            "\n  §16 acceptance: {} of {} scenario(s) declared by a journey  \
             (svrn conformance --scenarios)",
            declared.len(),
            registry.scenarios.len()
        );

        // Two routes, because the requirement's own class decides which one is
        // available — and offering only the test route is how 311 `cli`-class
        // requirements got covered by unit tests that assert something adjacent
        // to the clause.
        println!(
            "\n  Claim a `structural` one:  /// covers: <ID>  above a #[test], then\n    \
             UPDATE_CONFORMANCE_TAGS=1 cargo test -p kernel-types --test conformance_tags\n\
             \n  Claim a `cli` one:  requirements = [\"<ID>\"]  on the journey step whose\n    \
             expect block falsifies the clause, in {MANIFEST}\n    \
             (the step must assert OUTPUT, not just an exit code — the gates refuse the rest)"
        );
    }

    // A MUST that failed is the only thing that fails this command. never-ran
    // is the honest starting state of 623 of the 625 and must not be red.
    if n("failed") > 0 {
        1
    } else {
        0
    }
}

/// Render `REQUIREMENTS.md §16` — the 19 acceptance scenarios — in two columns.
///
/// **DEMONSTRATED** is the scenario actually run: a journey declaring
/// `demonstrates = ["A-8"]`, resolved through the journey lane exactly like a
/// requirement claim. **CITED** is the roll-up of the requirements the scenario
/// names, worst-first.
///
/// They are never folded into one number, and the reason is §16.1's A-1: the
/// demonstration "must have been watched to fail". Proving IN-10 and OP-7 by
/// other means does not mean anyone killed the numerical engine mid-decode
/// (A-8). A single column would let green cites stand in for a scenario nobody
/// ran, which is the substitution §16 exists to prevent (ARCH §18.3).
fn render_scenarios(
    registry: &RequirementRegistry,
    rows: &[Row],
    contract: Option<&Contract>,
    steps: Option<&StepReport>,
    json: bool,
) -> i32 {
    let by_id: BTreeMap<&str, &Row> = rows.iter().map(|r| (r.id.as_str(), r)).collect();
    let claims = contract.map(Contract::scenario_claims).unwrap_or_default();

    struct ScenarioRow<'a> {
        id: &'a str,
        suite: &'a str,
        /// Worst verdict across the journeys demonstrating it; `None` when no
        /// journey declares it at all — which is NOT `never-ran`, because
        /// never-ran means something claimed it and no lane executed the claim.
        demonstrated: Option<(Verdict, String)>,
        cited: Vec<(&'a str, Verdict)>,
    }

    let mut out: Vec<ScenarioRow> = Vec::new();
    for sc in &registry.scenarios {
        let mut demonstrated: Option<(Verdict, String)> = None;
        for claim in claims.iter().filter(|c| c.scenario == sc.id) {
            // A journey is demonstrated by its LAST live step: the scenario is
            // the whole sequence, so the sequence is only run when its final
            // executed step is. An earlier step passing while a later one is
            // absent means the lane stopped part-way through the scenario.
            let verdict = journey_sequence_verdict(claim.journey, steps);
            let detail = format!("{} — {}", claim.journey.id, verdict.1);
            if demonstrated
                .as_ref()
                .is_none_or(|(v, _)| verdict.0.rank() < v.rank())
            {
                demonstrated = Some((verdict.0, detail));
            }
        }
        let cited: Vec<(&str, Verdict)> = sc
            .cites
            .iter()
            .filter_map(|id| {
                // Resolve through the registry so an alias cite lands on the
                // row its target owns rather than reading as an unknown id.
                let target = registry.resolve(id)?;
                by_id
                    .get(target.id.as_str())
                    .map(|r| (id.as_str(), r.verdict))
            })
            .collect();
        out.push(ScenarioRow {
            id: &sc.id,
            suite: &sc.suite,
            demonstrated,
            cited,
        });
    }

    let n_demo = out.iter().filter(|r| r.demonstrated.is_some()).count();
    let n_passed = out
        .iter()
        .filter(|r| matches!(r.demonstrated, Some((Verdict::Passed, _))))
        .count();

    if json {
        let items: Vec<String> = out
            .iter()
            .map(|r| {
                let cites: Vec<String> = r
                    .cited
                    .iter()
                    .map(|(id, v)| {
                        format!("{{\"id\":{},\"verdict\":{}}}", esc(id), esc(v.as_str()))
                    })
                    .collect();
                format!(
                    "{{\"id\":{},\"suite\":{},\"demonstrated\":{},\"detail\":{},\"cites\":[{}]}}",
                    esc(r.id),
                    esc(r.suite),
                    r.demonstrated
                        .as_ref()
                        .map(|(v, _)| esc(v.as_str()))
                        .unwrap_or_else(|| "null".into()),
                    r.demonstrated
                        .as_ref()
                        .map(|(_, d)| esc(d))
                        .unwrap_or_else(|| "null".into()),
                    cites.join(",")
                )
            })
            .collect();
        println!(
            "{{\"scenarios\":{},\"declared\":{},\"passed\":{},\"items\":[{}]}}",
            out.len(),
            n_demo,
            n_passed,
            items.join(",")
        );
    } else {
        println!("── acceptance scenarios — REQUIREMENTS.md §16 \"How a rebuild is judged\" ──\n");
        println!(
            "  \"A rebuild that reproduces the feature list and fails the following is not\n   \
             a rebuild of this system.\"\n"
        );
        println!(
            "  DEMONSTRATED is the scenario actually run. CITED is the roll-up of the\n  \
             requirements it names. They are two columns and never one number: §16.1's A-1\n  \
             requires the demonstration to have been watched, so green cites over a scenario\n  \
             nobody ran is exactly the substitution §16 exists to prevent.\n"
        );
        println!(
            "  {:<6} {:<18} {:<34} {}",
            "id", "demonstrated", "cited requirements", "suite"
        );
        for r in &out {
            let demo = match &r.demonstrated {
                Some((v, _)) => v.as_str(),
                None => "not declared",
            };
            let passed = r
                .cited
                .iter()
                .filter(|(_, v)| *v == Verdict::Passed)
                .count();
            let worst = r
                .cited
                .iter()
                .map(|(_, v)| *v)
                .min_by_key(|v| v.rank())
                .unwrap_or(Verdict::NeverRan);
            let cited = if r.cited.is_empty() {
                "— names none".to_string()
            } else {
                format!(
                    "{passed}/{} passed · worst {}",
                    r.cited.len(),
                    worst.as_str()
                )
            };
            println!("  {:<6} {demo:<18} {cited:<34} {}", r.id, r.suite);
        }
        println!(
            "\n  {n_demo} of {} scenario(s) are declared by a journey; {n_passed} demonstrated.",
            out.len()
        );
        if n_demo < out.len() {
            println!(
                "  The rest are NOT NEVER-RAN — they are undeclared: no journey claims to run them,\n  \
                 which is a different and more honest state than a claim nothing executed."
            );
        }
        println!(
            "\n  Demonstrate one:  demonstrates = [\"A-8\"]  on the journey that runs the\n    \
             sequence, in {MANIFEST}. It is declared on the JOURNEY, not a step: a\n    \
             requirement is a clause one assertion falsifies, a scenario is a sequence."
        );
    }

    // Same rule as the requirement view: only a FAILURE is an error. An
    // undeclared scenario is the honest starting state of all 19.
    let failed = out.iter().any(|r| {
        matches!(r.demonstrated, Some((Verdict::Failed, _)))
            || r.cited.iter().any(|(_, v)| *v == Verdict::Failed)
    });
    i32::from(failed)
}

/// The verdict for a whole journey run as a scenario demonstration.
///
/// Worst-wins across every step a lane should have executed. A sequence is only
/// demonstrated when all of it ran: a lane that passed three steps and never
/// reached the fourth has not shown the scenario, and reporting the passing
/// prefix would be the partial-credit failure `cli-journey-sandbox.sh` already
/// refuses one layer down (its `partial` bucket).
fn journey_sequence_verdict(
    journey: &sovereign_cli_shared::cli_contract::Journey,
    steps: Option<&StepReport>,
) -> (Verdict, String) {
    let Some(steps) = steps else {
        return (
            Verdict::NeverRan,
            "no journey lane has run on this host".into(),
        );
    };
    let mut worst = (Verdict::Passed, "every step passed".to_string());
    let mut any = false;
    for (i, _step) in journey.live_steps() {
        any = true;
        let v = match steps.status.get(&(journey.id.clone(), i)) {
            None => (
                Verdict::NeverRan,
                format!("step [{i}] not in the last lane"),
            ),
            Some(st) => match st.as_str() {
                "pass" => continue,
                "fail" => (Verdict::Failed, format!("step [{i}] failed")),
                "unverifiable" | "unasserted" => {
                    (Verdict::CouldNotJudge, format!("step [{i}] was {st}"))
                }
                other => (Verdict::NeverRan, format!("step [{i}] {other}")),
            },
        };
        if v.0.rank() < worst.0.rank() {
            worst = v;
        }
    }
    if !any {
        return (
            Verdict::NeverRan,
            "no step of this journey runs live".into(),
        );
    }
    worst
}

/// The verdict for one claim, and why.
fn resolve(root: &Path, c: &Claim, report: Option<&Report>) -> Row {
    let Some(report) = report else {
        return Row {
            id: String::new(),
            claimed: true,
            verdict: Verdict::NeverRan,
            detail: format!("{} — no test report on disk", c.test),
        };
    };
    let Some(passed) = report.results.get(&c.test) else {
        return Row {
            id: String::new(),
            claimed: true,
            verdict: Verdict::NeverRan,
            detail: format!("{} not in the last run (filtered?)", c.test),
        };
    };
    if !passed {
        // DELIBERATELY ASYMMETRIC with the staleness rule below: a stale FAIL
        // stays failed, a stale PASS does not stay passed. Both directions
        // refuse to flatter — the one that would hide a defect is the one that
        // gets demoted, and a red that a later edit may already have fixed
        // costs a re-run, not a shipped regression.
        return Row {
            id: String::new(),
            claimed: true,
            verdict: Verdict::Failed,
            detail: format!("{} failed ({}:{})", c.test, c.file, c.line),
        };
    }
    // A pass recorded before the guard's source was last touched describes code
    // that may no longer exist. Not a pass (ARCH §18.2).
    let src = root.join(&c.file);
    if let (Ok(sm), Some(rm)) = (
        std::fs::metadata(&src).and_then(|m| m.modified()),
        report.modified,
    ) {
        if sm > rm {
            return Row {
                id: String::new(),
                claimed: true,
                verdict: Verdict::CouldNotJudge,
                detail: format!("{} passed, but {} changed since that run", c.test, c.file),
            };
        }
    }
    Row {
        id: String::new(),
        claimed: true,
        verdict: Verdict::Passed,
        detail: format!("{} ({} assertion(s))", c.test, c.asserts),
    }
}

const REGISTRY: &str = kernel_types::conformance::REGISTRY_PATH;
const CLAIMS_DIR: &str = "quality/conformance";
const MANIFEST: &str = "sovereign/docs/cli-contract.toml";

/// What the last journey lane recorded, keyed by `(journey id, step index)`.
struct StepReport {
    path: PathBuf,
    /// The runner's own status word for the step. Kept as written rather than
    /// pre-mapped to a [`Verdict`] so the detail line can say what actually
    /// happened — `skipped-mutating` and `skipped-no-fixture` are both
    /// never-ran, and the repair is different.
    status: BTreeMap<(String, usize), String>,
    modified: Option<std::time::SystemTime>,
}

/// Read the newest journey lane's per-step rows, if a lane has left any.
///
/// Deliberately a hand-rolled scan rather than a JSON dependency on the shape:
/// the file is append-only JSONL written by a shell script, one lane's rows
/// after another's, and a single malformed line must cost that line and not
/// the whole report. A row this cannot parse is simply absent — which reads as
/// `never-ran`, the honest verdict for evidence that did not arrive.
fn load_steps() -> Option<StepReport> {
    let path = sovereign_cli_shared::cli_contract_report::nightly_steps_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    let mut status = BTreeMap::new();
    for line in text.lines() {
        // Journey-level rows carry `"kind":"journey"` and no step index; they
        // are the lane's own summary and say nothing about a single claim.
        if line.contains("\"kind\":\"journey\"") {
            continue;
        }
        let (Some(j), Some(i), Some(st)) = (
            json_str(line, "journey"),
            json_num(line, "step"),
            json_str(line, "status"),
        ) else {
            continue;
        };
        // Last row wins: a journey re-run later in the same lane supersedes an
        // earlier attempt, which is what a reader means by "what it did".
        status.insert((j, i), st);
    }
    let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    Some(StepReport {
        path,
        status,
        modified,
    })
}

/// The verdict for one journey-step claim, and why.
///
/// The same four verdicts and the same deliberate asymmetry as [`resolve`]: a
/// stale FAIL stays failed, a stale PASS is demoted to could-not-judge. Here
/// "stale" means the MANIFEST changed after the lane ran — the step's `run`
/// line or its `expect` block may no longer be the one that passed.
fn resolve_journey(
    root: &Path,
    c: &sovereign_cli_shared::cli_contract::JourneyClaim<'_>,
    steps: Option<&StepReport>,
) -> Row {
    let where_ = format!("journey {} step [{}]", c.journey.id, c.step_index);
    let Some(steps) = steps else {
        return Row {
            id: String::new(),
            claimed: true,
            verdict: Verdict::NeverRan,
            detail: format!("{where_} — no journey lane has run on this host"),
        };
    };
    let Some(status) = steps.status.get(&(c.journey.id.clone(), c.step_index)) else {
        return Row {
            id: String::new(),
            claimed: true,
            verdict: Verdict::NeverRan,
            detail: format!("{where_} not in the last lane"),
        };
    };
    match status.as_str() {
        "fail" => Row {
            id: String::new(),
            claimed: true,
            verdict: Verdict::Failed,
            detail: format!("{where_} failed (`{}`)", c.step.run),
        },
        // The runner ran the command and could not decide. Never a pass.
        "unverifiable" | "unasserted" => Row {
            id: String::new(),
            claimed: true,
            verdict: Verdict::CouldNotJudge,
            detail: format!("{where_} ran but was {status}"),
        },
        "pass" => {
            let manifest = root.join(MANIFEST);
            if let (Ok(mm), Some(rm)) = (
                std::fs::metadata(&manifest).and_then(|m| m.modified()),
                steps.modified,
            ) {
                if mm > rm {
                    return Row {
                        id: String::new(),
                        claimed: true,
                        verdict: Verdict::CouldNotJudge,
                        detail: format!(
                            "{where_} passed, but the manifest changed since that lane"
                        ),
                    };
                }
            }
            Row {
                id: String::new(),
                claimed: true,
                verdict: Verdict::Passed,
                detail: format!("{where_} — `{}`", c.step.run),
            }
        }
        // Every `skipped*` word, and anything a future runner adds. Not judged,
        // and never quietly folded into a pass (ARCH §18.3).
        other => Row {
            id: String::new(),
            claimed: true,
            verdict: Verdict::NeverRan,
            detail: format!("{where_} {other}"),
        },
    }
}

/// `quality/requirements.toml` opens with a `#` comment block; `toml` handles
/// that, so this is a no-op today and a seam if the header ever stops being
/// comments.
fn strip_header(t: &str) -> &str {
    t
}

fn repo_root() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join(REGISTRY).is_file() {
            return Some(dir);
        }
        dir = dir.parent()?.to_path_buf();
    }
}

fn rel(p: &Path, root: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).display().to_string()
}

fn load_claims(root: &Path) -> Result<Vec<Claim>, String> {
    let dir = root.join(CLAIMS_DIR);
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        // No manifests yet is a real state, not an error: everything reads
        // never-ran, which is the honest answer.
        Err(_) => return Ok(out),
    };
    for e in entries.filter_map(|e| e.ok()) {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "toml") {
            let text =
                std::fs::read_to_string(&p).map_err(|err| format!("{}: {err}", p.display()))?;
            let f: ClaimFile =
                toml::from_str(&text).map_err(|err| format!("{}: {err}", p.display()))?;
            out.extend(f.claim);
        }
    }
    Ok(out)
}

/// A parsed nextest JUnit report.
struct Report {
    path: PathBuf,
    uuid: String,
    modified: Option<std::time::SystemTime>,
    /// `classname::name` → did it pass.
    results: BTreeMap<String, bool>,
}

/// Read the newest `target/nextest/*/junit.xml`.
///
/// Hand-scanned rather than parsed with an XML crate: this file has exactly one
/// producer and a fixed shape, and a new third-party dependency to read it
/// would cost more than it buys. The scan VALIDATES ITSELF against the
/// `tests="N"` attribute the producer writes, so a shape change is refused
/// rather than silently under-reported (ARCH §18.4).
fn load_junit(root: &Path) -> Option<Report> {
    let dir = root.join("target/nextest");
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for e in std::fs::read_dir(&dir).ok()?.filter_map(|e| e.ok()) {
        let p = e.path().join("junit.xml");
        let Ok(m) = std::fs::metadata(&p).and_then(|m| m.modified()) else {
            continue;
        };
        if best.as_ref().is_none_or(|(bm, _)| m > *bm) {
            best = Some((m, p));
        }
    }
    let (modified, path) = best?;
    let text = std::fs::read_to_string(&path).ok()?;
    let uuid = attr(&text, "uuid").unwrap_or_default();
    let declared: usize = attr(&text, "tests")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let mut results = BTreeMap::new();
    for chunk in text.split("<testcase ").skip(1) {
        let (head, body) = chunk.split_once('>').unwrap_or((chunk, ""));
        let (Some(name), Some(class)) = (attr(head, "name"), attr(head, "classname")) else {
            continue;
        };
        let case = body.split("</testcase>").next().unwrap_or("");
        let failed = case.contains("<failure") || case.contains("<error");
        let skipped = case.contains("<skipped");
        if !skipped {
            results.insert(format!("{class}::{name}"), !failed);
        }
    }
    if declared > 0 && results.len() + count(&text, "<skipped") != declared {
        eprintln!(
            "conformance: refusing {} — it declares tests=\"{declared}\" but {} testcase(s) \
             parsed. The report's shape changed; fix the reader rather than trusting a \
             partial join.",
            path.display(),
            results.len()
        );
        return None;
    }
    Some(Report {
        path,
        uuid,
        modified: Some(modified),
        results,
    })
}

fn count(hay: &str, needle: &str) -> usize {
    hay.matches(needle).count()
}

/// First `key="value"` in `s`.
/// `"key":"value"` out of one JSONL row. Distinct from [`attr`], which reads
/// the XML form `key="value"` — the two files this module joins are written by
/// different tools and neither shape parses the other.
fn json_str(s: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":\"");
    let i = s.find(&pat)? + pat.len();
    let rest = &s[i..];
    Some(rest[..rest.find('"')?].to_string())
}

/// `"key":123` out of one JSONL row.
fn json_num(s: &str, key: &str) -> Option<usize> {
    let pat = format!("\"{key}\":");
    let i = s.find(&pat)? + pat.len();
    let rest = &s[i..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn attr(s: &str, key: &str) -> Option<String> {
    let pat = format!("{key}=\"");
    let i = s.find(&pat)? + pat.len();
    let rest = &s[i..];
    Some(rest[..rest.find('"')?].to_string())
}

fn esc(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "svrn conformance",
    summary: "Which requirements of the specification are proven, in four verdicts.",
    sections: &[
        crate::util::help::HelpSection::Usage(
            "svrn conformance [--family <PREFIX>] [--scenarios] [--json]",
        ),
        crate::util::help::HelpSection::Examples(&[
            ("svrn conformance", "every in-scope requirement"),
            ("svrn conformance --family GR", "just the grounding domain"),
            (
                "svrn conformance --scenarios",
                "§16's 19 acceptance scenarios — how a rebuild is judged",
            ),
            (
                "svrn conformance --json",
                "the four-tuple, machine-readable",
            ),
        ]),
        crate::util::help::HelpSection::Notes(
            "Joins five artifacts: quality/requirements.toml (what the spec obliges), \
             quality/conformance/*.toml + the newest target/nextest/*/junit.xml (which TEST \
             claims each requirement and what it last did), and sovereign/docs/cli-contract.toml \
             + ~/.svrnmesh/journey-nightly/latest-steps.jsonl (which JOURNEY STEP claims it and \
             what that step last did). Two routes because the requirement's class decides which \
             applies: 260 are `structural` and want a #[test]; 311 are `cli` and want a journey \
             step. It runs nothing — run the tests, and the journey lane, first. A pass recorded before its guard's source was last edited reads \
             could-not-judge, never passed. Exits 1 only when a claimed requirement FAILED; \
             never-ran is the honest starting state and is not an error. --scenarios renders \
             REQUIREMENTS.md §16 instead: the demonstration a journey ran and the roll-up of \
             the requirements it cites, as two columns that are never one number.",
        ),
    ],
};
