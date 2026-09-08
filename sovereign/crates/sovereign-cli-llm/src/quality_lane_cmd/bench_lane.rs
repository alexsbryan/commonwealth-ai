// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn quality lane bench` — the six existing bench lanes, read for CATASTROPHE.
//!
//! # What this wrapper is for
//!
//! The six lanes below already exist and already run. What they do not do is
//! say a `Judgement`: `bench all` exits 1 for "regressed", "stale" AND
//! "missing baseline"; `knowledge-gym` exits 3 for "below 0.9"; chaos-monkey
//! exits 4 for "measured nothing". An exit code cannot carry four verdicts,
//! which is why ci-bench reconstructs them by `grep -qE` over lane PROSE
//! (`scripts/lib/ci-bench-verdict.sh`, 130 lines of coupling to wording no
//! lane ever promised to keep).
//!
//! This wrapper runs the lane's own verb unchanged, reads the REPORT it
//! writes, and turns it into rows. Nothing here parses a sentence.
//!
//! # Catastrophe only
//!
//! At the sample sizes a 30-minute check can afford — six probes, ten
//! questions, three fixtures — a one-item flip is 12-20 points, and a
//! tolerance band at that n is noise (RUNBOOK §6). So the scores are
//! TRACKED with no band and the HARD rows are the ones no sample size makes
//! ambiguous:
//!
//! | row | a failure means |
//! |---|---|
//! | errored items | an item raised, or the lane's own subprocess died |
//! | empty answers | an item came back with no answer at all |
//! | all-zero tally | the lane scored something and scored zero on all of it |
//! | refused an answerable probe | a probe whose declared `expected_action` is `answer` got a refusal |
//! | ungrounded assertion | any answer asserted a value the evidence does not carry |
//! | leaked to reader | a WRONG answer was released — `Partition::leaks_to_reader` |
//!
//! Drift is the nightly's question (`scripts/sovereign-ci-bench.sh`), which
//! runs the full banks against committed per-lane baselines. This is the
//! breakage check.
//!
//! # The argv is data
//!
//! `quality/instruments.toml` carries the whole inner command line in the
//! lane's `argv`, so
//! adding a lane is an edit to a table. Two substitutions happen inside it:
//!
//! - `{report}` — the file the inner verb should write, one per command.
//! - `{ids:<subset_id>}` — the ids `sovereign/bench/smoke.toml` declares for
//!   that subset, each repeated after the PRECEDING argv token. It is how
//!   `knowledge-gym --fixture {ids:knowledge-gym-v1}` becomes three
//!   `--fixture <slug>` pairs without the list living in two files.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use super::{LaneCtx, LaneReport};
use crate::bench_cmd::smoke_subset::{self, SmokeSelection};

/// Which report shape the lane writes. A closed set: an unknown reader is
/// refused, never guessed at from the file's contents.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LaneReader {
    /// `bench all --report <path>` — a JSON array of `BenchOutcome`.
    BenchAll,
    /// `bench chaos-monkey run --out <path>` — one `ResultRow` per line.
    ChaosMonkey,
    /// `knowledge-gym run --json` — an `AggregateSummary` on STDOUT, which is
    /// why this reader reads the captured stdout and not `{report}`.
    KnowledgeGym,
}

impl LaneReader {
    fn parse(s: &str) -> Option<LaneReader> {
        match s {
            "bench-all" => Some(LaneReader::BenchAll),
            "chaos-monkey" => Some(LaneReader::ChaosMonkey),
            "knowledge-gym" => Some(LaneReader::KnowledgeGym),
            _ => None,
        }
    }
    /// Does this reader read `{report}`, or the command's stdout?
    fn reads_stdout(self) -> bool {
        self == LaneReader::KnowledgeGym
    }
}

struct BenchLaneArgs {
    id: String,
    reader: LaneReader,
    /// One or more inner command lines. Several because a lane can span two
    /// corpora that no single `--filter` selects together (retrieval-prod is
    /// sep AND wikipedia), and folding them into one command would mean
    /// inventing a filter language.
    commands: Vec<Vec<String>>,
}

fn parse_args(args: &[String]) -> Result<BenchLaneArgs, String> {
    let mut id = None;
    let mut reader = None;
    let mut commands: Vec<Vec<String>> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--id" => {
                i += 1;
                id = Some(args.get(i).ok_or("--id needs a value")?.clone());
            }
            "--reader" => {
                i += 1;
                let v = args.get(i).ok_or("--reader needs a value")?;
                reader = Some(LaneReader::parse(v).ok_or_else(|| {
                    format!("unknown reader `{v}`; known: bench-all, chaos-monkey, knowledge-gym")
                })?);
            }
            "--" => {
                commands.push(Vec::new());
                let cur = commands.len() - 1;
                i += 1;
                while i < args.len() && args[i] != "--" {
                    commands[cur].push(args[i].clone());
                    i += 1;
                }
                continue;
            }
            other => return Err(format!("unknown flag `{other}`")),
        }
        i += 1;
    }
    let id = id.ok_or("--id is required")?;
    let reader = reader.ok_or("--reader is required")?;
    if commands.is_empty() || commands.iter().any(Vec::is_empty) {
        return Err("at least one non-empty inner command is required after `--`".into());
    }
    Ok(BenchLaneArgs {
        id,
        reader,
        commands,
    })
}

/// Apply `{report}` and `{ids:<subset>}` to one inner command line.
///
/// `{ids:…}` expands in place and REPEATS the token before it, which is the
/// shape every id-selecting flag in this repo already has
/// (`--fixture a --fixture b`). Expanding to a bare list instead would make
/// the wrapper's output depend on the callee's parser.
fn expand(cmd: &[String], report: &Path) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for tok in cmd {
        if tok == "{report}" {
            out.push(report.to_string_lossy().to_string());
            continue;
        }
        if let Some(subset) = tok.strip_prefix("{ids:").and_then(|t| t.strip_suffix('}')) {
            let flag = out
                .pop()
                .ok_or_else(|| format!("`{tok}` has no flag before it to repeat"))?;
            let sel = smoke_subset::selection_for_sole_bank(subset)?;
            let SmokeSelection::Ids(ids) = sel else {
                return Err(format!(
                    "subset `{subset}` is declared `mode = \"full\"`; there are no ids to expand"
                ));
            };
            for v in ids {
                out.push(flag.clone());
                out.push(v);
            }
            continue;
        }
        out.push(tok.clone());
    }
    Ok(out)
}

