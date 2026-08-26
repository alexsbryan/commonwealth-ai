// SPDX-License-Identifier: AGPL-3.0-or-later
//! `adopted` for the `Runtime` — how many processes still commission one, and
//! how many still carry their own recipe for it.
//!
//! `quality/TOPOLOGY.md` §6 says the register cannot see the only number that
//! decides whether an invariant holds: **the count of remaining constructors
//! other than the canonical one.** "`home = minted` is true for `Evidence` and
//! worth nothing, because nine other doors are open." This test is that count,
//! for the noun the whole daemon-convergence program turns on.
//!
//! # Why this counts TWO things, since 2026-08-25
//!
//! It used to count files containing `Runtime::new`. That was the right
//! instrument while every host built its own parts inline. It stopped being
//! one the moment `sovereign-runtime-recipe` landed: `commission` is now the
//! only `Runtime::new` in first-party production code, so the old count would
//! read **1** while four processes still commission a `Runtime` — a green
//! number for a target not met (ARCH §18.4, validate the instrument before the
//! result). Two numbers, because there are two distinct distances to close:
//!
//! 1. **Hosts still carrying their own recipe.** They call `Runtime::new`
//!    directly and assemble the router stack, the tool registry and the
//!    enrichment lane themselves. Measured drift across the three original
//!    copies (2026-08-25): of eleven optional slots, exactly ONE was wired by
//!    all three. Target: zero.
//! 2. **Processes that commission at all**, by either door. §3.5's target is
//!    "DAEMON — the only process that assembles a Runtime". Target: one.
//!
//! The second is the real bar and the first is the cheaper half of it: a host
//! on the shared recipe is a host whose conversion to a surface is a deletion
//! rather than a rewrite.
//!
//! # What this does NOT test
//!
//! That the builders are gone. They are gone, and the *compiler* holds it —
//! `Runtime::new` takes one `RuntimeParts` and there is no other entry point.
//! A source-scanning test asserting that would be a second opinion nobody
//! consults, which is why `launch_assembler_census.rs` was deleted the same
//! week rather than kept beside `MeshAdminWitness`.
//!
//! What no type can hold is *how many hosts still do it*. That distance is a
//! number nothing in the build reports.
//!
//! # Named failing input (ARCH §18.1)
//!
//! Add a fifth process that commissions a `Runtime`, by either door. Nothing
//! about the workspace breaks — that is the whole problem, and it is how the
//! split brain this file records came to exist. This test fails, names the new
//! file, and makes the author say whether they meant to widen the target or to
//! move a host onto the turn protocol.

use std::path::{Path, PathBuf};

/// The ONE file allowed to call `Runtime::new`.
///
/// `sovereign_runtime_recipe::commission` is a one-line wrapper, and the line
/// is the point: with every host reaching the constructor through it, "which
/// processes commission a Runtime" becomes a question about callers of one
/// function rather than a grep for a constructor.
const CANONICAL_CONSTRUCTOR: &str = "sovereign/crates/sovereign-runtime-recipe/src/lib.rs";

/// Hosts that still build a `Runtime` WITHOUT the shared recipe — their own
/// router stack, their own tool registry, their own enrichment lane.
///
/// **Target: empty.** Each entry is a copy of a recipe that has drifted from
/// the others before and will again; the list may SHRINK freely.
const UNSHARED_RECIPES: &[&str] = &[
    // The desktop, in `bootstrap_with_progress`. The hard one: embedded mode
    // runs the daemon in-process, so "talk to the daemon" and "be the daemon"
    // are the same sentence until Phase 6 separates them. It also has the most
    // genuinely-its-own slots — compaction, landscape digests, folder
    // metadata, the sensitivity oracle — which is why it is not simply a
    // struct-update over the recipe's baseline yet.
    "sovereign/crates/sovereign-desktop/src-tauri/src/state.rs",
    // `sovereign-server`. Its extra slot is `corpus_principal` (tenancy), and
    // its whole reason to exist as a separate assembly is the multi-tenant hub
    // shape the daemon does not have.
    "sovereign/crates/sovereign-server/src/main.rs",
];

