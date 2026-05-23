//! Git archaeology — temporal grounding for atlas atoms.
//!
//! Given a code corpus that's been indexed and atom-extracted, this
//! module enriches each atom with **provenance** (first-seen,
//! last-modified, stability days, primary authors), flags atoms whose
//! source has shifted since the atlas was built (**staleness**), and
//! computes file pairs that always change together (**co-evolution**).
//!
//! All of the above is mechanical and cheap: one batch call to
//! `git log --name-only` builds a per-file commit index in memory,
//! and every per-atom enrichment is a HashMap lookup against that
//! index. For a 5,000-atom atlas the entire pass is seconds.
//!
//! ## Design — sidecar, not envelope extension
//!
//! [`AtomEnvelope`] stays unchanged. Archaeology data is written to
//! `~/.sovereign/indexes/<corpus>/atlas/git_archaeology.json` via
//! [`GitArchaeologyReport`]. The drift-report renderer folds it in by
//! file-path, the same way the rough-edges sidecar is folded in. No
//! schema bump on existing atlases.
//!
//! ## Path semantics
//!
//! Every [`PathBuf`] in this module is **relative to the repo root**
//! (`git rev-parse --show-toplevel`). That's the format `git log`
//! emits natively, and any chunk file_path stored relative to a
//! sub-source-root has to be lifted into repo-root-relative form
//! by the caller before lookup. See [`discover_repo_root`] and
//! [`source_to_repo_relative`] for the helpers.
//!
//! ## What this module does NOT do
//!
//! - Doesn't follow renames (`git log --follow` doesn't compose with
//!   `--name-only` on multi-file batch). The report's manifest stamps
//!   `follows_renames: false` so consumers know.
//! - Doesn't normalise author identities (`alice@example.com` vs.
//!   `alice@laptop.local`). Raw email goes in, raw email comes out.
//!   v2's Person-Knowledge Locus work owns mailmap-aware merging.
//! - Doesn't introduce new atom types. Person-Knowledge Locus and
//!   Lineage atoms are deferred — those need new envelope variants
//!   and their own design.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

// ── Errors ─────────────────────────────────────────────────────

/// Anything that can go wrong while talking to git.
#[derive(Debug)]
pub enum GitArchaeologyError {
    /// `repo_root` doesn't look like a git repository (no `.git`
    /// directory and `git rev-parse --is-inside-work-tree` failed).
    NotGitRepo(PathBuf),
    /// `git` is not installed or not on PATH.
    GitNotInstalled(std::io::Error),
    /// The git subprocess ran but exited non-zero.
    GitCommandFailed { cmd: String, stderr: String },
    /// `git log` output didn't parse — usually means the format flags
    /// don't match what we asked for, which would be a bug here, not
    /// in the user's repo.
    Parse(String),
}

impl std::fmt::Display for GitArchaeologyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotGitRepo(p) => write!(f, "{} is not a git repository", p.display()),
            Self::GitNotInstalled(e) => write!(f, "git is not installed or not on PATH: {e}"),
            Self::GitCommandFailed { cmd, stderr } => {
                write!(f, "`{cmd}` failed: {}", stderr.trim())
            }
            Self::Parse(msg) => write!(f, "parse git log output: {msg}"),
        }
    }
}

impl std::error::Error for GitArchaeologyError {}

// ── Domain types (serde-friendly) ─────────────────────────────

/// One commit. `file_paths` is emitted by `git log --name-only` and
/// lists every file the commit touched, repo-root-relative.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommitRecord {
    pub hash: String,
    /// Author timestamp in **unix seconds, UTC**.
    pub timestamp: i64,
    pub author_email: String,
    pub subject: String,
    pub file_paths: Vec<PathBuf>,
}

/// Compact pointer to a single commit. Used inside [`AtomProvenance`]
/// to record the first/last commit that touched the atom's file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommitRef {
    pub hash: String,
    /// Author date in `YYYY-MM-DD` UTC.
    pub date_iso: String,
    pub author_email: String,
    pub subject: String,
}

impl CommitRef {
    fn from_record(c: &CommitRecord) -> Self {
        Self {
            hash: c.hash.clone(),
            date_iso: format_iso_date(c.timestamp),
            author_email: c.author_email.clone(),
            subject: c.subject.clone(),
        }
    }
}

