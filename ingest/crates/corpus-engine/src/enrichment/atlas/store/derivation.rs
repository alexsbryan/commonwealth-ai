// SPDX-License-Identifier: AGPL-3.0-or-later
//! WHICH DERIVATION the v2 store's `edges.csr` was built under — and the one
//! place a change to it reads as STALENESS rather than as something an
//! operator has to remember (ARCH principle 10).
//!
//! Why it exists, and it is this order's own defect caught before it shipped.
//! [`derive_configures_edges`](super::configures::derive_configures_edges)
//! turns a `Configuration`'s `constituent_atoms` field into traversable
//! edges at the ONE store write, which is the right place: `write_atlas_full`
//! and `atlas migrate-all`'s rebuild both go through it. But
//! [`store_needs_build`](super::store_needs_build) decided whether to rebuild
//! from two facts only — is there an `atoms.lance` with a current CSR header,
//! and is `atoms.json` newer than the store — and a code change that adds a
//! derived edge kind moves NEITHER. So every installed store would have kept
//! its old edges forever, `migrate-all` would have inspected them, said
//! `current`, and skipped, and an operator running the documented migration
//! would have got nothing and no message. A mechanism that ships and can
//! never be applied is worse than one that is missing, because the command
//! that should apply it succeeds.
//!
//! The shape is [`seed_population`](super::super::seed_population)'s, reused
//! rather than invented (ARCH §19): a small marker beside the artifact
//! carrying a version, and a freshness check that reads one line. It is the
//! same question one level down — that module asks "was this seed table built
//! under today's population?", this one asks "was this edge graph built under
//! today's derivation?".
//!
//! **It degrades to "rebuild", never to "cannot open".** The marker gates
//! `store_needs_build`, which only `atlas migrate-all` calls; nothing on the
//! read path consults it, so a store with a stale or absent marker keeps
//! serving the walk exactly as before until an operator rebuilds it. That is
//! the whole reason this is a sidecar and not a bump of `CSR_VERSION`:
//! `CsrEdges::open` rejects a stale-version CSR with NO fallback, so bumping
//! the header would take every corpus's walk dark on every peer and inside
//! every published snapshot until each was rebuilt. A missing edge kind is a
//! smaller harm than an unloadable graph.

use std::io;
use std::path::{Path, PathBuf};

/// The EDGE-DERIVATION version, written as the marker's first line and the
/// only thing [`derivation_marker_is_current`] parses.
///
/// Bump it when the set of edges [`super::write_store`] DERIVES from the atoms
/// changes — a new derived kind, a dropped one, a changed rule. Do not bump it
/// for a change to the edges a pipeline WRITES: those arrive through
/// `edges.json`, which the mtime clause already covers.
///
/// `1` is ei-5c, the first version there is: `Configures` from
/// `Configuration.constituent_atoms`. A store written before this marker
/// existed has none, which is stale by definition — nothing on disk records
/// what the code used to derive.
pub const EDGE_DERIVATION_SCHEMA: u32 = 1;

/// The marker file, beside `edges.csr` in the atlas dir. One line, on purpose:
/// `store_needs_build` runs it per corpus and it must stay a cheap read.
pub const DERIVATION_FILE: &str = "edges.csr.derivation";

/// The marker's path for an atlas dir.
pub fn derivation_marker_path(atlas_dir: &Path) -> PathBuf {
    atlas_dir.join(DERIVATION_FILE)
}

/// Record the derivation a store was just written under. Called by the ONE
/// store writer immediately after `edges.csr` lands, so the two are always
/// written together and the marker is never newer than what it describes.
pub fn write_derivation_marker(atlas_dir: &Path) -> io::Result<()> {
    std::fs::write(
        derivation_marker_path(atlas_dir),
        format!("{EDGE_DERIVATION_SCHEMA}\nconfigures\n"),
    )
}

