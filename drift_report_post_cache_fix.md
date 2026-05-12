# Drift Report — 36 actionable · 0 confirmed · 0 queued

**Code**: `commonwealth-ai-self-atlas`  ·  **Narrative**: `commonwealth-ai-system-overview`, `commonwealth-ai-arch-principles`

## Act on

**1. normative claim _(no anchor)_ — Work intentionally deferred must be listed in the Architecture Roadmap so that the next engineer inherits a todo list ra…** _(commonwealth-ai-system-overview sec_00002)_  
> Listed here so the next engineer inherits a todo list

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**2. normative claim _(no anchor)_ — [recipe.parameters] defines String/Int/Date/List parameters with defaults and required flags; Recipe::resolve_parameters…** _(commonwealth-ai-system-overview sec_00009)_  
> `[recipe.parameters]` (`corpus-engine/src/recipe.rs`) — String/Int/Date... `Recipe::resolve_parameters` validates...

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**3. normative claim _(no anchor)_ — [corpus] schema_version is bumped only when readers must opt in; the reader refuses recipes declaring schema_version > M…** _(commonwealth-ai-system-overview sec_00009)_  
> `[corpus] schema_version`... The reader refuses recipes declaring `schema_version > MAX_SCHEMA_VERSION`...

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**4. normative claim _(no anchor)_ — The open_index_for_corpus function always opens the directory corresponding to the corpus ID.** _(commonwealth-ai-system-overview sec_00010)_  
> ...`open_index_for_corpus(corpus_id)` which always opens `<index_dir>/<corpus_id>`.

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**5. normative claim _(no anchor)_ — corpus-engine` never embeds or generates text itself.** _(commonwealth-ai-system-overview sec_00011)_  
> `corpus-engine` never embeds或generates text itself.

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**6. normative claim _(no anchor)_ — Every plan file under ~/.claude/plans/*.md must answer four alignment questions: Context, What-extends, What-removes, Re…** _(commonwealth-ai-system-overview sec_00028)_  
> Every plan file under `~/.claude/`plans/*.md` must answer four alignment questions: Context, What-extends, Wha-t-removes, Rstraint patterns, Could-thi-s-be-less.

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**7. normative claim _(no anchor)_ — The Embed slot stays on its own `Arc<EmbedSlot>` and is never folded into chat.** _(commonwealth-ai-system-overview sec_00030)_  
> The Embed slot stays
on its own `Arc<`

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**8. normative claim _(no anchor)_ — The daemon does not auto-resolve 'fast' or 'primary' slot aliases on `/v1/chat/completions`; callers must pass actual mo…** _(commonwealth-ai-system-overview sec_00030)_  
> The daemon does not auto-resolve 'fast' or 'primary' slot aliases on `/v1/chat/completions`; callers must pass actual model names from config.

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**9. normative claim _(no anchor)_ — AppState::with_*` installers must run before `inner.clone()` in `EmbeddedDaemon::start_daemon`, otherwise `Arc::get_mut…** _(commonwealth-ai-system-overview sec_00030)_  
> `AppS

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**10. normative claim _(no anchor)_ — Requests to POST /v1/chat/completutions with LocalOnly privacy must return a 400 status code.** _(commonwealth-ai-system-overview sec_00042)_  
> `LocalOnly` privacy → 400.`GET  /v1/models`Loaded models w/ capabilities and performance estimates

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**11. normative claim _(no anchor)_ — A pinned listener-shape test exists at admin_http::tests::loopback_guard_works_under_production_listener_shape to verify…** _(commonwealth-ai-system-overview sec_00042)_  
> and a pinned listener-shape test (`admin_http::tests::loopback_guard_works_un der_production_listener_shape`). The listener must use `.into_make_service_with_connect_info::<SocketAddr>()` in `daemon::start_daemon` — bare `axum::serve` leaves `ConnectInfo` absent and the guards fail closed for every caller.

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**12. normative claim _(no anchor)_ — The listener configuration in daemon::start_daemon must invoke .into_make_service_with_connect_info::<SocketAddr>().** _(commonwealth-ai-system-overview sec_00042)_  
> The listener must use `.into_make_service_with_connect_infor::<SocketAddr()`

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**13. normative claim _(no anchor)_ — Sensitive folders never contribute to pre-turn ambient context but remain searchable on explicit query and in Inner Work…** _(commonwealth-ai-system-overview sec_00025)_  
> soa sensitiv efoldernevercontributestopre-t urnambientcontext，whileremainingsearchableonexplicitqueryandinInnerWorkmode(§ 4.15).

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**14. normative claim _(no anchor)_ — SYSTEM_OVERVIEW.md must describe what exists today, not what was planned or what's aspirational.** _(commonwealth-ai-arch-principles sec_00003)_  
> It must describe what exists today, notwhat was planned or what'saspirational.

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**15. normative claim _(no anchor)_ — File paths are assertions that must verify in SYSTEM_OVERVIEW.md.** _(commonwealth-ai-arch-principles sec_00003)_  
> File paths, tool counts, enum variants, CLI subcommands, HTTP routes — allare assertions, allmustverify.

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**16. normative claim _(no anchor)_ — Tool counts are assertions that must verify in the document.** _(commonwealth-ai-arch-principles sec_00003)_  
> File paths, tool counts,enumvariants,CLIsubcommands,HTTProutes—allareassertions,allmustverify.

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**17. normative claim _(no anchor)_ — Enum variants are assertions that must verify.** _(commonwealth-ai-arch-principles sec_00003)_  
> Filepaths,toolcounts,enumsvariants,CLIsubcommands,HTTProutes—allasertions,allmustverify.

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**18. normative claim _(no anchor)_ — CLI subcommands are assertions that must verify.** _(commonwealth-ai-arch-principles sec_00003)_  
> File paths,tool counts,enum variants, CLI subcommands,HTTP routes—allareassertions, allmustverify.

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**19. normative claim _(no anchor)_ — HTTP routes are assertions that must verify.** _(commonwealth-ai-arch-principles sec_00003)_  
> Filepaths,toolcounts,enumvariants,CLIsubcommands，HTTProutes—allasertions，allmustverify.

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**20. normative claim _(no anchor)_ — Feature docs should be phrased as prose honest about tradeoffs for a human, and must not cram implementation detail in t…** _(commonwealth-ai-arch-principles sec_00004)_  
> 

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**21. normative claim _(no anchor)_ — Good inline comments must name an invariant, a mechanism, and the consequence of breaking it.** _(commonwealth-ai-arch-principles sec_00005)_  
> Good comment: `// Hold the per-view mutex...` — names an invariant, a mechanism, and consequence...

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**22. normative claim _(no anchor)_ — Tests must be landed before moving code during refactoring.** _(commonwealth-ai-arch-principles sec_00009)_  
> Land the test first, then move.

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**23. normative claim _(no anchor)_ — Use write_note when ruling out an approach, discovering a constraint that must not be violated, making a choice with rea…** _(commonwealth-ai-arch-principles sec_00021)_  
> Use `write_note` when:

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**24. normative claim _(no anchor)_ — Additions must cite the PR, commit, or file associated with a real incident.** _(commonwealth-ai-arch-principles sec_00026)_  
> Cite the PR, commit, or file.

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**25. normative claim _(no anchor)_ — When string-id constants are part of the wire API (appearing on disk, in gossip, or in persisted config), they must be k…** _(commonwealth-ai-arch-principles sec_00028)_  
> Legitimate exception: when the strings appear on disk, in gossip, or persisted config, they're a wire contract. Keep the `pub const &str` form as an alias, but make it a `const fn` call so the enum stays the source of truth:

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**26. normative claim _(no anchor)_ — A test must exist that pins the equivalence between string-id constants and their corresponding enum ID calls.** _(commonwealth-ai-arch-principles sec_00028)_  
> A test pins the equivalence: #[test] fn legacy_view_id_constants_match_view() { assert_eq!(VIEW_PERSONAL_KNOWLEDGE, ViewKind::Personal.id()); }

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**27. normative claim _(no anchor)_ — Files with more than 1200 lines must be split, unless an exception is documented in §12 of SYSTEM_OVERVIEW.md.** _(commonwealth-ai-arch-principles sec_00031)_  
> > 1200 Split. No exceptions that aren't already documented in §12 of SYSTEM_overview.md.

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**28. normative claim _(no anchor)_ — Pure helpers like `estimate_tokens` and `format_landscape` must be moved before any code with I/O during refactoring.** _(commonwealth-ai-arch-principles sec_00032)_  
> Pick pure helpers first. Move `estimate_tokens` and `formatlandscape` before you move anything with I/O; they're the easiest to test in isolation and the safest to relocate.

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**29. normative claim _(no anchor)_ — External callers import KnowledgeViewManager from manager.rs, which never changes after a split.** _(commonwealth-ai-arch-principles sec_00032)_  
> Keep the public façade intact. External callers import `KnowledgeViewManager` from `manager.rs` — that never changed.

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**30. normative claim _(no anchor)_ — Tests must be moved along with their corresponding code artifacts rather than remaining in the original file.** _(commonwealth-ai-arch-principles sec_00032)_  
> Move the tests with the code. A test for `format_landscape` belongs in `digest.rs`, not in `manager.rs` after the move.

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**31. normative claim _(no anchor)_ — You never edit a match arm to add a thing; you call `register`.** _(commonwealth-ai-arch-principles sec_00034)_  
> You never edit a match arm to adding a thing; you call register.

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**32. normative claim _(no anchor)_ — The KnowledgeView feature promises that the three-map corpora never leave the user's machine.** _(commonwealth-ai-arch-principles sec_00045)_  
> The KnowledgeView feature promises that the four-map corpora never leave

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**33. normative claim _(no anchor)_ — A debug_assert!** _(commonwealth-ai-arch-principles sec_00047)_  
> It's insufficient for "user data must not leak."

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**34. normative claim _(no anchor)_ — There must be one version of tokio, serde, and reqwest per workspace.** _(commonwealth-ai-arch-principles sec_00049)_  
> One version of `tokio`, one version of `serde`, one version of `reqwest` per workspace.

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**35. normative claim _(no anchor)_ — Every non-obvious decision must emit a tracing event.** _(commonwealth-ai-arch-principles sec_00054)_  
> 9.1 Every non-obvious decision emits a tracing event

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**36. normative claim _(no anchor)_ — error!` is for a bug or an unrecoverable condition the operator must address.** _(commonwealth-ai-arch-principles sec_00055)_  
> `error!` — a bug or an unrecoverable

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

## Provenance & Evolution (3306 of 3398 atoms enriched)

_Repo `/Users/alexsbryan/dev/commonwealth-ai` · 73 co-evolution pairs · 2499 fresh / 807 moved · renames not followed in v1._

**Stability highlights** _(load-bearing — held longest unchanged)_

- `sovereign/crates/sovereign-store/src/sqlite.rs` · 36 days · 20 commits · alexbryan01@gmail.com, alexsbryan@gmail.com
- `sovereign/crates/sovereign-store/src/sqlite.rs` · 36 days · 20 commits · alexbryan01@gmail.com, alexsbryan@gmail.com
- `sovereign/crates/sovereign-desktop/src-tauri/src/main.rs` · 35 days · 44 commits · alexsbryan@gmail.com, alexbryan01@gmail.com
- `sovereign/crates/sovereign-desktop/src-tauri/src/state.rs` · 35 days · 50 commits · alexbryan01@gmail.com, alexsbryan@gmail.com
- `sovereign/crates/sovereign-store/src/postgres.rs` · 35 days · 11 commits · alexbryan01@gmail.com, alexsbryan@gmail.com

**Recent volatility** _(currently active surfaces)_

- `commonwealth/crates/commonwealth-api/src/routes_inference.rs` · last touched 2026-05-12 by alexsbryan@gmail.com — "more mesh fixes"
- `commonwealth/crates/commonwealth-api/src/routes_internal/mod.rs` · last touched 2026-05-12 by alexsbryan@gmail.com — "feat(mesh): peer-to-peer GGUF distribution over /internal/v1…"
- `commonwealth/crates/commonwealth-api/src/server.rs` · last touched 2026-05-12 by alexsbryan@gmail.com — "feat(mesh): peer-to-peer GGUF distribution over /internal/v1…"
- `commonwealth/crates/commonwealth-api/src/state.rs` · last touched 2026-05-12 by alexsbryan@gmail.com — "feat(mesh): peer-to-peer GGUF distribution over /internal/v1…"
- `corpus-engine/src/lib.rs` · last touched 2026-05-12 by alexsbryan@gmail.com — "peer inference fixes"

**Co-evolution clusters** _(implicit coupling)_

- `sovereign/crates/sovereign-tools/src/code/callees.rs` ↔ `sovereign/crates/sovereign-tools/src/code/callers.rs` · 100% (7 of 7)
- `sovereign/.sovereign/notes.db-shm` ↔ `sovereign/.sovereign/notes.db-wal` · 100% (6 of 6)
- `commonwealth/crates/commonwealth-core/src/capabilities.rs` ↔ `commonwealth/crates/commonwealth-discovery/src/gossip_service.rs` · 100% (5 of 5)
- `corpus-engine/src/enrichment/atlas/atoms.rs` ↔ `corpus-engine/src/enrichment/atlas/writer.rs` · 100% (5 of 5)
- `corpus-engine/src/enrichment/domains/conversational.rs` ↔ `corpus-engine/src/enrichment/domains/personal.rs` · 100% (5 of 5)

**Staleness queue** (807) _candidates for re-extraction — code touched since atlas built_

- `commonwealth/crates/commonwealth-api/src/auto_recover.rs` · last touched 2026-05-10
- `commonwealth/crates/commonwealth-api/src/middleware/mod.rs` · last touched 2026-05-10
- `commonwealth/crates/commonwealth-api/src/middleware/approval_gate.rs` · last touched 2026-05-10
- `commonwealth/crates/commonwealth-api/src/middleware/context_injector.rs` · last touched 2026-05-10
- `commonwealth/crates/commonwealth-api/src/middleware/decision_extractor.rs` · last touched 2026-05-10
- _+802 more (see git_archaeology.json)_

---
_Per-finding detail in the JSON sidecar._