/// Has the atom's file changed since the atlas was built?
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Staleness {
    /// File hasn't been touched since `atlas_built_at`. The atom's
    /// extraction is presumed-current.
    Fresh,
    /// At least one commit has touched the file since
    /// `atlas_built_at`. The atom needs re-validation.
    Moved,
}

/// Per-atom git-derived provenance. One per atom that has a
/// resolvable file path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtomProvenance {
    pub atom_id: String,
    pub file_path: PathBuf,
    pub first_seen: CommitRef,
    pub last_modified: CommitRef,
    /// Whole days between `first_seen` and `last_modified`. Saturates
    /// at 0 if last_modified < first_seen (clock skew).
    pub stability_days: u32,
    pub modification_count: u32,
    /// Top-N authors ranked by commit count touching this file
    /// (default N = 3). Ties broken alphabetically by email.
    pub primary_authors: Vec<String>,
    pub staleness: Staleness,
}

/// One pair of files that change together more than `threshold`
/// of the time, with at least `min_joint_commits` joint commits
/// to filter trivial scaffolding-era pairs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoEvolutionPair {
    pub file_a: PathBuf,
    pub file_b: PathBuf,
    pub joint_commits: u32,
    pub a_only: u32,
    pub b_only: u32,
    /// Jaccard index: `joint / (joint + a_only + b_only)`.
    pub correlation: f32,
}

/// Tally for the drift-report renderer.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StalenessSummary {
    pub fresh: usize,
    pub moved: usize,
}

/// The full sidecar payload — written to
/// `~/.sovereign/indexes/<corpus>/atlas/git_archaeology.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitArchaeologyReport {
    pub corpus_id: String,
    pub repo_root: PathBuf,
    /// Unix seconds. Used to compute [`AtomProvenance::staleness`].
    pub atlas_built_at: i64,
    pub atom_count: usize,
    /// Atoms whose chunk had a resolvable code-corpus `file_path` and
    /// for which git history was found. (Atoms anchored to chunks
    /// that don't carry `file_path` — e.g. Wikipedia atoms — are
    /// counted in `atom_count` but not in this number.)
    pub atoms_with_history: usize,
    /// V1 limitation flag — see module-level docs.
    pub follows_renames: bool,
    pub provenance: Vec<AtomProvenance>,
    pub co_evolution: Vec<CoEvolutionPair>,
    pub staleness_summary: StalenessSummary,
}

// ── Repo discovery ────────────────────────────────────────────

/// Resolve the outermost git repo root containing `source_path`.
/// Wraps `git rev-parse --show-toplevel`, matching the workspace's
/// existing git-subprocess idiom (see
/// `sovereign-tools/src/local_corpus/git.rs`).
pub fn discover_repo_root(source_path: &Path) -> Result<PathBuf, GitArchaeologyError> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(source_path)
        .output()
        .map_err(GitArchaeologyError::GitNotInstalled)?;
    if !out.status.success() {
        return Err(GitArchaeologyError::NotGitRepo(source_path.to_path_buf()));
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        return Err(GitArchaeologyError::NotGitRepo(source_path.to_path_buf()));
    }
    Ok(PathBuf::from(s))
}

/// Lift a chunk path (which is stored relative to the corpus's
/// `source_path`) into the **repo-root-relative** path that
/// `git log` emits. Caller must pass canonical/absolute forms of
/// both `source_path` and `repo_root`; otherwise the prefix
/// arithmetic falls back to the chunk path verbatim.
///
/// Example: corpus indexed from `/repo/sub`, chunk file_path
/// `mod/foo.rs`, repo_root `/repo` → returns `sub/mod/foo.rs`.
pub fn source_to_repo_relative(
    source_path: &Path,
    repo_root: &Path,
    chunk_file_path: &Path,
) -> PathBuf {
    let prefix = source_path.strip_prefix(repo_root).unwrap_or(Path::new(""));
    if prefix.as_os_str().is_empty() {
        chunk_file_path.to_path_buf()
    } else {
        prefix.join(chunk_file_path)
    }
}

// ── Batch walker ──────────────────────────────────────────────

