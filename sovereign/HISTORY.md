# HISTORY — how the system got its shape

Companion to [`SYSTEM_OVERVIEW.md`](./SYSTEM_OVERVIEW.md). The overview
states what IS, as a contract verifiable against the current commit;
this file preserves how it came to be — the reversals, decompositions,
and archaeology that explain why the current shape is the way it is.
Entries are dated and never rewritten to match later code: a citation
here describes the repo *as of its date*, so paths may no longer
resolve. (For exactly that reason this file is deliberately outside the
`docs-gate` citation check that guards the overview.) Bench-specific
history lives separately in [`bench/HISTORY.md`](./bench/HISTORY.md).

---

## Router-stack parity (2026-06-09)

Before 2026-06-09 the four boot classifiers were wired only in the
CLI/bench bootstrap, so the desktop app (bare router) and the daemon
(current-info only) silently under-routed to the fast slot while the
benches — which *did* wire the stack — kept improving ("desktop kind of
sucks even as benches get better"). The fix collapsed the three call
sites onto the single helper
`sovereign-core/src/router_bootstrap.rs::build_llm_router`, which every
surface now calls; `tests/router_bootstrap_parity.rs` asserts
`all_wired()` so the surfaces can't silently re-diverge. This is the
origin of the overview's "parity by construction" claim.

## The grounding-gate verdict reversal (2026-06-09 → 06-11)

The 2026-06-09 chaos result ruled Critic-as-gate out of prod
empirically (competence 0.46 → 0.08: it gated present-answerable
questions — `present-wife` at violation_prob 0.806 — when retrieval
missed the supporting passage). What changed the verdict two days later
was not the judge but the **evidence universe**: the v12–v15 gate
verifies claims against the *sealed corpus* (per-claim hybrid search via
`ClaimSearcher`), not just the prompt snapshot, and feeds failed claims'
corrective passages into the rewrite (replace, don't delete). Under that
stack the gate is net-positive and PASSED the full bank on 2026-06-11
(secret-agent 0.67/0.82/0.18 production-config; holdout honesty
0.91/0.09), and `runtime/grounding/` shipped the same day. The keystone
lesson: a verifier's verdict is a function of what it is allowed to see.

## Retrieval-pipeline convergence (2026-06-09 → 10)

The step bodies of today's `RetrievalPipeline` are verbatim transplants
of orchestration that previously lived inline — and duplicated, with
silent drift — in `prepare_knowledge_query_plan` and
`prepare_knowledge_context`.

The **Phase 2 convergence (2026-06-09, CI-bench-A/B'd)** moved the deep
path's atlas/RAPTOR grounding to the KQ post-floor position (the old
pre-floor position let the noise floor silently drop zero-overlap
virtual grounding chunks) and extended `dedupe_merged` to the KQ path.

The **2026-06-10 divergence-archaeology pass** resolved the remaining
per-intent divergences (see the module doc's resolution log): deep's
expansion decision was re-pointed at the same `decide_expansion_strategy`
SSOT the KQ planner uses (chunk-set-identical by the helper's internal
guard; emits the same `expansion_decision` audit), the personal-scope
filter became one shared whole-pool step on both paths (mesh strays now
drop on personal-scope turns), and the store-search leg was fixed to
reuse the pipeline's query embedding — closing a missed `embed_query`
retrofit from 2026-05-18. The last accretion artifact — KnowledgeQuery
turns silently skipping the mesh and the doc store (Deep/Simple had
searched both since 2026-04-21) — was resolved the same day by unifying
both pipelines onto `shared_head_steps()`, establishing the principle
the overview now states flatly: which knowledge sources exist is a
property of the install, not of the intent label.

## The §3.1 decomposition campaign (2026-06-08 → 06-15)

The ARCH §3.1 file-size ratchet drove a series of behaviour-preserving
splits. The completed ones are recorded here; live residuals stay in the
overview's §10 ledger.

### setup_cmd.rs — the reusable recipe (2026-06-09)

`sovereign-cli-daemon/src/setup_cmd.rs` (1609 lines) → `setup_cmd/`
(`mod.rs` 977 incl. tests + 6 submodules: `args`/`catalog`/`byom`/
`download`/`finish`/`opencode`). This split established **the recipe**
the later splits reused: shared `Opts`/`ModelPaths`/`Pick` types stay in
`mod.rs` (submodules read them as ancestor-privates → zero
field-visibility churn); the orchestrators (`run_setup`/`run_repair`)
keep byte-identical bodies and reach submodules via `use` imports;
cross-called fns become `pub(super)`; test modules stay in `mod.rs` with
explicit submodule `use`s. 51 daemon tests green. Related Phase 2 CLI
infra from the same period: the shared `sovereign_cli_shared::args`
parser + collapse of the three `util.rs` re-export shims.

### daemon_cmd.rs — three waves, and a refuted judgment (2026-06-09 → 06-15)

`sovereign-cli-daemon/src/daemon_cmd.rs` (3803 lines) → `daemon_cmd/`.
**Wave 1 (2026-06-09):** the separable concerns moved to submodules
following the `setup_cmd` recipe — `lifecycle` (start/stop/restart/
reload/status + pidfile + port-probe + shutdown), `workspace`
(auto-detect), `provider` (`LlamaCppFactory` hot-reload), `worker`
(ephemeral-pod entry), `tool_registry` (MCP registry + merged SCIP
graph). Cross-called fns `pub(super)`; `home_dir_buf` stays in `mod.rs`
as a shared ancestor-private; tests moved with their code (51 daemon
tests green). **Wave 2 (same day):** the two self-contained early phases
of the `run_daemon` bootstrap were extracted to `daemon_cmd/build/`:
`preflight` (VRAM-capacity check — no outputs) and `inference`
(`load_provider` — returns the provider + concrete engine handle +
resolved embed family). Pure relocations, compile-verified (this startup
path has no GGUF-free CI coverage). **Wave 3 (2026-06-15):** the full
bootstrap-TOC decomposition landed, **refuting the earlier
"interleaved → can't pure-relocate without reordering" judgment**
recorded in the ledger at the time: the remaining ~22 phases moved into
`daemon_cmd/bootstrap.rs` (20 phase fns + a `WatcherAtlasSetup` bundle
struct), taking `run_daemon` 1919→611 lines and `mod.rs` 2233→921. The
enabling technique is **strict in-place extraction** — every call site
stays in its exact position, so side-effect order is preserved *by
construction* and any capture/borrow slip surfaces as a compile error,
not a boot-time surprise — plus already-built handles passed as params,
and, for the one multi-output block (workspace watchers + work-atlas), a
**return-bundle struct destructured at the call site back into the
original local names**, leaving all ~7 downstream consumers
byte-unchanged. `resolve_self_node_id` deduped the two byte-identical
node-id resolutions. Verified: full-workspace `cargo check` +
`cargo test` green. Genuinely left inline (a readability call, *not*
interleaving): the config/stores preamble (flags → wizard → config →
VRAM → stores) — already-readable guard-clauses whose only extraction
blocker is early-`return <exit-code>` paths; threading those through
`Result`/`ControlFlow` would add boot-path indirection for little gain.

### mesh_cmd.rs / corpus_cmd.rs (2026-06-09)

`sovereign-cli-llm/src/mesh_cmd.rs` (3868 lines) had served both the
`mesh` AND `corpus` verbs — a dispatch naming lie. The `corpus` half
(~2960 lines of `cmd_corpus_*` + helpers + `HELP_CORPUS`) split into
`corpus_cmd.rs`; `run_corpus` re-pointed at both callers (`main.rs` +
`alignment_cmd.rs`); `mesh_data_dir` now imported from
`sovereign_cli_shared::dirs` in both files; `hostname` stayed private to
`mesh_cmd` — corpus turned out to use neither, so there was no
cross-module coupling. Same day, `corpus_cmd.rs` was further broken into
`corpus_cmd/{mod,fmt,inventory,diagnostics,partitions}.rs`: `fmt` the
shared-formatter leaf, `inventory`/`partitions` using it, `diagnostics`
borrowing the partition-discovery helpers, `mod` the dispatcher. All
five files landed under the §3.1 ceiling (mod 116, fmt 52, inventory
624, diagnostics 1155, partitions 1050); 498 llm tests green. (A stale
duplicate `sovereign-cli-dev/src/mesh_cmd.rs` — never compiled — was
deleted 2026-06-01.)

### embedded.rs → embedded/ (PR5b + 2026-06-10)

The 9,669-line `sovereign-inference/src/embedded.rs` monolith was
decomposed: one concern per submodule under `embedded/` (engine ~2,965 ·
model_slot ~3,475 · rpc_distribution ~1,168 · grammar ~1,146 ·
prompt_helpers ~786 · rpc_warm_cache ~668 · sampler ~567 · embed_slot
~548 · rerank_slot ~509), re-exported flat so `crate::embedded::<Item>`
paths are unchanged. The residual `model_slot.rs` remains a live §10
ledger entry.

### state.rs (desktop) — extraction of the contiguous phases (2026-06-09)

`sovereign-desktop/src-tauri/src/state.rs` (2347 lines → ~1430): config
→ `state/config.rs`, built-in skills → `state/builtin_skills.rs`, and
four `bootstrap_with_progress` sub-phases →
`state/builders/{health,store,inference,knowledge_view}.rs`. Each
builder takes a narrowed signature (its own handles, not `&AppState` —
ARCH §5.2) + a mock-backed unit test (a stub `InferenceProvider` + a
temp `CorpusEngine`, plus the inference reuse-seam) — establishing that
bootstrap phases ARE CI-testable via dependency injection; only the
literal model load isn't. 100 desktop tests green. The remaining inline
body is documented as a live §10 ledger entry.

### DesktopError — first PR + the burn-down enabler (2026-06-09)

A structured `{code, message, suggested_action}` error replaced the
`.map_err(|e| e.to_string())` → bare-`String` pattern (~295 handler
sites). Rust `DesktopError` + snake_case `ErrorCode` (wire shape pinned
by a serialization test) with `From<String>`/`From<&str>`, so a handler
flips to `Result<_, DesktopError>` while its neighbours still return
`String` and `?` keeps compiling across the seam. Frontend mirror:
`DesktopError` type + pure, tested `isDesktopError`/`normalizeError` +
`invokeChecked<T>()` + `toastError`. First consumers: `search_web` (via
the additive `AppState::runtime()` accessor) + budget.rs's 4 daemon-HTTP
commands. The burn-down enabler (same day): `invokeChecked` throws the
normalised error as an `Error` *instance* (structured fields attached
via `Object.assign`), so the ~150 existing
`e instanceof Error ? e.message : String(e)` catch blocks render the
message unchanged — migrating a command needs no per-caller edits, just
the Rust return-type flip + pointing its api.ts wrapper at
`invokeChecked`. The per-module burn-down is a live §10 ledger entry.

## Commonwealth CLI placeholders resolved (2026-07-01)

Two-thirds of the `commonwealth` binary's subcommands had printed
`(In production, this would …)` and exited 0 since the original scaffold.
Resolved per-command against the real HTTP control plane: `status`,
`models`, and `corpus status` were implemented as thin views over
`GET /status`, `/v1/models`, and `/internal/corpus/status`; the 14
unbacked commands (join/pause/resume/leave/logs, corpus
list/install/remove/update/consolidate/collaborate-status, mesh
set/members/peer) plus the always-erroring `mesh revoke` were deleted.
The grow-only-membership revocation constraint (`Mesh::merge_from` has
no tombstone, so a revoke cannot propagate — shipping the command before
the tombstone would report false success on a security action) is
preserved as a comment at the deletion site and in
`commonwealth/ARCHITECTURE.md` §11.
