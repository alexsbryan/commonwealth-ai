// SPDX-License-Identifier: AGPL-3.0-or-later
//! The mechanical pass: four checks, no model.
//!
//! Each check decides one KIND of claim that a transcript can settle
//! without judgement. They exist because the characteristic failure of an
//! agent report is not a false statement about the world — it is a claim
//! whose evidence is absent from the session that produced it, stated in
//! the same flat voice as the claims that have evidence. That difference is
//! a set operation, and a set operation has no false positives to argue
//! about.
//!
//! # What blocks and what only reports
//!
//! Only [`Check::GateClaim`] blocks. "The tests pass" when no test command
//! ran in the session is unambiguous, and the rule has no input it can
//! misread. The other three are advisory until the fixture bank
//! (`quality/report-audit/`) has measured a false-positive rate — a gate
//! whose FP rate nobody has measured is how people learn to reach for
//! `--no-verify` (AGENTS.md, on the advisory ratchets). [`Check::promote`]
//! is the one place that changes.

use super::evidence::Evidence;
use kernel_types::Verdict;
use regex::Regex;
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::OnceLock;

/// The closed set of things this pass knows how to decide (ARCH §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Check {
    /// A `path/to/file.rs` or `file.rs:120` the report cites.
    PathCitation,
    /// "the tests pass", "it compiles", "lint is clean".
    GateClaim,
    /// A backticked `TypeName` or `module::path` the report names.
    SymbolCitation,
    /// A figure in the report that appears nowhere in the session.
    NumberProvenance,
}

impl Check {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Check::PathCitation => "path-citation",
            Check::GateClaim => "gate-claim",
            Check::SymbolCitation => "symbol-citation",
            Check::NumberProvenance => "number-provenance",
        }
    }

    /// Does a finding from this check withhold the turn?
    ///
    /// See the module doc: one check is promoted, and promoting another is
    /// an evidence decision made here, against a measured FP rate.
    #[must_use]
    pub fn promote(self) -> bool {
        matches!(self, Check::GateClaim)
    }
}

/// One claim this pass decided.
#[derive(Debug, Clone)]
pub struct Finding {
    pub check: Check,
    pub verdict: Verdict,
    /// The literal the check ruled on — the path, symbol, figure, phrase.
    pub subject: String,
    /// The sentence it came from, so the reader sees the claim as written.
    pub claim: String,
    pub reason: String,
}

impl Finding {
    /// Findings that withhold the turn: a promoted check that did not pass.
    #[must_use]
    pub fn blocks(&self) -> bool {
        self.check.promote() && matches!(self.verdict, Verdict::Failed | Verdict::NeverRan)
    }
}

// ── Text preparation ────────────────────────────────────────────────────

fn rx(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static pattern"))
}

