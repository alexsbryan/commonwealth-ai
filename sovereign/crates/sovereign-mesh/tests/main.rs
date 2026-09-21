// SPDX-License-Identifier: AGPL-3.0-or-later
//! One integration-test binary for this crate.
//!
//! Each former `tests/<name>.rs` is now `tests/main/<name>.rs`, declared
//! below, so cargo links ONE executable instead of one per file. Every
//! test still runs; its name gains the module path as a prefix, so a
//! filter that named a file now names a module:
//!
//!     cargo test -p <crate> --test main <module>::
//!
//! `#[path]` is load-bearing: `tests/main.rs` is a CRATE ROOT, so a bare
//! `mod foo;` resolves to `tests/foo.rs` — which cargo would then also
//! link as its own test binary, which is the thing this file exists to
//! stop. The attribute keeps the sources in `tests/main/`, a directory
//! cargo does not scan for targets.
//!
//! Files still sitting directly in `tests/` are there on purpose — they
//! need process isolation, or a `.config/nextest.toml` override keys on
//! their binary name. Do not fold those in.

#[path = "main/dst.rs"]
#[cfg(feature = "dst")]
mod dst;
#[path = "main/dst_scenarios.rs"]
mod dst_scenarios;
// The loop tests moved here from `src/ring_sync/{tests,snapshot_tests,
// projection_tests}.rs` and `src/rail_kv_pump/tests.rs` at domains
// `dm-daemon-api-edge` (b): they assemble the host node, which now lives in
// `sovereign-daemon`, and a unit test inside this crate that named the daemon
// would put two builds of `sovereign-mesh` in the graph (a dev-dependency
// cycle) so the `FabricPart` types would not unify. An integration test
// resolves both to one build.
#[path = "main/local_pod_smoke.rs"]
mod local_pod_smoke;
#[path = "main/mesh_sim_ring_room.rs"]
mod mesh_sim_ring_room;
#[path = "main/mesh_sim_scoreboard.rs"]
mod mesh_sim_scoreboard;
#[path = "main/replication_sender_census.rs"]
mod replication_sender_census;
#[path = "main/scheduler_replay_agreement.rs"]
mod scheduler_replay_agreement;
#[path = "main/worker_e2e.rs"]
mod worker_e2e;

// ─── The wiring of this file is itself a gate ────────────────────────────────
//
// Every test above is a `#[path = "main/<name>.rs"] mod <name>;` PAIR, and
// three times during the sv-surface campaign an alphabetical insert landed a
// new `mod` line BETWEEN an existing attribute and its `mod` — which silently
// re-pointed one module at another's file and stranded the other with no path
// at all. Each time a whole test file stopped being compiled into this binary
// while the commit that added it reported green: `meshapp_parcels_e2e` (slice
// 3) never ran once, and `enrich_surface_e2e` (slice 4) never ran either.
//
// A comment asking the next person to be careful is what failed. This is the
// invariant as code (ARCH principle 10): a mis-wired pair, or a file in
// `tests/main/` that nothing declares, fails here.

/// `mod common;` is the one module resolved conventionally
/// (`tests/common/mod.rs`) rather than by an explicit path.
const UNPATHED_MODULES: &[&str] = &["common"];

#[test]
fn every_test_module_is_wired_to_its_own_file() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = std::fs::read_to_string(manifest.join("tests/main.rs"))
        .expect("this test binary's own root module is readable");
    let lines: Vec<&str> = source.lines().collect();

    let mut declared: Vec<String> = Vec::new();
    let mut problems: Vec<String> = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        let Some(name) = line
            .strip_prefix("mod ")
            .and_then(|rest| rest.strip_suffix(';'))
            .filter(|n| {
                !n.is_empty()
                    && n.chars()
                        .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit())
            })
        else {
            continue;
        };
        if UNPATHED_MODULES.contains(&name) {
            continue;
        }
        // Walk up past any `#[cfg(...)]` gates to the `#[path]` attribute.
        let mut j = i;
        let attr = loop {
            if j == 0 {
                break None;
            }
            j -= 1;
            let prev = lines[j].trim();
            if prev.starts_with("#[cfg") {
                continue;
            }
            break Some(prev);
        };
        match attr.and_then(|a| {
            a.strip_prefix("#[path = \"main/")
                .and_then(|r| r.strip_suffix(".rs\"]"))
        }) {
            None => problems.push(format!(
                "line {}: `mod {name};` has no `#[path = \"main/{name}.rs\"]` above it",
                i + 1
            )),
            Some(target) if target != name => problems.push(format!(
                "line {}: `mod {name};` is wired to `main/{target}.rs` — an insert landed \
                 between an attribute and its own `mod` line",
                i + 1
            )),
            Some(_) => declared.push(name.to_string()),
        }
    }

    let dir = manifest.join("tests/main");
    // An entry this gate cannot read is a broken fixture, not a finding:
    // swallowing it would let a file hide from the completeness check
    // below, which is the whole point of the check (ARCH principle 6).
    let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
        .expect("tests/main is readable")
        .map(|e| {
            e.expect("every entry under tests/main is readable")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter_map(|name| name.strip_suffix(".rs").map(|s| s.to_string()))
        .collect();
    on_disk.sort();

    for file in &on_disk {
        if !declared.contains(file) {
            problems.push(format!(
                "`tests/main/{file}.rs` exists but no `mod {file};` declares it — it is not \
                 compiled into this binary and its tests have never run"
            ));
        }
    }
    for name in &declared {
        if !on_disk.contains(name) {
            problems.push(format!(
                "`mod {name};` names `tests/main/{name}.rs`, which does not exist"
            ));
        }
    }

    assert!(
        problems.is_empty(),
        "tests/main.rs module wiring ({} declared, {} files on disk):\n  {}",
        declared.len(),
        on_disk.len(),
        problems.join("\n  ")
    );
}
