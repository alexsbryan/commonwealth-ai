// SPDX-License-Identifier: AGPL-3.0-or-later
//! Every test that boots a daemon binary's `run` pins its cw-rails
//! (five-programs-66, principle 10).
//!
//! A booted daemon brings cw-rails up at `[daemon] rails_base` (default
//! 127.0.0.1:9747) on `$CW_RAILS_DIR` (default ~/.commonwealth-rails). A test
//! that pins neither writes into the developer's real store when their own
//! cw-rails serves 9747, and otherwise leaves one on 9747 — the orphan that
//! minted this. So hermeticity is a census over test source, not a
//! convention: a file that spawns a sovereign-daemon binary with `run` must
//! name both `rails_base` and `CW_RAILS_DIR`.
//!
//! The failing input: delete `rails_base` from any file below. The scan also
//! fails if it finds none of the files it is known to cover, so an empty
//! walk is never green.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("sovereign-daemon lives three levels under the repo root")
        .to_path_buf()
}

/// Test source: a file under a `tests/` dir, or a `tests.rs` module.
fn test_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.filter_map(Result::ok) {
        let path = e.path();
        let name = e.file_name();
        if path.is_dir() {
            if name != "target" && name != "node_modules" {
                test_sources(&path, out);
            }
        } else if path.extension().is_some_and(|x| x == "rs")
            && (name == "tests.rs" || path.components().any(|c| c.as_os_str() == "tests"))
        {
            out.push(path);
        }
    }
}

/// Spawns a daemon binary's `run`. The needles are built with `concat!` so
/// this file does not match itself.
fn boots_a_daemon(text: &str) -> bool {
    const ARGS_RUN: &str = concat!("[\"", "run\",");
    const ARG_RUN: &str = concat!(".arg(\"", "run\")");
    text.contains("sovereign-daemon") && (text.contains(ARGS_RUN) || text.contains(ARG_RUN))
}

#[test]
fn every_test_that_boots_a_daemon_pins_its_cw_rails() {
    let root = repo_root();
    let mut files = Vec::new();
    for dir in ["sovereign/crates", "commonwealth/crates"] {
        test_sources(&root.join(dir), &mut files);
    }
    let booting: Vec<(String, String)> = files
        .iter()
        .filter_map(|p| {
            let text = std::fs::read_to_string(p)
                .unwrap_or_else(|e| panic!("the census cannot read {}: {e}", p.display()));
            let rel = p.strip_prefix(&root).unwrap_or(p).display().to_string();
            boots_a_daemon(&text).then_some((rel, text))
        })
        .collect();
    for known in [
        "sovereign/crates/sovereign-daemon/tests/main/admin_join_serves_venues_e2e.rs",
        "sovereign/crates/sovereign-daemon/tests/solo_rails_e2e.rs",
        "sovereign/crates/sovereign-cli-daemon/src/setup_cmd/terminal/join_child/tests.rs",
    ] {
        assert!(
            booting.iter().any(|(rel, _)| rel == known),
            "the census no longer sees {known} as booting a daemon — the scan is broken, \
             or the file moved; saw {:?}",
            booting.iter().map(|(r, _)| r).collect::<Vec<_>>()
        );
    }
    let unpinned: Vec<&str> = booting
        .iter()
        .filter(|(_, text)| !(text.contains("rails_base") && text.contains("CW_RAILS_DIR")))
        .map(|(rel, _)| rel.as_str())
        .collect();
    assert!(
        unpinned.is_empty(),
        "these tests boot a daemon's `run` without pinning `[daemon] rails_base` to an \
         ephemeral port and `CW_RAILS_DIR` under their temp root, so the cw-rails it brings \
         up lands on 9747 / ~/.commonwealth-rails: {unpinned:?}"
    );
}
