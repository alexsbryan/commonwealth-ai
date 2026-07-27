// SPDX-License-Identifier: AGPL-3.0-or-later
//! User-facing health checks — "is my install working?", answered for
//! the person *using* the app rather than the person who built it.
//!
//! **Deliberately not `sovereign doctor`.** Doctor's checks are
//! `scip_indexed`, `code_indexed`, `notes_db`, `test_runner`,
//! `lint_runner`, `watcher_live` — the developer code-intelligence
//! toolchain — and every remedy it prints is a shell command. That is
//! the right tool for this repository and the wrong one for a member
//! of a mesh who has a desktop app and no terminal. This module answers
//! the questions those people actually ask:
//!
//! - is the engine running?
//! - is a model loaded, and which one?
//! - am I on the mesh?
//! - can I see anybody else?
//! - is my knowledge intact?
//! - is the disk full?
//! - has this thing been crashing?
//!
//! Two rules the checks hold to, because both exist to make remote
//! triage possible from what a user can read off their own screen:
//!
//! 1. **Every non-OK check carries a `fix_hint` the user can perform
//!    without a terminal**, or says plainly that it needs the operator.
//!    A check that only says "broken" moves the work to the support
//!    channel instead of removing it.
//! 2. **`id` is stable and greppable.** "Your `mesh_peers` check is
//!    failing" has to mean the same thing on all twenty installs, in a
//!    screenshot, in a pasted report, and in a year.
//!
//! Split into a pure [`evaluate`] over [`HealthFacts`] and an IO-side
//! gather at the call site, for the same reason
//! [`crate::crash_bundle::render_report`] is pure: the verdict logic
//! and the wire format are exactly what triage depends on, so they are
//! what the tests pin. Gathering talks to the daemon and the mesh and
//! cannot be pinned; deciding can.

use serde::{Deserialize, Serialize};

/// Free space below this is a hard failure — ingest, index rebuilds
/// and model loads all fail in confusing, downstream-looking ways well
/// before the disk is actually full.
const DISK_FAIL_GB: f64 = 5.0;
/// Below this we warn. A corpus install or a model download will
/// plausibly exhaust it.
const DISK_WARN_GB: f64 = 20.0;

/// Crashes within this window count toward the stability check. Longer
/// than a session, short enough that a fixed problem stops being
/// reported.
pub const CRASH_WINDOW_SECS: u64 = 7 * 24 * 60 * 60;
/// This many crashes in the window is a failure rather than a warning.
const CRASH_FAIL_COUNT: usize = 3;

/// Verdict for a single check.
///
/// `Unknown` is a first-class outcome and not an error: if the daemon
/// is down we cannot know whether a model is loaded, and reporting
/// `Ok` or `Fail` there would both be lies. A report full of honest
/// `Unknown`s above a single `Fail` is exactly the shape that tells
/// triage where to look.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
    Unknown,
}

impl CheckStatus {
    /// Symbol for the markdown report and the CLI. Kept next to the
    /// enum so the report and the UI can never drift apart.
    pub fn glyph(&self) -> &'static str {
        match self {
            CheckStatus::Ok => "✓",
            CheckStatus::Warn => "!",
            CheckStatus::Fail => "✗",
            CheckStatus::Unknown => "?",
        }
    }

    /// Ordering for "what is the worst thing here" — see
    /// [`HealthReport::overall`].
    fn severity(&self) -> u8 {
        match self {
            CheckStatus::Ok => 0,
            CheckStatus::Unknown => 1,
            CheckStatus::Warn => 2,
            CheckStatus::Fail => 3,
        }
    }
}

/// One check: what was looked at, what was found, and what the user
/// can do about it.
///
/// `Serialize` only, deliberately: `id` and `label` are `&'static str`
/// because they are compile-time constants that a support conversation
/// depends on being identical everywhere, and that is worth more than
/// round-tripping. These travel outward to the UI and the report; they
/// are never parsed back.
#[derive(Debug, Clone, Serialize)]
pub struct HealthCheck {
    /// Stable, greppable identifier. Never localise or reword this —
    /// it is the handle a support conversation uses.
    pub id: &'static str,
    /// Plain-language name shown in the UI.
    pub label: &'static str,
    pub status: CheckStatus,
    /// What was actually observed, in the user's terms. This is the
    /// line a screenshot has to carry on its own.
    pub detail: String,
    /// What the user can do, with no terminal. `None` when the check
    /// passed or when nothing they can do would help — in the latter
    /// case `detail` says so rather than leaving them to guess.
    pub fix_hint: Option<String>,
}

