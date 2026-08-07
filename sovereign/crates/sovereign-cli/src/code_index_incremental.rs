// SPDX-License-Identifier: AGPL-3.0-or-later
//! Incremental refresh for `svrn code index`.
//!
//! `rebuild_code_corpus` is a *rebuild*: it clears the LanceDB artifacts and
//! re-embeds every chunk in the repository. That is the correct behaviour for
//! a first index or a poisoned corpus, and the wrong one for the common case —
//! "I have been working for a week, bring the chunks up to date" — where a
//! handful of files changed and the other ~20k chunks are byte-identical to
//! what is already committed.
//!
//! The engine already has the primitive for that case:
//! [`corpus_engine::CorpusEngine::reindex_file`] resolves a file's chunks by
//! `source_doc_id`, hash-matches them against the committed rows via
//! `chunk_delta`, and embeds ONLY the chunks whose content actually changed
//! (`reindex_file.noop` / `delete_only` / `delta_applied`). Until now its only
//! driver was the foreground `CodeWatcher` (`svrn code watch`), so there was no
//! way to ask for a one-shot catch-up. This module is that driver.
//!
//! What decides the file set is a stamp this module writes beside the index
//! (`code_index_state.json`): the commit the corpus was last indexed at, plus
//! the paths that were dirty at the time. The next run refreshes
//! `git diff <stamp>..HEAD` ∪ `git status` (now) ∪ `dirty` (then). That last
//! term is the one that is easy to miss: a file edited-but-not-committed during
//! run N, then reverted before run N+1, appears in NEITHER of the first two
//! sets, and its stale chunks would survive indefinitely.
//!
//! Deliberately unchanged: the SCIP graph. `rebuild_code_corpus` preserves
//! `scip_graph.db` because the daemon's Reindexer owns it on a parallel
//! cadence, and this path preserves it for the same reason.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Sidecar filename, written inside `<data_dir>/<corpus_id>/`.
pub const STATE_FILE: &str = "code_index_state.json";

/// Past this many changed files, a full rebuild is usually the cheaper plan:
/// `reindex_file` opens the `CorpusIndex` per call, so the per-file constant
/// dominates once the delta stops being small. Auto mode falls back to a
/// rebuild here and says so; `--incremental` overrides.
pub const LARGE_DELTA_FILES: usize = 500;

/// What the last `svrn code index` run left behind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexState {
    /// Schema tag so a future format change is detectable rather than silently
    /// misread as a v1 stamp.
    pub schema: String,
    /// `git rev-parse HEAD` at index time. Empty when the root is not a git
    /// repo — which is exactly the case incremental mode declines to handle.
    pub head: String,
    /// Paths that were modified-but-uncommitted at index time. See the module
    /// docs for why this is load-bearing.
    pub dirty: Vec<String>,
    /// Unix seconds.
    pub indexed_at: u64,
    /// The source root the corpus was built from, for a sanity check against
    /// the root the caller passed this time.
    pub root: String,
}

pub const SCHEMA_V1: &str = "code-index-state/v1";

impl IndexState {
    pub fn new(head: String, dirty: Vec<String>, root: String, indexed_at: u64) -> Self {
        Self {
            schema: SCHEMA_V1.to_string(),
            head,
            dirty,
            indexed_at,
            root,
        }
    }

    /// Is this stamp usable as an incremental baseline for `root`?
    ///
    /// A stamp from a different source root describes a different file set, and
    /// an unrecognised schema means we cannot trust the fields we would diff
    /// against. Both answer "no" — the caller falls back to a full rebuild
    /// rather than refreshing a partial, wrong set of files.
    pub fn is_usable_for(&self, root: &Path) -> bool {
        self.schema == SCHEMA_V1 && !self.head.is_empty() && Path::new(&self.root) == root
    }

    pub fn path_in(index_dir: &Path) -> PathBuf {
        index_dir.join(STATE_FILE)
    }