/// One inner command's outcome. `report` is the path it was told to write —
/// present or not; a missing report is a fact the caller reports, not one it
/// substitutes around.
struct Ran {
    argv: Vec<String>,
    exit: Option<i32>,
    report: PathBuf,
    stdout: PathBuf,
}

fn run_one(repo: &Path, argv: &[String], report: &Path, stdout: &Path) -> Result<Ran, String> {
    let program = if matches!(argv[0].as_str(), "svrn" | "sovereign" | "sovereign-cli-llm") {
        // THIS binary, never whatever the operator's PATH holds — the same
        // rule the runner applies to a lane's own program.
        std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?
    } else {
        PathBuf::from(&argv[0])
    };
    let so = std::fs::File::create(stdout).map_err(|e| format!("{}: {e}", stdout.display()))?;
    let err_path = stdout.with_extension("err");
    let se =
        std::fs::File::create(&err_path).map_err(|e| format!("{}: {e}", err_path.display()))?;
    tracing::debug!(argv = ?argv, "quality lane bench: inner command");
    eprintln!("  [{}] $ {}", argv[0], argv[1..].join(" "));
    let status = std::process::Command::new(&program)
        .args(&argv[1..])
        .current_dir(repo)
        .stdin(Stdio::null())
        .stdout(Stdio::from(so))
        .stderr(Stdio::from(se))
        .status()
        .map_err(|e| format!("cannot run `{}`: {e}", argv.join(" ")))?;
    Ok(Ran {
        argv: argv.to_vec(),
        exit: status.code(),
        report: report.to_path_buf(),
        stdout: stdout.to_path_buf(),
    })
}

// ─── What a reader found ────────────────────────────────────────────

/// The catastrophe census, one per lane. Every field is a COUNT plus the
/// names behind it, so a row's reason can say which item rather than how
/// many.
#[derive(Default)]
struct Census {
    /// The report could not be read at all — reported, never counted as
    /// zero findings.
    unreadable: Vec<String>,
    /// Scored units that NEVER RAN — the daemon refused them, the request
    /// could not be built. Distinct from `errored` (an item that ran and
    /// raised) because the verdicts differ: never-ran is could-not-judge,
    /// raised is failed.
    never_ran: Vec<String>,
    errored: Vec<String>,
    empty_answers: Vec<String>,
    /// Probes whose declared `expected_action` is `answer` and which were
    /// refused anyway. FIVE of the seven `PressureKind`s are answerable
    /// (present, distractor, provenance-trap, superseded-trap,
    /// partially-present); this read `qtype == "present"` until 2026-09-08
    /// and so checked ONE of them.
    refused_answerable: Vec<String>,
    /// Answers that asserted a value the scorer looked for and could not
    /// ground — on ANY probe. This was gated on absent probes only until
    /// 2026-09-08, which made a fabricated citation on a probe that HAS
    /// evidence invisible to the lane: run 20260908-123251 released
    /// `distract-bomb-maker` with `asserted_value_grounded: false` and the
    /// row passed.
    ungrounded_assertion: Vec<String>,
    /// Probes whose scored `partition` says a WRONG answer reached the
    /// reader — leaked-wrong, retrieval-miss-leaked or confab-leaked. The
    /// scorer computed this cell all along and no row read it: run
    /// 20260908-134848 released `present-killer-weapon` WRONG on evidence
    /// that was never retrieved (`partition: retrieval_miss_leaked`,
    /// `answer_correct: false`, `gate_action: released`) and the lane went
    /// red only for an unrelated probe. The same run scored `answered 2/4`
    /// against the previous run's `1/4` — the count IMPROVED while an honest
    /// abstention was replaced by a released falsehood.
    leaked_to_reader: Vec<String>,
    /// `(what, scored, correct)` per scored unit.
    tallies: Vec<(String, usize, usize)>,
}