/// The whole picture, as of one moment.
#[derive(Debug, Clone, Serialize)]
pub struct HealthReport {
    pub captured_at_unix: u64,
    pub checks: Vec<HealthCheck>,
}

impl HealthReport {
    /// The worst status present — what the UI colours the badge with.
    pub fn overall(&self) -> CheckStatus {
        self.checks
            .iter()
            .map(|c| c.status)
            .max_by_key(|s| s.severity())
            .unwrap_or(CheckStatus::Unknown)
    }

    /// One line naming the failing check ids, for the top of a report
    /// and for a user to read aloud. Empty-safe.
    pub fn summary_line(&self) -> String {
        let bad: Vec<&str> = self
            .checks
            .iter()
            .filter(|c| matches!(c.status, CheckStatus::Fail | CheckStatus::Warn))
            .map(|c| c.id)
            .collect();
        if bad.is_empty() {
            format!("{} checks, all healthy", self.checks.len())
        } else {
            format!(
                "{} of {} checks need attention: {}",
                bad.len(),
                self.checks.len(),
                bad.join(", ")
            )
        }
    }
}

/// What the daemon told us about itself. `None` on the parent when the
/// daemon could not be reached at all.
#[derive(Debug, Clone, Default)]
pub struct DaemonFacts {
    /// Basename of the primary chat model, when one is loaded.
    pub primary_model: Option<String>,
    /// How many models the daemon reports as resident.
    pub models_loaded: usize,
}

/// What this node believes about the mesh right now.
#[derive(Debug, Clone, Default)]
pub struct MeshFacts {
    pub joined: bool,
    pub mesh_name: Option<String>,
    /// Peers discovered and currently reachable.
    pub peers_visible: usize,
    /// Peers this node has ever seen — the gap against `peers_visible`
    /// is the whole "everyone else dropped off" diagnosis.
    pub peers_known: usize,
}

/// Knowledge-base state.
#[derive(Debug, Clone, Default)]
pub struct CorpusFacts {
    pub total: usize,
    pub failed: usize,
    pub in_progress: usize,
}

/// A crash inside [`CRASH_WINDOW_SECS`].
#[derive(Debug, Clone)]
pub struct CrashFact {
    pub captured_at_unix: u64,
    pub summary: String,
}

/// Everything [`evaluate`] is allowed to look at. Gathered by the
/// caller; `None` means "could not determine", never "absent".
#[derive(Debug, Clone, Default)]
pub struct HealthFacts {
    pub captured_at_unix: u64,
    pub daemon_running: bool,
    pub daemon: Option<DaemonFacts>,
    pub mesh: Option<MeshFacts>,
    pub corpora: Option<CorpusFacts>,
    pub free_disk_gb: Option<f64>,
    /// Already filtered to [`CRASH_WINDOW_SECS`] by the gatherer.
    pub recent_crashes: Vec<CrashFact>,
}

/// Decide every check from gathered facts. Pure — no IO, no clock.
pub fn evaluate(f: &HealthFacts) -> HealthReport {
    HealthReport {
        captured_at_unix: f.captured_at_unix,
        checks: vec![
            check_engine(f),
            check_model(f),
            check_mesh(f),
            check_peers(f),
            check_knowledge(f),
            check_disk(f),
            check_stability(f),
        ],
    }
}

fn check_engine(f: &HealthFacts) -> HealthCheck {
    if f.daemon_running {
        HealthCheck {
            id: "engine",
            label: "Engine",
            status: CheckStatus::Ok,
            detail: "Running.".into(),
            fix_hint: None,
        }
    } else {
        HealthCheck {
            id: "engine",
            label: "Engine",
            status: CheckStatus::Fail,
            detail: "Not running. Nothing else can work until this does.".into(),
            fix_hint: Some(
                "Quit svrnmesh completely and reopen it. If it still doesn't start, \
                 restart the computer — the engine sometimes can't reclaim its port \
                 until then. If it survives a restart, send this report."
                    .into(),
            ),
        }
    }
}

