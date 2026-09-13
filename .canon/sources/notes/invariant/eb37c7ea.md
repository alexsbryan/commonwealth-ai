# DAEMON-CONVERGENCE PHASES 3 + 4a + 4b LANDED SHAPE (2026-08-25) — and the five things not to undo.

DAEMON-CONVERGENCE PHASES 3 + 4a + 4b LANDED SHAPE (2026-08-25) — and the five things not to undo.

PHASE 3 — the crossing is closed. `state_store` moved from `DesktopServices` into `ServingCore`; `sovereign daemon run` opens `<data_root>/sovereign.db` beside `notes.db` and a failure is FATAL (core means cannot-serve; falling back to InMemoryStateStore reproduces the exact defect — a daemon answering every conversation lookup with a well-formed nothing). Safe because Phase 1's RunLock keys on that root.
DELETED: `DesktopServices` (one field once the store left, so `Desktop(Box<ServingProfile>)` now states the nesting IN THE TYPE); `DaemonServices::state_store()` (accessors 3 -> 2); the watched-folder subsystem's second InMemoryStateStore, whose delete_corpus_state was a no-op against an empty map — removing a watched folder left its rows in the real db forever.

PHASE 4a — 35 -> 0, machine-checked. `runtime/lane.rs`: `Lane` is a value stages receive. 26 live `self.<enrichment_field>` reads across runtime/retrieval/*, evidence_loop, streaming, turn, handlers -> zero. `PipelineState` carries it (steps read `st.lane`); other stages take `lane: &Lane` beside the enabled_corpora/corpus_ceiling they already took.
CORRECTNESS FIX RODE ALONG: meta_atlas is snapshotted ONCE per turn, not read per stage — the desktop's ~900MB background warm could previously land mid-pipeline and score two halves of one pool against two different indexes.
DELETED: the duplicated `rerank_config.enabled && rerank_fn.is_some()` decider; `Rerank::active()` is the one place.

PHASE 4b — MEASURED FIRST (note banked same day), and the measurement CHANGED THE MOVE. The pair-independence pass over 19 Runtime builders x 3 live sites does NOT factor: three mutually incomparable two-host classes, which no lattice has. The raggedness is OMISSION, not topology — and the ragged remainder is exactly §3.5's five departures, reproduced independently. So the move is TOTALITY, not product-to-sum: Runtime has no variants; the three-ness belongs to DaemonServices.
LANDED: `LaneSources` is a REQUIRED argument to Runtime::new; EIGHT with_* enrichment builders deleted. All three hosts gather their stack and commission once. `install_meta_atlas` survives, backed by an ArcSwapOption cell rather than a second storage.
THE ASSEMBLER: `sovereign_mesh::assemble(&Launch, LaunchParts) -> Result<DaemonServices, AssemblyRefusal>` — the one exhaustive match. All four sites go through it. HOME IS sovereign-mesh, NOT sovereign-cli-daemon as §10 settled on 2026-08-24: `svrn mesh create/join` live in sovereign-cli-llm, which does not depend on cli-daemon and should not start.

MUST NOT BE UNDONE:
1. `DaemonServices::desktop`/`::headless` are pub(crate). No crate outside sovereign-mesh can compose a serving daemon. `MeshAdmin` is still a bare variant — Phase 7's job — and `tests/launch_assembler_census.rs` covers that gap with its exemption STATED, not assumed.
2. `LaneSources` is a constructor ARGUMENT. A builder cannot enforce installation: from inside the Runtime a forgotten `with_gliner` and a host with no GLiNER are the same state. That is not hypothetical — it shipped: for months only `svrn chat` called with_rerank while the ledger reported the reranker available on all three hosts.
3. `Runtime::lane()` snapshots. Do not let a stage re-read the meta_atlas cell.
4. The census tests were WATCHED TO FAIL. `each_assembling_launch_produces_its_variant` was sabotaged (desktop arm returning MeshAdmin) and failed correctly. `each_live_path_supplies_the_parts_for_its_variant` PASSED under sabotage the first time — the desktop's own explanatory comment contains the literal `headless: None`, so prose about the invariant satisfied the check for the invariant (§18.1: a guard asserting on what the subject echoes back). Comments are now stripped before matching; do not remove `code_only`.
5. Falsifier 1 closed the same way: threading the `Launch` VALUE into daemon_cmd::run (not re-deriving from args) killed the last `--worker-mode` reader AND sidestepped the arg-shape mismatch that had deferred it. Readers 3 -> 0, writers 6 -> 0.

PHASE 10 OPENED: SOVEREIGN_USE_SUPERVISOR + SOVEREIGN_FORCE_LOCAL are `sovereign_contracts::launch::DaemonHost`, resolved once by the desktop's main beside Launch::parse. Three points of use -> one construction-time read, and a §10.6 duplicate closed on the way (bootstrap::detect and supervisor_setup::is_enabled each parsed FORCE_LOCAL independently, agreeing only by coincidence of both spelling `== "1"`). The in-process shape now carries WHY (ForceLocal vs KillSwitch) where the predicate could only report a bare false.

STILL OWED ON THE CRITICAL PATH: 9 (verbs — `retrieve` is the only door; this is the noun-convergence Evidence rung nc-4 and is a program, not a step) -> 5 -> 6 -> 7, with 10 closing before 7.
