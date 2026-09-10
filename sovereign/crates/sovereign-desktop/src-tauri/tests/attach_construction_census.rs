// SPDX-License-Identifier: AGPL-3.0-or-later
//! The attach-mode construction census — the floor instrument of
//! `quality/campaigns/sv-surface.toml`'s `sv-attach-pure-client` bar.
//!
//! # The state this makes unrepresentable
//!
//! An attach-mode desktop that quietly grows a new locally-constructed
//! decider. Attach mode means a daemon is already serving on the client
//! port; the campaign's claim is that the desktop in that mode is a CLIENT
//! (`svrn chat`'s shape), not a second assembler. The floor is the measured
//! inventory below — 11 constructions in `state.rs`'s spine plus the
//! attach-mode provider in the builders file (12 total), all reachable from
//! `bootstrap_with_progress` in BOTH boot modes. Each campaign rung deletes
//! needles; the total only moves through a rung row recorded in the campaign
//! file, never silently.
//!
//! # 14 -> 12, and the instrument was wrong about one of them (sv-surface D0)
//!
//! `WatchedSubsystem::install` was never attach-reachable: it sits inside
//! `if let Some((daemon_handle, cli_cfg)) = local_daemon_wiring`, which is
//! `None` in attach. A needle list that counts a Local-only construction
//! against the ATTACH floor overstates the bar and — worse for an instrument
//! — would have gone green on its deletion for the wrong reason. It moves to
//! `the_local_only_constructions_stay_local`, where its guard is what is
//! pinned. That is a MEASUREMENT correction: -1 with no code change.
//!
//! `CompactionWorker::spawn` was correctly counted and was a real defect. It
//! ran unconditionally, so an attached desktop spawned a second rolling-
//! summary compaction pass over the daemon's own `sovereign.db` while the
//! comment two lines up claimed "attach-mode leaves the worker `None`". D0
//! gated it on `local_daemon_wiring` — the config is an `Option` now, so
//! there is nothing to spawn from in attach — and it likewise moves to the
//! Local-only test. That is a FIX: -1 with a code change behind it.
//!
//! # The instrument's own weakness, recorded rather than hidden
//!
//! This is a NEEDLE LIST: it pins the constructions it knows by name and
//! count, so it cannot see an unlisted new one (the cw-lift 2c finding, the
//! same shape `every_sender_of_replicated_state_is_declared` carries).
//! Git history of the pinned total is the audit for additions; rung 6's
//! boot assertion — attach boots and the surface suite runs green with
//! zero of these constructed — is the stronger instrument that replaces
//! this census when the floor reaches zero.
//!
//! # Why D9 did not replace this census (measured 2026-09-10)
//!
//! The ladder's D9 row hoists attach out of `bootstrap_with_progress`
//! and takes the floor to zero, on the survey's premise that every
//! attach-time consumer already reads the wire because "all 15
//! `is_attach_mode` forks already have their wire form". The forks ARE
//! retired — `is_attach_mode()` appears twice in the whole command
//! surface. But the forks were never the population that matters.
//!
//! Counted over `src/` with `#[cfg(test)]` cut and comment lines
//! dropped, `require_runtime!` + `require_store!` +
//! `state.<needle>.read()` + the two typed accessors: **49 reads across
//! 13 files**, none behind a mode fork, every one of them live in
//! attach today because this spine builds all ten needles in BOTH
//! modes. Deleting the constructions deletes those 49 answers. The
//! sharpest is `commands/config_setup.rs`'s `is_backend_ready`, whose
//! own doc comment is written ABOUT attach mode: it reads
//! `state.runtime.is_some()` as the pull-based recovery for a missed
//! `backend-ready` event, so a null runtime hangs the splash forever.
//! Two more fail SILENTLY — `list_corpora` and `notebook_list` return
//! `Ok(Vec::new())` on `None`, rendering an empty picker rather than an
//! error.
//!
//! So the needle count is a LAGGING indicator in a second way the D-row
//! did not name: a floor of 12 says nothing about how many CONSUMERS
//! still need those 12. The consumer count is the number D9 has to
//! drive to zero, and it is not this test's number. Minting it as its
//! own ratchet is deferred only because the D5-D8 delete halves are in
//! flight in the same files as this is written, and a gate with a
//! guaranteed false positive is how people learn to reach for
//! `--no-verify`.
//!
//! Watched to fail: add or remove a construction site in `state.rs`'s
//! bootstrap spine (or edit an expected count here) and this goes red
//! naming the needle. Sabotage-verified at landing: one count edited,
//! watched red, reverted.