fn check_model(f: &HealthFacts) -> HealthCheck {
    // Every branch below is gated on the engine, because "no model
    // loaded" and "can't ask" are different findings and only one of
    // them is the user's problem.
    let Some(d) = f.daemon.as_ref() else {
        return HealthCheck {
            id: "model",
            label: "Model",
            status: CheckStatus::Unknown,
            detail: "Couldn't ask — the engine isn't reachable.".into(),
            fix_hint: None,
        };
    };
    match (&d.primary_model, d.models_loaded) {
        (Some(m), _) => HealthCheck {
            id: "model",
            label: "Model",
            status: CheckStatus::Ok,
            detail: format!("{m} is loaded."),
            fix_hint: None,
        },
        (None, 0) => HealthCheck {
            id: "model",
            label: "Model",
            status: CheckStatus::Fail,
            detail: "No model is loaded, so answers can't be generated.".into(),
            fix_hint: Some(
                "Open Settings → Models and confirm a chat model is downloaded and \
                 selected. A model file that stopped downloading part-way is the \
                 usual cause."
                    .into(),
            ),
        },
        (None, n) => HealthCheck {
            id: "model",
            label: "Model",
            status: CheckStatus::Warn,
            detail: format!("{n} model(s) loaded, but no primary chat model is set."),
            fix_hint: Some("Open Settings → Models and choose a primary chat model.".into()),
        },
    }
}

fn check_mesh(f: &HealthFacts) -> HealthCheck {
    let Some(m) = f.mesh.as_ref() else {
        return HealthCheck {
            id: "mesh",
            label: "Mesh",
            status: CheckStatus::Unknown,
            detail: "Couldn't determine mesh state.".into(),
            fix_hint: None,
        };
    };
    if !m.joined {
        // Solo is a legitimate way to run, so this is not a failure —
        // it is a fact the user may not realise about themselves, and
        // it explains every "why can't I see anyone" report.
        return HealthCheck {
            id: "mesh",
            label: "Mesh",
            status: CheckStatus::Warn,
            detail: "Not on a mesh — running solo. Everything works, but nothing is shared."
                .into(),
            fix_hint: Some(
                "If you were sent an invite link, open Settings → Mesh and join with it.".into(),
            ),
        };
    }
    HealthCheck {
        id: "mesh",
        label: "Mesh",
        status: CheckStatus::Ok,
        detail: match &m.mesh_name {
            Some(n) => format!("Joined \"{n}\"."),
            None => "Joined.".into(),
        },
        fix_hint: None,
    }
}

fn check_peers(f: &HealthFacts) -> HealthCheck {
    let Some(m) = f.mesh.as_ref() else {
        return HealthCheck {
            id: "mesh_peers",
            label: "Other people",
            status: CheckStatus::Unknown,
            detail: "Couldn't determine mesh state.".into(),
            fix_hint: None,
        };
    };
    if !m.joined {
        return HealthCheck {
            id: "mesh_peers",
            label: "Other people",
            status: CheckStatus::Unknown,
            detail: "Not on a mesh, so there is nobody to see.".into(),
            fix_hint: None,
        };
    }
    match (m.peers_visible, m.peers_known) {
        (0, 0) => HealthCheck {
            id: "mesh_peers",
            label: "Other people",
            status: CheckStatus::Warn,
            detail: "Nobody found yet. You may be the first one online.".into(),
            fix_hint: Some(
                "If others should be online: check you're on the same Wi-Fi, and that \
                 a VPN isn't running. Guest and corporate networks usually block the \
                 discovery this needs."
                    .into(),
            ),
        },
        // The sharp case: we have seen these people before and now
        // cannot. That is a network change, not a first-run problem,
        // and it deserves a different hint.
        (0, known) => HealthCheck {
            id: "mesh_peers",
            label: "Other people",
            status: CheckStatus::Fail,
            detail: format!(
                "{known} known, none reachable right now — they were visible before."
            ),
            fix_hint: Some(
                "Something changed on the network. Check whether a VPN switched on, or \
                 whether you moved to a different Wi-Fi than the others."
                    .into(),
            ),
        },
        (visible, known) => HealthCheck {
            id: "mesh_peers",
            label: "Other people",
            status: CheckStatus::Ok,
            detail: format!("{visible} reachable of {known} known."),
            fix_hint: None,
        },
    }
}

fn check_knowledge(f: &HealthFacts) -> HealthCheck {
    let Some(c) = f.corpora.as_ref() else {
        return HealthCheck {
            id: "knowledge",
            label: "Knowledge",
            status: CheckStatus::Unknown,
            detail: "Couldn't read the knowledge library.".into(),
            fix_hint: None,
        };
    };
    if c.failed > 0 {
        return HealthCheck {
            id: "knowledge",
            label: "Knowledge",
            status: CheckStatus::Fail,
            detail: format!(
                "{} of {} knowledge base(s) failed to finish importing.",
                c.failed, c.total
            ),
            fix_hint: Some(
                "Open Library and use Retry on the affected item. If it fails twice, \
                 send this report — the reason is in it."
                    .into(),
            ),
        };
    }
    if c.in_progress > 0 {
        return HealthCheck {
            id: "knowledge",
            label: "Knowledge",
            status: CheckStatus::Warn,
            detail: format!(
                "{} of {} still importing — answers may be incomplete until it finishes.",
                c.in_progress, c.total
            ),
            fix_hint: Some("Leave the app open; this continues in the background.".into()),
        };
    }
    HealthCheck {
        id: "knowledge",
        label: "Knowledge",
        status: CheckStatus::Ok,
        detail: format!("{} knowledge base(s), all healthy.", c.total),
        fix_hint: None,
    }
}

