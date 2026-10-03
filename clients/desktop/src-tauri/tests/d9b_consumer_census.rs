// SPDX-License-Identifier: AGPL-3.0-or-later
//! sv-surface D9b's CONSUMER census — the half the attach-construction
//! floor is blind to.
//!
//! # What the floor could not see
//!
//! `attach_construction_census.rs` counts CONSTRUCTIONS: how many deciders
//! the boot spine assembles that an attached desktop has no business
//! assembling. 65b92cd15 measured the other side and found the floor is a
//! lagging indicator — 49 reported (55 on this tree's re-count, below) READS
//! of those constructions across the command surface, none behind a mode
//! fork, every one live in attach because the spine builds every needle in
//! both modes. Deleting a construction while a read of it survives does not
//! purify the boot; it deletes an answer.
//!
//! D9b consumed 22 of them onto the routes 2a9a9e91e built. This census is
//! what stops them coming back: for each repointed command it pins the
//! LOCAL primitive that is gone, and the family-client call that replaced
//! it. A primitive reappearing is not a style regression — it is a second
//! decider for a question the daemon already answers, and in attach it is
//! the wrong answer, silently (ARCH §10.6, §18.3).
//!
//! # The re-count, and a correction to the recorded one
//!
//! Same method as 65b92cd15 (top-level `#[cfg(test)]` modules cut, comment
//! lines dropped, `require_runtime!`/`require_store!`/`state.<needle>.read()`
//! plus the two typed accessors), re-run on this tree BEFORE the rung:
//! **55 reads across 13 files**, not 49. Eleven of the thirteen per-file
//! numbers reproduce exactly; the two that do not are `chat.rs` (8, recorded
//! 3) and `recipe_testing.rs` (3, recorded 2). Re-running the counter at
//! 65b92cd15 itself yields 8 and 3 there too, so the recorded totals were an
//! undercount at the time, not drift since — the instrument is sound and the
//! two figures in the header it wrote are not (ARCH §18.4: validate the
//! instrument before the result).
//!
//! After D9b: **33 reads across 10 files**.
//!
//! # thin-desktop R2 (2026-09-12) — the store's readers, and the macro
//!
//! `require_store!` is GONE, which takes the census's own needle list with
//! it: there is no `state.store` slot, no `state.sqlite_store` slot and no
//! `AppState::store()` accessor left to read. Five consumers crossed —
//! `get_conversation`, `search_web`, `explore_insights`,
//! `get_chat_activity` and `lc_reenrich_note`'s correction write — and a
//! sixth (`state.rs`'s boot readiness sweep) lost its store write as
//! provably inert rather than repointed; the row below cites the reader that
//! makes it so. Each is pinned in both directions here: the retired
//! primitive, and the family-client call that answers it now.
//!
//! # Watched to fail
//!
//! Each test below was planted against and watched red at landing — the
//! local primitive re-added to the repointed command, the census naming the
//! file and the primitive — then restored. Registered in
//! `quality/twin-plants.toml` as `sv-desktop-d9b-consumer`.

use std::path::{Path, PathBuf};

fn manifest_src(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

/// Production source only: top-level `#[cfg(test)]` modules cut, comment
/// lines dropped. A doc comment that MENTIONS a retired primitive is
/// documentation — several of the repointed commands explain what they no
/// longer do, and a census that fired on its own explanation would be the
/// §18.1 smell (a check with no failing input anyone would write).
fn production_source(relative: &str) -> String {
    let path = manifest_src(relative);
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let lines: Vec<&str> = raw.lines().collect();
    let mut keep: Vec<&str> = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        if lines[i].starts_with("#[cfg(test)]") {
            let mut j = i + 1;
            while j < lines.len() && !lines[j].starts_with('}') {
                j += 1;
            }
            i = j + 1;
            continue;
        }
        let trimmed = lines[i].trim_start();
        if !(trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*')) {
            keep.push(lines[i]);
        }
        i += 1;
    }
    keep.join("\n")
}

/// One retired local primitive: the call the repointed command used to make
/// against a construction this process should not be holding in attach.
struct Retired {
    /// The command-surface file it was retired FROM. Scoped per file on
    /// purpose: `open_index_for_corpus` is gone from `budget.rs` and is
    /// still legitimately in `chat.rs`'s passage-context builder, and a
    /// workspace-wide needle would either miss the first or false-fire on
    /// the second.
    file: &'static str,
    /// The source text that must not appear in production code there.
    hay: &'static str,
    /// The route that answers this question now.
    route: &'static str,
}

