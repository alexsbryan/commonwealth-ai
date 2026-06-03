//! Archaeology eval — measure, iterate, improve.
//!
//! This module evaluates the output of [`crate::git_archaeology`]
//! against two surrogate signals (the closest thing we get to ground
//! truth without a curated dataset):
//!
//! 1. **Witness checks.** Every [`AtomProvenance`] makes claims about
//!    git: "this commit hash exists, touches this file, was authored
//!    by X." Each claim can be re-verified mechanically. Fail rate
//!    catches LLM fabrication and bit-rot in one signal. No LLM, no
//!    judgment — pure git operations.
//!
//! 2. **Baseline diff.** A previous archaeology run saved as
//!    `~/.sovereign/eval/baselines/<atlas>.eval.json` is the
//!    reference. Re-running against the same atlas yields a per-atom
//!    diff: `Added` / `Removed` / `ScoreDrifted`. Surprise drift
//!    flags prompt regressions even when no atom was strictly wrong.
//!
//! Plus a third — [`Inquiry`] — which is a curated regression case:
//! "the `(corpus_id, chunk_id)` invariant should resolve to atoms
//! anchored on `installed_indexes` and reference these keywords in
//! their commit history." Inquiries are how you teach the eval what
//! "good" looks like for cases you understand.
//!
//! ## Design pivot vs. v2
//!
//! v1 atoms (today's `AtomProvenance`) only carry per-file git data.
//! True Lineage atoms (v2) will carry richer citations: a precipitating
//! event, multiple anchor commits, narrative reasoning. The five
//! witness checks generalise — `WitnessKind` is open-ended — but in
//! v0 (this module) we ship four always-on checks that fit v1 atoms,
//! plus inquiry-driven keyword/author/date checks. When v2 lands, the
//! `Inquiry` schema picks up new fields and `WitnessKind` picks up
//! new variants. The reporting surface stays the same.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::git_archaeology::{AtomProvenance, GitArchaeologyError};

// ── Inquiry: a curated expectation ────────────────────────────

/// One expected lineage / invariant the eval should verify is captured
/// by archaeology output. Authored by humans; lives in
/// `inquiries/*.toml`. Becomes a permanent regression case once
/// merged.
///
/// Selectors are `Option<…>`: a missing field doesn't constrain the
/// match. The minimum useful inquiry has just an `id`, `title`, and
/// `file_globs` — that already gives you "atoms anchored to these
/// files exist and pass the always-on checks."
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Inquiry {
    pub id: String,
    pub title: String,
    /// Glob patterns matched against `AtomProvenance.file_path`
    /// (relative). At least one match required for the inquiry to
    /// have any subjects. `**/foo.rs` style.
    #[serde(default)]
    pub file_globs: Vec<String>,
    /// `[start, end]` ISO dates (inclusive). When set, the atom's
    /// `last_modified.date_iso` must fall within.
    #[serde(default)]
    pub date_range: Option<DateRange>,
    /// Any of these (case-insensitive substring) must appear in at
    /// least one commit subject in the atom's file's history. We don't
    /// check every commit's body — too noisy. Subject-line presence is
    /// the conservative signal.
    #[serde(default)]
    pub keywords: Vec<String>,
    /// `primary_authors` must overlap with this set if non-empty.
    #[serde(default)]
    pub authors: Vec<String>,
    /// Per-atom witness score below which this inquiry FAILS. Default
    /// 0.5 — half the witnesses must pass.
    #[serde(default = "default_min_score")]
    pub min_score: f32,
}