fn check_disk(f: &HealthFacts) -> HealthCheck {
    let Some(gb) = f.free_disk_gb else {
        return HealthCheck {
            id: "disk",
            label: "Disk space",
            status: CheckStatus::Unknown,
            detail: "Couldn't read free space.".into(),
            fix_hint: None,
        };
    };
    // Ordered worst-first so the boundaries can't overlap.
    if gb < DISK_FAIL_GB {
        HealthCheck {
            id: "disk",
            label: "Disk space",
            status: CheckStatus::Fail,
            detail: format!(
                "{gb:.1} GB free. Imports and model loads will fail, usually with an \
                 error that blames something else."
            ),
            fix_hint: Some(
                "Free up space, then reopen the app. Unused models under Settings → \
                 Models are usually the largest thing you can safely delete."
                    .into(),
            ),
        }
    } else if gb < DISK_WARN_GB {
        HealthCheck {
            id: "disk",
            label: "Disk space",
            status: CheckStatus::Warn,
            detail: format!("{gb:.1} GB free — enough for now, tight for a new import."),
            fix_hint: Some("Free up space before adding a large knowledge base.".into()),
        }
    } else {
        HealthCheck {
            id: "disk",
            label: "Disk space",
            status: CheckStatus::Ok,
            detail: format!("{gb:.1} GB free."),
            fix_hint: None,
        }
    }
}