/// Every process that commissions a `Runtime`, by either door.
///
/// **Target: one — the daemon.** Every other entry is a host waiting to become
/// a surface over the turn protocol (`sovereign_mesh::turn_http`), which since
/// 2026-08-25 exists and is driven end-to-end by
/// `sovereign-mesh/tests/turn_surface.rs`.
const COMMISSIONING_PROCESSES: &[&str] = &[
    // THE TARGET. `sovereign daemon run` — the process §3.5 says should be the
    // only one on this list. It arrived 2026-08-25 (phase 5c); before that the
    // daemon held every ingredient of an answer and served none.
    "sovereign/crates/sovereign-cli-daemon/src/daemon_cmd/mod.rs",
    // `svrn chat`. On the shared recipe since 2026-08-25, so what remains is a
    // surface conversion rather than a rewrite: it already refuses to start
    // without a daemon (`probe_or_bail` against `GET /v1/models`) and its
    // provider is already remote. The blocker is breadth, not depth —
    // `build_session` has ~10 callers beyond chat itself (the chaos harness,
    // the atlas backfills, `govern ask`, the portfolio and proxy asks).
    "sovereign/crates/sovereign-cli-llm/src/chat_cmd/bootstrap.rs",
    "sovereign/crates/sovereign-desktop/src-tauri/src/state.rs",
    "sovereign/crates/sovereign-server/src/main.rs",
];

fn repo_root() -> PathBuf {
    // tests/ -> sovereign-core -> crates -> sovereign -> repo root
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("repo root is four levels above sovereign-core")
        .to_path_buf()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if p.is_dir() {
            // `target` is build output; `tests` are integration-test crates,
            // which are consumers rather than hosts; and any dot-directory is
            // tooling — this repo carries a vendored crate registry at
            // `.cargo-container/`, which supplies ~40 unrelated `Runtime::new`
            // hits (datafusion benches, aws-smithy) if it is walked.
            if matches!(name, "target" | "node_modules" | "vendor" | "tests")
                || name.starts_with('.')
            {
                continue;
            }
            rust_files(&p, out);
        } else if name.ends_with(".rs") {
            out.push(p);
        }
    }
}