fn default_min_score() -> f32 {
    0.5
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DateRange {
    pub start: String, // YYYY-MM-DD
    pub end: String,
}

/// Parse a single inquiry from a TOML body. Wraps an outer
/// `[inquiry]` table to keep the file shape obvious to humans.
pub fn parse_inquiry_toml(body: &str) -> Result<Inquiry, String> {
    #[derive(Deserialize)]
    struct Wrap {
        inquiry: Inquiry,
    }
    let parsed: Wrap = toml::from_str(body).map_err(|e| format!("parse inquiry: {e}"))?;
    Ok(parsed.inquiry)
}

// ── Witness primitives ────────────────────────────────────────

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// The check passed. Claim is corroborated by git.
    Pass,
    /// The check failed. Claim contradicts git (fabrication, bit-rot,
    /// or rename that v1 doesn't follow).
    Fail,
    /// The check couldn't run (commit hash not present in shallow
    /// clone, file not under git, etc.). Counted separately so it
    /// doesn't silently mask Fails.
    Stale,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum WitnessKind {
    /// `git cat-file -e <hash>` succeeds for first_seen.
    FirstSeenCommitExists,
    /// Same for last_modified.
    LastModifiedCommitExists,
    /// `git show --name-only first_seen.hash` lists `file_path`.
    FirstSeenTouchesFile,
    /// `file_path` exists in working tree at HEAD.
    FileExistsAtHead,
    /// Inquiry: at least one commit on this file's history has a
    /// subject containing one of the inquiry's keywords.
    KeywordPresent,
    /// Inquiry: `primary_authors` overlaps with `inquiry.authors`.
    AuthorPresent,
    /// Inquiry: `last_modified.date_iso` falls within
    /// `inquiry.date_range`.
    DateInRange,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WitnessCheck {
    pub atom_id: String,
    pub kind: WitnessKind,
    pub verdict: Verdict,
    /// Free-text detail for failures and stale results.
    /// Empty on Pass.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

/// Per-atom witness summary: the one number you watch trend over
/// iterations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtomWitness {
    pub atom_id: String,
    pub file_path: PathBuf,
    pub passed: u32,
    pub failed: u32,
    pub stale: u32,
    /// `passed / (passed + failed)` — Stale is excluded so a partial-
    /// clone repo doesn't artificially deflate the score.
    pub score: f32,
    /// Inquiries that mentioned this atom (by glob match). Empty when
    /// the atom isn't part of any curated case.
    #[serde(default)]
    pub matched_inquiries: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InquiryVerdict {
    pub inquiry_id: String,
    pub title: String,
    /// Atoms that matched the inquiry's `file_globs`.
    pub matched_atoms: Vec<String>,
    /// All matched atoms cleared `inquiry.min_score`.
    pub passing: bool,
    /// `passed / total_checks` aggregated across matched atoms.
    pub aggregate_score: f32,
    /// Reasons the inquiry failed (when `passing == false`).
    #[serde(default)]
    pub notes: Vec<String>,
}

// ── Eval report (the per-run artifact) ────────────────────────

/// What gets saved as `~/.sovereign/eval/baselines/<atlas>.eval.json`
/// and what the markdown renderer consumes. Everything in here is
/// derived from the archaeology sidecar + git + inquiries — no LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalReport {
    pub atlas_corpus_id: String,
    pub repo_root: PathBuf,
    /// Unix seconds.
    pub generated_at: i64,
    pub atom_count: usize,
    /// `total_passed / (total_passed + total_failed)` across every
    /// atom's witness checks.
    pub witness_rate: f32,
    /// Number of atoms whose `FirstSeenCommitExists` failed — i.e.
    /// the cited commit hash is not in the repo at all. This is the
    /// signal that detects pure fabrication.
    pub fabricated_atoms: u32,
    pub atom_witnesses: Vec<AtomWitness>,
    pub witness_checks: Vec<WitnessCheck>,
    pub inquiry_verdicts: Vec<InquiryVerdict>,
}

/// Always-on witness checks plus inquiry-driven ones.
///
/// Cost: ~1 git subprocess per unique commit hash (cached) + 1 per
/// unique file path. For a 1,872-atom self-atlas the dominant cost
/// is `cat-file -e` calls; with the cache it stays under a second.
pub fn run_witness_checks(
    repo_root: &Path,
    provenance: &[AtomProvenance],
    inquiries: &[Inquiry],
) -> Result<Vec<WitnessCheck>, GitArchaeologyError> {
    let mut commit_exists: HashMap<String, bool> = HashMap::new();
    let mut commit_files: HashMap<String, HashSet<PathBuf>> = HashMap::new();
    let mut head_paths: HashSet<PathBuf> = HashSet::new();
    let mut head_paths_loaded = false;

    let mut out = Vec::with_capacity(provenance.len() * 4);

    for atom in provenance {
        // 1. FirstSeenCommitExists
        let first_hash = atom.first_seen.hash.clone();
        let first_ok = *commit_exists
            .entry(first_hash.clone())
            .or_insert_with(|| git_commit_exists(repo_root, &first_hash));
        out.push(WitnessCheck {
            atom_id: atom.atom_id.clone(),
            kind: WitnessKind::FirstSeenCommitExists,
            verdict: if first_ok {
                Verdict::Pass
            } else {
                Verdict::Fail
            },
            detail: if first_ok {
                String::new()
            } else {
                format!("commit {first_hash} not in repo (fabricated or rebased)")
            },
        });

        // 2. LastModifiedCommitExists
        let last_hash = atom.last_modified.hash.clone();
        let last_ok = *commit_exists
            .entry(last_hash.clone())
            .or_insert_with(|| git_commit_exists(repo_root, &last_hash));
        out.push(WitnessCheck {
            atom_id: atom.atom_id.clone(),
            kind: WitnessKind::LastModifiedCommitExists,
            verdict: if last_ok {
                Verdict::Pass
            } else {
                Verdict::Fail
            },
            detail: if last_ok {
                String::new()
            } else {
                format!("commit {last_hash} not in repo (fabricated or rebased)")
            },
        });

        // 3. FirstSeenTouchesFile
        let touches_kind = WitnessKind::FirstSeenTouchesFile;
        if !first_ok {
            // Can't ask git about file membership when the commit
            // itself is absent — mark Stale rather than Fail.
            out.push(WitnessCheck {
                atom_id: atom.atom_id.clone(),
                kind: touches_kind,
                verdict: Verdict::Stale,
                detail: "first_seen commit not present".into(),
            });
        } else {
            let files = commit_files
                .entry(first_hash.clone())
                .or_insert_with(|| git_commit_files(repo_root, &first_hash));
            let touched = files.contains(&atom.file_path);
            out.push(WitnessCheck {
                atom_id: atom.atom_id.clone(),
                kind: touches_kind,
                verdict: if touched {
                    Verdict::Pass
                } else {
                    Verdict::Fail
                },
                detail: if touched {
                    String::new()
                } else {
                    format!(
                        "commit {first_hash} did not touch {}",
                        atom.file_path.display()
                    )
                },
            });
        }

        // 4. FileExistsAtHead
        if !head_paths_loaded {
            head_paths = git_head_paths(repo_root);
            head_paths_loaded = true;
        }
        let exists = head_paths.contains(&atom.file_path);
        out.push(WitnessCheck {
            atom_id: atom.atom_id.clone(),
            kind: WitnessKind::FileExistsAtHead,
            verdict: if exists { Verdict::Pass } else { Verdict::Fail },
            detail: if exists {
                String::new()
            } else {
                format!(
                    "{} not present at HEAD (renamed or deleted)",
                    atom.file_path.display()
                )
            },
        });
    }

    // ── Inquiry-driven checks ────────────────────────────────
    if !inquiries.is_empty() {
        // Cache: file_path → all subject lines across the file's
        // history. Loaded lazily and only for files actually
        // referenced by inquiries.
        let mut subjects_for: HashMap<PathBuf, Vec<String>> = HashMap::new();

        for atom in provenance {
            for inquiry in inquiries {
                if !inquiry_matches_atom(inquiry, atom) {
                    continue;
                }

                if !inquiry.keywords.is_empty() {
                    let subjects = subjects_for
                        .entry(atom.file_path.clone())
                        .or_insert_with(|| git_file_subjects(repo_root, &atom.file_path));
                    let mut hit = false;
                    for k in &inquiry.keywords {
                        let needle = k.to_lowercase();
                        if subjects.iter().any(|s| s.to_lowercase().contains(&needle)) {
                            hit = true;
                            break;
                        }
                    }
                    out.push(WitnessCheck {
                        atom_id: atom.atom_id.clone(),
                        kind: WitnessKind::KeywordPresent,
                        verdict: if hit { Verdict::Pass } else { Verdict::Fail },
                        detail: if hit {
                            String::new()
                        } else {
                            format!(
                                "no commit subject in {}'s history mentions any of: {:?}",
                                atom.file_path.display(),
                                inquiry.keywords
                            )
                        },
                    });
                }

                if !inquiry.authors.is_empty() {
                    let expected: HashSet<&str> =
                        inquiry.authors.iter().map(String::as_str).collect();
                    let observed: HashSet<&str> =
                        atom.primary_authors.iter().map(String::as_str).collect();
                    let overlap = !expected.is_disjoint(&observed);
                    out.push(WitnessCheck {
                        atom_id: atom.atom_id.clone(),
                        kind: WitnessKind::AuthorPresent,
                        verdict: if overlap {
                            Verdict::Pass
                        } else {
                            Verdict::Fail
                        },
                        detail: if overlap {
                            String::new()
                        } else {
                            format!(
                                "primary_authors {:?} doesn't overlap inquiry.authors {:?}",
                                atom.primary_authors, inquiry.authors
                            )
                        },
                    });
                }

                if let Some(range) = &inquiry.date_range {
                    let in_range = atom.last_modified.date_iso.as_str() >= range.start.as_str()
                        && atom.last_modified.date_iso.as_str() <= range.end.as_str();
                    out.push(WitnessCheck {
                        atom_id: atom.atom_id.clone(),
                        kind: WitnessKind::DateInRange,
                        verdict: if in_range {
                            Verdict::Pass
                        } else {
                            Verdict::Fail
                        },
                        detail: if in_range {
                            String::new()
                        } else {
                            format!(
                                "last_modified.date {} outside [{}, {}]",
                                atom.last_modified.date_iso, range.start, range.end
                            )
                        },
                    });
                }
            }
        }
    }

    Ok(out)
}

/// Aggregate per-atom witnesses into a summary list, attaching the
/// inquiry IDs that picked the atom up.
pub fn summarise_witnesses(
    provenance: &[AtomProvenance],
    checks: &[WitnessCheck],
    inquiries: &[Inquiry],
) -> Vec<AtomWitness> {
    let mut by_atom: BTreeMap<String, AtomWitness> = BTreeMap::new();
    for atom in provenance {
        by_atom.insert(
            atom.atom_id.clone(),
            AtomWitness {
                atom_id: atom.atom_id.clone(),
                file_path: atom.file_path.clone(),
                passed: 0,
                failed: 0,
                stale: 0,
                score: 0.0,
                matched_inquiries: inquiries
                    .iter()
                    .filter(|i| inquiry_matches_atom(i, atom))
                    .map(|i| i.id.clone())
                    .collect(),
            },
        );
    }
    for c in checks {
        if let Some(w) = by_atom.get_mut(&c.atom_id) {
            match c.verdict {
                Verdict::Pass => w.passed += 1,
                Verdict::Fail => w.failed += 1,
                Verdict::Stale => w.stale += 1,
            }
        }
    }
    for w in by_atom.values_mut() {
        let denom = w.passed + w.failed;
        w.score = if denom == 0 {
            0.0
        } else {
            w.passed as f32 / denom as f32
        };
    }
    by_atom.into_values().collect()
}

/// Compute a verdict per inquiry from per-atom witnesses.
pub fn evaluate_inquiries(
    provenance: &[AtomProvenance],
    witnesses: &[AtomWitness],
    inquiries: &[Inquiry],
) -> Vec<InquiryVerdict> {
    let by_atom: HashMap<&str, &AtomWitness> =
        witnesses.iter().map(|w| (w.atom_id.as_str(), w)).collect();
    let mut out = Vec::new();
    for inquiry in inquiries {
        let matched: Vec<&AtomProvenance> = provenance
            .iter()
            .filter(|a| inquiry_matches_atom(inquiry, a))
            .collect();
        let mut notes = Vec::new();
        if matched.is_empty() {
            out.push(InquiryVerdict {
                inquiry_id: inquiry.id.clone(),
                title: inquiry.title.clone(),
                matched_atoms: Vec::new(),
                passing: false,
                aggregate_score: 0.0,
                notes: vec![format!(
                    "no atoms matched file_globs {:?}",
                    inquiry.file_globs
                )],
            });
            continue;
        }
        let mut passing = true;
        let mut total_passed: u32 = 0;
        let mut total_checks: u32 = 0;
        for atom in &matched {
            let Some(w) = by_atom.get(atom.atom_id.as_str()) else {
                continue;
            };
            total_passed += w.passed;
            total_checks += w.passed + w.failed;
            if w.score < inquiry.min_score {
                passing = false;
                notes.push(format!(
                    "atom {} score {:.2} < min_score {:.2}",
                    w.atom_id, w.score, inquiry.min_score
                ));
            }
        }
        let aggregate = if total_checks == 0 {
            0.0
        } else {
            total_passed as f32 / total_checks as f32
        };
        out.push(InquiryVerdict {
            inquiry_id: inquiry.id.clone(),
            title: inquiry.title.clone(),
            matched_atoms: matched.iter().map(|a| a.atom_id.clone()).collect(),
            passing,
            aggregate_score: aggregate,
            notes,
        });
    }
    out
}

// ── Baseline diff ──────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BaselineDiff {
    /// atom_ids present in current but not baseline.
    pub added: Vec<String>,
    /// atom_ids present in baseline but not current.
    pub removed: Vec<String>,
    /// atom_id, prev_score, curr_score for atoms whose witness score
    /// changed by more than `score_epsilon` (default 0.01). Sorted by
    /// `curr_score - prev_score` descending (improvements first).
    pub score_changes: Vec<ScoreChange>,
    /// atom_id, prev_path, curr_path when an atom kept its id but
    /// changed file_path (rename / re-anchor).
    pub path_changes: Vec<PathChange>,
    /// `curr_witness_rate − prev_witness_rate` at the report level.
    pub witness_rate_delta: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreChange {
    pub atom_id: String,
    pub prev_score: f32,
    pub curr_score: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathChange {
    pub atom_id: String,
    pub prev_path: PathBuf,
    pub curr_path: PathBuf,
}

pub fn diff_against_baseline(curr: &EvalReport, prev: &EvalReport) -> BaselineDiff {
    let curr_ids: BTreeSet<&str> = curr
        .atom_witnesses
        .iter()
        .map(|w| w.atom_id.as_str())
        .collect();
    let prev_ids: BTreeSet<&str> = prev
        .atom_witnesses
        .iter()
        .map(|w| w.atom_id.as_str())
        .collect();
    let added: Vec<String> = curr_ids
        .difference(&prev_ids)
        .map(|s| s.to_string())
        .collect();
    let removed: Vec<String> = prev_ids
        .difference(&curr_ids)
        .map(|s| s.to_string())
        .collect();

    let prev_by_id: HashMap<&str, &AtomWitness> = prev
        .atom_witnesses
        .iter()
        .map(|w| (w.atom_id.as_str(), w))
        .collect();
    const EPS: f32 = 0.01;
    let mut score_changes: Vec<ScoreChange> = Vec::new();
    let mut path_changes: Vec<PathChange> = Vec::new();
    for w in &curr.atom_witnesses {
        let Some(p) = prev_by_id.get(w.atom_id.as_str()) else {
            continue;
        };
        if (w.score - p.score).abs() > EPS {
            score_changes.push(ScoreChange {
                atom_id: w.atom_id.clone(),
                prev_score: p.score,
                curr_score: w.score,
            });
        }
        if w.file_path != p.file_path {
            path_changes.push(PathChange {
                atom_id: w.atom_id.clone(),
                prev_path: p.file_path.clone(),
                curr_path: w.file_path.clone(),
            });
        }
    }
    score_changes.sort_by(|a, b| {
        (b.curr_score - b.prev_score)
            .partial_cmp(&(a.curr_score - a.prev_score))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    BaselineDiff {
        added,
        removed,
        score_changes,
        path_changes,
        witness_rate_delta: curr.witness_rate - prev.witness_rate,
    }
}

// ── Selectors ─────────────────────────────────────────────────

/// Does the inquiry's selectors target this atom?
fn inquiry_matches_atom(inquiry: &Inquiry, atom: &AtomProvenance) -> bool {
    if inquiry.file_globs.is_empty() {
        return false;
    }
    inquiry
        .file_globs
        .iter()
        .any(|pat| glob_match(pat, &atom.file_path))
}

/// Trivial glob — supports `**`, `*`, and literal segments. Powerful
/// enough for "this file" / "everything under this dir" matchers
/// without pulling in a glob crate.
///
/// Public so consumers (e.g. the brief assembler) can match files
/// against inquiry globs directly without reaching into eval-internal
/// state.
pub fn glob_match(pattern: &str, path: &Path) -> bool {
    let path_str = path.to_string_lossy();
    glob_match_str(pattern, &path_str)
}

/// Load every `*.toml` inquiry under `dir`. Files that fail to parse
/// are skipped with a warn-level log so a single broken inquiry
/// doesn't abort the whole load. Sorted by `id` for determinism.
pub fn load_inquiries_from_dir(dir: &Path) -> std::io::Result<Vec<Inquiry>> {
    let mut out = Vec::new();
    if !dir.exists() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let body = match std::fs::read_to_string(&path) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(file = %path.display(), error = %e, "load_inquiries_from_dir: read failed");
                continue;
            }
        };
        match parse_inquiry_toml(&body) {
            Ok(inq) => out.push(inq),
            Err(e) => {
                tracing::warn!(file = %path.display(), error = %e, "load_inquiries_from_dir: parse failed");
            }
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// Return the subset of `inquiries` whose globs match at least one
/// file in `files`. Used by the brief assembler to decide which
/// principles to surface for a given working set.
pub fn inquiries_matching_files<'a>(
    inquiries: &'a [Inquiry],
    files: &[PathBuf],
) -> Vec<&'a Inquiry> {
    inquiries
        .iter()
        .filter(|inq| {
            inq.file_globs
                .iter()
                .any(|pat| files.iter().any(|f| glob_match(pat, f)))
        })
        .collect()
}

fn glob_match_str(pattern: &str, target: &str) -> bool {
    // Convert the glob to a regex-like state machine. We support:
    //  - `**`  matches any number of path segments (incl. zero)
    //  - `*`   matches any number of non-`/` characters
    //  - other characters match literally
    fn matches(pat: &[u8], s: &[u8]) -> bool {
        let mut pi = 0;
        let mut si = 0;
        let mut star: Option<(usize, usize)> = None; // (pat_idx_after_*, str_idx_at_match_start)
        let mut globstar: Option<(usize, usize)> = None;
        while si < s.len() {
            if pi < pat.len() {
                if pi + 1 < pat.len() && pat[pi] == b'*' && pat[pi + 1] == b'*' {
                    // `**` — record fallback and skip the marker.
                    globstar = Some((pi + 2, si));
                    pi += 2;
                    // `**/` — also consume the slash since `**` can match zero segments.
                    if pi < pat.len() && pat[pi] == b'/' {
                        pi += 1;
                    }
                    continue;
                }
                if pat[pi] == b'*' {
                    star = Some((pi + 1, si));
                    pi += 1;
                    continue;
                }
                if pat[pi] == s[si] {
                    pi += 1;
                    si += 1;
                    continue;
                }
            }
            // Backtrack to single-`*` if we have one.
            if let Some((p_resume, s_anchor)) = star {
                if s[si] == b'/' {
                    // `*` can't cross a `/` — fall through to globstar.
                    star = None;
                } else {
                    pi = p_resume;
                    si = s_anchor + 1;
                    star = Some((p_resume, si));
                    continue;
                }
            }
            // Backtrack to globstar — `**` can swallow anything.
            if let Some((p_resume, s_anchor)) = globstar {
                pi = p_resume;
                si = s_anchor + 1;
                globstar = Some((p_resume, si));
                continue;
            }
            return false;
        }
        // Drain trailing `*`/`**` in the pattern.
        while pi < pat.len() {
            if pat[pi] == b'*' {
                pi += 1;
                continue;
            }
            return false;
        }
        true
    }
    matches(pattern.as_bytes(), target.as_bytes())
}

// ── Git helpers (subprocess, matches workspace idiom) ──────────

fn git_commit_exists(repo_root: &Path, hash: &str) -> bool {
    let out = Command::new("git")
        .args(["cat-file", "-e", hash])
        .current_dir(repo_root)
        .output();
    matches!(out, Ok(o) if o.status.success())
}

fn git_commit_files(repo_root: &Path, hash: &str) -> HashSet<PathBuf> {
    let out = Command::new("git")
        .args(["show", "--name-only", "--format=", hash])
        .current_dir(repo_root)
        .output();
    let Ok(o) = out else { return HashSet::new() };
    if !o.status.success() {
        return HashSet::new();
    }
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| PathBuf::from(l.trim()))
        .collect()
}

fn git_head_paths(repo_root: &Path) -> HashSet<PathBuf> {
    let out = Command::new("git")
        .args(["ls-tree", "-r", "--name-only", "HEAD"])
        .current_dir(repo_root)
        .output();
    let Ok(o) = out else { return HashSet::new() };
    if !o.status.success() {
        return HashSet::new();
    }
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(|l| PathBuf::from(l.trim()))
        .filter(|p| !p.as_os_str().is_empty())
        .collect()
}

/// Subjects of every commit that touched `path`. Used by
/// `KeywordPresent`. Conservative: subjects only — bodies are noisy
/// and dominate cost in a 1000-file repo.
fn git_file_subjects(repo_root: &Path, path: &Path) -> Vec<String> {
    let out = Command::new("git")
        .args(["log", "--format=%s", "--", &path.to_string_lossy()])
        .current_dir(repo_root)
        .output();
    let Ok(o) = out else { return Vec::new() };
    if !o.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(|l| l.to_string())
        .collect()
}

// ── Aggregator ────────────────────────────────────────────────

/// Run the full eval. Pulls together witnesses, summaries, inquiry
/// verdicts; computes top-level rates; stitches into [`EvalReport`].
pub fn run_eval(
    atlas_corpus_id: &str,
    repo_root: &Path,
    provenance: &[AtomProvenance],
    inquiries: &[Inquiry],
) -> Result<EvalReport, GitArchaeologyError> {
    let checks = run_witness_checks(repo_root, provenance, inquiries)?;
    let witnesses = summarise_witnesses(provenance, &checks, inquiries);
    let verdicts = evaluate_inquiries(provenance, &witnesses, inquiries);

    let mut total_passed = 0u32;
    let mut total_failed = 0u32;
    let mut fabricated = 0u32;
    for c in &checks {
        match c.verdict {
            Verdict::Pass => total_passed += 1,
            Verdict::Fail => {
                total_failed += 1;
                if matches!(c.kind, WitnessKind::FirstSeenCommitExists) {
                    fabricated += 1;
                }
            }
            Verdict::Stale => {}
        }
    }
    let denom = total_passed + total_failed;
    let witness_rate = if denom == 0 {
        0.0
    } else {
        total_passed as f32 / denom as f32
    };

    let generated_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    Ok(EvalReport {
        atlas_corpus_id: atlas_corpus_id.to_string(),
        repo_root: repo_root.to_path_buf(),
        generated_at,
        atom_count: provenance.len(),
        witness_rate,
        fabricated_atoms: fabricated,
        atom_witnesses: witnesses,
        witness_checks: checks,
        inquiry_verdicts: verdicts,
    })
}

// ── Tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_archaeology::{
        batch_harvest_all_commits, enrich_atom, AtomProvenance, CommitRef,
    };
    use std::collections::HashMap;
    use std::process::Command as Cmd;

    fn init_repo(dir: &Path) {
        assert!(Cmd::new("git")
            .args(["init", "--initial-branch=main"])
            .current_dir(dir)
            .status()
            .unwrap()
            .success());
        for (k, v) in [("user.email", "alice@example.com"), ("user.name", "Alice")] {
            assert!(Cmd::new("git")
                .args(["config", k, v])
                .current_dir(dir)
                .status()
                .unwrap()
                .success());
        }
    }

    fn commit_at(dir: &Path, msg: &str, ts: i64) {
        let date_str = format!("{ts} +0000");
        assert!(Cmd::new("git")
            .args(["commit", "-m", msg, "--allow-empty"])
            .current_dir(dir)
            .env("GIT_AUTHOR_DATE", &date_str)
            .env("GIT_COMMITTER_DATE", &date_str)
            .status()
            .unwrap()
            .success());
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

    /// Build a tiny repo + a single AtomProvenance pointing at a
    /// real commit. Returns (repo_root, provenance vec).
    fn make_fixture() -> (tempfile::TempDir, Vec<AtomProvenance>) {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        write_and_add(repo, "src/lib.rs", "fn v1() {}\n");
        commit_at(
            repo,
            "fix: dedup installed_indexes by (corpus_id, chunk_id)",
            1_700_000_000,
        );
        let history = batch_harvest_all_commits(repo).unwrap();
        let prov = enrich_atom(
            "entity-0001",
            Path::new("src/lib.rs"),
            &history,
            1_700_000_000 + 1,
        )
        .unwrap();
        (tmp, vec![prov])
    }

    #[test]
    fn glob_match_handles_globstar_and_star() {
        assert!(glob_match_str("**/lib.rs", "src/lib.rs"));
        assert!(glob_match_str("**/lib.rs", "lib.rs"));
        assert!(glob_match_str("**/lib.rs", "a/b/c/lib.rs"));
        assert!(!glob_match_str("**/lib.rs", "src/main.rs"));
        assert!(glob_match_str("src/*.rs", "src/lib.rs"));
        assert!(!glob_match_str("src/*.rs", "src/sub/lib.rs"));
        assert!(glob_match_str(
            "**/installed.rs",
            "crates/sovereign-tools/src/local_corpus/installed.rs"
        ));
    }

    #[test]
    fn inquiry_parses_minimum_shape() {
        let body = r#"
            [inquiry]
            id = "test"
            title = "test inquiry"
            file_globs = ["**/lib.rs"]
        "#;
        let inq = parse_inquiry_toml(body).unwrap();
        assert_eq!(inq.id, "test");
        assert_eq!(inq.file_globs, vec!["**/lib.rs"]);
        assert!(inq.keywords.is_empty());
        assert_eq!(inq.min_score, 0.5);
    }

    #[test]
    fn inquiry_parses_full_shape() {
        let body = r#"
            [inquiry]
            id = "corpus_id_chunk_id"
            title = "(corpus_id, chunk_id) invariant"
            file_globs = ["**/installed.rs"]
            keywords = ["dedup", "Garth"]
            authors = ["alice@example.com"]
            min_score = 0.8

            [inquiry.date_range]
            start = "2026-05-01"
            end = "2026-05-10"
        "#;
        let inq = parse_inquiry_toml(body).unwrap();
        assert_eq!(inq.keywords, vec!["dedup", "Garth"]);
        assert_eq!(inq.min_score, 0.8);
        let dr = inq.date_range.unwrap();
        assert_eq!(dr.start, "2026-05-01");
        assert_eq!(dr.end, "2026-05-10");
    }

    #[test]
    fn always_on_witnesses_pass_for_real_atom() {
        let (tmp, prov) = make_fixture();
        let report = run_eval("test-atlas", tmp.path(), &prov, &[]).unwrap();
        assert_eq!(report.atom_count, 1);
        assert_eq!(report.fabricated_atoms, 0);
        assert!(
            report.witness_rate > 0.99,
            "all 4 always-on checks should pass"
        );
        let w = &report.atom_witnesses[0];
        assert_eq!(w.passed, 4);
        assert_eq!(w.failed, 0);
    }

    #[test]
    fn fabricated_commit_hash_fails_first_seen_check() {
        let (tmp, prov) = make_fixture();
        // Mutate first_seen.hash to a non-existent commit.
        let mut bad = prov.clone();
        bad[0].first_seen.hash = "0000000000000000000000000000000000000000".into();
        let report = run_eval("test-atlas", tmp.path(), &bad, &[]).unwrap();
        assert_eq!(report.fabricated_atoms, 1, "fabrication detector must fire");
        // FirstSeenTouchesFile should have flipped to Stale (commit
        // absent), not Fail — keeps the staleness signal honest.
        let stale_count = report
            .witness_checks
            .iter()
            .filter(|c| {
                matches!(c.kind, WitnessKind::FirstSeenTouchesFile)
                    && matches!(c.verdict, Verdict::Stale)
            })
            .count();
        assert_eq!(stale_count, 1);
    }

    #[test]
    fn keyword_inquiry_passes_when_subject_matches() {
        let (tmp, prov) = make_fixture();
        let inquiry = Inquiry {
            id: "test".into(),
            title: "matching inquiry".into(),
            file_globs: vec!["**/lib.rs".into()],
            date_range: None,
            keywords: vec!["dedup".into()],
            authors: vec![],
            min_score: 0.5,
        };
        let report = run_eval("test-atlas", tmp.path(), &prov, &[inquiry]).unwrap();
        let kw_check = report
            .witness_checks
            .iter()
            .find(|c| matches!(c.kind, WitnessKind::KeywordPresent))
            .expect("keyword check ran");
        assert_eq!(kw_check.verdict, Verdict::Pass);
        assert_eq!(report.inquiry_verdicts.len(), 1);
        assert!(report.inquiry_verdicts[0].passing);
    }

    #[test]
    fn keyword_inquiry_fails_when_subject_misses() {
        let (tmp, prov) = make_fixture();
        let inquiry = Inquiry {
            id: "test".into(),
            title: "missing-keyword inquiry".into(),
            file_globs: vec!["**/lib.rs".into()],
            date_range: None,
            keywords: vec!["watermelon".into()],
            authors: vec![],
            min_score: 0.5,
        };
        let report = run_eval("test-atlas", tmp.path(), &prov, &[inquiry]).unwrap();
        let kw_check = report
            .witness_checks
            .iter()
            .find(|c| matches!(c.kind, WitnessKind::KeywordPresent))
            .unwrap();
        assert_eq!(kw_check.verdict, Verdict::Fail);
    }

    #[test]
    fn baseline_diff_categorises_changes() {
        // Simulate two consecutive runs with one atom added, one
        // removed, one with a score change.
        let stable = AtomWitness {
            atom_id: "A".into(),
            file_path: PathBuf::from("a.rs"),
            passed: 4,
            failed: 0,
            stale: 0,
            score: 1.0,
            matched_inquiries: vec![],
        };
        let regressed_prev = AtomWitness {
            atom_id: "B".into(),
            file_path: PathBuf::from("b.rs"),
            passed: 4,
            failed: 0,
            stale: 0,
            score: 1.0,
            matched_inquiries: vec![],
        };
        let regressed_curr = AtomWitness {
            score: 0.5,
            ..regressed_prev.clone()
        };
        let removed = AtomWitness {
            atom_id: "C".into(),
            file_path: PathBuf::from("c.rs"),
            passed: 4,
            failed: 0,
            stale: 0,
            score: 1.0,
            matched_inquiries: vec![],
        };
        let added = AtomWitness {
            atom_id: "D".into(),
            file_path: PathBuf::from("d.rs"),
            passed: 4,
            failed: 0,
            stale: 0,
            score: 1.0,
            matched_inquiries: vec![],
        };
        let prev = EvalReport {
            atlas_corpus_id: "x".into(),
            repo_root: PathBuf::from("/"),
            generated_at: 0,
            atom_count: 3,
            witness_rate: 1.0,
            fabricated_atoms: 0,
            atom_witnesses: vec![stable.clone(), regressed_prev, removed],
            witness_checks: vec![],
            inquiry_verdicts: vec![],
        };
        let curr = EvalReport {
            atlas_corpus_id: "x".into(),
            repo_root: PathBuf::from("/"),
            generated_at: 1,
            atom_count: 3,
            witness_rate: 0.83,
            fabricated_atoms: 0,
            atom_witnesses: vec![stable, regressed_curr, added],
            witness_checks: vec![],
            inquiry_verdicts: vec![],
        };
        let diff = diff_against_baseline(&curr, &prev);
        assert_eq!(diff.added, vec!["D".to_string()]);
        assert_eq!(diff.removed, vec!["C".to_string()]);
        assert_eq!(diff.score_changes.len(), 1);
        assert_eq!(diff.score_changes[0].atom_id, "B");
        assert!((diff.witness_rate_delta - (-0.17)).abs() < 0.02);
    }

    #[test]
    fn date_range_inquiry_compares_iso_strings() {
        let (tmp, mut prov) = make_fixture();
        // Forge the last_modified date so it's outside the range.
        prov[0].last_modified = CommitRef {
            hash: prov[0].last_modified.hash.clone(),
            date_iso: "2025-01-01".into(),
            author_email: prov[0].last_modified.author_email.clone(),
            subject: prov[0].last_modified.subject.clone(),
        };
        let inquiry = Inquiry {
            id: "test".into(),
            title: "date inquiry".into(),
            file_globs: vec!["**/lib.rs".into()],
            date_range: Some(DateRange {
                start: "2026-05-01".into(),
                end: "2026-05-10".into(),
            }),
            keywords: vec![],
            authors: vec![],
            min_score: 0.5,
        };
        let report = run_eval("test-atlas", tmp.path(), &prov, &[inquiry]).unwrap();
        let date_check = report
            .witness_checks
            .iter()
            .find(|c| matches!(c.kind, WitnessKind::DateInRange))
            .unwrap();
        assert_eq!(date_check.verdict, Verdict::Fail);
    }

    /// Defeat the unused-warning on HashMap import in non-test builds.
    #[allow(dead_code)]
    fn _hashmap_witness() -> HashMap<String, String> {
        HashMap::new()
    }
}