fn check_stability(f: &HealthFacts) -> HealthCheck {
    let n = f.recent_crashes.len();
    if n == 0 {
        return HealthCheck {
            id: "stability",
            label: "Stability",
            status: CheckStatus::Ok,
            detail: "No crashes in the last 7 days.".into(),
            fix_hint: None,
        };
    }
    // The most recent summary is the single most useful string in the
    // whole report, so it goes in `detail` where a screenshot catches
    // it rather than only in the attached log.
    let latest = f
        .recent_crashes
        .iter()
        .max_by_key(|c| c.captured_at_unix)
        .map(|c| c.summary.as_str())
        .unwrap_or("(no summary)");
    let status = if n >= CRASH_FAIL_COUNT {
        CheckStatus::Fail
    } else {
        CheckStatus::Warn
    };
    HealthCheck {
        id: "stability",
        label: "Stability",
        status,
        detail: format!("{n} crash(es) in the last 7 days. Most recent: {latest}"),
        fix_hint: Some(
            "Send this report — it already contains the crash detail needed to \
             diagnose it."
                .into(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy() -> HealthFacts {
        HealthFacts {
            captured_at_unix: 1_700_000_000,
            daemon_running: true,
            daemon: Some(DaemonFacts {
                primary_model: Some("qwen3.5-35b-q4.gguf".into()),
                models_loaded: 2,
            }),
            mesh: Some(MeshFacts {
                joined: true,
                mesh_name: Some("Meshsonics".into()),
                peers_visible: 3,
                peers_known: 3,
            }),
            corpora: Some(CorpusFacts {
                total: 2,
                failed: 0,
                in_progress: 0,
            }),
            free_disk_gb: Some(400.0),
            recent_crashes: Vec::new(),
        }
    }

    fn get<'a>(r: &'a HealthReport, id: &str) -> &'a HealthCheck {
        r.checks.iter().find(|c| c.id == id).expect("check present")
    }

    #[test]
    fn a_healthy_install_is_all_green() {
        let r = evaluate(&healthy());
        assert_eq!(r.overall(), CheckStatus::Ok);
        assert!(r.summary_line().contains("all healthy"));
    }

    #[test]
    fn every_non_ok_check_tells_the_user_what_to_do() {
        // The load-bearing property of this whole module: a report
        // that says "broken" without a next step moves the work into
        // the support channel instead of removing it. Exceptions must
        // be deliberate — `Unknown` checks are allowed to have no hint
        // because the actionable finding is the check they depend on.
        let mut f = HealthFacts::default();
        f.free_disk_gb = Some(1.0);
        f.mesh = Some(MeshFacts {
            joined: true,
            peers_visible: 0,
            peers_known: 4,
            ..Default::default()
        });
        f.corpora = Some(CorpusFacts {
            total: 3,
            failed: 1,
            in_progress: 0,
        });
        f.recent_crashes = vec![CrashFact {
            captured_at_unix: 1,
            summary: "llama.cpp abort".into(),
        }];
        let r = evaluate(&f);
        for c in &r.checks {
            if matches!(c.status, CheckStatus::Fail | CheckStatus::Warn) {
                assert!(
                    c.fix_hint.is_some(),
                    "check `{}` is {:?} with no fix_hint — a user reading this has \
                     nowhere to go",
                    c.id,
                    c.status
                );
            }
        }
    }

    #[test]
    fn a_dead_engine_does_not_fabricate_a_model_verdict() {
        // Reporting Ok or Fail for the model when we couldn't ask is
        // the exact failure this enum's `Unknown` exists to prevent.
        let f = HealthFacts {
            daemon_running: false,
            daemon: None,
            ..healthy()
        };
        let r = evaluate(&f);
        assert_eq!(get(&r, "engine").status, CheckStatus::Fail);
        assert_eq!(get(&r, "model").status, CheckStatus::Unknown);
        assert_eq!(r.overall(), CheckStatus::Fail);
    }

    #[test]
    fn known_but_unreachable_peers_outrank_never_seen_peers() {
        // "I used to see them and now I don't" is a network change and
        // gets a different hint from "nobody here yet" — conflating
        // the two sends first-run users chasing a VPN they don't have.
        let never = evaluate(&HealthFacts {
            mesh: Some(MeshFacts {
                joined: true,
                peers_visible: 0,
                peers_known: 0,
                ..Default::default()
            }),
            ..healthy()
        });
        let lost = evaluate(&HealthFacts {
            mesh: Some(MeshFacts {
                joined: true,
                peers_visible: 0,
                peers_known: 4,
                ..Default::default()
            }),
            ..healthy()
        });
        assert_eq!(get(&never, "mesh_peers").status, CheckStatus::Warn);
        assert_eq!(get(&lost, "mesh_peers").status, CheckStatus::Fail);
        assert_ne!(
            get(&never, "mesh_peers").fix_hint,
            get(&lost, "mesh_peers").fix_hint
        );
    }

    #[test]
    fn solo_is_a_fact_not_a_failure_and_peers_stay_unknown() {
        let r = evaluate(&HealthFacts {
            mesh: Some(MeshFacts {
                joined: false,
                ..Default::default()
            }),
            ..healthy()
        });
        assert_eq!(get(&r, "mesh").status, CheckStatus::Warn);
        assert_eq!(get(&r, "mesh_peers").status, CheckStatus::Unknown);
    }

    #[test]
    fn summary_line_names_the_failing_ids() {
        let r = evaluate(&HealthFacts {
            free_disk_gb: Some(1.0),
            ..healthy()
        });
        let s = r.summary_line();
        assert!(s.contains("disk"), "summary should name the id: {s}");
    }

    #[test]
    fn disk_thresholds_do_not_overlap() {
        let at = |gb: f64| {
            evaluate(&HealthFacts {
                free_disk_gb: Some(gb),
                ..healthy()
            })
            .checks
            .iter()
            .find(|c| c.id == "disk")
            .unwrap()
            .status
        };
        assert_eq!(at(DISK_FAIL_GB - 0.1), CheckStatus::Fail);
        assert_eq!(at(DISK_FAIL_GB), CheckStatus::Warn);
        assert_eq!(at(DISK_WARN_GB - 0.1), CheckStatus::Warn);
        assert_eq!(at(DISK_WARN_GB), CheckStatus::Ok);
    }

    #[test]
    fn three_crashes_escalate_from_warn_to_fail() {
        let crashes = |n: usize| HealthFacts {
            recent_crashes: (0..n)
                .map(|i| CrashFact {
                    captured_at_unix: i as u64,
                    summary: format!("crash {i}"),
                })
                .collect(),
            ..healthy()
        };
        assert_eq!(
            get(&evaluate(&crashes(1)), "stability").status,
            CheckStatus::Warn
        );
        assert_eq!(
            get(&evaluate(&crashes(CRASH_FAIL_COUNT)), "stability").status,
            CheckStatus::Fail
        );
    }

    #[test]
    fn stability_detail_carries_the_latest_summary_for_a_screenshot() {
        let r = evaluate(&HealthFacts {
            recent_crashes: vec![
                CrashFact {
                    captured_at_unix: 10,
                    summary: "older".into(),
                },
                CrashFact {
                    captured_at_unix: 99,
                    summary: "newest failure".into(),
                },
            ],
            ..healthy()
        });
        assert!(get(&r, "stability").detail.contains("newest failure"));
    }
}
