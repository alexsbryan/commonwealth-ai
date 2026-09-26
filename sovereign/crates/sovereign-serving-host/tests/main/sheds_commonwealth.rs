// SPDX-License-Identifier: AGPL-3.0-or-later
//! phase-b pb-serve-sheds-core's census: the serving host names no
//! commonwealth crate, so serve's lifted closure holds none.
//!
//! The last edge was `commonwealth-core`, for four things that were never
//! the mesh core's: `NodeId` (kernel-types owns it), the model-transfer wire
//! (now `oicp_types::model_transfer`), `PeerHealthTracker` (now
//! `sovereign_scheduler::peer_health`) — plus the slot-alias function local
//! serving reached through the scheduler (now `sovereign_contracts::venue`).
//! This pins that none comes back: a dependency line in Cargo.toml, or a
//! `commonwealth_*` path in code (comments are prose and do not count).
//!
//! Watched to fail: re-add `commonwealth-core` to Cargo.toml with one
//! `commonwealth_core::ids::NodeId` use, and this goes red naming the site.

use std::path::{Path, PathBuf};

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|x| x == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn serving_host_names_no_commonwealth_crate() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sites = Vec::new();

    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).expect("read Cargo.toml");
    for (i, line) in manifest.lines().enumerate() {
        let line = line.trim_start();
        if !line.starts_with('#') && line.starts_with("commonwealth-") {
            sites.push(format!("Cargo.toml:{}: {line}", i + 1));
        }
    }

    let mut files = Vec::new();
    rust_sources(&root.join("src"), &mut files);
    rust_sources(&root.join("tests"), &mut files);
    assert!(
        files.len() > 20,
        "the walk found {} files — wrong root?",
        files.len()
    );
    for file in &files {
        if file.ends_with("sheds_commonwealth.rs") {
            continue;
        }
        let src = std::fs::read_to_string(file).expect("read source");
        for (i, line) in src.lines().enumerate() {
            if !line.trim_start().starts_with("//") && line.contains("commonwealth_") {
                let rel = file.strip_prefix(root).unwrap_or(file);
                sites.push(format!("{}:{}: {}", rel.display(), i + 1, line.trim()));
            }
        }
    }

    assert!(
        sites.is_empty(),
        "sovereign-serving-host names a commonwealth crate again, so serve's \
         lifted closure takes it (phase-b pb-serve-sheds-core). NodeId is \
         kernel_types', the model-transfer wire oicp_types::model_transfer's, \
         PeerHealthTracker sovereign_scheduler::peer_health's. Sites:\n{}",
        sites.join("\n")
    );
}
