// SPDX-License-Identifier: AGPL-3.0-or-later
//! Verifying a daemon rebuild nudge: `svrn refresh` asks the daemon to
//! rebuild, then polls `/v1/projects` until the graph reaches the HEAD the
//! nudge was sent at or a named failure ends the wait. Moved whole from
//! `refresh.rs`, which sat in the 800-1200 band.

use super::*;

// ─── Nudge verification ──────────────────────────────────────

/// Budget for waiting on a nudge to complete. A full-workspace SCIP
/// export on a large repo can take minutes; 50 minutes is far beyond
/// any legitimate rebuild (the daemon's own watchdog aborts wedged
/// rebuilds at 45 minutes), so expiry means the daemon is wedged or
/// gone — never "still going".
const VERIFY_BUDGET: std::time::Duration = std::time::Duration::from_secs(50 * 60);

/// Poll cadence for nudge verification.
const VERIFY_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// How many consecutive failed `/v1/projects` polls count as "daemon
/// gone" (a daemon restart can transiently refuse connections for
/// seconds; one bad poll is a hiccup, three in a row is a story).
const VERIFY_DAEMON_GONE_POLLS: u32 = 3;

/// The five-verdict answer a `refresh` nudge can get (ARCH §18.2 —
/// never a silent sixth). `Pending` is internal to the poll loop and
/// never returned to the caller.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum NudgeVerdict {
    /// The on-disk graph is indexed at git HEAD.
    Completed,
    /// The daemon recorded a rebuild failure after the nudge.
    Failed {
        error: String,
    },
    /// The rebuild task panicked / the watcher is in the crashed
    /// state. The daemon self-clears its slots, so one re-nudge is
    /// the sanctioned recovery.
    Crashed {
        reason: String,
    },
    /// Neither completed nor failed within the budget.
    Wedged {
        detail: String,
    },
    /// The daemon stopped answering mid-verification.
    DaemonGone {
        detail: String,
    },
    Pending,
}

/// Pure verdict from one `/v1/projects` sample (ARCH §18.5 — one
/// sample, one verdict, no hidden state).
fn nudge_verdict(
    status_state: Option<&str>,
    last_error: Option<&str>,
    last_error_ts: Option<u64>,
    baseline_error_ts: u64,
    indexed_head: Option<&str>,
    git_head: Option<&str>,
    since_start: std::time::Duration,
    descends: &dyn Fn(&str, &str) -> bool,
) -> NudgeVerdict {
    if status_state == Some("crashed") || status_state == Some("disabled") {
        return NudgeVerdict::Crashed {
            reason: last_error
                .map(str::to_string)
                .unwrap_or_else(|| format!("watcher is in the {} state", status_state.unwrap())),
        };
    }
    // A failure recorded AFTER the pre-nudge baseline belongs to this
    // nudge; one recorded before it is stale history and not our
    // fault. `record_rebuild_success` clears the record, so a fresh
    // failure here means the current rebuild genuinely failed.
    if let (Some(e), Some(ts)) = (last_error, last_error_ts) {
        if ts > baseline_error_ts {
            return NudgeVerdict::Failed {
                error: e.to_string(),
            };
        }
    }
    // Covered when indexed at the nudge's HEAD or a later commit on it: a
    // repo that commits while the daemon rebuilds is stamped past that HEAD,
    // never at it (zoracite, 2026-10-06: 11 commits in the wait).
    if let (Some(i), Some(g)) = (indexed_head, git_head) {
        if i == g || descends(g, i) {
            return NudgeVerdict::Completed;
        }
    }
    if since_start > VERIFY_BUDGET {
        return NudgeVerdict::Wedged {
            detail: format!(
                "graph never reached the nudge's HEAD {} within {:.0} min (indexed at: {})",
                git_head.unwrap_or("<unread>"),
                VERIFY_BUDGET.as_secs_f64() / 60.0,
                indexed_head.unwrap_or("<never indexed>"),
            ),
        };
    }
    NudgeVerdict::Pending
}