    pub fn load(index_dir: &Path) -> Option<Self> {
        let raw = std::fs::read_to_string(Self::path_in(index_dir)).ok()?;
        serde_json::from_str(&raw).ok()
    }

    /// Best-effort write. A failed stamp is not a failed index: the corpus on
    /// disk is still correct, the next run just cannot compute a delta and
    /// falls back to a rebuild. So this warns rather than failing the command.
    pub fn save(&self, index_dir: &Path) {
        let path = Self::path_in(index_dir);
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    eprintln!(
                        "warning: could not write {} ({e}) — the next run will do a full rebuild",
                        path.display()
                    );
                }
            }
            Err(e) => eprintln!("warning: could not serialize index state: {e}"),
        }
    }
}

/// Union the three sources of "this file may have changed since we last
/// indexed" into one deduplicated, sorted path list.
///
/// Pure on purpose — the git plumbing is the caller's job, so the union rule
/// (the part with the subtle case in it) is directly testable.
pub fn plan_change_set(
    committed_since_stamp: &[String],
    dirty_now: &[String],
    dirty_at_stamp: &[String],
) -> Vec<String> {
    let mut set: BTreeSet<String> = BTreeSet::new();
    for src in [committed_since_stamp, dirty_now, dirty_at_stamp] {
        for p in src {
            let t = p.trim();
            if !t.is_empty() {
                set.insert(t.to_string());
            }
        }
    }
    set.into_iter().collect()
}

/// Files touched by commits between `base` and HEAD. Empty on any git failure
/// (unknown base after a rebase, not a repo) — the caller treats an empty plan
/// plus no dirty files as "nothing to do", and an unusable stamp as "rebuild",
/// so a silent empty here cannot be mistaken for a successful delta.
pub fn git_changed_since(repo_root: &Path, base: &str) -> Option<Vec<String>> {
    let out = std::process::Command::new("git")
        .args(["diff", "--name-only", &format!("{base}..HEAD")])
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(nonempty_lines(&String::from_utf8_lossy(&out.stdout)))
}

/// Modified-but-uncommitted + untracked paths, i.e. everything git would
/// report as not matching HEAD.
pub fn git_dirty_paths(repo_root: &Path) -> Vec<String> {
    let out = std::process::Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=all"])
        .current_dir(repo_root)
        .output();
    let Ok(o) = out else { return Vec::new() };
    if !o.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(parse_porcelain_path)
        .collect()
}

/// Pull the path out of one `git status --porcelain` line.
///
/// The format is a two-character status field, a space, then the path — and for
/// renames, `old -> new`. We want the destination: that is the file on disk now,
/// and the one whose chunks need rewriting. (The source path's chunks are
/// handled by `reindex_file`'s deleted-file branch when it is visited, since it
/// no longer exists.)
fn parse_porcelain_path(line: &str) -> Option<String> {
    if line.len() < 4 {
        return None;
    }
    let path = line[3..].trim();
    let path = match path.split_once(" -> ") {
        Some((_old, new)) => new,
        None => path,
    };
    let path = path.trim().trim_matches('"');
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

pub fn git_head(repo_root: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_root)
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

fn nonempty_lines(s: &str) -> Vec<String> {
    s.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect()
}

/// Why the command chose the mode it did — printed verbatim, so a run that
/// silently rebuilt everything is impossible to confuse with one that refreshed
/// 12 files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Refresh exactly these repo-relative paths.
    Incremental { files: Vec<String>, base: String },
    /// Clear and re-embed. `reason` is partner-facing.
    Full { reason: String },
    /// Corpus is already current — do nothing at all.
    UpToDate { base: String },
}

