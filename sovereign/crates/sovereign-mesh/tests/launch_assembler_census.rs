// SPDX-License-Identifier: AGPL-3.0-or-later
//! **Falsifier 3** (`quality/TOPOLOGY.md` §10): exactly ONE exhaustive match
//! over `Launch` that constructs anything.
//!
//! The acceptance criterion is a navigability one — "a glanceable path to click
//! through a few files in an IDE and follow the daemon runtime in each of its
//! invocations to its actual implementation, with remarkably finite deviations"
//! — and its spine is three files: `launch.rs` (what this process is), the
//! assembler (what that invocation builds), `daemon_services.rs` (what got
//! built). A second place composing a `DaemonServices` breaks the middle link:
//! a reader following `Launch` no longer arrives at every shape the system can
//! take, and the count 8 -> 3 -> 3 stops being true of the code.
//!
//! ## What the type system already does, and what this covers
//!
//! `DaemonServices::desktop` and `::headless` are `pub(crate)` as of Phase 4b,
//! so **no crate outside `sovereign-mesh` can compose a serving daemon at
//! all** — the compiler, not this test, holds that half. What it cannot hold
//! is `MeshAdmin`: a bare enum variant is nameable wherever the enum is, and
//! closing it needs a private witness field, which is Phase 7's "make the
//! second assembly uncompilable". Until then this census covers the gap.
//!
//! ## The exemption, stated rather than assumed
//!
//! `sovereign-mesh`'s own tree is exempt. The assembler lives here, the
//! composite constructors are `pub(crate)` *for* here, and this crate's own
//! suite building fixture daemons directly is the type exercising itself — not
//! a second assembly. The invariant that matters is that **no HOST composes
//! its own**, and that is what is checked. Naming the exemption is the point:
//! an unstated one is how a gate quietly stops covering the thing it was
//! written for (§18.1).
//!
//! ## Named failing input (ARCH §18.1)
//!
//! Add `EmbeddedDaemon::new(root, cfg, DaemonServices::MeshAdmin)` in a new
//! command module in any host crate. That file will not mention `assemble`,
//! and this test fails naming it. It is the exact edit the three repointed
//! sites each used to contain.

use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    // sovereign/crates/sovereign-mesh -> repo root
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("repo root above sovereign/crates/<crate>")
        .to_path_buf()
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
        if p.is_dir() {
            // `target/` is build output and `node_modules` is not ours.
            if name == "target" || name == "node_modules" || name == ".git" {
                continue;
            }
            rust_sources(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

/// Files that commission a daemon, and whether each one goes through the
/// assembler. The unit is the FILE, not the line: a site that names
/// `assemble` in the same file is reading the one match's answer, and a site
/// that does not is answering for itself.
fn commissioning_files() -> Vec<(String, bool)> {
    let root = workspace_root();
    let mut files = Vec::new();
    rust_sources(&root, &mut files);
    let mut out = Vec::new();
    for f in files {
        let rel = f.strip_prefix(&root).unwrap_or(&f).display().to_string();
        // The exemption, and the only one — see the module docs.
        if rel.starts_with("sovereign/crates/sovereign-mesh/") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        if !text.contains("EmbeddedDaemon::new(") {
            continue;
        }
        out.push((rel, text.contains("assemble(")));
    }
    out
}

/// ARCH §18.4 — validate the instrument first. A census that walked no files,
/// or that failed to find the sites we already know exist, would report a
/// clean sweep for the wrong reason.
#[test]
fn the_census_finds_the_commissioning_sites_it_should() {
    let found = commissioning_files();
    assert!(
        found.len() >= 3,
        "instrument check failed: the census found {} host file(s) calling \
         `EmbeddedDaemon::new`, but the daemon is commissioned from at least \
         three outside `sovereign-mesh` — the headless bootstrap \
         (`sovereign-cli-daemon`), the desktop (`sovereign-desktop`), and the \
         mesh verb (`sovereign-cli-llm`). The scan is not reading the tree, so \
         its verdict below means nothing. Found: {:?}",
        found.len(),
        found.iter().map(|(f, _)| f).collect::<Vec<_>>()
    );
}

/// The bar itself.
#[test]
fn every_commissioning_site_goes_through_the_assembler() {
    let offenders: Vec<String> = commissioning_files()
        .into_iter()
        .filter(|(_, via_assembler)| !via_assembler)
        .map(|(f, _)| f)
        .collect();
    assert!(
        offenders.is_empty(),
        "daemon-convergence Falsifier 3: {} file(s) commission a daemon \
         without going through `sovereign_mesh::assemble` — the one exhaustive \
         match over `Launch` that constructs anything. Hand it \
         `LaunchParts` and let it name the variant:\n{}",
        offenders.len(),
        offenders
            .iter()
            .map(|f| format!("  {f}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
