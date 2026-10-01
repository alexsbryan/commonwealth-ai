// SPDX-License-Identifier: AGPL-3.0-or-later
//! Every test that boots a daemon binary's `run` pins its cw-rails
//! (five-programs-66, principle 10).
//!
//! A booted daemon dials cw-rails at `[daemon] rails_base` (default
//! 127.0.0.1:9747); until pb-rails-untether it also brought one up there on
//! `$CW_RAILS_DIR` (default ~/.commonwealth-rails), leaving the orphan that
//! minted this. A test that pins neither still writes into the developer's
//! real store when their own cw-rails serves 9747. So hermeticity is a census over test source, not a
//! convention: a file that spawns a sovereign-daemon binary with `run` must
//! name both `rails_base` and `CW_RAILS_DIR`.
//!
//! The failing input: delete `rails_base` from any file below. The scan also
//! fails if it finds none of the files it is known to cover, so an empty
//! walk is never green.
//!
//! The same scan, widened to in-process boots, is the ONE rule for the
//! nextest `daemon-boot` test group (.config/nextest.toml, phase-b-4): a file
//! that boots a daemon has exactly one clause in the group's filter, and the
//! filter has no clause for a file that does not. The failing input: add a
//! boot to a file outside the group, or drop a clause from the filter.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("sovereign-daemon lives three levels under the repo root")
        .to_path_buf()
}

/// Test source: a file under a `tests/` dir, a `tests.rs` module, or a
/// `*_tests.rs` file a `#[path]` attribute mounts.
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
            && (name == "tests.rs"
                || name.to_string_lossy().ends_with("_tests.rs")
                || path.components().any(|c| c.as_os_str() == "tests"))
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
    let runs =
        text.contains("sovereign-daemon") && (text.contains(ARGS_RUN) || text.contains(ARG_RUN));
    runs || boots_the_admin_join(text)
}

/// Spawns a daemon binary's admin-join launch through the setup wizard's
/// join child (`join_child::join`, whose result is a `JoinFailure`): the
/// child starts the daemon and joins through cw-rails at its `rails_base`,
/// so it pins and groups like a `run` (pb-mesh-exit-transport, where the
/// wizard test stopped booting a `run` founder).
fn boots_the_admin_join(text: &str) -> bool {
    text.contains(concat!("JoinFailure", "::"))
        && (text.contains("sovereign-stock") || text.contains("sovereign-daemon"))
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

/// Boots a daemon in process: names `EmbeddedDaemon` and calls its `start()`,
/// the one boot since pb-mesh-exit-transport. Needles built with `concat!`,
/// as above.
fn boots_in_process(text: &str) -> bool {
    text.contains(concat!("Embedded", "Daemon")) && text.contains(concat!(".start", "()"))
}

/// The filter clause that places one test file's tests: its test binary for
/// a `tests/<name>.rs` target, else the module prefix nextest names its tests
/// by. A shape this cannot place is a panic, so no booting file is ever
/// silently outside the group.
fn group_clause(root: &Path, file: &Path) -> String {
    let rel: Vec<String> = file
        .strip_prefix(root)
        .unwrap_or(file)
        .iter()
        .map(|c| c.to_string_lossy().into_owned())
        .collect();
    let module = |parts: &[String]| {
        parts
            .iter()
            .map(|p| p.trim_end_matches(".rs"))
            .filter(|p| *p != "mod")
            .collect::<Vec<_>>()
            .join("::")
    };
    let (krate, rest) = (&rel[2], &rel[3..]);
    match rest {
        [t, name] if t == "tests" => format!("binary_id({krate}::{})", name.trim_end_matches(".rs")),
        [t, m, path @ ..] if t == "tests" && m == "main" => format!("test(/^{}::/)", module(path)),
        [s, path @ .., name] if s == "src" => {
            // A `#[path = "<name>"] mod x;` in a sibling places it at `<sibling>::x`.
            let dir = file.parent().expect("a file has a parent");
            let needle = format!("#[path = \"{name}\"]");
            let host = std::fs::read_dir(dir)
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .find_map(|e| {
                    let text = std::fs::read_to_string(e.path()).ok()?;
                    let after = text.split_once(&needle)?.1;
                    let m = after.split_once("mod ")?.1.split(|c: char| !c.is_alphanumeric() && c != '_').next()?;
                    Some((e.file_name().to_string_lossy().into_owned(), m.to_string()))
                });
            let mut parts = path.to_vec();
            match host {
                Some((sibling, m)) => {
                    parts.push(sibling);
                    parts.push(m);
                }
                None => parts.push(name.clone()),
            }
            format!("test(/^{}::/)", module(&parts))
        }
        _ => panic!(
            "the census cannot name the nextest filter clause for {} — teach group_clause its shape",
            file.display()
        ),
    }
}

#[test]
fn every_test_that_boots_a_daemon_runs_in_the_daemon_boot_group() {
    let root = repo_root();
    let mut files = Vec::new();
    for dir in ["sovereign/crates", "commonwealth/crates"] {
        test_sources(&root.join(dir), &mut files);
    }
    let booting: BTreeSet<String> = files
        .iter()
        .filter(|p| {
            let text = std::fs::read_to_string(p)
                .unwrap_or_else(|e| panic!("the census cannot read {}: {e}", p.display()));
            boots_a_daemon(&text) || boots_in_process(&text)
        })
        .map(|p| group_clause(&root, p))
        .collect();
    assert!(
        booting.contains("test(/^local_only_boot::/)")
            && booting.contains("binary_id(sovereign-daemon::solo_rails_e2e)"),
        "the scan no longer sees the known in-process and binary boots — it is broken: {booting:?}"
    );

    let config_path = root.join(".config/nextest.toml");
    let config: toml::Value = toml::from_str(
        &std::fs::read_to_string(&config_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", config_path.display())),
    )
    .expect("nextest.toml parses");
    let max_threads = config["test-groups"]["daemon-boot"]["max-threads"].as_integer();
    assert!(
        max_threads.is_some_and(|n| n >= 1),
        "the daemon-boot group must bound its threads, got {max_threads:?}"
    );
    let filters: Vec<&str> = config["profile"]["default"]["overrides"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|o| o.get("test-group").and_then(|g| g.as_str()) == Some("daemon-boot"))
        .filter_map(|o| o.get("filter").and_then(|f| f.as_str()))
        .collect();
    let [filter] = filters.as_slice() else {
        panic!(
            "exactly one default-profile override places the daemon-boot group, found {filters:?}"
        );
    };
    let grouped: BTreeSet<String> = filter
        .split('|')
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .collect();
    let outside: Vec<_> = booting.difference(&grouped).collect();
    let stale: Vec<_> = grouped.difference(&booting).collect();
    assert!(
        outside.is_empty() && stale.is_empty(),
        "the daemon-boot group's filter in .config/nextest.toml and the test files that boot \
         a daemon disagree. Booting but not grouped (add the clause): {outside:?}. Grouped but \
         not booting (remove it): {stale:?}"
    );
}