const RETIRED: &[Retired] = &[
    // ── conversation.rs ──────────────────────────────────────
    Retired {
        file: "src/commands/conversation.rs",
        hay: "insert_empty_conversation",
        route: "POST /v1/conversations",
    },
    Retired {
        file: "src/commands/conversation.rs",
        hay: "require_runtime!",
        route: "POST /v1/conversations/{id}/end",
    },
    Retired {
        file: "src/commands/conversation.rs",
        hay: "state::rebuild_runtime",
        route: "PUT /v1/skills/{id}/active",
    },
    Retired {
        file: "src/commands/conversation.rs",
        hay: "project_message_metadata(&msg.metadata)",
        route: "GET /v1/conversations/{id} (the daemon runs the projection)",
    },
    // thin-desktop R2 (2026-09-12): `get_conversation` was the LAST
    // `require_store!` caller in the crate, and the macro is gone with it.
    // D9b left it local naming the blocker exactly — the route dropped
    // `metadata` and `enabled_corpora`. `metadata` crossed at svt-3;
    // `enabled_corpora` is what R2 added to `ConversationResponse`.
    Retired {
        file: "src/commands/conversation.rs",
        hay: "require_store!",
        route: "GET /v1/conversations/{id} (which grew `enabled_corpora` for it)",
    },
    // sv-surface correctness wave — the two wrong-store WRITES. A read that
    // came back from the wrong store rendered a stale sidebar; these two
    // silently discarded the user's action, which is worse: the rename
    // reverted on the next list, and every corpus chip was written where
    // retrieval never looks.
    Retired {
        file: "src/commands/conversation.rs",
        hay: ".update_conversation_title(",
        route: "PATCH /v1/conversations/{id} (trim, empty-refusal and the 200-char clamp moved with it)",
    },
    Retired {
        file: "src/commands/conversation.rs",
        hay: ".set_conversation_enabled_corpora(",
        route: "PUT /v1/conversations/{id}/enabled-corpora (which also VALIDATES — the local write never did)",
    },
    // The two SCOPED LISTINGS. The needle is each command's own
    // store-unavailable sentence, not the method name: the method name is
    // now the CLIENT call in the same file, and a census that fired on its
    // own replacement would be the §18.1 smell.
    Retired {
        file: "src/commands/conversation.rs",
        hay: "list_conversations: sqlite store unavailable",
        route: "GET /v1/conversations?skill_id= (empty = the default surface)",
    },
    Retired {
        file: "src/commands/conversation.rs",
        hay: "notebook_conversations: sqlite store unavailable",
        route: "GET /v1/conversations?corpus_id=",
    },
    // ── document_asset.rs ────────────────────────────────────
    Retired {
        file: "src/commands/document_asset.rs",
        hay: ".list_document_assets()",
        route: "GET /v1/documents",
    },
    Retired {
        file: "src/commands/document_asset.rs",
        hay: ".save_document_asset(",
        route: "POST /v1/documents/legacy/promote",
    },
    Retired {
        file: "src/commands/document_asset.rs",
        hay: ".list_sources()",
        route: "GET /v1/documents/legacy",
    },
    Retired {
        file: "src/commands/document_asset.rs",
        hay: ".get_chunks_by_source(",
        route: "GET /v1/documents/legacy + POST /v1/documents/legacy/promote",
    },
    // ── corpus.rs ────────────────────────────────────────────
    Retired {
        file: "src/commands/corpus.rs",
        hay: ".builtin_corpora()",
        route: "GET /internal/corpus/catalog",
    },
    Retired {
        file: "src/commands/corpus.rs",
        hay: ".get_vector_index_ready(",
        route: "GET /internal/corpus/catalog (the two-source rule is the daemon's)",
    },
    Retired {
        file: "src/commands/corpus.rs",
        hay: "tiers_for(",
        route: "GET /internal/corpus/catalog (`tiers` rides the row)",
    },
    Retired {
        file: "src/commands/corpus.rs",
        hay: "collection_parent_of(",
        route: "GET /internal/corpus/notebooks (the prefix walk moved down)",
    },
    // ── budget.rs ────────────────────────────────────────────
    Retired {
        file: "src/commands/budget.rs",
        hay: ".open_index_for_corpus(",
        route: "GET /internal/corpus/{corpus}/health",
    },
    Retired {
        file: "src/commands/budget.rs",
        hay: ".load_field_skeleton()",
        route: "GET /internal/corpus/{corpus}/health",
    },
    Retired {
        file: "src/commands/budget.rs",
        hay: "reprocess_skeleton_failures(",
        route: "POST /internal/corpus/{corpus}/retry-enrichment",
    },
    // ── corpus_install.rs ────────────────────────────────────
    Retired {
        file: "src/commands/corpus_install.rs",
        hay: ".diagnose_indexes()",
        route: "GET /internal/corpus/diagnose",
    },
    // ── recipe_testing.rs ────────────────────────────────────
    Retired {
        file: "src/commands/recipe_testing.rs",
        hay: "CorpusSpec::Builtin(",
        route: "POST /internal/corpus/install (one install decider)",
    },
    Retired {
        file: "src/commands/recipe_testing.rs",
        hay: "tiers_for(",
        route: "GET /internal/corpus/catalog",
    },
    // ── config_setup.rs ──────────────────────────────────────
    Retired {
        file: "src/commands/config_setup.rs",
        hay: "state.runtime.read()",
        route: "GET /v1/ready",
    },
    // ── models.rs ────────────────────────────────────────────
    // The SEARCH stays in the app on egress custody; the two message
    // WRITES do not. `state.store()` was this file's whole reason to hold
    // a store handle, and the accessor it named is deleted.
    Retired {
        file: "src/commands/models.rs",
        hay: "state.store()",
        route: "POST /v1/conversations/{id}/messages/record",
    },
    Retired {
        file: "src/commands/models.rs",
        hay: ".save_message(",
        route: "POST /v1/conversations/{id}/messages/record",
    },
    // ── contribution.rs ──────────────────────────────────────
    Retired {
        file: "src/commands/contribution.rs",
        hay: "state.sqlite_store",
        route: "GET /v1/admin/chat-activity?window_secs=N",
    },
    // ── insight_commands.rs ──────────────────────────────────
    // Both halves crossed: the row that `save_message`'s conversation
    // upsert used to mint implicitly (via an EMPTY user message whose own
    // comment read "dummy to create conversation") and the preamble.
    Retired {
        file: "src/insight_commands.rs",
        hay: ".save_message(",
        route: "POST /v1/conversations + POST /v1/conversations/{id}/messages/record",
    },
    // ── local_corpus_commands.rs ─────────────────────────────
    Retired {
        file: "src/local_corpus_commands.rs",
        hay: ".upsert_summary_correction(",
        route: "POST /internal/corpus/watch/{corpus_id}/enrich/reenrich-note (the hint rides the body; the daemon's manager writes the ledger)",
    },
    // ── state.rs ─────────────────────────────────────────────
    Retired {
        file: "src/state.rs",
        hay: ".set_vector_index_ready(",
        route: "no route: the write was inert — its one reader (corpus_catalog_http::catalog) prefers the on-disk meta, which the readiness probe self-heals",
    },
    // ── commands/mod.rs ──────────────────────────────────────
    Retired {
        file: "src/commands/mod.rs",
        hay: "fn tiers_for(",
        route: "GET /internal/corpus/catalog — the eleven-arm §2.1 match lives on the daemon",
    },
    Retired {
        file: "src/commands/mod.rs",
        hay: "fn ingest_progress_to_payload(",
        route: "no route: the desktop runs no local ingest, so it maps no local progress",
    },
];