/// Drop fenced code blocks. A path or symbol inside a code sample is an
/// illustration, not a claim about this repo, and checking it is the
/// cheapest way to manufacture a false positive.
#[must_use]
pub fn strip_code_fences(report: &str) -> String {
    let mut out = String::new();
    let mut fenced = false;
    for line in report.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if !fenced {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Split a report into claims: line first, then sentence.
///
/// The sentence half reuses the deep-research splitter so the audit and the
/// research gate cut prose the same way (ARCH §10.6). The LINE half is this
/// surface's own: a report to the operator is markdown, and a table row or
/// a bullet is a claim on its own. Run on the whole text, the splitter sees
/// a nine-row table as one sentence and quotes the entire table back as
/// "the claim" for each citation inside it — measured on a real report,
/// 2026-09-12, before this line existed.
fn sentences(report: &str) -> Vec<String> {
    report
        .lines()
        .filter(|l| !l.trim().is_empty())
        .flat_map(sovereign_core::deep_research::audit::split_claims)
        .collect()
}

// ── Check 1: path citations ─────────────────────────────────────────────

static PATH_RX: OnceLock<Regex> = OnceLock::new();

/// Placeholder shapes that are illustrations, never citations.
const PLACEHOLDER_SEGMENTS: &[&str] = &["path/to", "foo/", "bar/", "your/", "<", ">", "*", "…"];

fn path_candidates(sentence: &str) -> Vec<(String, Option<usize>)> {
    let re = rx(
        &PATH_RX,
        r"[A-Za-z0-9_.\-/]+\.(?:rs|toml|py|sh|md|json|ts|js|mjs|yml|yaml|sql|lock|txt|jsonl)(?::(\d+))?",
    );
    let mut out = Vec::new();
    for c in re.captures_iter(sentence) {
        let whole = c.get(0).map_or("", |m| m.as_str());
        if PLACEHOLDER_SEGMENTS.iter().any(|p| whole.contains(p)) {
            continue;
        }
        let line = c.get(1).and_then(|m| m.as_str().parse::<usize>().ok());
        let path = whole.split(':').next().unwrap_or(whole).to_string();
        // A bare filename with no directory is too ambiguous to rule on
        // unless the report pinned it to a line.
        if !path.contains('/') && line.is_none() {
            continue;
        }
        out.push((path, line));
    }
    out
}

/// Every tracked file in the repo, once per process.
fn tracked_files(root: &Path) -> &'static Vec<String> {
    static FILES: OnceLock<Vec<String>> = OnceLock::new();
    FILES.get_or_init(|| {
        std::process::Command::new("git")
            .args(["ls-files"])
            .current_dir(root)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// Does exactly one tracked file end with this path?
///
/// The difference between an ABBREVIATED citation and a FABRICATED one.
/// `grounding/judge.rs` does not resolve from the repo root and is not made
/// up — it is `sovereign/crates/sovereign-core/src/runtime/grounding/judge.rs`
/// with the prefix dropped. Reporting both as "no such file" buries the
/// fabrication, which is the finding that matters, under a pile of
/// shorthand. Same move as `citation_attribution`'s SNAP correction: try
/// exact, then try the near match, and say which one you took.
fn unique_suffix_match(root: &Path, path: &str) -> Option<String> {
    let needle = format!("/{}", path.trim_start_matches('/'));
    let mut hits = tracked_files(root)
        .iter()
        .filter(|f| f.ends_with(&needle) || *f == path);
    let first = hits.next()?.clone();
    if hits.next().is_some() {
        return None; // ambiguous: not a correction we can make for them
    }
    Some(first)
}

fn check_paths(report: &str, ev: &Evidence, root: &Path) -> Vec<Finding> {
    let mut seen = BTreeSet::new();
    let mut findings = Vec::new();
    for sentence in sentences(report) {
        for (path, line) in path_candidates(&sentence) {
            if !seen.insert((path.clone(), line)) {
                continue;
            }
            let abs = if Path::new(&path).is_absolute() {
                std::path::PathBuf::from(&path)
            } else {
                root.join(&path)
            };
            let subject = match line {
                Some(n) => format!("{path}:{n}"),
                None => path.clone(),
            };
            let (verdict, reason) = if !abs.exists() {
                match unique_suffix_match(root, &path) {
                    Some(full) => (
                        Verdict::CouldNotJudge,
                        format!("abbreviated: the file is at `{full}`, which is what a reader has to click"),
                    ),
                    None => (Verdict::Failed, "no such file in the repo".to_string()),
                }
            } else if let Some(n) = line {
                let count = std::fs::read_to_string(&abs)
                    .map(|s| s.lines().count())
                    .unwrap_or(0);
                if n == 0 || n > count {
                    (
                        Verdict::Failed,
                        format!("file has {count} lines; the citation names {n}"),
                    )
                } else if ev.mentions(&path) {
                    (Verdict::Passed, "opened in this session".to_string())
                } else {
                    (
                        Verdict::CouldNotJudge,
                        "the file exists but nothing in this session opened it — recalled, not cited"
                            .to_string(),
                    )
                }
            } else if ev.mentions(&path) {
                (Verdict::Passed, "opened in this session".to_string())
            } else {
                (
                    Verdict::CouldNotJudge,
                    "the file exists but nothing in this session opened it — recalled, not cited"
                        .to_string(),
                )
            };
            {
                findings.push(Finding {
                    check: Check::PathCitation,
                    verdict,
                    subject,
                    claim: sentence.trim().to_string(),
                    reason,
                });
            }
        }
    }
    findings
}

// ── Check 2: gate claims ────────────────────────────────────────────────

/// A family of commands that can settle one kind of verification claim.
struct GateFamily {
    name: &'static str,
    /// Phrases in a report that assert this family ran and passed.
    phrases: &'static [&'static str],
    /// Command substrings that count as a run of this family.
    commands: &'static [&'static str],
}

const FAMILIES: &[GateFamily] = &[
    GateFamily {
        name: "test",
        phrases: &[
            "tests pass",
            "tests passed",
            "test suite passes",
            "suite passes",
            "all tests",
            "tests are green",
            "tests still pass",
            "test run is clean",
        ],
        commands: &[
            "sovereign-test.sh",
            "cargo test",
            "cargo nextest",
            "npm test",
            "npm run test",
            "pytest",
            "vitest",
        ],
    },
    GateFamily {
        name: "build",
        phrases: &[
            "it compiles",
            "compiles clean",
            "it builds",
            "builds clean",
            "build is clean",
            "lint is clean",
            "lint clean",
            "clippy clean",
            "no warnings",
            "type-checks",
            "typechecks",
        ],
        commands: &[
            "sovereign-lint.sh",
            "pre-push.sh",
            "cargo build",
            "cargo check",
            "cargo clippy",
            "npm run check",
            "xtask quality",
            "tsc",
        ],
    },
];

/// Banners a run prints when it measured nothing. A command that exits 0
/// having run no tests is the same defect as `sovereign-test.sh`'s exit 4:
/// an instrument that said nothing verified nothing (ARCH §18.1). Without
/// this, `cargo test --filter typo` grounds "all tests pass".
const EMPTY_RUN_MARKERS: &[&str] = &[
    "pass: 0 fail: 0",
    "running 0 tests",
    "0 passed",
    "no tests to run",
    "0 tests run",
    "no test target",
];

fn measured_nothing(c: &super::evidence::Command) -> bool {
    let hay = format!("{}\n{}", c.stdout, c.stderr).to_lowercase();
    EMPTY_RUN_MARKERS.iter().any(|m| hay.contains(m))
}

fn check_gates(report: &str, ev: &Evidence) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut fired: BTreeSet<&str> = BTreeSet::new();
    for sentence in sentences(report) {
        let lower = sentence.to_lowercase();
        for fam in FAMILIES {
            let Some(phrase) = fam.phrases.iter().find(|p| lower.contains(**p)) else {
                continue;
            };
            if !fired.insert(fam.name) {
                continue;
            }
            let runs = ev.commands_matching(fam.commands);
            let (verdict, reason) = if runs.is_empty() {
                (
                    Verdict::NeverRan,
                    format!(
                        "no {} command ran in this session (looked for: {})",
                        fam.name,
                        fam.commands.join(", ")
                    ),
                )
            } else if runs.iter().all(|c| c.is_error) {
                (
                    Verdict::Failed,
                    format!(
                        "the {} run in this session returned an error: `{}`",
                        fam.name,
                        runs[0].line.lines().next().unwrap_or("")
                    ),
                )
            } else if runs
                .iter()
                .filter(|c| !c.is_error)
                .all(|c| measured_nothing(c))
            {
                (
                    Verdict::NeverRan,
                    format!(
                        "the {} run exited clean having measured nothing: `{}`",
                        fam.name,
                        runs[0].line.lines().next().unwrap_or("")
                    ),
                )
            } else {
                let who = runs
                    .iter()
                    .find(|c| !c.is_error && !measured_nothing(c))
                    .map_or("", |c| c.line.lines().next().unwrap_or(""));
                (
                    Verdict::Passed,
                    format!("grounded in this session's run of `{who}`"),
                )
            };
            {
                findings.push(Finding {
                    check: Check::GateClaim,
                    verdict,
                    subject: (*phrase).to_string(),
                    claim: sentence.trim().to_string(),
                    reason,
                });
            }
        }
    }
    findings
}

// ── Check 3: symbol citations ───────────────────────────────────────────

static SYMBOL_RX: OnceLock<Regex> = OnceLock::new();
static CAMEL_RX: OnceLock<Regex> = OnceLock::new();

/// Backticked tokens that look like Rust type or path references.
fn symbol_candidates(sentence: &str) -> Vec<String> {
    let ticks = rx(&SYMBOL_RX, r"`([A-Za-z0-9_:<>]+)`");
    let camel = rx(&CAMEL_RX, r"^[A-Z][a-z0-9]+[A-Z][A-Za-z0-9]*$");
    let mut out = Vec::new();
    for c in ticks.captures_iter(sentence) {
        let tok = c.get(1).map_or("", |m| m.as_str());
        let bare = tok.trim_end_matches("<>").trim_matches(':');
        if bare.len() < 4 {
            continue;
        }
        if bare.contains("::") || camel.is_match(bare) {
            out.push(bare.to_string());
        }
    }
    out
}

/// `git grep -F -l` for one literal. `None` when git itself could not run —
/// absence of an answer is never reported as an answer (ARCH §18.3).
fn in_repo(root: &Path, needle: &str) -> Option<bool> {
    let out = std::process::Command::new("git")
        .args(["grep", "-F", "-l", "-e", needle])
        .current_dir(root)
        .output()
        .ok()?;
    // git grep exits 1 for "no match", >1 for a real error.
    match out.status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

fn check_symbols(report: &str, ev: &Evidence, root: &Path) -> Vec<Finding> {
    let mut seen = BTreeSet::new();
    let mut findings = Vec::new();
    for sentence in sentences(report) {
        for sym in symbol_candidates(&sentence) {
            if ev.mentions(&sym) || ev.operator_supplied(&sym) {
                continue; // grounded in the session; no subprocess needed
            }
            if !seen.insert(sym.clone()) {
                continue;
            }
            let (verdict, reason) = match in_repo(root, &sym) {
                Some(true) => (
                    Verdict::CouldNotJudge,
                    "the symbol exists but nothing in this session looked it up — recalled, not cited"
                        .to_string(),
                ),
                Some(false) => (
                    Verdict::Failed,
                    "no definition or use of this name anywhere in the repo".to_string(),
                ),
                None => (
                    Verdict::CouldNotJudge,
                    "git grep could not run, so this name was not checked".to_string(),
                ),
            };
            findings.push(Finding {
                check: Check::SymbolCitation,
                verdict,
                subject: sym,
                claim: sentence.trim().to_string(),
                reason,
            });
        }
    }
    findings
}

// ── Check 4: number provenance ──────────────────────────────────────────

static NUMBER_RX: OnceLock<Regex> = OnceLock::new();

fn check_numbers(report: &str, ev: &Evidence) -> Vec<Finding> {
    // Three digits or a decimal point. Smaller integers are ordinals,
    // counts of list items and versions — too common to rule on, and the
    // sabotage cases that matter are measurements.
    let re = rx(
        &NUMBER_RX,
        r"\b\d{1,3}(?:,\d{3})+\b|\b\d{3,}\b|\b\d+\.\d+\b",
    );
    let mut seen = BTreeSet::new();
    let mut findings = Vec::new();
    for sentence in sentences(report) {
        // A figure inside a path citation is a line number, already ruled on.
        let scrubbed = rx(
            &PATH_RX,
            r"[A-Za-z0-9_.\-/]+\.(?:rs|toml|py|sh|md|json|ts|js|mjs|yml|yaml|sql|lock|txt|jsonl)(?::(\d+))?",
        )
        .replace_all(&sentence, " ");
        for m in re.find_iter(&scrubbed) {
            let tok = m.as_str();
            let plain = tok.replace(',', "");
            // A year is prose, not a measurement.
            if plain.len() == 4 && (plain.starts_with("19") || plain.starts_with("20")) {
                continue;
            }
            if ev.mentions(tok)
                || ev.mentions(&plain)
                || ev.operator_supplied(tok)
                || ev.operator_supplied(&plain)
            {
                continue;
            }
            if !seen.insert(plain.clone()) {
                continue;
            }
            findings.push(Finding {
                check: Check::NumberProvenance,
                verdict: Verdict::CouldNotJudge,
                subject: tok.to_string(),
                claim: sentence.trim().to_string(),
                reason: "this figure appears nowhere in the session's tool output".to_string(),
            });
        }
    }
    findings
}

// ── The pass ────────────────────────────────────────────────────────────

/// Run every check over one report against one evidence pool.
#[must_use]
pub fn run(report: &str, ev: &Evidence, root: &Path) -> Vec<Finding> {
    let prose = strip_code_fences(report);
    let mut all = check_gates(&prose, ev);
    all.extend(check_paths(&prose, ev, root));
    all.extend(check_symbols(&prose, ev, root));
    all.extend(check_numbers(&prose, ev));
    all
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report_audit::evidence;

    fn ev_with(cmd: &str, stdout: &str, is_error: bool) -> Evidence {
        let t = format!(
            concat!(
                r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"t1","name":"Bash","input":{{"command":{cmd}}}}}]}}}}"#,
                "\n",
                r#"{{"type":"user","toolUseResult":{{"stdout":{out},"stderr":""}},"message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t1","is_error":{err},"content":{out}}}]}}}}"#,
            ),
            cmd = serde_json::to_string(cmd).unwrap(),
            out = serde_json::to_string(stdout).unwrap(),
            err = is_error,
        );
        evidence::from_jsonl(&t, "sess")
    }

    fn empty() -> Evidence {
        Evidence::default()
    }

    /// The findings a caller would act on — what `mod.rs` keeps.
    fn issues(f: Vec<Finding>) -> Vec<Finding> {
        f.into_iter()
            .filter(|x| x.verdict != Verdict::Passed)
            .collect()
    }

    #[test]
    fn tests_pass_with_no_test_run_is_never_ran_and_blocks() {
        let f = check_gates("The change is in. All tests pass.", &empty());
        assert_eq!(f.len(), 1, "one gate claim");
        assert_eq!(f[0].verdict, Verdict::NeverRan);
        assert!(f[0].blocks(), "an unrun gate claim withholds the turn");
        assert!(f[0].reason.contains("no test command ran"));
    }

    #[test]
    fn tests_pass_with_a_clean_test_run_is_silent() {
        let ev = ev_with(
            "./scripts/sovereign-test.sh --human",
            "pass: 12 fail: 0",
            false,
        );
        // The check KEEPS its passing ruling so `--all` can show the
        // decision (ARCH §1); `mod.rs` drops it before anything counts it
        // as a finding. "Silent" therefore means: nothing non-passing.
        assert!(issues(check_gates("All tests pass.", &ev)).is_empty());
    }

    #[test]
    fn tests_pass_over_an_errored_run_is_failed() {
        let ev = ev_with("cargo test -p foo", "error", true);
        let f = check_gates("All tests pass.", &ev);
        assert_eq!(f[0].verdict, Verdict::Failed);
        assert!(f[0].blocks());
    }

    #[test]
    fn a_clean_run_that_measured_nothing_is_never_ran() {
        let ev = ev_with(
            "./scripts/sovereign-test.sh --filter typo",
            "pass: 0 fail: 0",
            false,
        );
        let f = check_gates("All tests pass.", &ev);
        assert_eq!(f[0].verdict, Verdict::NeverRan);
        assert!(f[0].blocks());
        assert!(f[0].reason.contains("measured nothing"));
    }

    #[test]
    fn a_build_claim_is_not_settled_by_a_test_run() {
        let ev = ev_with("cargo test -p foo", "ok", false);
        let f = check_gates("It compiles.", &ev);
        assert_eq!(f[0].verdict, Verdict::NeverRan, "families are separate");
    }

    #[test]
    fn a_fabricated_path_is_failed() {
        let root = std::path::PathBuf::from(".");
        let f = check_paths("Fixed in src/definitely/not/here.rs.", &empty(), &root);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].verdict, Verdict::Failed);
        assert_eq!(f[0].subject, "src/definitely/not/here.rs");
    }

    #[test]
    fn a_line_past_the_end_of_a_real_file_is_failed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "one\ntwo\n").unwrap();
        let f = check_paths("See src/a.rs:900.", &empty(), dir.path());
        assert_eq!(f[0].verdict, Verdict::Failed);
        assert!(f[0].reason.contains("2 lines"));
    }

    #[test]
    fn a_real_path_nobody_opened_is_could_not_judge_not_failed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "one\n").unwrap();
        let f = check_paths("See src/a.rs:1.", &empty(), dir.path());
        assert_eq!(f[0].verdict, Verdict::CouldNotJudge);
        assert!(!f[0].blocks(), "advisory until the FP rate is measured");
    }

    #[test]
    fn a_real_path_the_session_opened_is_silent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "one\n").unwrap();
        let ev = ev_with("cat src/a.rs", "one", false);
        assert!(issues(check_paths("See src/a.rs:1.", &ev, dir.path())).is_empty());
    }

    #[test]
    fn placeholder_paths_are_not_citations() {
        let root = std::path::PathBuf::from(".");
        assert!(check_paths("Put it in path/to/file.rs.", &empty(), &root).is_empty());
    }

    #[test]
    fn code_fences_are_not_claims() {
        let report =
            "Here is the shape:\n```\nlet x = load(\"src/nowhere/at/all.rs\");\n```\nDone.";
        let root = std::path::PathBuf::from(".");
        assert!(check_paths(&strip_code_fences(report), &empty(), &root).is_empty());
    }

    #[test]
    fn an_unsourced_measurement_is_flagged() {
        let f = check_numbers("The sweep took 4,812 ms.", &empty());
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].subject, "4,812");
        assert_eq!(f[0].verdict, Verdict::CouldNotJudge);
    }

    #[test]
    fn a_measurement_the_session_printed_is_silent() {
        let ev = ev_with("./bench.sh", "elapsed 4812 ms", false);
        assert!(
            check_numbers("The sweep took 4,812 ms.", &ev).is_empty(),
            "comma formatting must not hide a real figure"
        );
    }

    #[test]
    fn small_integers_and_years_are_not_measurements() {
        assert!(check_numbers("Two of the 12 lanes, as of 2026.", &empty()).is_empty());
    }

    #[test]
    fn symbol_candidates_take_backticked_types_only() {
        let got = symbol_candidates("`ToolRegistry` and CamelCaseProse and `x` and `a::b`.");
        assert_eq!(got, vec!["ToolRegistry".to_string(), "a::b".to_string()]);
    }

    #[test]
    fn a_symbol_the_session_printed_is_not_grepped() {
        let ev = ev_with("grep -n ToolRegistry", "hit", false);
        let root = std::path::PathBuf::from("/definitely/not/a/repo");
        assert!(check_symbols("It lives in `ToolRegistry`.", &ev, &root).is_empty());
    }
}

