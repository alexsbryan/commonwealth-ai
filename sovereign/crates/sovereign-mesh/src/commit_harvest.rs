// SPDX-License-Identifier: AGPL-3.0-or-later
//! Commit-message harvester (Phase 7.1).
//!
//! Reads non-noisy git commit messages between two HEAD positions
//! and writes them as `source='committed'` notes. Extends the
//! daemon reindexer's existing git-HEAD poll: when the poll
//! detects `old_head != new_head`, [`harvest_between`] fires.
//!
//! ## Why
//!
//! Engineers commit decisions in the message body all the time
//! ("switch storage to async channels — sync deadlocked under
//! load"). Without a harvester, those decisions live only in
//! `git log`, which the audit doesn't read. Phase 7's audit
//! contract — "non-empty for any session that did real work" —
//! relies on this stream of `committed`-source notes alongside
//! the `agent`/`extracted`/`inferred`/`observed` ones.
//!
//! ## Noise filter
//!
//! We skip commit messages that match a noise regex
//! (`^(wip|fix typo|save|merge|bump|format|rename)\b`,
//! case-insensitive) or whose subject + body word count is
//! shorter than 10 words. That cuts the kind of mechanical commits
//! that don't represent decisions ("wip", "fix typo", branch
//! merges) without filtering by author or anything more invasive.
//!
//! ## Kind inference
//!
//! We map the conventional-commits prefix to a note kind so the
//! audit's section grouping isn't dominated by `decision` rows:
//!
//! - `fix:` / `bugfix:` → `decision` (the fix-vs-not call is the
//!   decision)
//! - `feat:` / `feature:` → `decision`
//! - `refactor:` → `decision`
//! - `docs:` → `reflection`
//! - no recognised prefix → `decision` (the conservative default)
//!
//! ## Sample-size cap
//!
//! `harvest_between(old, new, …)` walks at most [`MAX_COMMITS`]
//! messages. If the user `git pull`'d 500 commits we don't want
//! to spam the audit with 500 rows. The audit floor is "non-empty
//! for any session that did real work" — quality, not quantity.

use std::path::Path;

use corpus_engine_notes::{NoteScope, NoteSource, NoteStore};

/// Maximum number of commits to harvest in a single poll. A `git
/// pull` of 500 commits should NOT inject 500 notes into the
/// audit's "Decisions" section. Pick whichever's freshest and
/// cap there.
pub const MAX_COMMITS: usize = 50;

/// Minimum word count (subject + body) for a commit to be
/// harvest-worthy. "Bump tokio to 1.0" is 4 words; "Switch
/// storage layer to async channels because sync deadlocked under
/// concurrent ingest" is 12. The line falls naturally around 10.
const MIN_WORD_COUNT: usize = 10;

/// One harvested commit, ready to be written to the NoteStore.
/// Pulled out of the harvester proper so the pure-logic side is
/// trivially testable without touching the DB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarvestedCommit {
    /// Full commit hash (40 hex chars). Used to dedup if the
    /// harvester is invoked twice on the same range.
    pub hash: String,
    /// Note kind inferred from the commit message prefix.
    /// See [the module docs](self) for the mapping.
    pub kind: &'static str,
    /// The note body — by convention the full commit message
    /// (subject + body), since the audit renders multi-paragraph
    /// content gracefully and the prefix is itself information.
    pub message: String,
}

/// Pure entry point: given a list of (hash, message) pairs,
/// return only the harvest-worthy ones with their inferred kind.
/// Caller is responsible for fetching the pairs (typically via
/// `git log`) and persisting the result. Sample size is capped
/// at [`MAX_COMMITS`].
///
/// Order is preserved — caller passes commits oldest-first if
/// they want the audit to render them chronologically.
pub fn filter_and_classify(
    commits: impl IntoIterator<Item = (String, String)>,
) -> Vec<HarvestedCommit> {
    let mut out = Vec::new();
    for (hash, message) in commits.into_iter().take(MAX_COMMITS) {
        if !is_harvest_worthy(&message) {
            continue;
        }
        out.push(HarvestedCommit {
            hash,
            kind: infer_kind(&message),
            message,
        });
    }
    out
}

