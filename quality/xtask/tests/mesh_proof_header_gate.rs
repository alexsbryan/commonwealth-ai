// SPDX-License-Identifier: AGPL-3.0-or-later
//! sovereign-daemon's `mesh_proof_header_gate` ratchet, moved here by
//! pb-distribution-svrn-lift-2: it scans the monorepo's `cmnwlth/crates`
//! and `sovereign/crates`, which a lifted svrn does not carry. Its lists stay
//! at their historical path, `sovereign_daemon::mesh_proof_header_gate`,
//! mounted here and not copied.

#[path = "../../../svrn/crates/sovereign-daemon/src/mesh_proof_header_gate.rs"]
mod gate;
#[path = "shared/repo_root.rs"]
mod repo_root;

use gate::*;
use repo_root::repo_root;
use std::path::{Path, PathBuf};

/// The gate's own file spells the wire form in its lists, as it may.
const GATE_FILE: &str = "svrn/crates/sovereign-daemon/src/mesh_proof_header_gate.rs";

fn production_sources(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if name != "tests" && name != "target" {
                production_sources(&path, out);
            }
        } else if name.ends_with(".rs") && !name.ends_with("_tests.rs") && name != "tests.rs" {
            out.push(path);
        }
    }
}

/// THE failing input: spell `"x-mesh-proof"` in any third production
/// file and this goes red, naming the file and the line.
#[test]
fn only_the_minter_and_the_resolver_spell_the_mesh_proof_header() {
    let root = repo_root();
    let mut files = Vec::new();
    for dir in repo_root::member_dirs() {
        production_sources(&dir, &mut files);
    }
    assert!(
        files.len() > 500,
        "the walk found only {} files — it is not scanning the trees it \
             claims to scan, which would make this gate pass vacuously",
        files.len()
    );

    let mut hits = Vec::new();
    for path in files {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if PROOF_HEADER_ALLOWED.iter().any(|a| rel.ends_with(a)) || rel.ends_with(GATE_FILE) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (n, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("#[cfg(test)]") {
                break;
            }
            // Prose may name the header — that is how the next reader
            // learns where it comes from. Only code is scanned.
            let code = line.trim_start();
            if code.starts_with("//") || code.starts_with("*") {
                continue;
            }
            if PROOF_WIRE_FORM.iter().any(|f| line.contains(f)) {
                hits.push(format!("{rel}:{}: {}", n + 1, line.trim()));
            }
        }
    }

    assert!(
        hits.is_empty(),
        "a production file outside {PROOF_HEADER_ALLOWED:?} spells the mesh-proof \
             header. Mint it with `commonwealth_transport::mesh_proof::mesh_proof_stamp` \
             and apply the pair it returns; read it only in the internal resolver.\n{}",
        hits.join("\n")
    );
}