/// Run `git log --name-only` once across the whole repo and return a
/// map keyed by file path. The resulting `Vec<CommitRecord>` for each
/// file is ordered **oldest first** (matches `--reverse`).
///
/// Performance: one subprocess, one stdout parse. For a repo with
/// ~10,000 commits across ~3,000 files this returns in well under
/// a second; the dominant cost is git itself, not the parse.
pub fn batch_harvest_all_commits(
    repo_root: &Path,
) -> Result<HashMap<PathBuf, Vec<CommitRecord>>, GitArchaeologyError> {
    // %H = hash, %ct = committer-time unix-seconds, %ae = author email,
    // %s = subject. Field-separator %x1f (US); record-separator %x1e
    // (RS). These are git's standard "won't appear in real data"
    // delimiters; commit_harvest uses the same pair.
    //
    // The RS is placed at the **start** of every record so the path
    // block emitted by `--name-only` (which trails the format string)
    // naturally trails the formatted fields without colliding with
    // the next record's header. Splitting on `\u{1e}` then yields one
    // chunk per commit shaped `HASH<US>TS<US>EMAIL<US>SUBJECT\npath\npath\n`.
    let out = Command::new("git")
        .args([
            "log",
            "--name-only",
            "--format=%x1e%H%x1f%ct%x1f%ae%x1f%s",
            "--reverse",
            "--all",
        ])
        .current_dir(repo_root)
        .output()
        .map_err(GitArchaeologyError::GitNotInstalled)?;
    if !out.status.success() {
        return Err(GitArchaeologyError::GitCommandFailed {
            cmd: "git log --name-only --reverse --all".into(),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        });
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut by_path: HashMap<PathBuf, Vec<CommitRecord>> = HashMap::new();

    for record in stdout.split('\u{1e}') {
        // First split is the empty leader before the first RS.
        let trimmed = record.trim_matches('\n');
        if trimmed.is_empty() {
            continue;
        }
        // Header is the first line; everything after is the path block.
        let (header, paths_block) = match trimmed.split_once('\n') {
            Some((h, rest)) => (h, rest),
            None => (trimmed, ""),
        };
        let mut parts = header.splitn(4, '\u{1f}');
        let hash = parts
            .next()
            .ok_or_else(|| GitArchaeologyError::Parse("missing hash".into()))?
            .trim()
            .to_string();
        if hash.is_empty() {
            continue;
        }
        let timestamp_raw = parts
            .next()
            .ok_or_else(|| GitArchaeologyError::Parse("missing timestamp".into()))?;
        let timestamp: i64 = timestamp_raw.trim().parse().map_err(|e| {
            GitArchaeologyError::Parse(format!("timestamp `{timestamp_raw}`: {e}"))
        })?;
        let author_email = parts
            .next()
            .ok_or_else(|| GitArchaeologyError::Parse("missing author".into()))?
            .trim()
            .to_string();
        let subject = parts.next().unwrap_or("").trim().to_string();

        let file_paths: Vec<PathBuf> = paths_block
            .split('\n')
            .filter_map(|line| {
                let l = line.trim();
                if l.is_empty() {
                    None
                } else {
                    Some(PathBuf::from(l))
                }
            })
            .collect();

        let rec = CommitRecord {
            hash,
            timestamp,
            author_email,
            subject,
            file_paths: file_paths.clone(),
        };

        for p in file_paths {
            by_path.entry(p).or_default().push(rec.clone());
        }
    }

    // Each per-path list is already oldest-first because of `--reverse`.
    // Belt-and-braces: stable-sort by timestamp so a tie or out-of-order
    // record doesn't fool the consumer.
    for v in by_path.values_mut() {
        v.sort_by_key(|c| c.timestamp);
    }

    Ok(by_path)
}

// ── Per-atom enrichment ───────────────────────────────────────

/// Build [`AtomProvenance`] for one atom-id-and-path pair. Returns
/// `None` if the file has no commit history (untracked or outside the
/// repo) — the caller treats that as "no provenance available."
///
/// `atlas_built_at` is unix seconds; any commit with
/// `timestamp > atlas_built_at` flips the staleness flag to
/// [`Staleness::Moved`].
pub fn enrich_atom(
    atom_id: &str,
    file_path: &Path,
    history: &HashMap<PathBuf, Vec<CommitRecord>>,
    atlas_built_at: i64,
) -> Option<AtomProvenance> {
    let commits = history.get(file_path)?;
    if commits.is_empty() {
        return None;
    }
    let first = commits.first().expect("non-empty");
    let last = commits.last().expect("non-empty");

    let stability_days = days_between(first.timestamp, last.timestamp);

    // Author histogram → top 3 by count, tiebreak alphabetical.
    let mut counts: HashMap<&str, u32> = HashMap::new();
    for c in commits {
        *counts.entry(c.author_email.as_str()).or_insert(0) += 1;
    }
    let mut ranked: Vec<(&str, u32)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let primary_authors: Vec<String> =
        ranked.into_iter().take(3).map(|(e, _)| e.to_string()).collect();

    let staleness = if last.timestamp > atlas_built_at {
        Staleness::Moved
    } else {
        Staleness::Fresh
    };

    Some(AtomProvenance {
        atom_id: atom_id.to_string(),
        file_path: file_path.to_path_buf(),
        first_seen: CommitRef::from_record(first),
        last_modified: CommitRef::from_record(last),
        stability_days,
        modification_count: commits.len() as u32,
        primary_authors,
        staleness,
    })
}

// ── Co-evolution ──────────────────────────────────────────────

/// Compute file pairs that change together. Two thresholds:
///
/// - `correlation_threshold` — minimum jaccard index. Default 0.5
///   ("more than half of all commits that touch either file touch
///   both"). Range \[0.0, 1.0].
/// - `min_joint_commits` — minimum joint commit count. Default 5
///   (drops the common pathology where two files were both edited
///   in one initial scaffolding commit and never together again).
///
/// Output is sorted descending by correlation. Returns at most
/// the natural pair count — caller can `.take(N)` for the digest.
pub fn compute_co_evolution(
    history: &HashMap<PathBuf, Vec<CommitRecord>>,
    correlation_threshold: f32,
    min_joint_commits: u32,
) -> Vec<CoEvolutionPair> {
    // Build per-file commit-hash sets.
    let mut hashes_for: HashMap<&Path, std::collections::HashSet<&str>> = HashMap::new();
    for (path, commits) in history {
        let set: std::collections::HashSet<&str> =
            commits.iter().map(|c| c.hash.as_str()).collect();
        hashes_for.insert(path.as_path(), set);
    }

    // Iterate over canonical (a, b) pairs (a < b lexicographically).
    let mut paths: Vec<&Path> = hashes_for.keys().copied().collect();
    paths.sort();

    let mut out: Vec<CoEvolutionPair> = Vec::new();
    for i in 0..paths.len() {
        for j in (i + 1)..paths.len() {
            let a = paths[i];
            let b = paths[j];
            let sa = &hashes_for[a];
            let sb = &hashes_for[b];
            let joint = sa.intersection(sb).count() as u32;
            if joint < min_joint_commits {
                continue;
            }
            let a_only = (sa.len() as u32).saturating_sub(joint);
            let b_only = (sb.len() as u32).saturating_sub(joint);
            let denom = (joint + a_only + b_only) as f32;
            if denom == 0.0 {
                continue;
            }
            let correlation = joint as f32 / denom;
            if correlation < correlation_threshold {
                continue;
            }
            out.push(CoEvolutionPair {
                file_a: a.to_path_buf(),
                file_b: b.to_path_buf(),
                joint_commits: joint,
                a_only,
                b_only,
                correlation,
            });
        }
    }

    out.sort_by(|x, y| {
        y.correlation
            .partial_cmp(&x.correlation)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| y.joint_commits.cmp(&x.joint_commits))
            .then_with(|| x.file_a.cmp(&y.file_a))
    });
    out
}