/// True iff `message` survives the noise filter (not a wip/typo/
/// merge commit) AND the minimum word count.
pub(crate) fn is_harvest_worthy(message: &str) -> bool {
    if message.trim().is_empty() {
        return false;
    }
    let lower = message.trim_start().to_lowercase();
    // Anchored prefix match — a commit like "Wipe the cache" with
    // unrelated capitalisation should NOT be filtered. The match
    // is on word boundary so `wip:` / `wip ` / `wip\n` all hit but
    // `wiper-blade-fix:` does NOT.
    const NOISE: &[&str] = &[
        "wip", "fix typo", "save", "merge", "bump", "format", "rename",
    ];
    for prefix in NOISE {
        if let Some(after) = lower.strip_prefix(prefix) {
            // Require a word boundary after the prefix: next char
            // (if any) must be whitespace or a punctuation mark.
            let boundary_ok = after.is_empty()
                || after
                    .chars()
                    .next()
                    .map(|c| c.is_ascii_whitespace() || matches!(c, ':' | '-' | '!' | '.' | ','))
                    .unwrap_or(true);
            if boundary_ok {
                return false;
            }
        }
    }
    let words = message.split_whitespace().count();
    words >= MIN_WORD_COUNT
}

/// Infer the note kind from the commit message prefix. Defaults
/// to `decision` for any unrecognised shape — the conservative
/// choice; the audit's "Decisions" section then carries it.
pub(crate) fn infer_kind(message: &str) -> &'static str {
    let trimmed = message.trim_start().to_lowercase();
    // Conventional-commits style: `<type>(<scope>): <subject>`.
    // We only inspect the type; scope-and-subject aren't relevant
    // for kind inference.
    let prefix = trimmed.split([':', '(', ' ']).next().unwrap_or("");
    match prefix {
        "fix" | "bugfix" | "feat" | "feature" | "refactor" | "perf" | "revert" => "decision",
        "docs" | "doc" | "comment" | "comments" => "reflection",
        _ => "decision",
    }
}

/// Read commit messages between `old_head` and `new_head` in
/// `repo_root`'s git, returning oldest-first `(hash, message)`
/// pairs. Returns an empty vec if the repo isn't a git repo or
/// either head is missing — in that case the harvester silently
/// no-ops.
///
/// `old_head` empty → no historical baseline; we don't try to
/// harvest the entire history. The first poll on a freshly-
/// registered project produces zero notes; subsequent polls
/// catch up from there.
pub fn read_commits_between(
    repo_root: &Path,
    old_head: &str,
    new_head: &str,
) -> Vec<(String, String)> {
    if old_head.is_empty() || new_head.is_empty() || old_head == new_head {
        return Vec::new();
    }
    // `git log <old>..<new> --format=%H%x1f%B%x1e --reverse` —
    // reverse for oldest-first. `%H` is the hash, `%B` is the
    // raw body (subject + body), `%x1f` (US, 0x1f) is the
    // field separator, `%x1e` (RS, 0x1e) terminates the record.
    // These low-byte separators are git's standard "won't appear
    // in commit messages" delimiters.
    let output = std::process::Command::new("git")
        .args([
            "log",
            "--format=%H%x1f%B%x1e",
            "--reverse",
            &format!("{old_head}..{new_head}"),
        ])
        .current_dir(repo_root)
        .output();
    let Ok(out) = output else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut commits = Vec::new();
    for record in stdout.split('\u{1e}') {
        let record = record.trim();
        if record.is_empty() {
            continue;
        }
        let mut parts = record.splitn(2, '\u{1f}');
        let Some(hash) = parts.next() else { continue };
        let Some(body) = parts.next() else { continue };
        commits.push((hash.trim().to_string(), body.trim().to_string()));
    }
    commits
}

