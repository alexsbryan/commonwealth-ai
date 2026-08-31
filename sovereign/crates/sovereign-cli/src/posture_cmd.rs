// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn posture` — one read-only roll-up of every posture-bearing subsystem.
//!
//! WHY A VERB. Each quality subsystem here keeps its own freshness artifact —
//! drift reports, arch reports, capability maps, the CLI-contract nightly,
//! the watcher heartbeat, the env-gate baseline, bench baselines — and each
//! can be asked individually, which in practice means none of them are: on
//! 2026-07-29 the drift AND arch reports were both weeks stale and nothing
//! anywhere aggregated that fact. A corner that can only rot silently will.
//! This verb is the aggregation: one table, one row per subsystem, each row
//! naming its artifact's age and the command that refreshes it.
//!
//! READ-ONLY by contract: every row reads an artifact some other command
//! wrote; nothing here computes, refreshes, or mutates. Sources register in
//! [`sources`] — a new posture-bearing subsystem adds a row there rather
//! than being forgotten (the registry-over-memory pattern, ARCH §4).
//!
//! # A row is a [`Judgement`], and that is the point
//!
//! This verb was `NOUN_CONVERGENCE.md` §10.1's first exhibit: seven rows
//! printed in **seven status vocabularies** (`fresh`, `stale`, `fail (stale)`,
//! `off (by design)`, `present`, `present (gaps)`) and **seven age formats**
//! (`12d`, `1h`, `16d`, `7d ago`, `-`, `6d`, `9d..95d`). An aggregator over a
//! concept with no type, so every subsystem it aggregates invented its own.
//!
//! Each row is now a `kernel_types::Judgement`: one of four verdicts (ARCH
//! §18.2), a reason it cannot omit, and — where an artifact backs it — a date
//! and a horizon. Three rows got MORE honest in the conversion, not merely
//! more uniform:
//!
//! - `bench-baselines` printed `present (gaps)` while its own comment argued
//!   that a bank with no baseline "can only ever report first-run, which is a
//!   could-not-judge, not a pass". It now says could-not-judge.
//! - `contract-nightly` printed the lane's raw verdict string, so a `fail`
//!   was a word no aggregation could count. It is now `Verdict::Failed` and
//!   the footer counts it.
//! - `no repo context` was a seventh vocabulary for "I could not judge this".
//!
//! `STALE_AFTER_DAYS` was also compared at two sites in this file — one
//! threshold, two implementations (ARCH §10.6). It is now a horizon set once
//! and banded by the type.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use kernel_types::{honesty_footer, render_rows, Judgement, Reason};

/// The registry: every posture-bearing subsystem, in display order.
/// `repo` is the enclosing source checkout, when the CWD is inside one —
/// repo-scoped sources degrade to an honest could-not-judge without it.
fn sources(repo: Option<&Path>) -> Vec<Judgement> {
    vec![
        drift_row(),
        arch_row(),
        capability_row(),
        contract_nightly_row(),
        watcher_row(repo),
        env_gate_row(repo),
        oicp_conformance_row(),
        bench_baselines_row(repo),
    ]
}