#[test]
fn the_retired_local_primitives_do_not_return() {
    let mut offenders: Vec<String> = Vec::new();
    for r in RETIRED {
        let src = production_source(r.file);
        if src.contains(r.hay) {
            offenders.push(format!(
                "{}: `{}` is back — that question is answered by `{}`",
                r.file, r.hay, r.route
            ));
        }
    }
    assert!(
        offenders.is_empty(),
        "sv-surface D9b: a repointed command re-derives an answer the daemon \
         serves — {offenders:#?}. Each of these was a read of a construction \
         this process holds in BOTH boot modes; in attach it reads a \
         different store, index or registry than the one the turn is \
         answered against, and it does so silently. Consume the route, or \
         move the rung row in quality/campaigns/sv-surface.toml."
    );
}

/// The positive half. Without it, the rule above passes for the wrong
/// reason the day someone deletes the command instead of repointing it —
/// the shape `the_conversation_surface_defines_no_wire_envelope` learned to
/// carry, and the reason a needle list alone is not a gate (§18.1).
#[test]
fn every_repointed_command_reaches_the_family_client() {
    const REPOINTED: &[(&str, &[&str])] = &[
        ("src/commands/models.rs", &[".record_messages("]),
        (
            "src/insight_commands.rs",
            &[".create_conversation(", ".record_messages("],
        ),
        ("src/commands/contribution.rs", &[".chat_activity("]),
        ("src/local_corpus_commands.rs", &[".reenrich_note("]),
        (
            "src/commands/conversation.rs",
            &[
                ".create_conversation(",
                ".delete_conversation(",
                ".end_conversation(",
                ".get_conversation(",
                ".set_skill_active::<SkillEntry>(",
                ".rename_conversation(",
                ".set_enabled_corpora(",
                ".list_conversations_for_surface(",
                ".list_conversations_for_corpus(",
            ],
        ),
        (
            "src/commands/document_asset.rs",
            &[
                ".get_document::<",
                ".rebuild_document_skeleton::<",
                ".list_documents::<",
                ".delete_document(",
                ".list_legacy_documents::<",
                ".promote_legacy_document::<",
            ],
        ),
        (
            "src/commands/corpus.rs",
            &[
                ".corpus_coverage_card::<",
                ".corpus_catalog::<CorpusEntry>()",
                ".corpus_notebooks::<NotebookSummary>()",
            ],
        ),
        (
            "src/commands/budget.rs",
            &[
                ".corpus_health::<CorpusHealthDetail>(",
                ".corpus_retry_enrichment(",
            ],
        ),
        ("src/commands/corpus_install.rs", &[".corpus_diagnose()"]),
        (
            "src/commands/recipe_testing.rs",
            &[
                ".corpus_catalog::<CorpusEntry>()",
                "corpus_install::request_daemon_install(",
            ],
        ),
        ("src/commands/config_setup.rs", &[".backend_ready()"]),
    ];

    let mut missing: Vec<String> = Vec::new();
    for (file, calls) in REPOINTED {
        let src = production_source(file);
        for call in *calls {
            if !src.contains(call) {
                missing.push(format!("{file}: `{call}`"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "sv-surface D9b: a repointed command no longer reaches the family \
         client — {missing:#?}. A command that stopped asking locally AND \
         stopped asking the daemon answers nothing; the ban list above would \
         stay green through exactly that, which is why this half exists."
    );
}

/// ONE install decider. `install_corpus` asks the daemon; the setup
/// wizard's tier install used to carry its own copy that did not ask the
/// daemon at all — it ran `CorpusEngine::ingest` in this process, so the
/// two paths were different pipelines writing the same index directory and
/// only one of them was on the daemon's status poller.
#[test]
fn the_desktop_runs_no_local_corpus_ingest() {
    let mut offenders: Vec<String> = Vec::new();
    for file in [
        "src/commands/corpus.rs",
        "src/commands/corpus_install.rs",
        "src/commands/recipe_testing.rs",
    ] {
        let src = production_source(file);
        for needle in ["engine.ingest(", ".ingest(&spec"] {
            if src.contains(needle) {
                offenders.push(format!("{file}: `{needle}`"));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "sv-surface D9b: the desktop ingests a corpus in-process again — \
         {offenders:#?}. The daemon is the single owner of ingest lifecycle \
         (`POST /internal/corpus/install`, idempotent, narrated by the \
         status poller). A second pipeline writing the same index directory \
         is not a slower install, it is two writers."
    );

    // The positive half: exactly ONE implementation of the install request.
    let src = production_source("src/commands/corpus_install.rs");
    assert_eq!(
        src.matches("fn request_daemon_install(").count(),
        1,
        "sv-surface D9b: the install request must have exactly one \
         implementation (ARCH §10.6) — `install_corpus` and \
         `start_tier_installs` both call it"
    );
}