/// Was this store's edge graph built under today's derivation?
///
/// Two ways to be stale, and both are real on this box:
///
/// - **No marker.** Every store written before ei-5c — which is all 2,160 of
///   them — and they carry no `Configures` edge however many Configuration
///   atoms they hold.
/// - **A different [`EDGE_DERIVATION_SCHEMA`].** The code's derivation moved
///   under a store that cannot know it.
///
/// No mtime clause, deliberately: unlike the seed population, this derivation
/// is not read off anything on disk that a corpus can re-declare. It is a
/// property of the CODE, so a version is the whole of it.
pub fn derivation_marker_is_current(atlas_dir: &Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(derivation_marker_path(atlas_dir)) else {
        return false;
    };
    matches!(
        raw.lines().next().map(|v| v.trim().parse::<u32>()),
        Some(Ok(v)) if v == EDGE_DERIVATION_SCHEMA
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three states, each watched (§18.1). Failing input for the first:
    /// have `derivation_marker_is_current` default to `true` when the file is
    /// absent, which is exactly the "every existing store looks current"
    /// behaviour this marker exists to end.
    #[test]
    fn absent_and_versioned_apart_are_both_stale() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(
            !derivation_marker_is_current(tmp.path()),
            "a store with no marker predates the derivation and must rebuild"
        );

        write_derivation_marker(tmp.path()).expect("write");
        assert!(
            derivation_marker_is_current(tmp.path()),
            "a just-written marker is current"
        );

        std::fs::write(derivation_marker_path(tmp.path()), "0\nconfigures\n").expect("write");
        assert!(
            !derivation_marker_is_current(tmp.path()),
            "a marker from another derivation version is stale"
        );

        // Garbage is stale, not current: the permissive read is the one that
        // silently keeps an old graph.
        std::fs::write(derivation_marker_path(tmp.path()), "configures\n").expect("write");
        assert!(!derivation_marker_is_current(tmp.path()));
    }

    /// The DERIVATION marker, both directions (§18.6).
    ///
    /// A store can be readable, newer than its `atoms.json`, and still have
    /// been built by code that did not know about `Configures` — which is
    /// every store on this box before ei-5c. Neither of the two clauses above
    /// can see that: adding a derived edge kind changes no header and no
    /// mtime. Without this the derivation ships and `atlas migrate-all`, the
    /// documented migration, inspects the store, says `current`, and skips.
    ///
    /// Failing inputs, both run here: delete the marker from a freshly built
    /// store (hop 1 — an installed store must rebuild), and rebuild it (hop 2
    /// — a store built under the marker must NOT, or every migrate-all becomes
    /// a full rebuild forever).
    #[test]
    fn store_needs_build_flags_a_store_built_under_an_older_derivation() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let atoms = vec![
            super::super::tests::entity(1, "Alice"),
            super::super::tests::entity(2, "Bob"),
        ];
        super::super::write_store_blocking(dir, "c1", &atoms, &[]).unwrap();
        assert!(
            !super::super::store_needs_build(dir),
            "a store built by this code is current, marker and all"
        );

        // An installed store: current CSR header, `atoms.json` no newer, and
        // no marker at all because it predates the derivation.
        std::fs::remove_file(derivation_marker_path(dir)).unwrap();
        assert_eq!(
            super::super::edges_csr_version(&dir.join(super::super::EDGES_CSR_FILENAME)),
            Some(super::super::CSR_VERSION)
        );
        assert!(
            super::super::store_needs_build(dir),
            "a store with no derivation marker must rebuild — this is the clause \
         that makes `atlas migrate-all` actually apply a new derived edge kind"
        );

        // And a marker from a superseded derivation is stale for the same reason.
        std::fs::write(
            derivation_marker_path(dir),
            format!("{}\nconfigures\n", EDGE_DERIVATION_SCHEMA + 1),
        )
        .unwrap();
        assert!(super::super::store_needs_build(dir));

        // Rebuilding restamps it, so the rebuild is once and not forever.
        super::super::write_store_blocking(dir, "c1", &atoms, &[]).unwrap();
        assert!(!super::super::store_needs_build(dir));
    }
}
