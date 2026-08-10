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

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// One subsystem's posture line.
struct PostureRow {
    /// Subsystem name (stable, greppable).
    name: &'static str,
    /// Honest one-word-ish verdict: `fresh`/`present`/`stale`/`never_run`/
    /// `off (by design)`/`no repo context`/…
    verdict: String,
    /// Age of the newest artifact, when one exists.
    age: Option<String>,
    /// One line of detail + the refresh command where it isn't obvious.
    detail: String,
}

/// The registry: every posture-bearing subsystem, in display order.
/// `repo` is the enclosing source checkout, when the CWD is inside one —
/// repo-scoped sources degrade to an honest "no repo context" without it.
fn sources(repo: Option<&Path>) -> Vec<PostureRow> {
    vec![
        drift_row(),
        arch_row(),
        capability_row(),
        contract_nightly_row(),
        watcher_row(repo),
        env_gate_row(repo),
        bench_baselines_row(repo),
    ]
}

pub async fn run(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP);
        return 0;
    }
    let repo = find_repo_root();
    // Payload to stdout — this is a report, not narration.
    println!("── posture — artifact age per quality subsystem (read-only) ──");
    let mut stale = 0usize;
    for row in sources(repo.as_deref()) {
        let age = row.age.as_deref().unwrap_or("-");
        if row.verdict.contains("stale") || row.verdict == "never_run" {
            stale += 1;
        }
        println!(
            "  {:<18} {:<16} {:<12} {}",
            row.name, row.verdict, age, row.detail
        );
    }
    match &repo {
        Some(r) => println!("  repo context: {}", r.display()),
        None => println!("  repo context: none (repo-scoped rows degrade; run from a checkout)"),
    }
    if stale > 0 {
        println!("  {stale} subsystem(s) stale or never run — each row names its refresh command");
    }
    0
}

// ─── Per-user artifact rows ─────────────────────────────────────────

/// Staleness horizon for report-shaped artifacts: older than this and the
/// row SAYS stale. Two weeks — generous against the observed rot mode
/// (reports going a month unnoticed), tight enough to prompt a refresh.
const STALE_AFTER_DAYS: u64 = 14;

fn drift_row() -> PostureRow {
    let path = sovereign_contracts::rebrand::drift_dir().join("latest.md.json");
    aged_artifact_row(
        "drift",
        &path,
        "narrative-vs-code drift report — refresh: sovereign drift detect",
    )
}

fn arch_row() -> PostureRow {
    per_corpus_newest(
        "arch",
        &sovereign_contracts::rebrand::data_dir().join("arch"),
        "arch_report.json",
        "architectural census — refresh: sovereign code arch-report",
    )
}

fn capability_row() -> PostureRow {
    per_corpus_newest(
        "capability",
        &sovereign_contracts::rebrand::data_dir().join("capabilities"),
        "capability_map.json",
        "capability map — refresh: sovereign code capability-map",
    )
}

fn contract_nightly_row() -> PostureRow {
    // The trigger comes from the same decider `svrn contract nightly` uses, so
    // the two surfaces cannot disagree about what schedules this lane. Naming
    // it in the row is the fix for a row that implied a daily cadence on a
    // host where nothing scheduled the lane at all.
    let trigger = sovereign_cli_shared::cli_contract_report::nightly_trigger();
    match sovereign_cli_shared::cli_contract_report::nightly_posture() {
        Some(n) => PostureRow {
            name: "contract-nightly",
            verdict: if n.is_stale() {
                format!("{} (stale)", n.verdict)
            } else {
                n.verdict.clone()
            },
            age: Some(n.age_human()),
            detail: format!(
                "{} — trigger: {} — detail: svrn contract nightly",
                n.summary,
                trigger.label()
            ),
        },
        None => PostureRow {
            name: "contract-nightly",
            verdict: "never_run".into(),
            age: None,
            detail: format!(
                "no journey-lane verdict on this host (trigger: {}) — run: scripts/cli-journey-nightly.sh",
                trigger.label()
            ),
        },
    }
}