impl Census {
    fn all_zero(&self) -> Vec<String> {
        self.tallies
            .iter()
            .filter(|(_, scored, correct)| *scored > 0 && *correct == 0)
            .map(|(what, scored, _)| format!("{what} 0/{scored}"))
            .collect()
    }
    fn scored(&self) -> usize {
        self.tallies.iter().map(|(_, s, _)| s).sum()
    }
    fn summary(&self) -> String {
        if self.tallies.is_empty() {
            return "no scored unit".into();
        }
        self.tallies
            .iter()
            .map(|(what, s, c)| format!("{what} {c}/{s}"))
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

fn read_json(path: &Path) -> Result<serde_json::Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// `bench all --report` — a JSON array of `BenchOutcome`.
fn census_bench_all(v: &serde_json::Value, c: &mut Census) {
    let Some(rows) = v.as_array() else {
        c.unreadable.push("the report is not a JSON array".into());
        return;
    };
    if rows.is_empty() {
        c.unreadable
            .push("the report holds no outcome — the filter matched no bank".into());
        return;
    }
    for o in rows {
        let id = o
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("<unnamed>")
            .to_string();
        // `stale` is `bench all`'s own word for "the subprocess failed" and
        // for "every scored row errored" — both catastrophes, and neither a
        // score that moved.
        if o.get("status").and_then(|v| v.as_str()) == Some("stale") {
            let why = o
                .get("note")
                .and_then(|v| v.as_str())
                .unwrap_or("no detail");
            c.errored.push(format!("{id}: {why}"));
            continue;
        }
        if let Some(results) = o
            .get("retrieval")
            .and_then(|r| r.get("current"))
            .and_then(|r| r.get("results"))
            .and_then(|r| r.as_array())
        {
            let mut scored = 0usize;
            let mut correct = 0usize;
            for r in results {
                let qid = r
                    .get("question_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("<unnamed>");
                if let Some(e) = r.get("error").and_then(|v| v.as_str()) {
                    c.errored.push(format!("{id}/{qid}: {e}"));
                    continue;
                }
                // A synth run that produced no text at all. `None` is a
                // retrieval-only run, which has no answer to be empty.
                if let Some(ans) = r
                    .get("synth")
                    .and_then(|s| s.get("answer"))
                    .and_then(|v| v.as_str())
                {
                    if ans.trim().is_empty() {
                        c.empty_answers.push(format!("{id}/{qid}"));
                        continue;
                    }
                }
                scored += 1;
                let hit = ["fact_score", "source_score"].iter().any(|k| {
                    r.get(k)
                        .and_then(|s| s.get("ratio"))
                        .and_then(|v| v.as_f64())
                        .is_some_and(|x| x > 0.0)
                });
                if hit {
                    correct += 1;
                }
            }
            c.tallies.push((id.clone(), scored, correct));
            continue;
        }
        if let Some(cur) = o.get("enrichment").and_then(|e| e.get("current")) {
            // The enrichment surface is phases of `{expected, matched}`.
            let mut expected = 0usize;
            let mut matched = 0usize;
            if let Some(obj) = cur.as_object() {
                for phase in obj.values() {
                    let e = phase.get("expected").and_then(|v| v.as_u64());
                    let m = phase.get("matched").and_then(|v| v.as_u64());
                    if let (Some(e), Some(m)) = (e, m) {
                        expected += e as usize;
                        matched += m as usize;
                    }
                }
            }
            c.tallies.push((id.clone(), expected, matched));
            continue;
        }
        // The ROUTING surface: neither arm, and the tally is the field
        // `BenchOutcome::tally` exists for.
        if let Some(t) = o.get("tally") {
            let scored = t.get("scored").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let correct = t.get("correct").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            c.tallies.push((id.clone(), scored, correct));
            continue;
        }
        c.unreadable.push(format!(
            "{id}: the outcome carries neither a scored arm nor a tally"
        ));
    }
}

/// `chaos-monkey run --out` — one `ResultRow` per line.
fn census_chaos(text: &str, c: &mut Census) {
    let mut n = 0usize;
    let mut answered = 0usize;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(r) = serde_json::from_str::<serde_json::Value>(line) else {
            c.unreadable.push("a result line is not JSON".into());
            continue;
        };
        n += 1;
        let id = r.get("id").and_then(|v| v.as_str()).unwrap_or("<unnamed>");
        let action = r.get("agent_action").and_then(|v| v.as_str()).unwrap_or("");
        let excerpt = r
            .get("answer_excerpt")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if action == "answered" {
            answered += 1;
            if excerpt.trim().is_empty() {
                c.empty_answers.push(id.to_string());
                continue;
            }
        }
        // What the probe DEMANDS, read from the row rather than re-derived
        // from its `qtype`. `PressureKind::expected_action()` already owns
        // that mapping and answers `Answer` for five of its seven kinds, so
        // a second copy here keyed on one literal was both a duplicate
        // decider and a wrong one (ARCH §10.6). A row that does not carry
        // the field is REPORTED, never assumed either way (ARCH §18.3).
        match r.get("expected_action").and_then(|v| v.as_str()) {
            None => c.unreadable.push(format!(
                "{id}: no `expected_action`, so what the probe demanded cannot be told"
            )),
            // An answerable probe that got a refusal. This is the #57 shape
            // at bench scale and it is a catastrophe at any n.
            Some("answer") if action == "abstained" => c.refused_answerable.push(id.to_string()),
            _ => {}
        }
        // An answer asserting a value the evidence does not carry, on ANY
        // probe — a fabricated citation is the same defect whether or not
        // the corpus happens to hold a real answer. `asserted_value_grounded`
        // is `false` only when the scorer LOOKED and found no grounding;
        // absent (null) says nothing either way and is not read as one.
        if action == "answered"
            && r.get("asserted_value_grounded").and_then(|v| v.as_bool()) == Some(false)
        {
            c.ungrounded_assertion.push(id.to_string());
        }
        // A wrong answer that reached the reader, whichever subsystem is at
        // fault. The membership is `Partition::leaks_to_reader`, which lives
        // beside the counts that already summed the same three cells, so this
        // lane cannot disagree with the histogram in the same report
        // (ARCH §10.6). It is reached through `bench_cmd` because that is the
        // only module allowed to name the back-of-house harness (`lib.rs`,
        // `bench_cmd_is_the_only_module_naming_the_eval_harness`). The
        // producer stamps the cell on every row (`chaos_monkey.rs:1207`), so
        // its ABSENCE is a transcript this lane cannot judge, not a clean one.
        match r.get("partition").and_then(|v| v.as_str()) {
            None => c.unreadable.push(format!(
                "{id}: no readable `partition`, so a wrong answer reaching the reader cannot be told"
            )),
            Some(cell) => match crate::bench_cmd::chaos_monkey::partition_leaks_to_reader(cell) {
                Some(true) => c.leaked_to_reader.push(id.to_string()),
                Some(false) => {}
                None => c
                    .unreadable
                    .push(format!("{id}: `partition` is not a known cell: {cell}")),
            },
        }
    }
    if n == 0 {
        c.unreadable.push("the run wrote no result rows".into());
        return;
    }
    // "Answered at all" is the tally that makes sense across mixed probe
    // types: a run in which every probe abstained scored nothing, whatever
    // the per-probe verdicts say.
    c.tallies.push(("answered".into(), n, answered));
}

/// `knowledge-gym run --json` — an `AggregateSummary` on stdout.
fn census_gym(v: &serde_json::Value, c: &mut Census) {
    let Some(per) = v.get("per_fixture").and_then(|p| p.as_array()) else {
        c.unreadable
            .push("the summary carries no per_fixture array".into());
        return;
    };
    if per.is_empty() {
        c.unreadable.push("no fixture ran".into());
        return;
    }
    for row in per {
        let Some(obj) = row.as_object() else {
            c.unreadable
                .push("a per_fixture row is not an object".into());
            continue;
        };
        let slug = obj
            .get("slug")
            .and_then(|v| v.as_str())
            .unwrap_or("<unnamed>")
            .to_string();
        // REQUIRED, not defaulted. Defaulting `errored` to 0 would mean "no
        // replay was refused" on a rollup that never said so — the exact
        // substitution this reader exists to remove, reintroduced one layer
        // out (ARCH §18.3). A row missing any of the three is unreadable,
        // which is could-not-judge.
        let num = |k: &str| obj.get(k).and_then(|v| v.as_u64()).map(|n| n as usize);
        let (Some(passes), Some(replays), Some(errored)) =
            (num("passed"), num("replays"), num("errored"))
        else {
            c.unreadable.push(format!(
                "per_fixture row `{slug}` is missing passed/replays/errored"
            ));
            continue;
        };
        let judged = replays.saturating_sub(errored);

        // A replay the daemon refused never ran, so it is could-not-judge and
        // NOT a zero in the tally. Folding it in scored a contended host as a
        // broken model: three 503s read as `0/3`, the same shape a real
        // honesty failure makes (ARCH §18.3).
        if errored > 0 {
            c.never_ran.push(format!(
                "{slug}: {errored} of {replays} replay(s) never ran"
            ));
        }
        if judged == 0 {
            // Nothing to score. Pushing `(slug, 0, 0)` would be scored-zero.
            continue;
        }
        c.tallies.push((slug, judged, passes));
    }
}

// ─── The lane ───────────────────────────────────────────────────────

pub(crate) async fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "Usage: svrn quality lane bench --id <lane> --reader <kind> -- <argv> [-- <argv>]"
        );
        println!();
        println!("Runs an existing bench verb unchanged and reads its REPORT for the");
        println!("catastrophes a 30-minute check can judge at n<=10: errored items,");
        println!("empty answers, an all-zero tally, an abstention on a present probe,");
        println!("a confabulated answer on an absent one. Scores are tracked, not gated.");
        println!();
        println!("Readers: bench-all, chaos-monkey, knowledge-gym.");
        println!("Substitutions in the inner argv: {{report}}, {{ids:<subset_id>}}.");
        return 0;
    }
    let parsed = match parse_args(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("svrn quality lane bench: {e}");
            return 2;
        }
    };
    let ctx = LaneCtx::from_env();
    let mut report = LaneReport::new(&parsed.id);

    let Some(repo) = find_repo_root() else {
        report.cannot_judge(
            "lane",
            "the lane runs repo-relative commands; run it from a source checkout".into(),
        );
        return report.finish();
    };
    // Artifacts go beside the runner's other lane logs when there is a run,
    // and into a temp dir when there is not, so a hand run leaves nothing
    // behind and still behaves identically.
    let tmp = tempfile::tempdir().ok();
    let out_dir = ctx
        .out_dir
        .clone()
        .or_else(|| tmp.as_ref().map(|t| t.path().to_path_buf()));
    let Some(out_dir) = out_dir else {
        report.cannot_judge("lane", "no writable directory for the lane's report".into());
        return report.finish();
    };

    let mut census = Census::default();
    let mut ran: Vec<Ran> = Vec::new();
    for (n, cmd) in parsed.commands.iter().enumerate() {
        let report_path = out_dir.join(format!("bench-{}-{n}.json", parsed.id));
        let stdout_path = out_dir.join(format!("bench-{}-{n}.stdout", parsed.id));
        let argv = match expand(cmd, &report_path) {
            Ok(a) => a,
            Err(e) => {
                report.cannot_judge("lane", format!("command {n}: {e}"));
                return report.finish();
            }
        };
        match run_one(&repo, &argv, &report_path, &stdout_path) {
            Ok(r) => ran.push(r),
            Err(e) => {
                report.cannot_judge("lane", format!("command {n}: {e}"));
                return report.finish();
            }
        }
    }

    for r in &ran {
        // Exit 2 is "the flags were wrong" in every one of these verbs: the
        // lane never ran, which is not a lane that measured nothing.
        if r.exit == Some(2) {
            census.unreadable.push(format!(
                "`{}` exited 2 — the command line was refused, so nothing ran",
                r.argv.join(" ")
            ));
            continue;
        }
        let source = if parsed.reader.reads_stdout() {
            &r.stdout
        } else {
            &r.report
        };
        match parsed.reader {
            LaneReader::ChaosMonkey => match std::fs::read_to_string(source) {
                Ok(t) => census_chaos(&t, &mut census),
                Err(e) => census.unreadable.push(format!(
                    "{} unreadable ({e}); the command exited {}",
                    source.display(),
                    r.exit.map_or("on a signal".to_string(), |c| c.to_string())
                )),
            },
            LaneReader::BenchAll | LaneReader::KnowledgeGym => match read_json(source) {
                Ok(v) if parsed.reader == LaneReader::BenchAll => census_bench_all(&v, &mut census),
                Ok(v) => census_gym(&v, &mut census),
                Err(e) => census.unreadable.push(format!(
                    "{e}; the command exited {}",
                    r.exit.map_or("on a signal".to_string(), |c| c.to_string())
                )),
            },
        }
    }

    rows(&mut report, &parsed.id, parsed.reader, &census);
    report.finish()
}

