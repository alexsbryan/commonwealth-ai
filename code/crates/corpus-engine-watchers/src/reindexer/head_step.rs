// SPDX-License-Identifier: AGPL-3.0-or-later
//! What one git-poll tick does when HEAD has moved past the indexed head.
//!
//! A commit used to launch the full rust-analyzer export at once, on the
//! reading that a commit is the operator saying "done here". Agents commit
//! every few minutes, so on 2026-10-16 three 12-17 GiB exports started inside
//! two hours beside a resident 35B model and the kernel killed the daemon at
//! 11:34. A commit now waits for the same cooldown a save does; the indexed
//! head stays behind until the export runs, so every later tick sees the drift
//! again and the export goes as soon as it is due. Commit messages are
//! harvested at once, each commit once.

use std::time::{Duration, Instant};

use super::full_export_due;

/// One tick's answer to a moved HEAD.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct HeadStep {
    /// Harvest commit messages from this head to the new one; `None` when an
    /// earlier tick already harvested up to the new head.
    pub(super) harvest_from: Option<String>,
    /// Launch the full export now.
    pub(super) rebuild: bool,
}

/// `old_head` is the graph's indexed head and `last_harvested` the head an
/// earlier tick harvested to, so a deferred export never harvests a commit
/// twice. An export in flight, or one that finished inside `cooldown`, defers.
pub(super) fn head_step(
    old_head: &str,
    new_head: &str,
    last_harvested: Option<&str>,
    in_flight: bool,
    last_finished: Option<Instant>,
    cooldown: Duration,
) -> HeadStep {
    let harvest_from = match last_harvested {
        Some(h) if h == new_head => None,
        Some(h) => Some(h.to_string()),
        None => Some(old_head.to_string()),
    };
    HeadStep {
        harvest_from,
        rebuild: !in_flight && full_export_due(last_finished, cooldown),
    }
}

/// Phase 7.1: write the commits from `from` to `new_head` as
/// `source='committed'` notes. Runs before the export, and once per commit
/// even while the export is deferred.
pub(super) async fn harvest(
    notes: Option<&corpus_engine_notes::NoteStore>,
    from: Option<&str>,
    root: &std::path::Path,
    new_head: &str,
    corpus_id: &str,
) {
    let (Some(notes), Some(from)) = (notes, from) else {
        return;
    };
    let session_id = format!("harvest-{corpus_id}");
    let wrote =
        crate::commit_harvest::harvest_between(root, from, new_head, notes, &session_id).await;
    if wrote > 0 {
        tracing::info!(
            corpus_id,
            new_notes = wrote,
            "commit_harvest: persisted committed-source notes"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COOL: Duration = Duration::from_secs(900);

    #[test]
    fn a_commit_inside_the_cooldown_harvests_and_defers_the_export() {
        let just_now = Some(Instant::now());
        assert_eq!(
            head_step("a", "b", None, false, just_now, COOL),
            HeadStep {
                harvest_from: Some("a".into()),
                rebuild: false
            }
        );
    }

    #[test]
    fn a_commit_while_an_export_runs_defers_it() {
        assert!(!head_step("a", "b", None, true, None, COOL).rebuild);
    }

    #[test]
    fn a_due_commit_exports_and_a_deferred_head_is_not_harvested_twice() {
        let long_ago = Instant::now().checked_sub(Duration::from_secs(901));
        // First export ever: due.
        assert!(head_step("a", "b", None, false, None, COOL).rebuild);
        // The tick after a deferral: same head, already harvested, now due.
        assert_eq!(
            head_step("a", "b", Some("b"), false, long_ago, COOL),
            HeadStep {
                harvest_from: None,
                rebuild: true
            }
        );
        // A second commit during the deferral harvests only what is new.
        assert_eq!(
            head_step("a", "c", Some("b"), false, Some(Instant::now()), COOL).harvest_from,
            Some("b".into())
        );
    }
}