/// Poll `/v1/projects` until the graph reaches git HEAD, the rebuild
/// fails loudly, the budget expires, or the daemon stops answering.
/// Returns a terminal verdict; `Pending` is never returned.
pub(super) async fn await_rebuild_completion(
    corpus_id: &str,
    repo_root: &Path,
    baseline_error_ts: u64,
    quiet: bool,
) -> NudgeVerdict {
    let git_head = git_head(repo_root);
    if git_head.is_none() {
        // Absence is REFUSED, not defaulted (ARCH §18.3): without a
        // readable HEAD we cannot verify daemon completion by commit,
        // and a head-less poll would burn the whole budget to say so.
        // Fall back to the local rebuild, which produces the graph
        // and its own honest success/failure either way.
        return NudgeVerdict::Wedged {
            detail: format!(
                "git HEAD unreadable at {} — daemon completion cannot be verified; falling back to the in-process rebuild",
                repo_root.display()
            ),
        };
    }
    let start = std::time::Instant::now();
    let mut gone_polls: u32 = 0;
    loop {
        let project = match daemon_get("/v1/projects").await {
            Ok(body) => body["projects"]
                .as_array()
                .and_then(|ps| {
                    ps.iter()
                        .find(|p| p["corpus_id"].as_str() == Some(corpus_id))
                })
                .cloned()
                .unwrap_or(serde_json::Value::Null),
            Err(e) => {
                gone_polls += 1;
                if gone_polls >= VERIFY_DAEMON_GONE_POLLS {
                    return NudgeVerdict::DaemonGone { detail: e };
                }
                tokio::time::sleep(VERIFY_POLL_INTERVAL).await;
                continue;
            }
        };
        if project.is_null() {
            return NudgeVerdict::DaemonGone {
                detail: format!("project \"{corpus_id}\" is no longer registered with the daemon"),
            };
        }
        gone_polls = 0;
        let indexed_head = project["last_indexed_head"].as_str();
        let verdict = nudge_verdict(
            project["status"]["scip"]["state"].as_str(),
            project["last_rebuild_error"][0].as_str(),
            project["last_rebuild_error"][1].as_u64(),
            baseline_error_ts,
            indexed_head,
            git_head.as_deref(),
            start.elapsed(),
            &|nudged, indexed| descends_from(repo_root, nudged, indexed),
        );
        match verdict {
            NudgeVerdict::Pending => {
                if !quiet {
                    eprint!(
                        "\r    Rebuilding in the daemon... ({:.0}s)      ",
                        start.elapsed().as_secs_f64()
                    );
                }
                tokio::time::sleep(VERIFY_POLL_INTERVAL).await;
            }
            terminal => {
                if !quiet {
                    eprintln!("\r                                                        \r");
                    if terminal == NudgeVerdict::Completed && indexed_head != git_head.as_deref() {
                        eprintln!(
                            "    graph indexed at {}, past the HEAD the nudge was sent at ({}): \
                             the repo committed during the rebuild",
                            indexed_head.unwrap_or_default(),
                            git_head.as_deref().unwrap_or_default(),
                        );
                    }
                }
                return terminal;
            }
        }
    }
}

