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

fn tree() -> Vec<String> {
    let out = Command::new(env!("CARGO"))
        .args([
            "tree",
            "-p",
            "corpus-mcp",
            "-e",
            "normal",
            "--prefix",
            "none",
        ])
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

#[test]
fn the_dep_tree_carries_no_inference_stack() {
    let crates = tree();
    // Positive control (§18.1): the tree resolved and holds what it must.
    for must in ["corpus-engine", "corpus-engine-vocab", "lancedb", "tantivy"] {
        assert!(
            crates.iter().any(|c| c == must),
            "tree lacks `{must}` — did cargo tree run?"
        );
    }
    let present: Vec<&String> = crates
        .iter()
        .filter(|c| FORBIDDEN.contains(&c.as_str()))
        .collect();
    assert!(
        present.is_empty(),
        "corpus-mcp links an inference/mesh crate it must not: {present:?}"
    );
}