/// The OICP v0.4 wire contract, as last certified against a live host.
///
/// The lane is `scripts/oicp-conformance-lane.sh`, scheduled through
/// `scripts/run-if-stale.sh oicp-conformance`. It drives the certifier in
/// `commonwealth/crates/oicp-conformance` against the committed baseline at
/// `quality/baselines/oicp/`, so this row answers "does this host still speak
/// the protocol it did last time", which no other row covers.
///
/// The lane's word is mapped here rather than re-derived: `could-not-judge` is
/// its own verdict because a stopped or model-less daemon cannot be told from
/// a broken one by exit code alone, and calling that a failure is how a gate
/// stops being read.
fn oicp_conformance_row() -> Judgement {
    let path = sovereign_cli_shared::dirs::sovereign_root()
        .join("oicp-conformance")
        .join("latest.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Judgement::never_ran(
            "oicp-conformance",
            Reason::literal(
                "the OICP wire contract has never been certified on this host — \
                 run: scripts/oicp-conformance-lane.sh",
            ),
        );
    };
    let field = |k: &str| -> Option<String> {
        let pat = format!("\"{k}\":\"");
        let rest = text.split_once(&pat)?.1;
        Some(rest.split_once('"')?.0.to_string())
    };
    let verdict = field("verdict").unwrap_or_default();
    let summary = field("summary").unwrap_or_else(|| "no summary".into());
    let name = "oicp-conformance";
    let reason = |s: String| Reason::new(s).expect("a lane summary is never a placeholder");
    let j = match verdict.as_str() {
        "pass" => Judgement::passed(name, reason(format!("{summary} vs quality/baselines/oicp"))),
        "fail" => Judgement::failed(
            name,
            reason(format!("{summary} — detail: scripts/oicp-conformance-lane.sh")),
        ),
        // Never collapse an unknown word into a verdict: a lane that starts
        // emitting a word this row does not know must say so, not pick one.
        other => Judgement::could_not_judge(
            name,
            reason(format!(
                "lane reported `{}` — {summary}",
                if other.is_empty() { "no verdict" } else { other }
            )),
        ),
    };
    match mtime(&path) {
        Some(at) => j.as_of(at).stale_after(REPORT_HORIZON),
        None => j,
    }
}

pub async fn run(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP);
        return 0;
    }
    let repo = find_repo_root();
    // Payload to stdout — this is a report, not narration.
    println!("── posture — artifact age per quality subsystem (read-only) ──");
    let rows = sources(repo.as_deref());
    print!("{}", render_rows(&rows));
    match &repo {
        Some(r) => println!("  repo context: {}", r.display()),
        None => println!("  repo context: none (repo-scoped rows degrade; run from a checkout)"),
    }
    if let Some(footer) = honesty_footer(&rows) {
        println!("  {footer} — each row names its refresh command");
    }
    0
}

// ─── Per-user artifact rows ─────────────────────────────────────────

/// Staleness horizon for report-shaped artifacts: older than this and the
/// row SAYS stale. Two weeks — generous against the observed rot mode
/// (reports going a month unnoticed), tight enough to prompt a refresh.
///
/// One threshold, one decider (ARCH §10.6). It used to be compared by hand in
/// `aged_artifact_row` AND `per_corpus_newest`; it is now handed to
/// `Judgement::stale_after` and the banding happens in one place.
const REPORT_HORIZON: Duration = Duration::from_secs(14 * 86_400);

fn drift_row() -> Judgement {
    let path = sovereign_contracts::rebrand::drift_dir().join("latest.md.json");
    aged_artifact_row(
        "drift",
        &path,
        "narrative-vs-code drift report — refresh: sovereign drift detect",
    )
}

fn arch_row() -> Judgement {
    per_corpus_newest(
        "arch",
        &sovereign_contracts::rebrand::data_dir().join("arch"),
        "arch_report.json",
        "architectural census — refresh: sovereign code arch-report",
    )
}

fn capability_row() -> Judgement {
    per_corpus_newest(
        "capability",
        &sovereign_contracts::rebrand::data_dir().join("capabilities"),
        "capability_map.json",
        "capability map — refresh: sovereign code capability-map",
    )
}

fn contract_nightly_row() -> Judgement {
    // The trigger comes from the same decider `svrn contract nightly` uses, so
    // the two surfaces cannot disagree about what schedules this lane. Naming
    // it in the row is the fix for a row that implied a daily cadence on a
    // host where nothing scheduled the lane at all.
    let trigger = sovereign_cli_shared::cli_contract_report::nightly_trigger();
    match sovereign_cli_shared::cli_contract_report::nightly_posture() {
        // The lane's word -> one of the four is decided ONCE, by the module
        // that owns the lane's vocabulary. This row used to map it here as
        // well, which is how `svrn posture` and `svrn contract nightly` came
        // to render the same lane two different ways.
        Some(n) => n.judgement(),
        None => Judgement::never_ran(
            "contract-nightly",
            Reason::new(format!(
                "no journey-lane verdict on this host (trigger: {}) — run: scripts/cli-journey-nightly.sh",
                trigger.label()
            ))
            .expect("a trigger label and a run command is never a placeholder"),
        ),
    }
}