/// The census as rows. Split out so the rules are testable against a census
/// built by hand, with no daemon and no subprocess.
fn rows(report: &mut LaneReport, id: &str, reader: LaneReader, c: &Census) {
    if !c.unreadable.is_empty() {
        report.cannot_judge(
            "report",
            format!(
                "{} report problem(s): {}",
                c.unreadable.len(),
                c.unreadable.join("; ")
            ),
        );
    }

    if !c.never_ran.is_empty() {
        report.cannot_judge(
            "replays that ran",
            format!(
                "{} fixture(s) had replays that never ran: {}",
                c.never_ran.len(),
                c.never_ran.join("; ")
            ),
        );
    }

    if c.errored.is_empty() {
        report.passed("errored items", "no item raised".into());
    } else {
        report.failed(
            "errored items",
            format!("{}: {}", c.errored.len(), c.errored.join("; ")),
        );
    }

    if c.empty_answers.is_empty() {
        report.passed("empty answers", "every item came back with text".into());
    } else {
        report.failed(
            "empty answers",
            format!(
                "{} item(s) answered with nothing: {}",
                c.empty_answers.len(),
                c.empty_answers.join(", ")
            ),
        );
    }

    let zero = c.all_zero();
    if c.tallies.is_empty() {
        report.cannot_judge(
            "all-zero tally",
            "nothing was scored, so a zero cannot be told from an absence".into(),
        );
    } else if zero.is_empty() {
        report.passed(
            "all-zero tally",
            format!("{} item(s) scored — {}", c.scored(), c.summary()),
        );
    } else {
        report.failed(
            "all-zero tally",
            format!(
                "scored and scored zero on: {} (of {})",
                zero.join(", "),
                c.summary()
            ),
        );
    }

    if reader == LaneReader::ChaosMonkey {
        if c.refused_answerable.is_empty() {
            report.passed(
                "refused an answerable probe",
                "every probe whose expected action is `answer` was answered".into(),
            );
        } else {
            report.failed(
                "refused an answerable probe",
                format!(
                    "{} answerable probe(s) refused: {}",
                    c.refused_answerable.len(),
                    c.refused_answerable.join(", ")
                ),
            );
        }
        if c.leaked_to_reader.is_empty() {
            report.passed("leaked to reader", "no wrong answer was released".into());
        } else {
            report.failed(
                "leaked to reader",
                format!(
                    "{} wrong answer(s) reached the reader: {}",
                    c.leaked_to_reader.len(),
                    c.leaked_to_reader.join(", ")
                ),
            );
        }
        if c.ungrounded_assertion.is_empty() {
            report.passed(
                "ungrounded assertion",
                "no answer asserted a value the evidence does not carry".into(),
            );
        } else {
            report.failed(
                "ungrounded assertion",
                format!(
                    "{} answer(s) asserted a value the evidence does not carry: {}",
                    c.ungrounded_assertion.len(),
                    c.ungrounded_assertion.join(", ")
                ),
            );
        }
    }

    // TRACKED, always passed: the numbers are in the reason and the nightly
    // is where a band lives. A one-item flip at n<=10 is 10-20 points.
    report.passed("scores (tracked)", format!("lane `{id}` — {}", c.summary()));
}

