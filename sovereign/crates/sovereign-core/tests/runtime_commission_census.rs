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
///
/// # The tool-registry blocker is CLOSED (2026-08-26, phase 7b)
///
/// What stalled this list was never effort or plumbing — `common_parts`
/// already anticipated both hosts by name. It was that **the recipe's tool
/// registry was not a superset of theirs**: counted by type name,
/// `sovereign-server` registered 31 and the recipe 11, and the sets were not
/// nested in either direction (the server had no `knowledge_lookup` and no
/// `attached_document_search`; the recipe had no code intel, no notes, no
/// recipe authoring). Adopting the recipe as it stood would have DELETED ~20
/// tools from the hub, so the open question read as policy — "which of those
/// twenty belong to every host?" — and on a multi-tenant hub, code intel over
/// another tenant's workspace looked like a security decision.
///
/// It was a structural defect wearing a policy costume. The recipe OWNED the
/// list, so no host could add a family without editing a file every host
/// shares — open/closed stated as a bug. `sovereign_contracts::tool_bundle`
/// inverts it: a host composes `Vec<Box<dyn ToolBundle>>` and the recipe
/// folds it, naming no tool (falsifier:
/// `sovereign-runtime-recipe/tests/recipe_names_no_tool.rs`, watched to fail).
///
/// The capability question answers itself under the seam. A bundle is
/// constructed FROM the collaborators its tools need — `CodeIntelTools` from
/// a `ScipGraphHandle`, `NotesTools` from an open `NoteStore` — so a host can
/// only offer code intel over an index it owns. A tenant-scoped host has no
/// other tenant's handle to compose from, which makes the hazard
/// unrepresentable rather than merely disallowed (ARCH §7).
///
/// # What is left, therefore
///
/// Per-host wiring, not tools. Each remaining entry names its own.
const UNSHARED_RECIPES: &[&str] = &[
    // The desktop ADOPTED on 2026-08-26 and is no longer on this list. What
    // unblocked it was `LaneWarmth` reaching `lane.gliner`: the recipe loaded
    // the extractor eagerly while the desktop loaded it lazily, and the fix
    // was not to pick a winner but to notice that ONE declaration was being
    // read two ways — `sovereign daemon run` already said `Deferred` and this
    // one lane member ignored it (`load_gliner`, and its `warmth_census`
    // falsifier). The desktop's own slots — compaction, landscape digests,
    // folder metadata, the sensitivity oracle, the Tauri routing sink — are
    // struct-update overrides on `CommonParts::parts`, which is what that
    // field is for, and there are six of them in a 1,659-line function that
    // used to hold the whole recipe.
    // `sovereign-server` ADOPTED on 2026-08-26, the same day, and this list is
    // now EMPTY. Its 31 tools became the bundles it composes — `CodeIntelTools`
    // over the SCIP handle it opens, `NotesTools` over its `notes.db`,
    // `RecipeAuthoringTools`, `WorkflowAuthoringTools`, `ComputeTools`,
    // `DocumentOperations` — so adoption stopped costing it a capability, which
    // is the whole reason the seam was built. Its extra Runtime slots are
    // `corpus_principal` (tenancy), `landscape_digests` and the narration sink,
    // all struct-update overrides.
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

/// The turn-execution methods. Calling one of these IS executing a turn.
const TURN_ENTRY_POINTS: &[&str] = &[
    ".handle_message_stream(",
    ".handle_message_stream_as(",
    ".handle_message_stream_naked(",
    ".handle_message_any(",
    ".handle_message(",
    ".handle_turn(",
];

/// Files outside the runtime that execute a turn themselves.
///
/// # Why this bar exists, and why `COMMISSIONING_PROCESSES` could not serve
///
/// ARCH §18.4 — validate the instrument before the result. The commissioning
/// census above counts FILES THAT BUILD A RUNTIME, and phase 6 cannot move it:
/// `chat_cmd/bootstrap.rs` is one entry, and it stays on the list while ANY of
/// its thirteen callers still needs an in-process assembly — including the
/// atlas backfills and the chaos harness, which are not turns and never
/// become surfaces. Converting the two real chat surfaces moved that number
/// by zero. A number that cannot move under real progress is not a
/// measurement of it.
///
/// This counts what §3.5 actually cares about: **who re-implements driving a
/// turn**.
///
/// The bar is ONE DRIVER, not one process — a correction made after the first
/// pass of this list called three hosts blocked that were not. An in-process
/// host reaches the driver through a [`TurnSink`](sovereign_core::runtime::TurnSink);
/// an out-of-process one reaches it through `sovereign-turn-client`. Both go
/// through `serve_turn`, which is the property that matters: one answer to
/// "which handler runs, what happens on a turn that cannot stream, and how is
/// the result projected".
///
/// Requiring every host to speak HTTP would have been a different and worse
/// bar. `svrn govern ask` needs a corpus engine to render its sources footer,
/// which is not a turn concern and does not belong on the turn wire; it can
/// hold one and still not own a turn loop. Conflating those two is what made
/// the first version of this list read as blocked.
///
/// **Target: empty.** The list may SHRINK freely; adding to it means a new
/// host learned to run a turn in-process, which is the thing phase 6 exists
/// to stop.
///
/// Each entry is annotated with what it would take to remove it, because
/// "still on the list" and "cannot come off the list yet" are different
/// states and only one of them is work.
const TURN_EXECUTION_SITES: &[&str] = &[
    // `govern_cmd/ask.rs`, `portfolio_cmd/ask.rs` and `proxy_cmd/ask.rs` were
    // here, called blocked on two capabilities the turn protocol lacked.
    // One was real and is now closed — a caller-pinned intent is
    // `TurnRequest::Message.intent`. The other was a category error: they
    // read corpus indexes for their own sources footer, which never made
    // them turn hosts. All three drive `serve_turn` through a sink now, and
    // converting them deleted the SAME double-write from each.
    // ── Measurement harnesses that run the NON-STREAMING pipeline ──
    //
    // The only remaining entries, and the reason is §18.4 rather than effort.
    // `collect_turn` drives `serve_turn`, which is the STREAMING pipeline
    // with a collecting sink. These call `handle_message` / `handle_turn`,
    // which is a different pipeline with different synthesis. Converting them
    // is a re-baseline of every bank they score, so it needs a pre-registered
    // expected direction of movement and a bench run (§18.6) — not a build.
    //
    // The three harnesses that were ALREADY on the streaming path
    // (`eval_cmd/runner.rs`, `eval_cmd/runner_threads.rs`,
    // `bench_cmd/live_runner.rs`) converted for exactly that reason: for them
    // `collect_turn` is instrument-neutral, so it was a deletion of a
    // hand-rolled drain rather than a change to what is measured.
    "sovereign/crates/sovereign-cli-llm/src/bench_cmd/book_report.rs",
    // PARTIALLY converted, and on the list because of what is left. Its main
    // scoring path drove `handle_message_stream` and moved to `collect_turn`
    // (instrument-neutral). Its DOCUMENT-SESSION path still calls
    // `handle_turn` directly, and that one is not neutral: the question
    // carries no `[Document attached: ` prefix — the document is attached via
    // a session row — so `serve_turn` would route it through the streaming
    // classifier instead of the document path the bench means to measure.
    //
    // This test caught that; the file had been removed from this list on the
    // assumption that converting the main path converted the file. A census
    // that only counts what you remembered to look at is not one.
    "sovereign/crates/sovereign-cli-llm/src/bench_cmd/live_runner.rs",
    "sovereign/crates/sovereign-cli-llm/src/voice_eval/runner.rs",
    "sovereign/crates/sovereign-cli-llm/src/inner_chaos/recall.rs",
    "sovereign/crates/sovereign-cli-llm/src/inner_chaos/replay.rs",
    "sovereign/crates/sovereign-cli-llm/src/inner_chaos/runner.rs",
    "sovereign/crates/sovereign-cli-llm/src/inner_chaos/synth.rs",
    // `sovereign-desktop`'s `commands/chat.rs` and `commands/document_asset.rs`
    // were here, and the reasons given were wrong twice over.
    //
    // First the GLiNER fork was cited; that blocks the desktop adopting the
    // shared RECIPE (phase 5a) and has nothing to do with driving a turn —
    // since 5c the desktop hands its OWN `Runtime` to its in-process daemon.
    // Then a structural mismatch was cited: `send_message_stream` returns the
    // message id SYNCHRONOUSLY, before a token exists, and `serve_turn` owns
    // handle acquisition. That one was real and is now solved rather than
    // worked around — `TurnSink::on_turn_started` fires at acquisition, which
    // is the same moment the old code learned the id, so the UI places its
    // placeholder exactly as early as before.
    //
    // Nothing moved in TypeScript. The sink emits the same three events with
    // the same payloads, and reads the persisted metadata blob in-process —
    // `serve_turn` projects typed values for callers across a socket, and a
    // caller that owns the store is not one of those.
    //
    // Two behaviours became everyone's instead of the desktop's alone: the
    // graceful guards (oversize paste, contentless message) are answered as a
    // turn rather than errored, which only this host used to get right; and
    // `send_message` / `document_ask` now run the same pipeline as
    // `send_message_stream`, where the answer used to depend on which door
    // the user came through.
    // `sovereign-server/src/routes.rs` and `tenant.rs` were here. Both are
    // gone: the REST route drives `collect_turn` — the same driver its own
    // WebSocket route uses — and `TenantRuntime`'s two turn wrappers were
    // deleted outright, because what that type owns is TENANCY. Running a
    // turn only looked like its job because there was nowhere else to put the
    // call. `Runtime::handle_message_any` went with them.
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
        .find(|(i, _)| {
            src[*i..]
                .split_once('\n')
                .is_some_and(|(_, r)| r.contains("mod tests"))
        })
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
    files_containing(|prod| {
        prod.matches("sovereign_runtime_recipe::commission(")
            .count()
    })
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

/// Files outside `sovereign-core`'s runtime that execute a turn.
fn turn_execution_sites() -> Vec<String> {
    files_containing(|prod| {
        TURN_ENTRY_POINTS
            .iter()
            .map(|entry| prod.matches(entry).count())
            .sum()
    })
    .into_iter()
    // The runtime owns these methods and `serve_turn` is the sanctioned
    // caller — that is the whole point of the bar, not a violation of it.
    .filter(|f| !f.starts_with("sovereign/crates/sovereign-core/src/runtime"))
    .collect()
}

/// The phase 6 bar: a turn runs where the `Runtime` lives, and nowhere else.
///
/// Watched to fail before it was kept (ARCH §18.1): adding a
/// `runtime.handle_message(..)` call back into the converted
/// `chat_cmd/ask.rs` fails this test naming that exact file, and telling the
/// reader the two ways out — convert it, or add it to the list with the
/// reason it cannot be.
#[test]
fn turn_execution_happens_where_the_runtime_lives() {
    let found = turn_execution_sites();
    let expected: Vec<String> = TURN_EXECUTION_SITES.iter().map(|s| s.to_string()).collect();

    let new_hosts: Vec<&String> = found.iter().filter(|f| !expected.contains(f)).collect();
    assert!(
        new_hosts.is_empty(),
        "a new host learned to execute a turn in-process — phase 6 moves the other way.\n\
         Convert it to `sovereign_turn_client::TurnClient`, or add it to \
         TURN_EXECUTION_SITES with the reason it cannot be:\n  {new_hosts:#?}"
    );

    let converted: Vec<&String> = expected.iter().filter(|e| !found.contains(e)).collect();
    assert!(
        converted.is_empty(),
        "these no longer execute a turn — delete them from TURN_EXECUTION_SITES \
         so the count keeps meaning something (ARCH §18.4):\n  {converted:#?}"
    );
}