/// End-to-end harvest: read git, filter, write `source='committed'`
/// notes. Returns the number of notes written. Errors are logged
/// at warn level and never bubble up — harvest failures must not
/// affect the surrounding rebuild logic.
pub async fn harvest_between(
    repo_root: &Path,
    old_head: &str,
    new_head: &str,
    notes: &NoteStore,
    session_id: &str,
) -> usize {
    let commits = read_commits_between(repo_root, old_head, new_head);
    let harvest = filter_and_classify(commits);
    let mut wrote = 0_usize;
    for c in harvest {
        // Idempotency guard keyed on (source='committed', hash).
        // Several registered projects can watch the same monorepo,
        // and a poll can re-cover an already-harvested range — both
        // used to write duplicates (4.5x measured on the live store
        // 2026-07-23). First writer wins; everyone else skips.
        match notes
            .note_exists_for_entity(NoteSource::Committed, &c.hash)
            .await
        {
            Ok(true) => {
                tracing::debug!(hash = %c.hash, "commit_harvest: already harvested — skipping");
                continue;
            }
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(
                    hash = %c.hash,
                    error = %e,
                    "commit_harvest: dedup lookup failed — writing anyway"
                );
            }
        }
        match notes
            .write_note_with_source(
                c.kind,
                &c.message,
                Vec::new(),
                Vec::new(),
                session_id,
                NoteScope::Global,
                None,
                Some(&c.hash),
                NoteSource::Committed,
                None,
            )
            .await
        {
            Ok(_) => wrote += 1,
            Err(e) => {
                tracing::warn!(
                    repo_root = %repo_root.display(),
                    hash = %c.hash,
                    error = %e,
                    "commit_harvest: failed to persist committed-source note"
                );
            }
        }
    }
    wrote
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `is_harvest_worthy`:
    /// - rejects each noise prefix with appropriate boundaries,
    /// - rejects messages below the word-count floor,
    /// - accepts substantial messages with no noise prefix.
    #[test]
    fn noise_prefixes_are_rejected_with_word_boundaries() {
        // Each on its own line so the failure points at the
        // specific case.
        assert!(!is_harvest_worthy("wip"));
        assert!(!is_harvest_worthy("WIP: still in progress"));
        assert!(!is_harvest_worthy("fix typo: trailing whitespace"));
        assert!(!is_harvest_worthy("Save: snapshot before rebase"));
        assert!(!is_harvest_worthy("Merge branch 'feature/x'"));
        assert!(!is_harvest_worthy("bump tokio to 1.45"));
        assert!(!is_harvest_worthy("format with rustfmt"));
        assert!(!is_harvest_worthy("Rename foo to bar"));
        // Substantial commit, no noise prefix → accepted.
        assert!(is_harvest_worthy(
            "Switch the storage layer from sync to async channels because sync \
             deadlocked under concurrent ingest workloads"
        ));
    }

    /// A noise word inside the message but not at the start should
    /// NOT match — the filter is anchored.
    #[test]
    fn noise_word_in_middle_does_not_match() {
        let msg = "Refactor the wiper-blade controller to expose a typed \
                   handle for downstream callers";
        assert!(is_harvest_worthy(msg));
    }

    /// A "wip" inside a longer prefix must not false-match. We
    /// require a word boundary so `wiper-blade-fix:` survives.
    /// Message must also clear the 10-word floor to reach the
    /// boundary check, so we pad it.
    #[test]
    fn boundary_check_prevents_substring_match() {
        let msg = "Wiper-blade replacement gating logic must reset on power cycle \
                   to avoid stuck-state hangs";
        assert!(is_harvest_worthy(msg));
    }

    /// Word-count floor — "Add foo" is 2 words, gets rejected
    /// regardless of prefix.
    #[test]
    fn short_messages_are_rejected() {
        assert!(!is_harvest_worthy("Add foo"));
        assert!(!is_harvest_worthy("Did the thing"));
        // 9 words (one shy of the floor) — still too short.
        assert!(!is_harvest_worthy(
            "One two three four five six seven eight nine"
        ));
        // 10 words — passes.
        assert!(is_harvest_worthy(
            "One two three four five six seven eight nine ten"
        ));
    }

    /// `infer_kind` maps conventional-commits prefixes; defaults to
    /// `decision` on anything unrecognised.
    #[test]
    fn kind_inference_maps_conventional_prefixes() {
        assert_eq!(infer_kind("fix: race in the pull path"), "decision");
        assert_eq!(infer_kind("feat: add canonical fingerprint"), "decision");
        assert_eq!(infer_kind("refactor: extract the harvester"), "decision");
        assert_eq!(infer_kind("perf: cache the spec stat result"), "decision");
        assert_eq!(
            infer_kind("docs: clarify the harvester contract"),
            "reflection"
        );
        // Unrecognised → conservative default.
        assert_eq!(infer_kind("Switch storage to async"), "decision");
    }

    /// `filter_and_classify` is a one-shot: all logic lives in
    /// `is_harvest_worthy` + `infer_kind`, and the cap is honoured.
    #[test]
    fn filter_and_classify_caps_at_max_commits() {
        let raw: Vec<(String, String)> = (0..(MAX_COMMITS + 5))
            .map(|i| {
                (
                    format!("hash{i:04}"),
                    "An adequate message with at least ten distinct word tokens \
                     so it passes the filter"
                        .to_string(),
                )
            })
            .collect();
        let out = filter_and_classify(raw);
        assert_eq!(out.len(), MAX_COMMITS);
    }

    /// `read_commits_between` is a no-op when either head is
    /// missing or they're equal. We don't shell out to git in
    /// these cases.
    #[test]
    fn read_commits_between_is_noop_for_empty_or_equal_heads() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_commits_between(tmp.path(), "", "abc").is_empty());
        assert!(read_commits_between(tmp.path(), "abc", "").is_empty());
        assert!(read_commits_between(tmp.path(), "abc", "abc").is_empty());
    }

    /// End-to-end with a real git repo: write a substantial commit
    /// after a baseline, run `harvest_between`, expect one
    /// `source='committed'` note in the store.
    #[tokio::test]
    async fn harvest_between_writes_committed_note_for_real_commit() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let store = NoteStore::open(&dir.path().join("notes.db")).unwrap();

        // git init + identity
        let init = std::process::Command::new("git")
            .args(["init", "--initial-branch=main"])
            .current_dir(repo)
            .output()
            .expect("git init");
        assert!(init.status.success());
        for (k, v) in [("user.email", "t@e.com"), ("user.name", "T")] {
            assert!(std::process::Command::new("git")
                .args(["config", k, v])
                .current_dir(repo)
                .status()
                .unwrap()
                .success());
        }

        // baseline commit
        std::fs::write(repo.join("README.md"), b"# r\n").unwrap();
        assert!(std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(repo)
            .status()
            .unwrap()
            .success());
        assert!(std::process::Command::new("git")
            .args([
                "commit",
                "-m",
                "baseline commit one two three four five six seven"
            ])
            .current_dir(repo)
            .status()
            .unwrap()
            .success());
        let old_head = head(repo);

        // substantial second commit
        std::fs::write(repo.join("README.md"), b"# r\n# update\n").unwrap();
        assert!(std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(repo)
            .status()
            .unwrap()
            .success());
        assert!(std::process::Command::new("git")
            .args([
                "commit",
                "-m",
                "Switch the storage layer from sync to async channels because sync \
                 deadlocked under concurrent ingest workloads",
            ])
            .current_dir(repo)
            .status()
            .unwrap()
            .success());
        let new_head = head(repo);

        let wrote = harvest_between(repo, &old_head, &new_head, &store, "harvest-test").await;
        assert_eq!(wrote, 1, "expected exactly one harvested commit");

        // Idempotency: the same range harvested again — as happens
        // when several registered projects watch one monorepo, or a
        // poll re-covers the range — writes nothing new.
        let rewrote = harvest_between(repo, &old_head, &new_head, &store, "harvest-other").await;
        assert_eq!(
            rewrote, 0,
            "re-harvest of the same range must dedup to zero"
        );

        // The new note exists with source='committed'.
        let rows = store
            .read_notes(None, &[], &[], &["decision".into()], 100, false)
            .await
            .unwrap();
        let committed: Vec<_> = rows
            .iter()
            .filter(|n| n.source == NoteSource::Committed.as_str())
            .collect();
        assert_eq!(committed.len(), 1);
        assert!(committed[0].content.contains("storage layer"));
    }

    fn head(repo: &Path) -> String {
        let out = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo)
            .output()
            .expect("git rev-parse");
        String::from_utf8(out.stdout).unwrap().trim().into()
    }
}