// ── The denominator ─────────────────────────────────────────────────────

/// How many claims in this report are of a kind the pass knows how to
/// decide.
///
/// The denominator behind every finding count, and the reason a report with
/// no decidable claim is judged `never-ran` rather than passed: a pass over
/// nothing verified nothing. Counts CANDIDATES, not findings — a citation
/// that checked out is decided and belongs in the denominator. Enumeration
/// only, so it touches neither the filesystem nor git.
#[must_use]
pub fn decidable_count(report: &str, ev: &Evidence, _root: &Path) -> usize {
    let prose = strip_code_fences(report);
    let mut n = 0;
    let mut fired: BTreeSet<&str> = BTreeSet::new();
    let num = rx(
        &NUMBER_RX,
        r"\b\d{1,3}(?:,\d{3})+\b|\b\d{3,}\b|\b\d+\.\d+\b",
    );
    for sentence in sentences(&prose) {
        n += path_candidates(&sentence).len();
        n += symbol_candidates(&sentence).len();
        let lower = sentence.to_lowercase();
        for fam in FAMILIES {
            if fam.phrases.iter().any(|p| lower.contains(*p)) && fired.insert(fam.name) {
                n += 1;
            }
        }
        for m in num.find_iter(&sentence) {
            let plain = m.as_str().replace(',', "");
            if plain.len() == 4 && (plain.starts_with("19") || plain.starts_with("20")) {
                continue;
            }
            if ev.operator_supplied(m.as_str()) {
                continue;
            }
            n += 1;
        }
    }
    n
}

#[cfg(test)]
mod denominator_tests {
    use super::*;

    #[test]
    fn a_report_with_no_decidable_claim_counts_zero() {
        let ev = Evidence::default();
        let root = std::path::PathBuf::from(".");
        assert_eq!(
            decidable_count(
                "I reorganised the argument and it reads better now.",
                &ev,
                &root
            ),
            0
        );
    }

    #[test]
    fn a_passing_citation_still_counts_in_the_denominator() {
        let ev = Evidence::default();
        let root = std::path::PathBuf::from(".");
        assert!(decidable_count("See src/a.rs:12 for the fix.", &ev, &root) >= 1);
    }
}