fn find_repo_root() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join("quality").is_dir() && dir.join("sovereign").is_dir() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel_types::Verdict;

    fn verdicts(c: &Census, reader: LaneReader) -> Vec<(String, Verdict)> {
        let mut r = LaneReport::new("t");
        rows(&mut r, "t", reader, c);
        r.rows_for_test()
            .iter()
            .map(|j| (j.subject().to_string(), j.verdict()))
            .collect()
    }

    fn find(v: &[(String, Verdict)], subject: &str) -> Verdict {
        v.iter()
            .find(|(s, _)| s == subject)
            .unwrap_or_else(|| panic!("no row `{subject}` in {v:?}"))
            .1
    }

    #[test]
    fn a_clean_census_passes_every_hard_row() {
        let c = Census {
            tallies: vec![("sep".into(), 5, 4)],
            ..Census::default()
        };
        let v = verdicts(&c, LaneReader::BenchAll);
        assert!(
            v.iter().all(|(_, verdict)| *verdict == Verdict::Passed),
            "{v:?}"
        );
    }

    /// Scored-zero and scored-nothing are two answers. Collapsing them would
    /// let a lane that never ran an item read as a lane that ran them and
    /// failed — or worse, the other way round.
    #[test]
    fn scoring_zero_fails_and_scoring_nothing_cannot_judge() {
        let zero = Census {
            tallies: vec![("sep".into(), 5, 0)],
            ..Census::default()
        };
        assert_eq!(
            find(&verdicts(&zero, LaneReader::BenchAll), "all-zero tally"),
            Verdict::Failed
        );
        let nothing = Census::default();
        assert_eq!(
            find(&verdicts(&nothing, LaneReader::BenchAll), "all-zero tally"),
            Verdict::CouldNotJudge
        );
    }

    #[test]
    fn an_errored_item_and_an_empty_answer_each_fail_their_own_row() {
        let c = Census {
            errored: vec!["sep/q1: timeout".into()],
            empty_answers: vec!["sep/q2".into()],
            tallies: vec![("sep".into(), 3, 2)],
            ..Census::default()
        };
        let v = verdicts(&c, LaneReader::BenchAll);
        assert_eq!(find(&v, "errored items"), Verdict::Failed);
        assert_eq!(find(&v, "empty answers"), Verdict::Failed);
        assert_eq!(find(&v, "all-zero tally"), Verdict::Passed);
    }

    /// The two chaos-only rows exist only for the chaos reader. A bench-all
    /// lane that emitted them would be claiming a check it never made.
    #[test]
    fn the_chaos_rows_appear_only_for_the_chaos_reader() {
        let c = Census {
            tallies: vec![("answered".into(), 6, 4)],
            ..Census::default()
        };
        let chaos = verdicts(&c, LaneReader::ChaosMonkey);
        assert!(chaos
            .iter()
            .any(|(s, _)| s == "refused an answerable probe"));
        assert!(chaos.iter().any(|(s, _)| s == "ungrounded assertion"));
        assert!(chaos.iter().any(|(s, _)| s == "leaked to reader"));
        let all = verdicts(&c, LaneReader::BenchAll);
        assert!(!all.iter().any(|(s, _)| s == "refused an answerable probe"));
    }

    #[test]
    fn a_refusal_on_an_answerable_probe_and_an_ungrounded_assertion_both_fail() {
        let c = Census {
            refused_answerable: vec!["present-wife".into()],
            ungrounded_assertion: vec!["absent-heat-firstname".into()],
            tallies: vec![("answered".into(), 6, 5)],
            ..Census::default()
        };
        let v = verdicts(&c, LaneReader::ChaosMonkey);
        assert_eq!(find(&v, "refused an answerable probe"), Verdict::Failed);
        assert_eq!(find(&v, "ungrounded assertion"), Verdict::Failed);
    }

    /// A report nobody could read is could-not-judge NAMING it — never a
    /// clean census, which is what "no findings" would say.
    #[test]
    fn an_unreadable_report_is_could_not_judge() {
        let c = Census {
            unreadable: vec!["exited 2".into()],
            ..Census::default()
        };
        let v = verdicts(&c, LaneReader::BenchAll);
        assert_eq!(find(&v, "report"), Verdict::CouldNotJudge);
    }

    // ── The report readers, against the real report shapes ──────────

    #[test]
    fn the_bench_all_reader_finds_errors_stale_outcomes_and_the_routing_tally() {
        let v: serde_json::Value = serde_json::from_str(
            r#"[
              {"id":"cells_v1","group":"routing","corpus_id":"w","surface":"retrieval",
               "status":"green","levers":[],"tally":{"scored":27,"correct":0}},
              {"id":"questions","group":"sep","corpus_id":"sep","surface":"retrieval",
               "status":"green","levers":[],
               "retrieval":{"current":{"bank_name":"sep","corpus":"sep","limit":5,
                 "started_at_unix":0,"results":[
                   {"question_id":"q1","category":"a","question":"?","retrieved":[],
                    "source_score":{"matched":[],"missing":[],"total_expected":1,"ratio":0.5},
                    "fact_score":{"matched":[],"missing":[],"total_expected":1,"ratio":0.0},
                    "embed_ms":1,"search_ms":1,"corpora_hit":[],"vector_eligible":true},
                   {"question_id":"q2","error":"boom","category":"a","question":"?",
                    "retrieved":[],
                    "source_score":{"matched":[],"missing":[],"total_expected":1,"ratio":null},
                    "fact_score":{"matched":[],"missing":[],"total_expected":1,"ratio":null},
                    "embed_ms":1,"search_ms":1,"corpora_hit":[],"vector_eligible":true}]}}},
              {"id":"other","group":"sep","corpus_id":"sep","surface":"retrieval",
               "status":"stale","levers":[],"note":"`eval run` exited 1"}
            ]"#,
        )
        .unwrap();
        let mut c = Census::default();
        census_bench_all(&v, &mut c);
        assert_eq!(c.errored.len(), 2, "{:?}", c.errored);
        assert!(c.errored.iter().any(|e| e.contains("q2")));
        assert!(c.errored.iter().any(|e| e.contains("exited 1")));
        // routing 0/27 is the all-zero catastrophe that used to live only in
        // `note` prose.
        assert!(c.all_zero().iter().any(|s| s.contains("cells_v1 0/27")));
        // q1 scored (source 0.5 > 0), q2 errored and is not scored.
        assert!(c.tallies.contains(&("questions".to_string(), 1, 1)));
    }

    #[test]
    fn the_chaos_reader_separates_a_refusal_from_a_confabulation() {
        let jsonl = r#"
{"id":"present-wife","qtype":"present","expected_action":"answer","agent_action":"abstained","answer_excerpt":"I cannot say","partition":"retrieval_miss"}
{"id":"present-target","qtype":"present","expected_action":"answer","agent_action":"answered","answer_excerpt":"Greenwich","partition":"correct"}
{"id":"absent-heat","qtype":"absent_adjacent","expected_action":"abstain","agent_action":"answered","answer_excerpt":"Tom","asserted_value_grounded":false,"partition":"confab_leaked"}
{"id":"absent-embassy","qtype":"absent_adjacent","expected_action":"abstain","agent_action":"abstained","answer_excerpt":"","partition":"abstain_correct"}
{"id":"ood-berlin","qtype":"absent_out_of_domain","expected_action":"abstain","agent_action":"answered","answer_excerpt":"1989","asserted_value_grounded":null,"partition":"released_best_effort"}
"#;
        let mut c = Census::default();
        census_chaos(jsonl, &mut c);
        assert_eq!(c.refused_answerable, vec!["present-wife".to_string()]);
        assert_eq!(c.ungrounded_assertion, vec!["absent-heat".to_string()]);
        // A null `asserted_value_grounded` says nothing either way and is
        // NOT read as a confabulation.
        assert!(!c.ungrounded_assertion.iter().any(|s| s == "ood-berlin"));
        // NEGATIVE CONTROL for both widened predicates: an answerable probe
        // that was answered, and an abstain probe that abstained, are the
        // two correct behaviours and neither may appear as a finding.
        assert!(!c.refused_answerable.iter().any(|s| s == "absent-embassy"));
        assert!(!c.ungrounded_assertion.iter().any(|s| s == "present-target"));
        // `confab_leaked` is one of the three cells that reach the reader;
        // `retrieval_miss` (an honest decline) and `released_best_effort` are
        // not, so the leak row separates them.
        assert_eq!(c.leaked_to_reader, vec!["absent-heat".to_string()]);
        assert_eq!(c.tallies, vec![("answered".to_string(), 5, 3)]);
        assert!(c.empty_answers.is_empty());
        assert!(c.unreadable.is_empty(), "{:?}", c.unreadable);
    }

    /// THE WIDENING, half one. `expected_action` is `answer` for five of the
    /// seven pressure kinds and the predicate matched the literal
    /// `qtype == "present"`, so a refusal on a distractor — a probe with real
    /// evidence behind it — was not a finding. The bank's own
    /// `PressureKind::expected_action()` is the decider now.
    #[test]
    fn a_refusal_on_a_distractor_is_a_refusal_on_an_answerable_probe() {
        let jsonl = r#"
{"id":"distract-bomb-maker","qtype":"distractor","expected_action":"answer","agent_action":"abstained","answer_excerpt":"I could not confirm","partition":"retrieval_miss"}
{"id":"trap-provenance","qtype":"provenance_trap","expected_action":"answer","agent_action":"abstained","answer_excerpt":"I could not confirm","partition":"synth_wrong_caught"}
"#;
        let mut c = Census::default();
        census_chaos(jsonl, &mut c);
        assert_eq!(
            c.refused_answerable,
            vec![
                "distract-bomb-maker".to_string(),
                "trap-provenance".to_string()
            ]
        );
    }

    /// THE WIDENING, half two, and the run that found it. In
    /// `target/quality-check/20260908-123251` the chaos lane released
    /// `distract-bomb-maker` with `asserted_value_grounded: false` — the
    /// answer "The Professor" over a quote that does not contain it — and
    /// the row PASSED, because the predicate required an absent probe. A
    /// fabricated citation is the same defect whichever probe it lands on.
    #[test]
    fn an_ungrounded_assertion_on_an_answerable_probe_is_a_confabulation() {
        let jsonl = r#"
{"id":"distract-bomb-maker","qtype":"distractor","expected_action":"answer","agent_action":"answered","answer_excerpt":"The Professor","asserted_value_grounded":false,"gate_action":"citation_grounded","partition":"correct"}
{"id":"present-target","qtype":"present","expected_action":"answer","agent_action":"answered","answer_excerpt":"Greenwich","asserted_value_grounded":true,"partition":"correct"}
"#;
        let mut c = Census::default();
        census_chaos(jsonl, &mut c);
        assert_eq!(
            c.ungrounded_assertion,
            vec!["distract-bomb-maker".to_string()]
        );
        // NEGATIVE CONTROL: a value the scorer DID ground is not a finding,
        // so the widened predicate cannot be passing by firing on everything.
        assert!(!c.ungrounded_assertion.iter().any(|s| s == "present-target"));
        assert!(c.refused_answerable.is_empty());
    }

    /// A row with no `expected_action` cannot say what the probe demanded,
    /// and reading it as "not answerable" would be the silent substitution
    /// that turns a missing field into a clean census (ARCH §18.3).
    #[test]
    fn a_row_without_an_expected_action_is_unreadable_not_clean() {
        let jsonl = r#"
{"id":"legacy-row","qtype":"present","agent_action":"abstained","answer_excerpt":"I cannot say","partition":"retrieval_miss"}
"#;
        let mut c = Census::default();
        census_chaos(jsonl, &mut c);
        assert!(c.refused_answerable.is_empty());
        assert_eq!(c.unreadable.len(), 1, "{:?}", c.unreadable);
        assert!(c.unreadable[0].contains("legacy-row"));
        assert!(c.unreadable[0].contains("expected_action"));
    }

    /// THE THIRD PREDICATE, and the run that demanded it. In
    /// `target/quality-check/20260908-134848` the same probe that had honestly
    /// abstained one run earlier ANSWERED, was WRONG, and the gate RELEASED it
    /// on evidence retrieval never surfaced. The scorer named the cell
    /// (`retrieval_miss_leaked`) and NO row read it, so the lane went red only
    /// for an unrelated probe — while `scores (tracked)` reported `answered
    /// 2/4` against the previous run's `1/4`, an improvement on the count for
    /// a strictly worse behaviour.
    #[test]
    fn a_wrong_answer_that_reached_the_reader_is_a_catastrophe() {
        let jsonl = r#"
{"id":"present-killer-weapon","qtype":"present","expected_action":"answer","agent_action":"answered","answer_excerpt":"a knife","answer_correct":false,"gate_action":"released","retrieval_present":false,"partition":"retrieval_miss_leaked"}
{"id":"present-target","qtype":"present","expected_action":"answer","agent_action":"answered","answer_excerpt":"Greenwich","partition":"leaked_wrong"}
{"id":"absent-heat","qtype":"absent_adjacent","expected_action":"abstain","agent_action":"answered","answer_excerpt":"Tom","partition":"confab_leaked"}
"#;
        let mut c = Census::default();
        census_chaos(jsonl, &mut c);
        assert_eq!(
            c.leaked_to_reader,
            vec![
                "present-killer-weapon".to_string(),
                "present-target".to_string(),
                "absent-heat".to_string()
            ]
        );
        // The refusal row is SILENT here — every probe answered. A leak and a
        // refusal are different findings and the lane must not conflate them.
        assert!(c.refused_answerable.is_empty());
    }

    /// NEGATIVE CONTROL for the leak predicate, and the reason run
    /// 20260908-123251 must stay green on this row: an honest abstention on a
    /// retrieval miss is the behaviour we WANT, and a clean win is a win.
    /// A predicate that fired on either would make the lane useless.
    #[test]
    fn an_honest_abstention_on_a_miss_is_not_a_leak() {
        let jsonl = r#"
{"id":"present-killer-weapon","qtype":"present","expected_action":"answer","agent_action":"abstained","answer_excerpt":"I could not confirm","retrieval_present":false,"partition":"retrieval_miss"}
{"id":"absent-embassy","qtype":"absent_adjacent","expected_action":"abstain","agent_action":"abstained","answer_excerpt":"I could not confirm","partition":"abstain_correct"}
{"id":"distract-bomb-maker","qtype":"distractor","expected_action":"answer","agent_action":"answered","answer_excerpt":"The Professor","partition":"correct"}
"#;
        let mut c = Census::default();
        census_chaos(jsonl, &mut c);
        assert!(c.leaked_to_reader.is_empty(), "{:?}", c.leaked_to_reader);
        // The refusal on the answerable probe IS still a finding — the leak
        // row going quiet must not take the refusal row with it.
        assert_eq!(
            c.refused_answerable,
            vec!["present-killer-weapon".to_string()]
        );
        assert!(c.unreadable.is_empty(), "{:?}", c.unreadable);
    }

    /// A `partition` the producer always stamps (`chaos_monkey.rs:1207`), so
    /// its absence is a transcript this lane cannot judge — never a clean one.
    #[test]
    fn a_row_without_a_partition_is_unreadable_not_clean() {
        let jsonl = r#"
{"id":"no-cell","qtype":"present","expected_action":"answer","agent_action":"answered","answer_excerpt":"a knife"}
{"id":"bad-cell","qtype":"present","expected_action":"answer","agent_action":"answered","answer_excerpt":"a knife","partition":"invented_cell"}
"#;
        let mut c = Census::default();
        census_chaos(jsonl, &mut c);
        assert!(c.leaked_to_reader.is_empty());
        assert_eq!(c.unreadable.len(), 2, "{:?}", c.unreadable);
        assert!(c.unreadable[0].contains("no-cell"));
        assert!(c.unreadable[1].contains("invented_cell"));
    }

    #[test]
    fn the_gym_reader_reads_per_fixture_and_an_all_zero_fixture_is_a_catastrophe() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"fixtures":2,"total_replays":6,"total_passes":3,"pass_rate":0.5,
                "total_errored":0,
                "per_fixture":[
                  {"slug":"05_noresults_honesty","passed":3,"errored":0,"replays":3,"pass_rate":1.0},
                  {"slug":"06_fabricated_id_blocked","passed":0,"errored":0,"replays":3,"pass_rate":0.0}]}"#,
        )
        .unwrap();
        let mut c = Census::default();
        census_gym(&v, &mut c);
        assert_eq!(
            c.all_zero(),
            vec!["06_fabricated_id_blocked 0/3".to_string()]
        );
        assert!(c.never_ran.is_empty(), "nothing errored in this report");
    }

    /// The addendum defect, as a test: a fixture whose replays the daemon
    /// REFUSED must not read as a fixture the model failed.
    ///
    /// `Replay::passed()` folded `runner_error` into the failure count, so
    /// three 503s under a peer's load scored `0/3` — the same shape a real
    /// honesty failure makes, and `all-zero tally` is a HARD row. A replay
    /// that never ran is could-not-judge (ARCH §18.3).
    #[test]
    fn gym_replays_that_never_ran_are_could_not_judge_and_never_a_zero_tally() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"fixtures":2,"total_replays":6,"total_passes":3,"pass_rate":1.0,
                "total_errored":3,
                "per_fixture":[
                  {"slug":"01_corpus_definitional","passed":3,"errored":0,"replays":3,"pass_rate":1.0},
                  {"slug":"05_noresults_honesty","passed":0,"errored":3,"replays":3,"pass_rate":null}]}"#,
        )
        .unwrap();
        let mut c = Census::default();
        census_gym(&v, &mut c);

        // The refused fixture contributes NO tally, so it cannot be a zero.
        assert_eq!(
            c.all_zero(),
            Vec::<String>::new(),
            "a fixture whose replays never ran must not appear as scored-zero"
        );
        assert_eq!(
            c.tallies,
            vec![("01_corpus_definitional".to_string(), 3, 3)]
        );
        assert_eq!(
            c.never_ran,
            vec!["05_noresults_honesty: 3 of 3 replay(s) never ran".to_string()]
        );

        // And it reaches the lane as could-not-judge, not as a failure.
        let v = verdicts(&c, LaneReader::KnowledgeGym);
        assert_eq!(find(&v, "replays that ran"), Verdict::CouldNotJudge);
        assert_eq!(
            find(&v, "all-zero tally"),
            Verdict::Passed,
            "the fixture that DID run passed 3/3, and the refused one must not \
             turn this row red"
        );
    }

    /// A partially-refused fixture is judged on what RAN, not on what was
    /// dispatched — otherwise the denominator silently charges the model for
    /// the daemon's refusals.
    #[test]
    fn gym_partial_refusal_scores_only_the_replays_that_ran() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"fixtures":1,"total_replays":3,"total_passes":1,"pass_rate":1.0,
                "total_errored":2,
                "per_fixture":[
                  {"slug":"01_corpus_definitional","passed":1,"errored":2,"replays":3,"pass_rate":1.0}]}"#,
        )
        .unwrap();
        let mut c = Census::default();
        census_gym(&v, &mut c);
        assert_eq!(
            c.tallies,
            vec![("01_corpus_definitional".to_string(), 1, 1)],
            "one replay ran and it passed — 1/1, not 1/3"
        );
        assert_eq!(c.all_zero(), Vec::<String>::new());
        assert_eq!(
            c.never_ran,
            vec!["01_corpus_definitional: 2 of 3 replay(s) never ran".to_string()]
        );
    }

    // ── argv ────────────────────────────────────────────────────────

    #[test]
    fn several_inner_commands_split_on_the_bare_separator() {
        let a = parse_args(&[
            "--id".into(),
            "retrieval-prod".into(),
            "--reader".into(),
            "bench-all".into(),
            "--".into(),
            "svrn".into(),
            "bench".into(),
            "--filter".into(),
            "sep".into(),
            "--".into(),
            "svrn".into(),
            "bench".into(),
            "--filter".into(),
            "wikipedia".into(),
        ])
        .unwrap();
        assert_eq!(a.id, "retrieval-prod");
        assert_eq!(a.reader, LaneReader::BenchAll);
        assert_eq!(a.commands.len(), 2);
        assert_eq!(a.commands[1].last().unwrap(), "wikipedia");
    }

    #[test]
    fn an_unknown_reader_and_a_missing_command_are_both_refused() {
        assert!(parse_args(&[
            "--id".into(),
            "x".into(),
            "--reader".into(),
            "telepathy".into(),
            "--".into(),
            "svrn".into()
        ])
        .is_err());
        assert!(parse_args(&[
            "--id".into(),
            "x".into(),
            "--reader".into(),
            "bench-all".into()
        ])
        .is_err());
    }

    #[test]
    fn the_report_placeholder_is_substituted_and_other_tokens_are_untouched() {
        let cmd: Vec<String> = ["svrn", "bench", "--report", "{report}", "--filter", "sep"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let got = expand(&cmd, Path::new("/tmp/r.json")).unwrap();
        assert_eq!(got[3], "/tmp/r.json");
        assert_eq!(got[5], "sep");
    }

    /// `{ids:…}` with nothing before it has no flag to repeat, and guessing
    /// one would produce a command line nobody wrote.
    #[test]
    fn an_ids_placeholder_with_no_flag_before_it_is_refused() {
        let cmd: Vec<String> = vec!["{ids:whatever}".to_string()];
        assert!(expand(&cmd, Path::new("/tmp/r.json")).is_err());
    }
}
