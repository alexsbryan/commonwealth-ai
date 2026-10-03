// SPDX-License-Identifier: AGPL-3.0-or-later
//! sv-surface D8: the three surfaces that WROTE through a second object —
//! a corpus's governance oplog, the MCP server config, and a recipe-author
//! project — hold no such object.
//!
//! These three did not fit an existing family file: `atlas_meshapp_wire_census`
//! pins atlas/meshapp READERS, `local_corpus_wire_census` the local-corpus
//! registry, `notes_sink_wire_census` the note and insight stores. What the
//! three below share is the D8 rung, so they share a file — and each test
//! stands alone, so a later rung can lift one into its own family without
//! disturbing the others.
//!
//! # The state this makes unrepresentable
//!
//! **A second writer.** Every one of these was a WRITE path, which is what
//! separates this file from the read-side censuses:
//!
//!   * `governance_commands.rs` opened `Oplog::<GovernanceOpKind>::new(dir)`
//!     over `<data_dir>/indexes/<corpus>/atlas` and appended adjudication
//!     ops under a process-wide `Mutex` — a lock that closes the race only
//!     for THIS process, over an append the engine performs without an
//!     advisory lock. The CLI `svrn govern` verbs append to the same file.
//!     On an attached boot the desktop's `data_dir` need not be the
//!     daemon's, so a decision could land in a log `govern ask` never reads.
//!   * `commands/mcp_servers.rs` read `SetupConfig::load()` — this process's
//!     `config.toml` — and this process's secret dir, while the daemon
//!     connected servers out of its own.
//!   * `recipe_author_commands.rs` composed a `RecipeProject` over the
//!     desktop's `notes.db` + `features.db` handles and wrote the artifact
//!     TOML into the desktop's own recipes dir — where an attached daemon
//!     never looks, so the corpus would enrich down the wrong pipeline.
//!
//! # What is NOT needled, and why (ARCH §18.1 — name the failing input)
//!
//!   * `governance_export_write`'s `std::fs::write` — the destination is a
//!     path the USER picked in a save dialog and the content is composed in
//!     the webview. There is nothing for a route to serve in its place.
//!   * `mcp_add_server` / `mcp_remove_server`'s `SetupConfig::load()` +
//!     `save()`. Measured: `SetupConfig::save()` has zero call sites in
//!     `sovereign-mesh`; every config writer in this workspace is CLI-side.
//!     A write sent to a daemon that cannot persist it is a silent no-op,
//!     which is worse than the second reader. That is why the MCP needles
//!     below are the READ spellings and the connect-flag fabrication, not
//!     `SetupConfig` as such.
//!   * `dev_flags::force_first_run()` in the project list — it replays THIS
//!     app's onboarding surface, and the daemon has no onboarding.
//!
//! # Calibration
//!
//! PRODUCTION lines only, and only above `#[cfg(test)]`; whole-line comments
//! are dropped, for the reason `reading_wire_types_census` records — a doc
//! comment that explains why a spelling is gone has to NAME it to do so, and
//! prose about a needle is documentation, not a second path. This file's own
//! header is the proof: it says `Oplog::`, `SetupConfig::load` and
//! `RecipeProject::load` freely.
//!
//! Watched to fail: see `quality/twin-plants.toml`, families
//! `governance-oplog`, `mcp-config-wire` and `recipe-project-composition`.

use std::path::Path;