/// One pinned construction: the needle `state.rs` must contain exactly
/// `count` times, and the rung that owns its deletion (campaign ladder).
struct Needle {
    /// The source text scanned for (a constructor call, spelled as it
    /// appears in `state.rs`).
    hay: &'static str,
    /// Occurrences pinned today.
    count: usize,
    /// Why it is on the floor's list, one line.
    why: &'static str,
}

/// THE floor: every construction the attach-reachable bootstrap spine
/// performs, 2026-09-08. Order follows `bootstrap_with_progress`.
///
/// This table is DATA, not law: a rung that deletes a needle updates its
/// count to 0 (or removes the row) IN THE SAME COMMIT as the deletion, and
/// the campaign file's floor number moves with it.
const FLOOR: &[Needle] = &[
    Needle {
        hay: "builders::store::open_store(",
        count: 1,
        why: "the desktop's own sovereign.db handle — the second opener beside the daemon's",
    },
    Needle {
        hay: "match NoteStore::open(",
        count: 1,
        why: "notes.db, the unconditional (attach-reachable) open at the spine — the two `corpus_engine_notes::NoteStore::open` sites below are the Local-only daemon-MCP opens",
    },
    Needle {
        hay: "RecipeProjectStore::open(",
        count: 1,
        why: "features.db, both modes",
    },
    Needle {
        hay: "SkillRegistry::new(",
        count: 1,
        why: "the skill registry, built host-side",
    },
    Needle {
        hay: "sovereign_gliner::load_gliner_extractor(",
        count: 1,
        why: "GLiNER ONNX extractor, loaded host-side",
    },
    Needle {
        hay: "sovereign_tools::enrichment_bootstrap::build_folder_tiered_provider(",
        count: 1,
        why: "the in-process tiered-enrichment provider",
    },
    Needle {
        hay: "corpus_engine::CorpusEngine::new(",
        count: 1,
        why: "a FULL local corpus engine — lance opens over installed indexes, constructed in attach mode too",
    },
    Needle {
        hay: "LocalCorpusManager::init_with_recipes_dir(",
        count: 1,
        why: "the local-knowledge manager over that engine",
    },
    Needle {
        hay: "builders::knowledge_view::build_knowledge_view(",
        count: 1,
        why: "the knowledge-view manager (attach-gated params, constructed both modes)",
    },
    Needle {
        hay: "sovereign_runtime_recipe::common_parts(",
        count: 1,
        why: "the shared recipe's parts — tool registry, router, MCP, atlas, wiki graph, reranker — gathered host-side",
    },
    Needle {
        hay: "sovereign_runtime_recipe::commission(",
        count: 1,
        why: "THE Runtime, commissioned in attach mode too, over the remote provider (the C2 divergence's root)",
    },
];