// ── Helpers ───────────────────────────────────────────────────

fn days_between(start_ts: i64, end_ts: i64) -> u32 {
    if end_ts <= start_ts {
        return 0;
    }
    ((end_ts - start_ts) / 86_400) as u32
}

/// Format a unix timestamp (seconds, UTC) as `YYYY-MM-DD`.
/// Hand-rolled to avoid pulling chrono into this module's public API
/// when callers only ever want a displayable string.
fn format_iso_date(ts: i64) -> String {
    // chrono is already a corpus-engine dependency.
    use chrono::TimeZone;
    chrono::Utc
        .timestamp_opt(ts, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| format!("unix:{ts}"))
}

// ── Tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as Cmd;

    /// Helper: initialise a git repo with a deterministic identity.
    fn init_repo(dir: &Path) {
        assert!(Cmd::new("git")
            .args(["init", "--initial-branch=main"])
            .current_dir(dir)
            .status()
            .unwrap()
            .success());
        for (k, v) in [("user.email", "test@example.com"), ("user.name", "Test")] {
            assert!(Cmd::new("git")
                .args(["config", k, v])
                .current_dir(dir)
                .status()
                .unwrap()
                .success());
        }
    }

    /// Make a commit with `msg`, with a chosen author timestamp (unix
    /// seconds). Uses GIT_AUTHOR_DATE / GIT_COMMITTER_DATE so we can
    /// build a reproducible historical timeline inside a temp repo.
    fn commit(dir: &Path, msg: &str, ts: i64, author_email: Option<&str>) {
        let date_str = format!("{ts} +0000");
        let mut cmd = Cmd::new("git");
        cmd.args(["commit", "-m", msg, "--allow-empty"])
            .current_dir(dir)
            .env("GIT_AUTHOR_DATE", &date_str)
            .env("GIT_COMMITTER_DATE", &date_str);
        if let Some(email) = author_email {
            cmd.env("GIT_AUTHOR_EMAIL", email)
                .env("GIT_COMMITTER_EMAIL", email);
        }
        assert!(cmd.status().unwrap().success(), "git commit `{msg}`");
    }

    fn write_and_add(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, body).unwrap();
        assert!(Cmd::new("git")
            .args(["add", rel])
            .current_dir(dir)
            .status()
            .unwrap()
            .success());
    }

    #[test]
    fn batch_harvest_returns_per_file_history_oldest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        // T0: introduce foo.rs
        write_and_add(repo, "foo.rs", "fn a() {}\n");
        commit(repo, "introduce foo", 1_700_000_000, None);

        // T1: introduce bar.rs alongside a foo touch
        write_and_add(repo, "foo.rs", "fn a() { let _ = 1; }\n");
        write_and_add(repo, "bar.rs", "fn b() {}\n");
        commit(repo, "joint touch", 1_700_086_400, None);

        // T2: bar.rs only
        write_and_add(repo, "bar.rs", "fn b() { let _ = 2; }\n");
        commit(repo, "bar update", 1_700_172_800, None);

        let history = batch_harvest_all_commits(repo).expect("walker");

        let foo = history.get(Path::new("foo.rs")).expect("foo.rs history");
        assert_eq!(foo.len(), 2, "foo.rs touched twice");
        assert!(foo[0].timestamp < foo[1].timestamp, "oldest first");
        assert_eq!(foo[0].subject, "introduce foo");
        assert_eq!(foo[1].subject, "joint touch");

        let bar = history.get(Path::new("bar.rs")).expect("bar.rs history");
        assert_eq!(bar.len(), 2);
        assert_eq!(bar[1].subject, "bar update");
    }

    #[test]
    fn enrich_atom_computes_stability_and_authors() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        // 3 commits over ~100 days, two authors.
        write_and_add(repo, "lib.rs", "fn v1() {}\n");
        commit(repo, "v1", 1_700_000_000, Some("alice@example.com"));
        write_and_add(repo, "lib.rs", "fn v2() {}\n");
        commit(repo, "v2", 1_700_000_000 + 50 * 86_400, Some("bob@example.com"));
        write_and_add(repo, "lib.rs", "fn v3() {}\n");
        commit(
            repo,
            "v3",
            1_700_000_000 + 100 * 86_400,
            Some("alice@example.com"),
        );

        let history = batch_harvest_all_commits(repo).unwrap();
        // atlas_built_at AFTER the last commit → fresh
        let atlas_built_at = 1_700_000_000 + 100 * 86_400 + 1;
        let prov = enrich_atom("entity-0001", Path::new("lib.rs"), &history, atlas_built_at)
            .expect("provenance");

        assert_eq!(prov.atom_id, "entity-0001");
        assert_eq!(prov.modification_count, 3);
        assert_eq!(prov.stability_days, 100);
        // Alice has 2 commits, Bob has 1 → Alice first.
        assert_eq!(prov.primary_authors[0], "alice@example.com");
        assert_eq!(prov.primary_authors[1], "bob@example.com");
        assert_eq!(prov.staleness, Staleness::Fresh);
    }

    #[test]
    fn enrich_atom_flips_to_moved_when_atlas_predates_last_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        write_and_add(repo, "lib.rs", "fn v1() {}\n");
        commit(repo, "v1", 1_700_000_000, None);
        // Atlas built_at is between v1 and v2.
        let atlas_built_at = 1_700_000_000 + 10 * 86_400;
        write_and_add(repo, "lib.rs", "fn v2() {}\n");
        commit(repo, "v2", 1_700_000_000 + 30 * 86_400, None);

        let history = batch_harvest_all_commits(repo).unwrap();
        let prov =
            enrich_atom("entity-0001", Path::new("lib.rs"), &history, atlas_built_at).unwrap();
        assert_eq!(prov.staleness, Staleness::Moved);
    }

    #[test]
    fn enrich_atom_returns_none_for_unknown_path() {
        let history: HashMap<PathBuf, Vec<CommitRecord>> = HashMap::new();
        assert!(enrich_atom("e", Path::new("nope.rs"), &history, 0).is_none());
    }

    #[test]
    fn co_evolution_filters_by_threshold_and_min_joint() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        // a.rs and b.rs co-change 8×; b.rs and c.rs co-change once.
        // Expectation: only (a, b) crosses both thresholds.
        for i in 0..8 {
            write_and_add(repo, "a.rs", &format!("fn a{i}() {{}}\n"));
            write_and_add(repo, "b.rs", &format!("fn b{i}() {{}}\n"));
            commit(repo, &format!("ab joint {i}"), 1_700_000_000 + i * 86_400, None);
        }
        // One b/c joint touch.
        write_and_add(repo, "b.rs", "fn b8() {}\n");
        write_and_add(repo, "c.rs", "fn c0() {}\n");
        commit(repo, "bc joint", 1_700_000_000 + 100 * 86_400, None);

        let history = batch_harvest_all_commits(repo).unwrap();
        let pairs = compute_co_evolution(&history, 0.5, 5);

        // Only a/b passes min_joint_commits=5 and correlation>=0.5.
        assert_eq!(pairs.len(), 1, "only a.rs/b.rs should pass");
        assert_eq!(pairs[0].file_a, PathBuf::from("a.rs"));
        assert_eq!(pairs[0].file_b, PathBuf::from("b.rs"));
        assert_eq!(pairs[0].joint_commits, 8);
        // a.rs has 8 commits, b.rs has 9 (8 ab + 1 bc), joint is 8.
        // jaccard = 8 / (8 + 0 + 1) = 0.888…
        assert!((pairs[0].correlation - (8.0 / 9.0)).abs() < 1e-4);
    }

    #[test]
    fn discover_repo_root_resolves_subdirectory() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        write_and_add(repo, "sub/file.rs", "fn x() {}\n");
        commit(repo, "create sub", 1_700_000_000, None);

        let sub = repo.join("sub");
        let resolved = discover_repo_root(&sub).expect("repo root");
        // tempdir paths can be symlinked on macOS — compare via canonicalize.
        let resolved_c = std::fs::canonicalize(&resolved).unwrap();
        let repo_c = std::fs::canonicalize(repo).unwrap();
        assert_eq!(resolved_c, repo_c);
    }

    #[test]
    fn source_to_repo_relative_lifts_chunk_paths() {
        let repo = Path::new("/repo");
        let source = Path::new("/repo/sub");
        let chunk = Path::new("mod/foo.rs");
        let lifted = source_to_repo_relative(source, repo, chunk);
        assert_eq!(lifted, PathBuf::from("sub/mod/foo.rs"));

        // Source == repo → identity.
        let lifted2 = source_to_repo_relative(repo, repo, chunk);
        assert_eq!(lifted2, PathBuf::from("mod/foo.rs"));
    }

    #[test]
    fn discover_repo_root_errors_outside_a_repo() {
        let tmp = tempfile::tempdir().unwrap();
        // No git init.
        let res = discover_repo_root(tmp.path());
        assert!(matches!(res, Err(GitArchaeologyError::NotGitRepo(_))));
    }

    #[test]
    fn days_between_saturates_on_clock_skew() {
        assert_eq!(days_between(100, 50), 0);
        assert_eq!(days_between(100, 100), 0);
        assert_eq!(days_between(0, 86_400 * 3), 3);
    }

    #[test]
    fn format_iso_date_renders_utc_year_month_day() {
        // 2023-11-14 22:13:20 UTC
        assert_eq!(format_iso_date(1_700_000_000), "2023-11-14");
    }
}