/// A resolved answer to "what changed, and since when?"
///
/// Two producers, and the second is what stops this feature from costing every
/// existing corpus one more full rebuild before it can help:
///
///  - [`resolve_from_stamp`] — the precise path. A commit to diff against plus
///    the paths that were dirty when it was written.
///  - [`resolve_from_mtime`] — the fallback for a corpus indexed before stamps
///    existed (i.e. every corpus on every machine today). The corpus's own
///    `_corpus_meta.json:last_updated` is a real "this index is current as of"
///    timestamp, so a source file with a newer mtime is exactly the set the
///    index has not seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDelta {
    /// Partner-facing description of the baseline, e.g. `commit a1b2c3d4` or
    /// `the last index run`. Printed, so it must read as prose.
    pub label: String,
    /// Paths git (or the mtime walk) says changed since the baseline.
    pub changed: Vec<String>,
    /// Paths recorded dirty at stamp time; empty for the mtime path.
    pub dirty_at_stamp: Vec<String>,
}

/// Decide between incremental and full.
///
/// `resolved` carries its own failure reason so every "we are rebuilding, and
/// here is precisely why" string is produced where the knowledge lives, rather
/// than being re-derived from a pile of Options here.
pub fn decide(
    corpus_exists: bool,
    resolved: Result<ResolvedDelta, String>,
    dirty_now: &[String],
    force_full: bool,
    force_incremental: bool,
) -> Plan {
    if force_full {
        return Plan::Full {
            reason: "--full requested".to_string(),
        };
    }
    if !corpus_exists {
        return Plan::Full {
            reason: "no existing index for this corpus".to_string(),
        };
    }
    let delta = match resolved {
        Ok(d) => d,
        Err(reason) => return Plan::Full { reason },
    };

    let files = plan_change_set(&delta.changed, dirty_now, &delta.dirty_at_stamp);
    if files.is_empty() {
        return Plan::UpToDate { base: delta.label };
    }
    if files.len() > LARGE_DELTA_FILES && !force_incremental {
        return Plan::Full {
            reason: format!(
                "{} files changed since {} — past the {LARGE_DELTA_FILES}-file mark a rebuild is \
                 usually faster (pass --incremental to refresh them one by one anyway)",
                files.len(),
                delta.label,
            ),
        };
    }
    Plan::Incremental {
        files,
        base: delta.label,
    }
}

/// Precise path: diff the stamped commit against HEAD.
pub fn resolve_from_stamp(
    state: &IndexState,
    root: &Path,
    is_git: bool,
) -> Result<ResolvedDelta, String> {
    if !is_git {
        return Err(
            "source root is not a git repository, so there is no commit delta to compute"
                .to_string(),
        );
    }
    if !state.is_usable_for(root) {
        return Err(format!(
            "the {STATE_FILE} stamp does not describe this source root"
        ));
    }
    // A missing base is NOT an empty delta. Reading it as one would report
    // "already up to date" over a corpus that is arbitrarily stale — the exact
    // silent-staleness failure this whole feature exists to end.
    let changed = git_changed_since(root, &state.head).ok_or_else(|| {
        format!(
            "cannot diff {}..HEAD — the recorded commit is unknown here (rebase, shallow clone, \
             or a pruned branch)",
            short(&state.head)
        )
    })?;
    Ok(ResolvedDelta {
        label: format!("commit {}", short(&state.head)),
        changed,
        dirty_at_stamp: state.dirty.clone(),
    })
}

/// Fallback path: every source file whose mtime is newer than the corpus's own
/// `last_updated`.
///
/// `list_source_files` is injected so the walk is the caller's concern and this
/// stays testable without a filesystem.
pub fn resolve_from_mtime(
    last_updated: Option<u64>,
    files_with_mtime: Option<Vec<(String, u64)>>,
) -> Result<ResolvedDelta, String> {
    let Some(last_updated) = last_updated else {
        return Err(format!(
            "no {STATE_FILE} stamp and no readable last_updated on the corpus — nothing to diff \
             against"
        ));
    };
    if last_updated == 0 {
        return Err("the corpus records no last_updated timestamp".to_string());
    }
    // A failed file listing must NOT read as "no files changed" — that would
    // report the corpus up to date while it rots. Refuse instead.
    let Some(files_with_mtime) = files_with_mtime else {
        return Err(
            "cannot list the repository's source files (not a git repo?), so there is no mtime \
             delta to compute"
                .to_string(),
        );
    };
    let changed = files_with_mtime
        .into_iter()
        .filter(|(_, mtime)| *mtime > last_updated)
        .map(|(p, _)| p)
        .collect();
    Ok(ResolvedDelta {
        label: "the last index run".to_string(),
        changed,
        dirty_at_stamp: Vec::new(),
    })
}