/// `state.rs` is the desktop's bootstrap spine; the census reads it at the
/// sibling path from this test's manifest dir.
fn state_rs() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/state.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The attach-mode inference provider's construction site lives one file
/// down, in the builders module `state.rs` calls.
fn builders_inference_rs() -> String {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/state/builders/inference.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The fourteenth needle, pinned in its own test because it lives in the
/// builders file: the provider that makes attach mode's Runtime "local" —
/// an OpenAI-compatible HTTP client pointed at the daemon
/// (`builders/inference.rs:339`). Deleting it is rung 6's centre.
#[test]
fn the_attach_provider_construction_is_pinned() {
    let src = builders_inference_rs();
    assert_eq!(
        src.match_indices("build_attach_provider(slots)?").count(),
        1,
        "sv-attach-pure-client: the attach-mode provider call site moved or \
         multiplied. This is the construction that lets an attached desktop \
         assemble its own Runtime over the daemon's models — the C2 \
         divergence's root. The floor only moves through a campaign rung row."
    );
    assert_eq!(
        src.match_indices("fn build_attach_provider(").count(),
        1,
        "the provider's definition moved or multiplied — one implementation, \
         one site, pinned"
    );
}

#[test]
fn the_attach_construction_floor_is_pinned() {
    let src = state_rs();
    let mut total = 0usize;
    for needle in FLOOR {
        let found = src.match_indices(needle.hay).count();
        assert_eq!(
            found,
            needle.count,
            "sv-attach-pure-client: needle `{}` moved (found {found}, pinned {}). \
             {why}. The floor only moves through a campaign rung row — update this \
             census IN THE SAME COMMIT as the rung that owns the change, never \
             silently.",
            needle.hay,
            needle.count,
            why = needle.why
        );
        total += needle.count;
    }
    assert_eq!(
        total, 11,
        "sv-attach-pure-client floor: the pinned spine total must be 11 here, \
         plus the attach-provider needle pinned in the builders file = the \
         campaign file's floor_basis (12, sv-surface D0: was 14 — \
         WatchedSubsystem::install was never attach-reachable and \
         CompactionWorker::spawn is gated on the local wiring now). A needle \
         added or removed without the campaign row moving is the exact silent \
         drift this census exists to catch."
    );
}

#[test]
fn the_local_only_constructions_stay_local() {
    // The constructions that are Local-mode-only today must keep an
    // attach-visible guard: `sovereign_mesh::assemble` (the EmbeddedDaemon
    // commission), the RunLock claim, the watched-folder subsystem and the
    // compaction worker all sit behind the `local_daemon_wiring` Option,
    // which is None in attach. If any ever runs in attach mode, that is not
    // a needle-count change — it is the whole bar inverting, and it should
    // fail HERE first.
    let src = state_rs();
    assert_eq!(
        src.match_indices("sovereign_mesh::assemble(").count(),
        1,
        "the EmbeddedDaemon commission site moved or multiplied — the one \
         exhaustive assembler must be called from exactly one site in the \
         desktop, behind the Local-only wiring"
    );
    assert_eq!(
        src.match_indices("corpus_engine_notes::NoteStore::open(")
            .count(),
        2,
        "the daemon-MCP NoteStore opens moved or multiplied — they are \
         Local-only constructions (inside the local wiring) and are pinned \
         HERE so they can never silently join the attach-reachable spine"
    );
    assert!(
        src.contains("local_daemon_wiring"),
        "the Local-only guard binding assemble+RunLock to Local mode was \
         renamed or removed — attach mode constructing a daemon is the bar \
         inverting, not a count moving"
    );

    // sv-surface D0 — the two that came OFF the attach floor. Both still
    // exist; what is pinned here is that each stays behind the local wiring.
    assert_eq!(
        src.match_indices("sovereign_mesh::watched_folder_setup::WatchedSubsystem::install(")
            .count(),
        1,
        "the watched-folder subsystem install moved or multiplied — it is \
         Local-only (inside the `if let Some((daemon_handle, cli_cfg)) = \
         local_daemon_wiring` arm) and is pinned HERE so it can never \
         silently join the attach-reachable spine"
    );
    assert_eq!(
        src.match_indices("CompactionWorker::spawn(").count(),
        1,
        "the memory-compaction worker spawn moved or multiplied — one per \
         host Runtime, and only where the host OWNS the memory store"
    );
    assert!(
        src.contains("let compaction_worker = compaction_config_for_runtime.map(|cfg| {"),
        "sv-surface D0: the compaction worker is spawned unconditionally \
         again. The config it needs is `Some` only under the local daemon \
         wiring; in attach the daemon owns that `sovereign.db` and runs its \
         own worker, so a second pass here is two writers deriving rolling \
         summaries from each other's rows."
    );
}
