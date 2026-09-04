// SPDX-License-Identifier: AGPL-3.0-or-later
//! The closure rule that makes this binary the proof it claims to be: no
//! llama.cpp, no ort, no iroh, no mesh transport, no local inference crate.
//! boundary-gate governs the IN-REPO closure (`quality/ARCH_LAYERS.toml`
//! `[[package]] corpus-mcp`); this pins the third-party half against
//! `cargo tree`, which is the artifact a reader would check by hand.

use std::process::Command;

const FORBIDDEN: &[&str] = &[
    "llama-cpp-4",
    "llama-cpp-sys-4",
    "ort",
    "ort-sys",
    "iroh",
    "iroh-net",
    "sovereign-inference",
    "sovereign-gliner",
    "commonwealth-transport",
    "sovereign-core",
    "sovereign-tools",
];

fn tree(package: &str) -> Vec<String> {
    let out = Command::new(env!("CARGO"))
        .args(["tree", "-p", package, "-e", "normal", "--prefix", "none"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo tree runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().next().map(str::to_string))
        .collect()
}

/// Assert one package's normal-dependency closure carries none of [`FORBIDDEN`],
/// with a positive control first: a `cargo tree` that silently returned nothing
/// would otherwise "pass" by having no forbidden crate in an empty list (§18.1).
fn assert_no_inference_stack(package: &str, must_contain: &[&str]) {
    let crates = tree(package);
    for must in must_contain {
        assert!(
            crates.iter().any(|c| c == must),
            "{package}: tree lacks `{must}` — did cargo tree run?"
        );
    }
    let present: Vec<&String> = crates
        .iter()
        .filter(|c| FORBIDDEN.contains(&c.as_str()))
        .collect();
    assert!(
        present.is_empty(),
        "{package} links an inference/mesh crate it must not: {present:?}"
    );
}

#[test]
fn the_dep_tree_carries_no_inference_stack() {
    assert_no_inference_stack(
        "corpus-mcp",
        &["corpus-engine", "corpus-engine-vocab", "lancedb", "tantivy"],
    );
}

/// The atlas build orchestrator is the WRITER half of the same promise: what
/// `corpus-mcp` reads, `svrn enrich build` produced. It sat on `sovereign-core`,
/// `sovereign-tools` and `sovereign-inference` until order ei-5a-build-cut took
/// all three out (closure 695 → 590 crates), and it is in the `corpus-mcp`
/// `[[package]]` in `quality/ARCH_LAYERS.toml` so boundary-gate refuses an
/// in-repo edge back. This pins the third-party half the same way, because the
/// gate cannot see llama.cpp arriving through a transitive crates.io dep.
///
/// The positive control differs from the host's: this crate is the orchestrator,
/// so it must carry the enrichment catalog and the OICP client it drives the
/// daemon through — but NOT lancedb/tantivy, which it reaches only via
/// corpus-engine.
#[test]
fn the_enrichment_build_orchestrator_carries_no_inference_stack() {
    assert_no_inference_stack(
        "sovereign-enrichment-build",
        &[
            "corpus-engine",
            "corpus-engine-vocab",
            "sovereign-enrichment-catalog",
            "sovereign-contracts",
            "oicp-client",
        ],
    );
}