/// Every file git considers part of the working tree — tracked plus untracked
/// that is not ignored — paired with its mtime in unix seconds.
///
/// Listed via git rather than a raw directory walk so `.gitignore` is honoured
/// for free. A raw walk would drag `target/` into the change set, which on this
/// workspace is millions of files and would blow past the large-delta guard on
/// every single run.
pub fn source_files_with_mtime(repo_root: &Path) -> Option<Vec<(String, u64)>> {
    let out = std::process::Command::new("git")
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let listing = String::from_utf8_lossy(&out.stdout);
    let mut files = Vec::new();
    for rel in listing.split('\0').filter(|s| !s.trim().is_empty()) {
        let abs = repo_root.join(rel);
        let Ok(meta) = std::fs::metadata(&abs) else {
            continue;
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        files.push((rel.to_string(), mtime));
    }
    Some(files)
}

/// `last_updated` out of the corpus's own metadata sidecar.
pub fn corpus_last_updated(index_dir: &Path) -> Option<u64> {
    let raw = std::fs::read_to_string(index_dir.join("_corpus_meta.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.get("last_updated").and_then(|x| x.as_u64())
}

fn short(sha: &str) -> String {
    sha.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(head: &str, dirty: &[&str], root: &str) -> IndexState {
        IndexState::new(
            head.to_string(),
            dirty.iter().map(|s| s.to_string()).collect(),
            root.to_string(),
            0,
        )
    }

    #[test]
    fn change_set_unions_and_dedupes() {
        let got = plan_change_set(
            &["a.rs".into(), "b.rs".into()],
            &["b.rs".into(), "c.rs".into()],
            &["d.rs".into()],
        );
        assert_eq!(got, vec!["a.rs", "b.rs", "c.rs", "d.rs"]);
    }

    /// The case the third argument exists for: a file edited but never
    /// committed during the last run, then reverted. It is in neither the
    /// commit diff nor the current dirty set, so without the recorded
    /// `dirty` list its stale chunks would never be refreshed.
    #[test]
    fn change_set_includes_files_dirty_at_last_stamp() {
        let got = plan_change_set(&[], &[], &["reverted.rs".into()]);
        assert_eq!(got, vec!["reverted.rs"]);
    }

    #[test]
    fn change_set_ignores_blank_lines() {
        let got = plan_change_set(&["".into(), "  ".into(), "a.rs".into()], &[], &[]);
        assert_eq!(got, vec!["a.rs"]);
    }

    #[test]
    fn porcelain_paths_cover_modified_untracked_and_renames() {
        assert_eq!(parse_porcelain_path(" M src/a.rs").unwrap(), "src/a.rs");
        assert_eq!(parse_porcelain_path("?? src/new.rs").unwrap(), "src/new.rs");
        // A rename yields the DESTINATION — that's the file on disk now.
        assert_eq!(
            parse_porcelain_path("R  src/old.rs -> src/new.rs").unwrap(),
            "src/new.rs"
        );
        assert!(parse_porcelain_path("").is_none());
    }

    #[test]
    fn state_round_trips_through_json() {
        let s = state("abc123", &["x.rs"], "/repo");
        let dir = std::env::temp_dir().join(format!("cis-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        s.save(&dir);
        let back = IndexState::load(&dir).expect("stamp readable");
        assert_eq!(back, s);
        std::fs::remove_dir_all(&dir).ok();
    }

    fn delta(label: &str, changed: &[&str], dirty_at_stamp: &[&str]) -> ResolvedDelta {
        ResolvedDelta {
            label: label.to_string(),
            changed: changed.iter().map(|s| s.to_string()).collect(),
            dirty_at_stamp: dirty_at_stamp.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn missing_corpus_forces_full() {
        assert!(matches!(
            decide(false, Ok(delta("c", &[], &[])), &[], false, false),
            Plan::Full { .. }
        ));
    }

    /// An unresolvable baseline carries its own reason and must surface as a
    /// rebuild — never as "up to date", which would leave the corpus stale
    /// while reporting success.
    #[test]
    fn unresolved_baseline_forces_full_not_uptodate() {
        match decide(true, Err("stamp is gone".into()), &[], false, false) {
            Plan::Full { reason } => assert_eq!(reason, "stamp is gone"),
            other => panic!("expected full, got {other:?}"),
        }
    }

    #[test]
    fn no_changes_is_up_to_date() {
        assert!(matches!(
            decide(true, Ok(delta("commit abc", &[], &[])), &[], false, false),
            Plan::UpToDate { .. }
        ));
    }

    #[test]
    fn small_delta_goes_incremental() {
        match decide(
            true,
            Ok(delta("commit abc", &["a.rs"], &[])),
            &["b.rs".to_string()],
            false,
            false,
        ) {
            Plan::Incremental { files, base } => {
                assert_eq!(files, vec!["a.rs", "b.rs"]);
                assert_eq!(base, "commit abc");
            }
            other => panic!("expected incremental, got {other:?}"),
        }
    }

    #[test]
    fn large_delta_falls_back_to_full_unless_forced() {
        let many: Vec<String> = (0..LARGE_DELTA_FILES + 1)
            .map(|i| format!("f{i}.rs"))
            .collect();
        let d = ResolvedDelta {
            label: "commit abc".into(),
            changed: many,
            dirty_at_stamp: vec![],
        };
        assert!(matches!(
            decide(true, Ok(d.clone()), &[], false, false),
            Plan::Full { .. }
        ));
        assert!(matches!(
            decide(true, Ok(d), &[], false, true),
            Plan::Incremental { .. }
        ));
    }

    #[test]
    fn force_full_beats_every_other_signal() {
        assert!(matches!(
            decide(true, Ok(delta("commit abc", &[], &[])), &[], true, false),
            Plan::Full { .. }
        ));
    }

    #[test]
    fn stamp_resolution_requires_git_and_a_matching_root() {
        let s = state("abc", &[], "/repo");
        assert!(resolve_from_stamp(&s, Path::new("/repo"), false).is_err());
        assert!(resolve_from_stamp(&s, Path::new("/elsewhere"), true).is_err());
    }

    /// The fallback that spares every pre-existing corpus one more full
    /// rebuild: files newer than the corpus's own last_updated are exactly
    /// the ones the index has not seen.
    #[test]
    fn mtime_fallback_selects_only_files_newer_than_last_index() {
        let files = vec![
            ("old.rs".to_string(), 100u64),
            ("same.rs".to_string(), 500),
            ("new.rs".to_string(), 900),
        ];
        let d = resolve_from_mtime(Some(500), Some(files)).expect("resolves");
        assert_eq!(d.changed, vec!["new.rs"]);
        assert!(d.dirty_at_stamp.is_empty());
    }

    #[test]
    fn mtime_fallback_refuses_without_a_timestamp() {
        assert!(resolve_from_mtime(None, Some(vec![])).is_err());
        assert!(resolve_from_mtime(Some(0), Some(vec![])).is_err());
        // A failed listing is not an empty delta.
        assert!(resolve_from_mtime(Some(500), None).is_err());
    }
}