fn watcher_row(repo: Option<&Path>) -> Judgement {
    // Honor the off-by-design posture FIRST: a repo that declares
    // `[watchers] enabled = false` has a healthy absence, not a fault.
    if let Some(repo) = repo {
        if let Ok(t) = std::fs::read_to_string(repo.join(".sovereign/sovereign.toml")) {
            if let Ok(v) = t.parse::<toml::Value>() {
                if v.get("watchers")
                    .and_then(|w| w.get("enabled"))
                    .and_then(|e| e.as_bool())
                    == Some(false)
                {
                    // A declared opt-out is a healthy absence, and `doctor`
                    // already reports it as Passed. Same answer here, so the
                    // two surfaces cannot disagree about what the opt-out
                    // MEANS — which is what a seventh vocabulary word
                    // ("off (by design)") left open.
                    return Judgement::passed(
                        "watchers",
                        Reason::literal(
                            "this repo opts out ([watchers] enabled = false); \
                             gate = the two toolbox scripts",
                        ),
                    );
                }
            }
        }
    }
    aged_artifact_row(
        "watchers",
        &sovereign_contracts::rebrand::data_dir().join("watcher-heartbeat"),
        "lint/test watcher heartbeat sidecar",
    )
}

// ─── Repo-scoped rows ───────────────────────────────────────────────

fn env_gate_row(repo: Option<&Path>) -> Judgement {
    let Some(repo) = repo else {
        return no_repo_row("env-gate");
    };
    let path = repo.join("quality/baselines/env_unregistered.txt");
    match std::fs::read_to_string(&path) {
        Ok(t) => {
            let count = t
                .lines()
                .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
                .count();
            let j = Judgement::passed(
                "env-gate",
                Reason::new(format!(
                    "{count} legacy env vars riding the shrink-only baseline — \
                     burn down via quality/env-flags.toml + `env-gate --tighten`"
                ))
                .expect("a count and a burn-down command is never a placeholder"),
            );
            // The baseline is a checked-in ratchet, not a report: it does not
            // rot on a two-week clock, so it is dated without a horizon and
            // reads `passed` at any age.
            match mtime(&path) {
                Some(at) => j.as_of(at),
                None => j,
            }
        }
        Err(_) => Judgement::never_ran(
            "env-gate",
            Reason::literal("no baseline — run: cargo run -p xtask -- env-gate --update-baseline"),
        ),
    }
}

