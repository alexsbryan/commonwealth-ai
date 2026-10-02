// SPDX-License-Identifier: AGPL-3.0-or-later
//! The refactor factory's checks on THIS repo's own data: the concept
//! register (`quality/CONCEPTS.toml`) against the working tree, and the wire
//! differ's negative control against `quality/refactors/node-id.toml`.
//!
//! # Why `xtask` and not `sovereign-cli-dev`
//!
//! The subject of each test is the monorepo's `quality/` data and source
//! tree, not the code program's mechanism, and a lifted code program carries
//! neither (pb-code-clean-lift, ARCH principle 12). They lived in
//! `sovereign-cli-dev` and climbed to the repo root with `git`, so they
//! failed in the code lift's sandbox. The resolver's MECHANISM tests stayed in
//! the crate, on a TempDir fixture (`refactor_cmd/destination.rs`). `xtask` is
//! in no package, so nothing lifts it, and the edge is DEV-only.
//!
//! The tree comes from `SOVEREIGN_WORKSPACE_ROOT` (`.cargo/config.toml`
//! `[env]` sets it to the repo root). If it is unset, the test panics and
//! names it. It never skips.

use std::path::PathBuf;

use kernel_types::{Judgement, Verdict};
use sovereign_cli_dev::{load_spec, prove, RegisterHealth, Resolution, Workspace};

fn repo_root() -> PathBuf {
    let root = std::env::var("SOVEREIGN_WORKSPACE_ROOT").expect(
        "SOVEREIGN_WORKSPACE_ROOT names the monorepo whose register these tests read \
         (.cargo/config.toml sets it in-tree)",
    );
    PathBuf::from(root)
}

fn workspace() -> Workspace {
    Workspace::scan(&repo_root()).expect("workspace scan")
}

/// A destination a worker can `use` today reads as usable.
#[test]
fn the_repaired_evidence_chain_canonicals_resolve() {
    let ws = workspace();
    for canonical in [
        "kernel_types::judgement::Verdict",
        "kernel_types::judgement::Judgement",
        "kernel_types::origin::Origin",
        "kernel_types::custody::Custody",
        "kernel_types::attribution::Attribution",
        "kernel_types::answer::Answer",
        "corpus_index::index::EvidenceSet",
    ] {
        let r = ws.resolve(canonical);
        assert!(r.exists(), "{canonical}: {}", r.render());
    }
}

/// THE NEGATIVE CONTROL (ARCH §18.1). A check with no failing input you can
/// name is not a check, so this pins the exact stale path the register
/// carried until 2026-08-24: `sovereign-contracts` has no `verdict` module
/// and never will — the type lives in the kernel, and `judgement.rs` says
/// why. If this ever reads `usable`, the resolver has gone blind and every
/// health line it prints is a false green.
#[test]
fn the_stale_canonical_this_check_was_built_for_is_refused() {
    let ws = workspace();
    let r = ws.resolve("sovereign_contracts::verdict::Verdict");
    assert!(!r.exists(), "the control must not resolve: {}", r.render());
    assert!(
        matches!(r, Resolution::Elsewhere { .. }),
        "Verdict is declared elsewhere in this workspace, so the verdict must \
         be Elsewhere and not Unbuilt — the two carry different diagnoses: {}",
        r.render()
    );
}

/// THE RATCHET, and it runs in both directions.
///
/// Every row declares `home`; the tree is the arbiter. A `minted` row whose
/// canonical stops resolving fails — that is the direction any check would
/// have caught. A `planned` row whose canonical has quietly acquired a home
/// fails too, and THAT is the direction that actually broke this register:
/// seven nouns were minted in kernel-types across three rungs while their
/// rows went on naming `sovereign_contracts::…` paths that never existed.
///
/// There is no exception list. An earlier draft carried one as a const here,
/// which is a second decider for a question `quality/CONCEPTS.toml` already
/// answers (ARCH §10.6) — and being a test-local const, it could only ever
/// encode the one direction its author thought of.
#[test]
fn every_register_home_matches_the_working_tree() {
    let health = RegisterHealth::survey(&repo_root()).expect("survey");
    let bad = health.disagreements();
    assert!(
        bad.is_empty(),
        "the register and the tree disagree on {} row(s):\n{}",
        bad.len(),
        bad.iter()
            .map(|r| format!(
                "  {} -> {}\n    tree says: {}\n    {}",
                r.name,
                r.canonical,
                r.found.render(),
                r.remedy()
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

fn row<'a>(rows: &'a [Judgement], subject_contains: &str) -> &'a Judgement {
    rows.iter()
        .find(|r| r.subject().contains(subject_contains))
        .unwrap_or_else(|| {
            panic!(
                "no row with subject containing {subject_contains:?}; have: {:?}",
                rows.iter().map(|r| r.subject()).collect::<Vec<_>>()
            )
        })
}

/// THE NEGATIVE CONTROL — the reason the wire differ exists. It must FAIL
/// `node_id: String -> NodeId`, with the production bytes in the verdict. A
/// gate with no failing input you can name is not a gate (ARCH §18.1). The
/// subject is `load_spec` against the LIVE spec, so a copy inside the crate
/// would drift and the test would stop meaning anything.
#[test]
fn negative_control_node_id_fails_with_the_production_bytes() {
    let spec = load_spec(&repo_root().join("quality/refactors/node-id.toml"))
        .expect("control spec must load");
    let report = prove(&spec);
    assert!(
        !report.passes(),
        "the negative control PASSED — the differ is a formatter"
    );
    assert_eq!(report.overall.verdict(), Verdict::Failed);

    // The Display form: exactly what StatusResponse.node_id serves today
    // (routes_status.rs:662 pins the same value).
    let display = row(&report.rows, "Display");
    assert_eq!(display.verdict(), Verdict::Failed);
    assert!(
        display
            .reason()
            .as_str()
            .contains("before=\"node-6c955b5f1361aaaa\""),
        "{}",
        display.reason()
    );
    assert!(
        display
            .reason()
            .as_str()
            .contains("after=[108,149,91,95,19,97,170,170,1,35,69,103,137,171,205,239]"),
        "{}",
        display.reason()
    );

    // The to_hex form (PrincipalRequestStatus.node_id) diverges too.
    let hex = row(&report.rows, "to_hex");
    assert_eq!(hex.verdict(), Verdict::Failed);
    assert!(
        hex.reason()
            .as_str()
            .contains("before=\"6c955b5f1361aaaa0123456789abcdef\""),
        "{}",
        hex.reason()
    );

    // And sqlite is unprovable — three incompatible id encodings in the
    // kernel; not a pass, not silently skipped.
    let sqlite = row(&report.rows, "wire sqlite");
    assert_eq!(sqlite.verdict(), Verdict::CouldNotJudge);
    assert!(sqlite
        .reason()
        .as_str()
        .contains("three incompatible id encodings"));
}
