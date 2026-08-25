// SPDX-License-Identifier: AGPL-3.0-or-later
//! Is some OTHER directory holding this machine's sovereign data?
//!
//! # The failure this refuses
//!
//! [`crate::rebrand::data_dir`] is the one accessor for "where does per-user
//! state live", and it always returns a plausible path — that is exactly what
//! makes a wrong answer invisible. Four directories have, at different times,
//! been a data root on a real machine:
//!
//! * `~/.svrnmesh` — the branded root, today's answer.
//! * `~/.sovereign` — the legacy root, still honoured when the branded one is
//!   absent or empty (`rebrand::resolve_branded_dir`).
//! * `<platform data dir>/svrnmesh` and `/sovereign` — what the deleted
//!   `rebrand::mesh_data_dir()` returned. `DesktopConfig::default_data_dir`
//!   used it, so a desktop-first install put mesh identity, corpora and
//!   `sovereign.db` there while the daemon used `~/.svrnmesh` (note
//!   `b2aa9fb8`, measured on the maintainer's own machine: 15G of it).
//!
//! With `mesh_data_dir()` deleted the platform root is no longer *written*.
//! That closes the split-brain going forward and opens a worse-looking one
//! for anybody whose live data is already there: the next boot resolves an
//! empty `~/.svrnmesh`, starts clean, and the user's mesh and corpora appear
//! to have vanished. Starting fresh on top of that is a silent substitution
//! (ARCH_PRINCIPLES §18.3) and the one shape here that must refuse.
//!
//! # Why classification, not policy
//!
//! [`RootConflict`] says what is on disk; the host decides what to do about
//! it. One decider (§10.6) — a daemon and a desktop that each re-derived
//! "is this the right root" would disagree, which is how the split arose in
//! the first place. Both hosts today refuse to bring a daemon up on
//! [`RootConflict::Stranded`] and log the rest.
//!
//! This module does NOT export a getter for any of the roots it names. It
//! enumerates places that must not *silently* hold live data; deriving a path
//! to write still goes through `rebrand`, so there is still exactly one
//! accessor per path.

use std::path::{Path, PathBuf};

/// Files a sovereign process writes into its data root and nothing else does.
/// Presence of any one means "a sovereign actually ran here" — as opposed to
/// a directory that exists because something once touched it.
const LIVE_MARKERS: &[&str] = &["sovereign.db", "mesh.json", "node_id", "notes.db"];

/// A data root that is not the resolved one and holds live data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignRoot {
    /// The directory, as resolved on disk.
    pub path: PathBuf,
    /// Which marker was found — so the operator can see *why* this counted,
    /// rather than being told a directory is "live" and having to guess.
    pub evidence: &'static str,
}

/// What the on-disk data roots say, relative to the one this process resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootConflict {
    /// The resolved root is the only one holding live data. The steady state.
    Clear,
    /// The resolved root holds live data and so does another. Both are real;
    /// which one a given surface reads depends on how it resolved. Loud, not
    /// fatal — this is the shape of a machine that has been migrated and left
    /// residue behind, and refusing here would break a working install.
    Split(Vec<ForeignRoot>),
    /// The resolved root holds NO live data while another root does. Booting
    /// here starts fresh on top of live data. This is the refusal.
    Stranded(Vec<ForeignRoot>),
}

impl RootConflict {
    /// Must a host refuse to bring a daemon up on this?
    ///
    /// True only for [`RootConflict::Stranded`]. One implementation so the
    /// daemon and the desktop cannot draw the line in different places.
    pub fn is_refusal(&self) -> bool {
        matches!(self, Self::Stranded(_))
    }

    /// The foreign roots this verdict is about; empty for [`Self::Clear`].
    pub fn others(&self) -> &[ForeignRoot] {
        match self {
            Self::Clear => &[],
            Self::Split(v) | Self::Stranded(v) => v,
        }
    }
}

impl std::fmt::Display for RootConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Clear => write!(f, "one data root, no conflict"),
            Self::Split(others) => {
                write!(
                    f,
                    "this data root holds live data, and so do {} other(s): {}. \
                     Nothing is lost, but two roots means two answers to \
                     \"where is my mesh?\" — consolidate them and delete the \
                     one you do not keep.",
                    others.len(),
                    render(others)
                )
            }
            Self::Stranded(others) => write!(
                f,
                "this data root is EMPTY while live sovereign data sits in {}. \
                 Starting here would create a second, empty universe and your \
                 mesh and corpora would look lost.\n  \
                 Point this process at the live root — `[data] dir` in \
                 config.toml, `data_dir` in desktop.toml, or SVRNMESH_DATA_DIR \
                 — or move that directory's contents into this one. Nothing is \
                 moved automatically: which root wins is yours to decide.",
                render(others)
            ),
        }
    }
}

fn render(others: &[ForeignRoot]) -> String {
    others
        .iter()
        .map(|o| format!("{} (has {})", o.path.display(), o.evidence))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Classify `resolved` — the data root this process is about to write —
/// against every other directory that has been a data root on some machine.
pub fn classify(resolved: &Path) -> RootConflict {
    classify_among(resolved, &candidate_roots())
}

/// Every directory that has been a sovereign data root, in any release. Not
/// public: a caller wanting a path to WRITE must go through `rebrand`.
fn candidate_roots() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = crate::rebrand::user_home() {
        out.push(home.join(".svrnmesh"));
        out.push(home.join(".sovereign"));
    }
    // What `rebrand::mesh_data_dir()` returned before it was deleted
    // (2026-08-24). Named here as a place to CHECK, never as a place to write
    // — which is why this is a private list and not a public accessor.
    if let Some(data) = dirs::data_dir() {
        out.push(data.join("svrnmesh"));
        out.push(data.join("sovereign"));
    }
    out
}

