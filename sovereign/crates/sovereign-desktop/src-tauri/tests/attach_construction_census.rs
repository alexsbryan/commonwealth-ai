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
//! # D9b consumed 22 of the reads and the floor did not move (2026-09-10)
//!
//! Re-counted first, same method, on the tree D9b started from: **55 reads
//! across 13 files**, not 49. Eleven of the thirteen per-file numbers above
//! reproduce exactly; `chat.rs` is 8 (recorded 3) and `recipe_testing.rs` is
//! 3 (recorded 2). Re-running the counter AT 65b92cd15 gives 8 and 3 there
//! too, so those two figures were an undercount when written, not drift
//! since — the instrument is sound, the two numbers it reported were not
//! (§18.4: validate the instrument before the result).
//!
//! D9b repointed 22 onto 2a9a9e91e's routes — the six document commands, the
//! corpus catalogue, the shelf, health, retry-enrichment, diagnose, the tier
//! installs, conversation create/delete/end/export, the skill toggle and
//! `is_backend_ready`. **55 -> 33 across 10 files.** The consumer census is
//! `d9b_consumer_census.rs`; it pins each retired primitive to the route
//! that answers it now.
//!
//! The FLOOR is unchanged at 12, and that is a measurement, not an omission.
//! Every one of the eleven spine needles is consumed by
//! `sovereign_runtime_recipe::common_parts` -> `commission` — `store`,
//! `sqlite_store`, `corpus_engine`, `notes`, `features` (via the tool
//! bundles), `skills`, the GLiNER extractor, the tiered provider and the
//! knowledge view all feed the Runtime this process commissions in attach
//! mode too. `notes` and `features` have NO command-surface reader at all
//! and are still not free for that reason. So the commission is the
//! blocker, and the commission has six live attach-time readers of its own:
//! `chat.rs` 110 and 954 (readiness gates), 1063 (the cancel fallback), 1161
//! (the session->conversation soft read), `document_asset.rs` 387
//! (`ask_document`'s turn half) and `models.rs` 34 (`search_web`'s tool
//! registry). Three of those six are named CANNOT-CROSS in 2a9a9e91e and one
//! more is a turn-path change owing a pre-registered bench (§18.6).
//!
//! `attach_bootstrap()` is therefore still not written, on the same rule the
//! D9 correction set: the floor reaches zero when the last consumer does,
//! and a hoist before then deletes answers. The remaining consumers and the
//! route each one needs are enumerated on the campaign's D9b row.
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
        count: 0,
        why: "notes.db — DELETED svt-3b with the commission that was its only consumer (the `RecipeAuthoringTools` bundle); `lessons` reaches the daemon's `/v1/notes`",
    },
    Needle {
        hay: "RecipeProjectStore::open(",
        count: 0,
        why: "features.db — DELETED svt-3b, same reason; `recipe_author_commands` reaches `/v1/features/*`",
    },
    Needle {
        hay: "SkillRegistry::new(",
        count: 0,
        why: "the skill registry — DELETED svt-3b: it fed the recipe and the knowledge view, and the skills PANE reads the daemon's `/v1/skills`",
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
        count: 0,
        why: "the local-knowledge manager — DELETED thin-desktop R1 (2026-09-12). `state.local_corpus` was written at the end of its own ~115-line commissioning block and read by NOTHING: `lc_pre_scan` (6e47d0abe) was the last command holding a manager and it took the wire, so `lc_http` serves all fifteen routes over the DAEMON's manager. The enrichment defaults and tiered deps went with it",
    },
    Needle {
        hay: "InsightService::new(",
        count: 0,
        why: "the insight capture service — DELETED thin-desktop R1 (2026-09-12), zero readers of `state.insight_service`; the insight surface is `insight_http` on the daemon (sv-surface rung 6). It was also the store builder's only reason to name an `InferenceProvider`",
    },
    Needle {
        hay: "builders::health::build_health_monitor(",
        count: 0,
        why: "the background health monitor — DELETED thin-desktop R1 (2026-09-12), zero readers of `state.health_monitor`; the app renders health from the daemon's `/status` and `/{corpus}/health`. A monitor polling a store, an engine and a provider inside a client, whose verdict nothing could ask for",
    },
    Needle {
        hay: "LazyGlinerExtractor::new_default_deferred(",
        count: 0,
        why: "a SECOND handle on the NER model as `dyn EntityExtractor` — DELETED thin-desktop R1 (2026-09-12), zero readers of `state.entity_extractor`; document ingest's skeleton pass is the daemon's since 2d5b569f6, which hands its manager `runtime.lane().gliner`",
    },
    Needle {
        hay: "builders::knowledge_view::build_knowledge_view(",
        count: 0,
        why: "the knowledge-view manager — DELETED svt-3b (the builder file with it): its own attach guard already returned `None`, and attach is the only mode",
    },
    Needle {
        hay: "sovereign_runtime_recipe::common_parts(",
        count: 0,
        why: "the shared recipe's parts — DELETED svt-3b; the daemon commissions through the SAME recipe and every turn has crossed the wire since R5",
    },
    Needle {
        hay: "sovereign_runtime_recipe::commission(",
        count: 0,
        why: "THE Runtime — DELETED svt-3b. This was the C2 divergence's root and the blocker the D9b note named: eleven spine needles were consumed by it and nothing else",
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

/// The twelfth needle, pinned in its own test because it lives in the
/// builders file: the provider that makes the desktop's Runtime "local" —
/// an OpenAI-compatible HTTP client pointed at the daemon. Deleting it is
/// rung 6's centre.
///
/// RENAMED at svt-3, `build_attach_provider` -> `build_daemon_provider`, when
/// the OTHER arm of that file was deleted. There is no attach/local fork left
/// to name it against: this is how the desktop gets inference, full stop.
#[test]
fn the_attach_provider_construction_is_pinned() {
    let src = builders_inference_rs();
    assert_eq!(
        src.match_indices("build_daemon_provider(slots)?").count(),
        1,
        "sv-attach-pure-client: the daemon-provider call site moved or \
         multiplied. This is the construction that lets the desktop assemble \
         its own Runtime over the daemon's models — the C2 divergence's root. \
         The floor only moves through a campaign rung row."
    );
    assert!(
        !src.contains("EmbeddedLlamaCpp"),
        "svt-3: the builders file names the in-process llama loader again. \
         A desktop that mmaps a GGUF is the daemon it is supposed to be a \
         client of (ARCH principle 12), and the crash-isolation subprocess \
         that guarded that load was deleted on the strength of this absence."
    );
    assert_eq!(
        src.match_indices("fn build_daemon_provider(").count(),
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
        total, 4,
        "sv-attach-pure-client floor: the pinned spine total must be 4 here, \
         plus the daemon-provider needle pinned in the builders file = 5 \
         (was 6 before thin-desktop R1, 12 at sv-surface D0, 14 before it). \
         svt-3b took six zeros: the commission and everything whose only \
         consumer was the commission. R1 took four more, and they were a \
         different kind — not repointed onto a route, DELETED, because each \
         was written by this spine and read by NOTHING. What is left is the \
         four the desktop's own surfaces still read: its `sovereign.db` \
         handle, the corpus engine, the tiered-enrichment provider and \
         GLiNER. Those four ARE a repoint, and the reader count is the thing \
         to watch — `build_corpus_index` is the corpus engine's last one. A \
         needle added or removed without the campaign row moving is the \
         exact silent drift this census exists to catch."
    );
}

/// Every construction that was Local-mode-only, pinned at ZERO (sv-surface
/// svt-3).
///
/// Until svt-3 these sat behind a `local_daemon_wiring` Option that was
/// `None` in attach, and this test pinned that GUARD. The guard is not what
/// was load-bearing — the ability was. `bootstrap_with_progress` commissioned
/// an `EmbeddedDaemon` over an assembled `ServingCapability`, claimed the data
/// root's `RunLock`, installed the watched-folder scheduler and spawned a
/// rolling-summary compaction worker whenever the boot concluded `Local`; a
/// desktop that does that is a daemon wearing a UI, whatever the count of
/// guarded call sites is (ARCH principle 12 — look where the ability is
/// GRANTED).
///
/// So the pins are zeros now, and a zero here is a real positive control:
/// each string names a call that USED to exist in this file and whose
/// reappearance is the regression. Watched to fail at landing: each needle
/// re-added by hand in turn, red, reverted.
///
/// What it deliberately does NOT pin is the `mesh` FIELD, which survives as a
/// permanently-`None` slot `mesh_commands.rs` still reads. The field grants
/// nothing; `EmbeddedDaemon::new` is the grant, and that is the needle below.
#[test]
fn the_in_process_daemon_is_gone() {
    let src = state_rs();
    const GONE: &[(&str, &str)] = &[
        (
            "sovereign_mesh::assemble(",
            "the one exhaustive assembler — calling it is how a host declares \
             itself a daemon",
        ),
        (
            "sovereign_mesh::EmbeddedDaemon::new(",
            "the daemon itself, built over that assembly",
        ),
        (
            "DeferredDaemon",
            "the late binding the in-process cycle needed: a provider that \
             wanted a peer source before the daemon existed",
        ),
        (
            "RunLock::acquire(",
            "the single-writer claim on the data root — a client does not own \
             the root and must never take it",
        ),
        (
            "local_daemon_wiring",
            "the Option that gated all of the above; its absence is what makes \
             the zeros above unconditional rather than guarded",
        ),
        (
            "corpus_engine_notes::NoteStore::open(",
            "the daemon-MCP notes opens (two of them), distinct from the \
             spine's `NoteStore::open` on the floor list above",
        ),
        (
            "sovereign_mesh::watched_folder_setup::WatchedSubsystem::install(",
            "the watched-folder scheduler — the attached daemon owns it",
        ),
        (
            "CompactionWorker::spawn(",
            "a second rolling-summary pass over a `sovereign.db` this process \
             does not own (sv-surface D0, structural here)",
        ),
        (
            "sovereign_mesh::EmbedAdvertisement",
            "what a NODE tells peers about its embedding model; this process \
             is not a node",
        ),
        (
            "sovereign_workflow_host::workflow_http_router(",
            "the workflow job routes the in-process daemon served",
        ),
    ];
    for (needle, why) in GONE {
        assert_eq!(
            src.match_indices(needle).count(),
            0,
            "svt-3: `{needle}` is back in the desktop's bootstrap spine. {why}. \
             The desktop holds no weights and commissions no daemon; a build \
             that does is the bar inverting, not a count moving."
        );
    }
}