fn watcher_row(repo: Option<&Path>) -> PostureRow {
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
                    return PostureRow {
                        name: "watchers",
                        verdict: "off (by design)".into(),
                        age: None,
                        detail: "this repo opts out ([watchers] enabled = false); \
                                 gate = the two toolbox scripts"
                            .into(),
                    };
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

fn env_gate_row(repo: Option<&Path>) -> PostureRow {
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
            PostureRow {
                name: "env-gate",
                verdict: "present".into(),
                age: mtime_age(&path).map(human_age),
                detail: format!(
                    "{count} legacy env vars riding the shrink-only baseline — \
                     burn down via quality/env-flags.toml + `env-gate --tighten`"
                ),
            }
        }
        Err(_) => PostureRow {
            name: "env-gate",
            verdict: "not yet present".into(),
            age: None,
            detail: "no baseline — run: cargo run -p xtask -- env-gate --update-baseline".into(),
        },
    }
}

fn bench_baselines_row(repo: Option<&Path>) -> PostureRow {
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
        return PostureRow {
            name: "bench-baselines",
            verdict: "never_run".into(),
            age: None,
            detail: format!("no latest.json under {}", bench_root.display()),
        };
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

    let mut detail = format!("{committed} committed baseline(s)");
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

    PostureRow {
        name: "bench-baselines",
        verdict: if local_only > 0 || !unmeasured.is_empty() {
            "present (gaps)".into()
        } else {
            "present".into()
        },
        age: Some(format!(
            "{}..{}",
            human_age(age_of(newest)),
            human_age(age_of(oldest))
        )),
        detail,
    }
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

fn aged_artifact_row(name: &'static str, path: &Path, what: &str) -> PostureRow {
    match mtime_age(path) {
        Some(age) => PostureRow {
            name,
            verdict: if age.as_secs() > STALE_AFTER_DAYS * 86_400 {
                "stale".into()
            } else {
                "fresh".into()
            },
            age: Some(human_age(age)),
            detail: what.to_string(),
        },
        None => PostureRow {
            name,
            verdict: "never_run".into(),
            age: None,
            detail: format!("{what} — expected at {}", path.display()),
        },
    }
}

/// Newest `<root>/<corpus>/<artifact>` across corpora subdirs.
fn per_corpus_newest(name: &'static str, root: &Path, artifact: &str, what: &str) -> PostureRow {
    let mut newest: Option<(String, std::time::Duration)> = None;
    let mut corpora = 0usize;
    if let Ok(entries) = std::fs::read_dir(root) {
        for e in entries.flatten() {
            let p = e.path().join(artifact);
            if let Some(age) = mtime_age(&p) {
                corpora += 1;
                let corpus = e.file_name().to_string_lossy().into_owned();
                if newest.as_ref().map_or(true, |(_, a)| age < *a) {
                    newest = Some((corpus, age));
                }
            }
        }
    }
    match newest {
        Some((corpus, age)) => PostureRow {
            name,
            verdict: if age.as_secs() > STALE_AFTER_DAYS * 86_400 {
                "stale".into()
            } else {
                "fresh".into()
            },
            age: Some(human_age(age)),
            detail: format!("{what} (newest of {corpora} corpus dir(s): {corpus})"),
        },
        None => PostureRow {
            name,
            verdict: "never_run".into(),
            age: None,
            detail: format!("{what} — expected under {}", root.display()),
        },
    }
}

fn no_repo_row(name: &'static str) -> PostureRow {
    PostureRow {
        name,
        verdict: "no repo context".into(),
        age: None,
        detail: "repo-scoped artifact; run from a source checkout".into(),
    }
}

// ─── Small helpers ──────────────────────────────────────────────────

fn mtime_age(path: &Path) -> Option<std::time::Duration> {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .map(age_of)
}

fn age_of(t: SystemTime) -> std::time::Duration {
    SystemTime::now()
        .duration_since(t)
        .unwrap_or(std::time::Duration::ZERO)
}

fn human_age(d: std::time::Duration) -> String {
    let s = d.as_secs();
    if s < 3_600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h", s / 3_600)
    } else {
        format!("{}d", s / 86_400)
    }
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