fn production_source(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    let src =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let prod = match src.find("\n#[cfg(test)]") {
        Some(i) => &src[..i],
        None => &src[..],
    };
    prod.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every spelling by which the panel used to reach a corpus's atlas dir
/// directly. The oplog ones are the writes; the reads beside them are what
/// composed the view the writes were decided against, and crossing one
/// without the other would adjudicate a conflict the panel is not showing.
const LOCAL_GOVERNANCE_SPELLINGS: &[&str] = &[
    "Oplog::<GovernanceOpKind>",
    "GovernanceView::from_atlas_dir(",
    "read_atlas_atoms(",
    "migrate_atlas_ids(",
    "section_titles(",
    "Op::new(",
];

#[test]
fn the_governance_panel_holds_no_oplog() {
    let code = production_source("src/governance_commands.rs");
    for spelling in LOCAL_GOVERNANCE_SPELLINGS {
        assert!(
            !code.contains(spelling),
            "sv-surface D8: governance_commands.rs reaches a corpus's atlas \
             dir with `{spelling}` again. The eight governance commands are \
             one TurnClient call each onto /internal/governance/{{corpus}} — \
             the daemon appends to the SAME governance_oplog.jsonl the CLI \
             `svrn govern` verbs write, and it is the only writer an \
             attached boot shares with them. A local append is a second \
             writer to a file whose append takes no advisory lock."
        );
    }
    // The staleness banner and the recipe template moved down with the
    // read; re-deriving either here is the §10.6 twin.
    for spelling in ["chapters.json", "GOVERNANCE_ONTOLOGY_GUIDANCE"] {
        assert!(
            !code.contains(spelling),
            "sv-surface D8/§10.6: governance_commands.rs re-derives \
             `{spelling}`. The route owns the staleness heuristic and the \
             recipe template — a second copy answers a question the panel \
             already asked the host."
        );
    }
    assert!(
        code.contains("governance_export_write"),
        "governance_export_write is the ONE governance command that cannot \
         cross (a user-picked save path, frontend-composed content). If it \
         moved, move this census's note about it too — an absence nobody \
         records is how a stay becomes a mystery."
    );
}

#[test]
fn the_mcp_pane_reads_the_daemons_config() {
    let code = production_source("src/commands/mcp_servers.rs");
    assert!(
        code.contains(".mcp_servers::<sovereign_contracts::daemon_wire::McpServersResponse>()"),
        "sv-surface D8: commands/mcp_servers.rs no longer lists servers over \
         GET /v1/mcp/servers. The pane used to render THIS process's \
         config.toml and THIS process's secret dir while the daemon \
         connected servers out of its own — two answers to one question."
    );
    for spelling in [
        "mgr.server_statuses(",
        "state.mcp_servers",
        "connect_http_mcp_server(",
        "secret_store::write_token(",
        "secret_store::has_token(",
    ] {
        assert!(
            !code.contains(spelling),
            "sv-surface D8: commands/mcp_servers.rs calls `{spelling}` again \
             — a local probe, status or secret read for a server the DAEMON \
             is the one to dial and to hold the token for. On an attached \
             boot this process's network position and secret dir are not the \
             daemon's."
        );
    }
    // ARCH §18.3: the host serves live_tool_count and deliberately serves NO
    // connect flag, stating the absence in mount.reason on every response.
    for fabrication in [
        "live_tool_count > 0",
        "connected: Some(",
        "s.live_tool_count > 0",
    ] {
        assert!(
            !code.contains(fabrication),
            "sv-surface D8 / ARCH §18.3: commands/mcp_servers.rs folds a \
             connect flag out of `{fabrication}`. The host keeps no \
             connection manager and says so in `mount.reason`; a green dot \
             derived from a tool count is the fact it declined to invent. \
             `connected` is None until something actually dials."
        );
    }
}

/// Every spelling by which the workspace used to compose a project itself.
const LOCAL_PROJECT_SPELLINGS: &[&str] = &[
    "RecipeProject::load(",
    "RecipeProject::new_with_kind(",
    "RecipeProjectStore",
    "NoteStoreRecipeNotes",
    "read_notes_scoped(",
    "list_checkpoints(",
    "do_restore_checkpoint(",
    "read_summary(",
    "write_summary(",
    "situated_context::render(",
    "local_recipes_dir(",
    "local_workflows_dir(",
    "toml.part",
];

#[test]
fn the_recipe_workspace_composes_no_project() {
    let code = production_source("src/recipe_author_commands.rs");
    for spelling in LOCAL_PROJECT_SPELLINGS {
        assert!(
            !code.contains(spelling),
            "sv-surface D8: recipe_author_commands.rs composes the project \
             itself with `{spelling}`. All seven commands are one TurnClient \
             call each onto /v1/recipe-projects — the daemon composes over \
             the stores it opened and writes the artifact under ITS data \
             root, which is the only place enrichment looks."
        );
    }
    // §18.1: the host reports "I could not judge this" as a distinct fact.
    // Collapsing it into a PASS is the one thing this mapping may not do.
    assert!(
        !code.contains("ok: true"),
        "sv-surface D8 / ARCH §18.1: recipe_author_commands.rs constructs a \
         PASSING validation report. The host reports an unjudged artifact as \
         `validation: null` + `validation_unavailable`, and the only honest \
         mapping into this IPC contract carries that sentence as the \
         message. A green pill over an artifact nobody parsed is the false \
         verdict the four-verdict rule exists to prevent."
    );
    assert!(
        code.contains("unjudged_report("),
        "sv-surface D8: the could-not-judge mapping is gone from \
         recipe_author_commands.rs. The route's `validation` is an Option \
         and this command's is not; something has to carry \
         `validation_unavailable` into the card, and dropping it renders an \
         unjudged workflow as though it had been judged."
    );
}