/// Production CODE only: everything from the first `#[cfg(test)]` onward is
/// dropped, files that are wholly test modules are skipped by their path, and
/// comments are stripped.
///
/// Comments are stripped for the reason `daemon_variant_census` learned by
/// sabotage on 2026-08-25 — prose about an invariant satisfying the check FOR
/// the invariant. This scanner hit it immediately: `sovereign-contracts/src/
/// types/mod.rs:37` says "Passed to `Runtime::new()`", and a contracts crate
/// that commissions nothing was reported as a fourth host.
fn production_source(src: &str) -> String {
    // Truncate at the inline test MODULE, not at any `#[cfg(test)]`.
    // `sovereign-server/src/main.rs:25` is `#[cfg(test)] mod http_tests;` — a
    // declaration pulling in a separate file, 700 lines ABOVE the server's
    // real commissioning site. Truncating there dropped the whole file and
    // took the scanner below its own instrument check, which is how this was
    // caught rather than shipped as a silently narrow scan.
    let cut = src
        .match_indices("#[cfg(test)]")
        .find(|(i, _)| src[*i..].split_once('\n').is_some_and(|(_, r)| r.contains("mod tests")))
        .map(|(i, _)| i);
    let code = match cut {
        Some(i) => &src[..i],
        None => src,
    };
    code.lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Scan every first-party production file for one token, returning repo-
/// relative paths.
fn files_containing(matches: impl Fn(&str) -> usize) -> Vec<String> {
    let root = repo_root();
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    let mut hits = Vec::new();
    for f in files {
        // `http_tests.rs` is a `#[cfg(test)]` module included from a binary's
        // `main.rs`; `production_source` cannot see that from inside the file.
        if f.file_name().and_then(|n| n.to_str()) == Some("http_tests.rs") {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(&f) else {
            continue;
        };
        if matches(production_source(&src).as_str()) > 0 {
            let rel = f.strip_prefix(&root).unwrap_or(&f);
            hits.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    hits.sort();
    hits
}

/// Files calling `Runtime::new` on a production path.
fn constructor_sites() -> Vec<String> {
    files_containing(|prod| {
        // Two different nouns share this suffix and neither is ours:
        // `tokio::runtime::Runtime::new()`, and `sovereign-server`'s
        // `TenantRuntime::new(..)` — which takes an `Arc<Runtime>` that some
        // other site already commissioned, so counting it would double-count
        // the server. Require a real token boundary before the match.
        prod.match_indices("Runtime::new(")
            .filter(|(i, _)| {
                let before = &prod[..*i];
                if before.ends_with("tokio::runtime::") {
                    return false;
                }
                // A preceding identifier character means this is `XRuntime`,
                // not `Runtime`.
                !before
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_')
            })
            .count()
    })
}

/// Files calling the shared recipe's `commission`.
fn recipe_callers() -> Vec<String> {
    files_containing(|prod| prod.matches("sovereign_runtime_recipe::commission(").count())
}

/// Every process that commissions a `Runtime`, by either door.
fn commissioning_processes() -> Vec<String> {
    let mut all: Vec<String> = constructor_sites()
        .into_iter()
        .filter(|f| f != CANONICAL_CONSTRUCTOR)
        .chain(recipe_callers())
        .collect();
    all.sort();
    all.dedup();
    all
}

/// Instrument check (ARCH §18.4): a scanner that finds nothing proves nothing.
///
/// Both halves are checked, because they fail differently — the constructor
/// scan breaks if the walk or the token filter breaks, and the recipe scan
/// breaks silently if a host switches to a bare `use ... commission` import,
/// which would drop it off the census while it still commissions.
#[test]
fn the_scanner_finds_both_kinds_of_commissioning_site() {
    let ctors = constructor_sites();
    assert!(
        ctors.contains(&CANONICAL_CONSTRUCTOR.to_string()),
        "the scanner cannot see `Runtime::new` in the recipe crate itself \
         ({CANONICAL_CONSTRUCTOR}) — the walk or the match is broken; fix it \
         before reading the bars below. Found: {ctors:?}"
    );
    let callers = recipe_callers();
    assert!(
        !callers.is_empty(),
        "the scanner found no caller of `sovereign_runtime_recipe::commission` \
         — either every host regressed to its own recipe, or a host is calling \
         it through a bare import the scanner cannot see. Both are worth \
         stopping for."
    );
}

/// Bar 1 — how many hosts still carry their own recipe.
#[test]
fn only_the_recipe_calls_runtime_new() {
    let found: Vec<String> = constructor_sites()
        .into_iter()
        .filter(|f| f != CANONICAL_CONSTRUCTOR)
        .collect();
    let expected: Vec<String> = UNSHARED_RECIPES.iter().map(|s| s.to_string()).collect();

    let unexpected: Vec<&String> = found.iter().filter(|f| !expected.contains(f)).collect();
    assert!(
        unexpected.is_empty(),
        "a host builds a `Runtime` without the shared recipe:\n  {}\n\n\
         `sovereign_runtime_recipe::common_parts` + `::commission` is the one \
         recipe; a host with genuinely extra slots overrides them with \
         struct-update syntax on the returned parts, which is what the desktop \
         and the server will do when they land there. If this really is a new \
         kind of assembly, add it to UNSHARED_RECIPES with the reason.",
        unexpected
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n  ")
    );

    let converted: Vec<&String> = expected.iter().filter(|e| !found.contains(e)).collect();
    assert!(
        converted.is_empty(),
        "these hosts no longer carry their own recipe:\n  {}\n\n\
         That is PROGRESS. Remove them from UNSHARED_RECIPES and record the new \
         count in `quality/TOPOLOGY.md` §10. The target is zero.",
        converted
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}

/// Bar 2 — THE COUNT. §6's `adopted`, for `Runtime`.
#[test]
fn only_the_declared_processes_commission_a_runtime() {
    let found = commissioning_processes();
    let expected: Vec<String> = COMMISSIONING_PROCESSES
        .iter()
        .map(|s| s.to_string())
        .collect();

    let unexpected: Vec<&String> = found.iter().filter(|f| !expected.contains(f)).collect();
    assert!(
        unexpected.is_empty(),
        "a process that is not on the list commissions a `Runtime`:\n  {}\n\n\
         §3.5's target is that the DAEMON is the only process that assembles \
         one. Every other entry on that list is a host waiting to become a \
         surface over `sovereign_mesh::turn_http`, so the list may SHRINK \
         freely — it must not grow without someone saying why. If this is a \
         genuinely new process, add it with a comment explaining what it does \
         that the turn protocol cannot.",
        unexpected
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n  ")
    );

    let converted: Vec<&String> = expected.iter().filter(|e| !found.contains(e)).collect();
    assert!(
        converted.is_empty(),
        "these processes no longer commission a `Runtime`:\n  {}\n\n\
         That is PROGRESS, not a failure — Phase 6 converting a host to a \
         surface is exactly what this looks like. Remove them from \
         COMMISSIONING_PROCESSES and record the new count in \
         `quality/TOPOLOGY.md` §10. The target is one: the daemon.",
        converted
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}