fn bench_baselines_row(repo: Option<&Path>) -> Judgement {
    let Some(repo) = repo else {
        return no_repo_row("bench-baselines");
    };
    let mut latest: Vec<(PathBuf, SystemTime)> = Vec::new();
    let bench_root = repo.join("sovereign/bench");
    if let Ok(lanes) = std::fs::read_dir(&bench_root) {
        for lane in lanes.flatten() {
            let baselines = lane.path().join("baselines");
            collect_latest_json(&baselines, &mut latest);
        }
    }
    if latest.is_empty() {
        return Judgement::never_ran(
            "bench-baselines",
            Reason::new(format!("no latest.json under {}", bench_root.display()))
                .expect("a path is never a placeholder"),
        );
    }
    let oldest = latest.iter().map(|(_, t)| *t).min().expect("non-empty");
    let newest = latest.iter().map(|(_, t)| *t).max().expect("non-empty");

    // COVERAGE, not just age. Until 2026-08-04 this row counted every
    // `latest.json` on disk and called them all "committed" — so a
    // baseline a bench run had just written locally, in a gitignored
    // directory, was indistinguishable from one peers can reproduce.
    // Both gaps below are invisible to an mtime-only view, and together
    // they are what let a conversation-retrieval default ship ungated
    // (note d2af7720): the only bank that could have measured it was
    // gitignored, and the run that "checked" it silently minted its own
    // reference from the very change under test.
    let paths: Vec<PathBuf> = latest.iter().map(|(p, _)| p.clone()).collect();
    let ignored = git_ignored(repo, &paths);
    let local_only = paths.iter().filter(|p| ignored.contains(*p)).count();
    let committed = latest.len() - local_only;
    let unmeasured = banks_without_baselines(&bench_root);

    let mut detail = format!(
        "{committed} committed baseline(s), newest {}",
        kernel_types::Judgement::passed("newest", Reason::literal("age probe"))
            .as_of(newest)
            .age_label()
    );
    if local_only > 0 {
        detail.push_str(&format!(
            ", {local_only} LOCAL-ONLY (gitignored — gates nothing for peers)"
        ));
    }
    if !unmeasured.is_empty() {
        // Name a few; the count carries the rest. A bank with no
        // baseline can only ever report `first-run`, which is a
        // could-not-judge, not a pass.
        const SHOWN: usize = 4;
        let head = unmeasured
            .iter()
            .take(SHOWN)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let more = unmeasured.len().saturating_sub(SHOWN);
        detail.push_str(&format!(
            "; {} bank(s) with NO baseline (first-run only): {head}{}",
            unmeasured.len(),
            if more > 0 {
                format!(", +{more} more")
            } else {
                String::new()
            }
        ));
    }
    detail.push_str("; recapture via `svrn bench gate <lane> --update-baseline`");

    let reason = Reason::new(detail)
        .expect("a baseline count and a recapture command is never a placeholder");
    // `present (gaps)` was the seventh vocabulary word and it understated the
    // finding. The comment above already reasons that a bank with no baseline
    // "can only ever report `first-run`, which is a could-not-judge, not a
    // pass" — the row simply had no way to SAY could-not-judge. It does now,
    // and the footer counts it.
    let j = if local_only > 0 || !unmeasured.is_empty() {
        Judgement::could_not_judge("bench-baselines", reason)
    } else {
        Judgement::passed("bench-baselines", reason)
    };
    // Dated to the OLDEST baseline, never the newest: an aggregate is not
    // fresher than its stalest input. `newest` still rides the reason so the
    // range the row used to print is not lost.
    let _ = newest;
    j.as_of(oldest).stale_after(REPORT_HORIZON)
}

/// Which of `paths` git ignores, as one batched `git check-ignore
/// --stdin` call. The row's whole point is telling a baseline peers can
/// reproduce apart from a local artifact that exists only here, and
/// mtime cannot distinguish them.
///
/// Any failure (git absent, not a checkout) yields an empty set, so the
/// row degrades to the filesystem view it has always shown rather than
/// inventing a verdict. `check-ignore` exits 1 for "nothing ignored" —
/// that is a normal result, not an error.
fn git_ignored(repo: &Path, paths: &[PathBuf]) -> std::collections::HashSet<PathBuf> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    let Ok(mut child) = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["check-ignore", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return std::collections::HashSet::new();
    };
    if let Some(mut stdin) = child.stdin.take() {
        for p in paths {
            let _ = writeln!(stdin, "{}", p.display());
        }
        // Dropped here: check-ignore reads to EOF before exiting.
    }
    let Ok(out) = child.wait_with_output() else {
        return std::collections::HashSet::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| PathBuf::from(l.trim()))
        .collect()
}

/// Bank directories under `sovereign/bench` that carry a question bank
/// (`*.toml`) but no baseline at all. A run against one of these can
/// only report `first-run` — and `bench all` then WRITES a baseline
/// from that same run, so the next run compares the change against
/// itself. Surfacing them here is what makes that structural rather
/// than something each engineer has to notice.
fn banks_without_baselines(bench_root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(lanes) = std::fs::read_dir(bench_root) else {
        return out;
    };
    for lane in lanes.flatten() {
        let dir = lane.path();
        if !dir.is_dir() {
            continue;
        }
        let has_bank = std::fs::read_dir(&dir)
            .map(|es| {
                es.flatten()
                    .any(|e| e.path().extension().is_some_and(|x| x == "toml"))
            })
            .unwrap_or(false);
        if !has_bank {
            continue;
        }
        let mut found = Vec::new();
        collect_latest_json(&dir.join("baselines"), &mut found);
        if found.is_empty() {
            if let Some(n) = dir.file_name().and_then(|n| n.to_str()) {
                out.push(n.to_string());
            }
        }
    }
    out.sort();
    out
}

