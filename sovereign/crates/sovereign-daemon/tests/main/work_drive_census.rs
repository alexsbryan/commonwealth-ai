// SPDX-License-Identifier: AGPL-3.0-or-later
//! **One job-execution drive** (FIVE_PROGRAMS §2c; pb-work-donor).
//!
//! A drive takes a unit off the `work` fold, and the act that says so is a
//! `Lease`. Since pb-work-donor the one drive is cw-rails' donor
//! (`commonwealth-rails/src/donor.rs`); the svrn daemon serves an execute
//! origin the donor forwards to, and takes nothing itself. So this census
//! reads every shipped source file in the workspace and names each one that
//! spells `WorkAct::Lease(` — constructing one, or matching one. Only the
//! owners below may, each for the reason beside it. The failing input: a
//! second donor loop, in the daemon or anywhere else — it has to lease, and
//! its file is named here.
//!
//! Shipped source means: not a test file (`tests/`, `tests.rs`, `*_tests.rs`),
//! not the part of a file after its first `#[cfg(test)]`, and not an
//! `examples/` binary. The one example that leases, commonwealth-work's
//! `work_peer`, is that package's own lift fixture (`[lift.cw-work]`) and is
//! built by no program.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("sovereign-daemon lives three levels under the repo root")
        .to_path_buf()
}

/// Shipped Rust source under `dir`, each with its text before `#[cfg(test)]`.
fn shipped(dir: &Path, out: &mut Vec<(PathBuf, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.filter_map(Result::ok) {
        let path = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if !matches!(
                name.as_str(),
                "target" | "node_modules" | "tests" | "examples"
            ) {
                shipped(&path, out);
            }
        } else if name.ends_with(".rs") && name != "tests.rs" && !name.ends_with("_tests.rs") {
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("the census cannot read {}: {e}", path.display()));
            // Cut at the test MODULE, not the first `#[cfg(test)]`: a file may
            // gate a single import that way above its shipped code.
            let product = match text.find("#[cfg(test)]\nmod ") {
                Some(i) => text[..i].to_string(),
                None => text,
            };
            out.push((path, product));
        }
    }
}

/// The files that may construct a `Lease`, and why.
const OWNERS: &[(&str, &str)] = &[
    (
        "commonwealth/crates/commonwealth-rails/src/donor.rs",
        "THE drive: cw-rails' donor leases, runs, renews and reports",
    ),
    (
        "oicp-types/src/work/act.rs",
        "the vocabulary: the act's own definition and its kind",
    ),
    (
        "commonwealth/crates/commonwealth-work/src/projection.rs",
        "the fold: it reads a Lease act back, and leases nothing",
    ),
    (
        "commonwealth/crates/commonwealth-rails/src/plane_seal.rs",
        "the seal's snapshot: it re-appends a lease this node already holds, and takes nothing",
    ),
];

#[test]
fn only_cw_rails_donor_takes_a_unit() {
    let root = repo_root();
    let mut files = Vec::new();
    for dir in [
        "commonwealth/crates",
        "sovereign/crates",
        "oicp-types",
        "kernel-types",
        "corpus-engine",
        "oicp-client",
    ] {
        shipped(&root.join(dir), &mut files);
    }
    let leasing: Vec<String> = files
        .iter()
        .filter(|(_, text)| text.contains("WorkAct::Lease("))
        .map(|(p, _)| p.strip_prefix(&root).unwrap_or(p).display().to_string())
        .collect();
    // The instrument, validated first: the drive itself must be seen, or an
    // empty walk would pass.
    assert!(
        leasing.iter().any(|f| f == OWNERS[0].0),
        "the census no longer sees the drive ({}) — the walk is broken; it saw {leasing:?}",
        OWNERS[0].0
    );
    let second: Vec<&String> = leasing
        .iter()
        .filter(|f| !OWNERS.iter().any(|(owner, _)| owner == f))
        .collect();
    assert!(
        second.is_empty(),
        "a second job-execution drive: these files lease a unit, and only cw-rails' donor \
         takes one (FIVE_PROGRAMS §2c; owners: {OWNERS:?}): {second:?}"
    );
}
