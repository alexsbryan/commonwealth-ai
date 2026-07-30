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
    match sovereign_cli_shared::cli_contract_report::nightly_posture() {
        Some(n) => PostureRow {
            name: "contract-nightly",
            verdict: if n.is_stale() {
                format!("{} (stale)", n.verdict)
            } else {
                n.verdict.clone()
            },
            age: Some(n.age_human()),
            detail: format!("{} — detail: svrn contract nightly", n.summary),
        },
        None => PostureRow {
            name: "contract-nightly",
            verdict: "never_run".into(),
            age: None,
            detail: "no journey-lane verdict on this host — run: scripts/cli-journey-nightly.sh"
                .into(),
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
    PostureRow {
        name: "bench-baselines",
        verdict: "present".into(),
        age: Some(format!(
            "{}..{}",
            human_age(age_of(newest)),
            human_age(age_of(oldest))
        )),
        detail: format!(
            "{} committed baseline(s); recapture via `svrn bench gate <lane> --update-baseline`",
            latest.len()
        ),
    }
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
fn per_corpus_newest(
    name: &'static str,
    root: &Path,
    artifact: &str,
    what: &str,
) -> PostureRow {
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