/// The comparison itself, over an explicit candidate list so the rule is
/// testable without touching `$HOME` or the platform dirs.
fn classify_among(resolved: &Path, candidates: &[PathBuf]) -> RootConflict {
    let resolved_id = identity(resolved);
    let mut others = Vec::new();
    let mut seen = Vec::new();
    for cand in candidates {
        let id = identity(cand);
        // Symlink-aware: `~/.sovereign` is a symlink to `~/.svrnmesh` on a
        // migrated machine, and on macOS the platform config and data dirs
        // are the same directory. Comparing strings would report a machine
        // as split against itself.
        if id == resolved_id || seen.contains(&id) {
            continue;
        }
        seen.push(id);
        if let Some(evidence) = live_marker(cand) {
            others.push(ForeignRoot {
                path: cand.clone(),
                evidence,
            });
        }
    }
    if others.is_empty() {
        RootConflict::Clear
    } else if live_marker(resolved).is_some() {
        RootConflict::Split(others)
    } else {
        RootConflict::Stranded(others)
    }
}

/// Canonical identity of a directory: the resolved path when it exists, the
/// literal path when it does not (a root that is not there yet cannot alias
/// anything).
fn identity(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn live_marker(root: &Path) -> Option<&'static str> {
    LIVE_MARKERS
        .iter()
        .copied()
        .find(|m| root.join(m).exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(dir: &Path, name: &str) {
        std::fs::create_dir_all(dir).expect("mkdir");
        std::fs::write(dir.join(name), b"x").expect("write");
    }

    /// The refusal case, and the reason this module exists: the data root we
    /// are about to write is empty while a former root holds the user's mesh.
    #[test]
    fn an_empty_root_beside_a_live_former_root_is_stranded() {
        let base = tempfile::tempdir().expect("tempdir");
        let fresh = base.path().join("home/.svrnmesh");
        let platform = base.path().join("Library/svrnmesh");
        std::fs::create_dir_all(&fresh).expect("mkdir");
        touch(&platform, "mesh.json");

        let v = classify_among(&fresh, &[platform.clone()]);
        assert!(v.is_refusal(), "{v:?}");
        assert_eq!(v.others().len(), 1);
        assert_eq!(v.others()[0].evidence, "mesh.json");
        assert!(format!("{v}").contains("SVRNMESH_DATA_DIR"), "{v}");
    }

    /// A migrated machine with residue: both hold data, nothing is lost, and
    /// refusing would break a working install. Loud, not fatal.
    #[test]
    fn two_live_roots_are_a_split_not_a_refusal() {
        let base = tempfile::tempdir().expect("tempdir");
        let live = base.path().join("home/.svrnmesh");
        let residue = base.path().join("Library/svrnmesh");
        touch(&live, "sovereign.db");
        touch(&residue, "notes.db");

        let v = classify_among(&live, &[residue]);
        assert!(matches!(v, RootConflict::Split(_)), "{v:?}");
        assert!(!v.is_refusal());
    }

    /// A fresh install: nothing anywhere. Must not refuse, or first boot on a
    /// clean machine fails.
    #[test]
    fn a_clean_machine_is_clear() {
        let base = tempfile::tempdir().expect("tempdir");
        let fresh = base.path().join(".svrnmesh");
        let absent = base.path().join("Library/svrnmesh");
        let v = classify_among(&fresh, &[absent]);
        assert_eq!(v, RootConflict::Clear);
    }

    /// `~/.sovereign` is a symlink to `~/.svrnmesh` on a migrated machine.
    /// Comparing strings would report that machine as split against itself
    /// and, on a not-yet-written branded root, would REFUSE to boot it.
    #[test]
    fn a_symlinked_alias_of_the_resolved_root_is_not_foreign() {
        let base = tempfile::tempdir().expect("tempdir");
        let real = base.path().join(".svrnmesh");
        touch(&real, "sovereign.db");
        let alias = base.path().join(".sovereign");
        std::os::unix::fs::symlink(&real, &alias).expect("symlink");

        assert_eq!(classify_among(&real, &[alias]), RootConflict::Clear);
    }

    /// The candidate list may name one directory twice (on macOS the platform
    /// config and data dirs are the same path). One foreign root, not two.
    #[test]
    fn a_duplicated_candidate_is_reported_once() {
        let base = tempfile::tempdir().expect("tempdir");
        let fresh = base.path().join(".svrnmesh");
        std::fs::create_dir_all(&fresh).expect("mkdir");
        let other = base.path().join("Library/svrnmesh");
        touch(&other, "node_id");

        let v = classify_among(&fresh, &[other.clone(), other]);
        assert_eq!(v.others().len(), 1, "{v:?}");
    }

    /// An empty directory is not live data. A root that exists because
    /// something once ran `mkdir -p` must not strand a boot.
    #[test]
    fn an_empty_foreign_directory_is_not_live() {
        let base = tempfile::tempdir().expect("tempdir");
        let fresh = base.path().join(".svrnmesh");
        let empty = base.path().join("Library/svrnmesh");
        std::fs::create_dir_all(&fresh).expect("mkdir");
        std::fs::create_dir_all(&empty).expect("mkdir");
        assert_eq!(classify_among(&fresh, &[empty]), RootConflict::Clear);
    }

    /// The real candidate list must include every root that has been one, or
    /// the check is blind to exactly the machines it exists for.
    #[test]
    fn the_candidate_list_names_all_four_historical_roots() {
        let roots = candidate_roots();
        let names: Vec<String> = roots
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        for suffix in [".svrnmesh", ".sovereign"] {
            assert!(
                names.iter().any(|n| n.ends_with(suffix)),
                "no candidate ends with {suffix}: {names:?}"
            );
        }
    }
}