/// The git commit at HEAD of `root`'s repository, or `None` when the
/// command cannot run (not a git checkout, git missing).
fn git_head(root: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("rev-parse")
        .arg("HEAD")
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Whether `commit` is `ancestor` or a later commit on it in `root`'s
/// history (`git merge-base --is-ancestor`). A commit git does not know, or a
/// git that cannot run, answers false: the poll keeps waiting, and the budget's
/// verdict names both commits.
fn descends_from(root: &Path, ancestor: &str, commit: &str) -> bool {
    std::process::Command::new("git")
        .args(["merge-base", "--is-ancestor", ancestor, commit])
        .current_dir(root)
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── nudge_verdict ────────────────────────────────────────
    //
    // The honesty surface of the nudge path (order defect 1): every
    // poll sample must map to a named verdict, never a silent
    // success. Red-first: `cmd_refresh`'s old nudge branch printed
    // "✓ Rebuild nudged" on any 2xx — these tests pin the four
    // verdicts that replace it.

    fn secs(n: u64) -> std::time::Duration {
        std::time::Duration::from_secs(n)
    }

    fn no_history(_: &str, _: &str) -> bool {
        false
    }

    /// The 2026-10-06 zoracite refresh: the nudge went out at 7b2d466, the
    /// repo committed 11 times in the next hour, and every rebuild the daemon
    /// finished was stamped with a later commit, 9e6d231 when the budget ran
    /// out. A graph indexed past the nudge's HEAD holds what the nudge asked
    /// for. FAILING INPUT: equality alone, which calls this wedged.
    #[test]
    fn verdict_completed_when_graph_is_indexed_past_the_nudge_head() {
        let descends = |nudged: &str, indexed: &str| (nudged, indexed) == ("7b2d466", "9e6d231");
        let v = nudge_verdict(
            Some("idle"),
            None,
            None,
            0,
            Some("9e6d231"),
            Some("7b2d466"),
            secs(51 * 60),
            &descends,
        );
        assert_eq!(v, NudgeVerdict::Completed);
        // A graph still at a commit the nudge's HEAD does not precede is not
        // there yet.
        let v = nudge_verdict(
            Some("active"),
            None,
            None,
            0,
            Some("1111111"),
            Some("7b2d466"),
            secs(30),
            &descends,
        );
        assert_eq!(v, NudgeVerdict::Pending);
    }

    #[test]
    fn descends_from_reads_the_repository_history() {
        let tmp = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t.invalid"])
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(tmp.path())
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q"]);
        git(&["commit", "-q", "--allow-empty", "-m", "a"]);
        let a = git_head(tmp.path()).unwrap();
        git(&["commit", "-q", "--allow-empty", "-m", "b"]);
        let b = git_head(tmp.path()).unwrap();
        assert!(descends_from(tmp.path(), &a, &b), "b is a later commit on a");
        assert!(descends_from(tmp.path(), &a, &a), "a commit covers itself");
        assert!(!descends_from(tmp.path(), &b, &a), "a precedes b");
        let unknown = "0".repeat(40);
        assert!(!descends_from(tmp.path(), &a, &unknown), "git knows no such commit");
    }

    #[test]
    fn verdict_completed_when_graph_reaches_git_head() {
        let v = nudge_verdict(
            Some("idle"),
            None,
            None,
            0,
            Some("abc123"),
            Some("abc123"),
            secs(30),
            &no_history,
        );
        assert_eq!(v, NudgeVerdict::Completed);
    }

    #[test]
    fn verdict_reports_new_failure_but_not_stale_ones() {
        // Failure recorded BEFORE the baseline is history, not this
        // nudge's fault — keep polling.
        let v = nudge_verdict(
            Some("idle"),
            Some("boom"),
            Some(100),
            200,
            Some("abc"),
            Some("def"),
            secs(30),
            &no_history,
        );
        assert_eq!(v, NudgeVerdict::Pending);
        // Failure AFTER the baseline belongs to this nudge.
        let v = nudge_verdict(
            Some("idle"),
            Some("boom"),
            Some(300),
            200,
            Some("abc"),
            Some("def"),
            secs(30),
            &no_history,
        );
        assert_eq!(
            v,
            NudgeVerdict::Failed {
                error: "boom".into()
            }
        );
    }

    #[test]
    fn verdict_never_silent_on_wedged_state() {
        let v = nudge_verdict(
            Some("idle"),
            None,
            None,
            0,
            Some("abc"),
            Some("def"),
            secs(51 * 60),
            &no_history,
        );
        assert!(matches!(v, NudgeVerdict::Wedged { .. }));
    }

    #[test]
    fn verdict_crashed_status_is_loud() {
        let v = nudge_verdict(
            Some("crashed"),
            Some("panic in export"),
            Some(300),
            0,
            Some("abc"),
            Some("abc"),
            secs(5),
            &no_history,
        );
        assert!(matches!(v, NudgeVerdict::Crashed { .. }));
    }

    #[test]
    fn verdict_head_mismatch_keeps_polling_then_wedges() {
        // Graph still at the OLD head mid-rebuild is the normal
        // in-flight state — must keep polling, not fail early.
        let v = nudge_verdict(
            Some("active"),
            None,
            None,
            0,
            Some("abc"),
            Some("def"),
            secs(30),
            &no_history,
        );
        assert_eq!(v, NudgeVerdict::Pending);
    }
}