/// `<dir>/**/latest.json`, one level of nesting (lane/baselines/<key>/latest.json).
fn collect_latest_json(dir: &Path, out: &mut Vec<(PathBuf, SystemTime)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_latest_json(&p, out);
        } else if p.file_name().is_some_and(|n| n == "latest.json") {
            if let Some(age) = std::fs::metadata(&p).ok().and_then(|m| m.modified().ok()) {
                out.push((p, age));
            }
        }
    }
}

// ─── Shared row builders ────────────────────────────────────────────

fn aged_artifact_row(name: &'static str, path: &Path, what: &str) -> Judgement {
    match mtime(path) {
        // The artifact exists, so the check ran. Whether it is still worth
        // quoting is the FRESHNESS band, not a second verdict word — which is
        // what `fresh`/`stale` conflated: `stale` erased the fact that the
        // report is there, `fresh` erased what it is.
        Some(at) => Judgement::passed(
            name,
            Reason::new(what).expect("a row description is never a placeholder"),
        )
        .as_of(at)
        .stale_after(REPORT_HORIZON),
        None => Judgement::never_ran(
            name,
            Reason::new(format!("{what} — expected at {}", path.display()))
                .expect("a row description and a path is never a placeholder"),
        ),
    }
}

/// Newest `<root>/<corpus>/<artifact>` across corpora subdirs.
fn per_corpus_newest(name: &'static str, root: &Path, artifact: &str, what: &str) -> Judgement {
    let mut newest: Option<(String, SystemTime)> = None;
    let mut corpora = 0usize;
    if let Ok(entries) = std::fs::read_dir(root) {
        for e in entries.flatten() {
            let p = e.path().join(artifact);
            if let Some(at) = mtime(&p) {
                corpora += 1;
                let corpus = e.file_name().to_string_lossy().into_owned();
                if newest.as_ref().map_or(true, |(_, t)| at > *t) {
                    newest = Some((corpus, at));
                }
            }
        }
    }
    match newest {
        Some((corpus, at)) => Judgement::passed(
            name,
            Reason::new(format!(
                "{what} (newest of {corpora} corpus dir(s): {corpus})"
            ))
            .expect("a row description is never a placeholder"),
        )
        .as_of(at)
        .stale_after(REPORT_HORIZON),
        None => Judgement::never_ran(
            name,
            Reason::new(format!("{what} — expected under {}", root.display()))
                .expect("a row description and a path is never a placeholder"),
        ),
    }
}

/// A repo-scoped row with no checkout under the CWD. This is a
/// could-not-judge and always was — "no repo context" was a seventh
/// vocabulary word for exactly that, which no footer could count.
fn no_repo_row(name: &'static str) -> Judgement {
    Judgement::could_not_judge(
        name,
        Reason::literal("repo-scoped artifact; run from a source checkout"),
    )
}

// ─── Small helpers ──────────────────────────────────────────────────

/// When the artifact was last written. Age computation and formatting are
/// [`Judgement`]'s job now — this file reads a date and hands it over.
fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

/// Walk up from the CWD to the enclosing checkout: the dir holding both
/// `quality/` and `sovereign/` (this monorepo's shape — cheap and specific,
/// no git dependency).
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

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "svrn posture",
    summary: "One read-only roll-up: artifact age + verdict per quality subsystem.",
    sections: &[
        crate::util::help::HelpSection::Usage("svrn posture"),
        crate::util::help::HelpSection::Examples(&[(
            "svrn posture",
            "drift / arch / capability / contract-nightly / watchers / env-gate / bench rows",
        )]),
    ],
};
