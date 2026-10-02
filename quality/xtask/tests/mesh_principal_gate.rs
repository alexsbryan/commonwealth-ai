// SPDX-License-Identifier: AGPL-3.0-or-later
//! sovereign-daemon's `mesh_principal_gate` ratchet, moved here by
//! pb-distribution-svrn-lift-2: it scans the monorepo's `sovereign/crates`,
//! which a lifted svrn does not carry. Its lists stay at their historical
//! path, `sovereign_daemon::mesh_principal_gate`, mounted here and not copied.

#[path = "../../../sovereign/crates/sovereign-daemon/src/mesh_principal_gate.rs"]
mod gate;
#[path = "shared/repo_root.rs"]
mod repo_root;

use gate::*;
use repo_root::repo_root;
use std::path::{Path, PathBuf};

/// The gate's own file spells the wire form in its lists, as it may.
const GATE_FILE: &str = "sovereign/crates/sovereign-daemon/src/mesh_principal_gate.rs";

/// Walk `sovereign/crates` for `.rs` files that are not tests.
///
/// "Not a test" is: not under a `tests/` directory, and not a file whose
/// own name says so. A `#[cfg(test)]` module INSIDE a production file is
/// deliberately still scanned — that is where a decider would hide.
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

/// bar `mp-no-decider-reads-the-header`:
///
/// THE failing input: restore any production read of `x-node-id` outside
/// the one allowed path and this goes red, naming the file and the line.
/// The comment this replaces asserted the same thing in English and was
/// false for as long as it stood.
#[test]
fn no_production_file_outside_the_one_allowed_path_reads_the_peer_header() {
    let root = repo_root();
    let mut files = Vec::new();
    for dir in repo_root::member_dirs() {
        production_sources(&dir, &mut files);
    }
    assert!(
        files.len() > 500,
        "the walk found only {} files — it is not scanning the tree it \
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
        if READ_ALLOWED.iter().any(|a| rel.ends_with(a)) || rel.ends_with(GATE_FILE) {
            continue;
        }
        let is_resolver = RESOLVERS.iter().any(|a| rel.ends_with(a));
        let is_sender = SENDERS.iter().any(|a| rel.ends_with(a));
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (n, line) in text.lines().enumerate() {
            // Everything from the first `#[cfg(test)]` on is test code.
            if line.trim_start().starts_with("#[cfg(test)]") {
                break;
            }
            // A doc comment or a prose comment may NAME the header — that
            // is how the next reader learns why it is gone. Only code is
            // scanned.
            let code = line.trim_start();
            if code.starts_with("//") || code.starts_with("*") {
                continue;
            }
            if line.contains(READER) && !is_resolver {
                hits.push(format!("{rel}:{}: {}", n + 1, line.trim()));
            }
            if WIRE_FORM.iter().any(|f| line.contains(f)) && !is_sender {
                hits.push(format!("{rel}:{}: {}", n + 1, line.trim()));
            }
        }
    }

    assert!(
        hits.is_empty(),
        "a production decider reads the peer header. It must read the \
             attached `Principal` instead — the header is a CLAIM and the \
             principal is what this daemon verified. The wire form lives in \
             {READ_ALLOWED:?}; only {RESOLVERS:?} may call it.\n{}",
        hits.join("\n")
    );
}
