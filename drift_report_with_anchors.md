# Drift Report — 20 actionable · 0 confirmed · 859 queued

**Code**: `commonwealth-ai-self-atlas`  ·  **Narrative**: `commonwealth-ai-system-overview`, `commonwealth-ai-arch-principles`

## Act on

**1. normative claim _(anchor `Architecture Roadmap` not in atlas)_ — Work intentionally deferred must be listed in the Architecture Roadmap so that the next engineer inherits a todo list ra…** _(commonwealth-ai-system-overview sec_00002)_  
> Listed here so the next engineer inherits a todo list

_Next step:_ Search the codebase for `Architecture Roadmap`; if present, the structural atlas is missing this symbol (separate fix). If absent, the principle is stale — revise or implement.

**2. normative claim _(anchor `~/.claude/` not in atlas)_ — Every plan file under ~/.claude/plans/*.md must answer four alignment questions: Context, What-extends, What-removes, Re…** _(commonwealth-ai-system-overview sec_00028)_  
> Every plan file under `~/.claude/`plans/*.md` must answer four alignment questions: Context, What-extends, Wha-t-removes, Rstraint patterns, Could-thi-s-be-less.

_Next step:_ Search the codebase for `~/.claude/`; if present, the structural atlas is missing this symbol (separate fix). If absent, the principle is stale — revise or implement.

**3. normative claim _(anchor `Arc<EmbedSlot>` not in atlas)_ — The Embed slot stays on its own `Arc<EmbedSlot>` and is never folded into chat.** _(commonwealth-ai-system-overview sec_00030)_  
> The Embed slot stays
on its own `Arc<`

_Next step:_ Search the codebase for `Arc<EmbedSlot>`; if present, the structural atlas is missing this symbol (separate fix). If absent, the principle is stale — revise or implement.

**4. normative claim _(anchor `/v1/chat/completions` not in atlas)_ — The daemon does not auto-resolve 'fast' or 'primary' slot aliases on `/v1/chat/completions`; callers must pass actual mo…** _(commonwealth-ai-system-overview sec_00030)_  
> The daemon does not auto-resolve 'fast' or 'primary' slot aliases on `/v1/chat/completions`; callers must pass actual model names from config.

_Next step:_ Search the codebase for `/v1/chat/completions`; if present, the structural atlas is missing this symbol (separate fix). If absent, the principle is stale — revise or implement.

**5. normative claim _(anchor `LocalOnly` not in atlas)_ — Requests to POST /v1/chat/completutions with LocalOnly privacy must return a 400 status code.** _(commonwealth-ai-system-overview sec_00042)_  
> `LocalOnly` privacy → 400.`GET  /v1/models`Loaded models w/ capabilities and performance estimates

_Next step:_ Search the codebase for `LocalOnly`; if present, the structural atlas is missing this symbol (separate fix). If absent, the principle is stale — revise or implement.

**6. normative claim _(anchor `/internal/latency/probe` not in atlas)_ — A pinned listener-shape test exists at admin_http::tests::loopback_guard_works_under_production_listener_shape to verify…** _(commonwealth-ai-system-overview sec_00042)_  
> and a pinned listener-shape test (`admin_http::tests::loopback_guard_works_un der_production_listener_shape`). The listener must use `.into_make_service_with_connect_info::<SocketAddr>()` in `daemon::start_daemon` — bare `axum::serve` leaves `ConnectInfo` absent and the guards fail closed for every caller.

_Next step:_ Search the codebase for `/internal/latency/probe`; if present, the structural atlas is missing this symbol (separate fix). If absent, the principle is stale — revise or implement.

**7. normative claim _(anchor `.into_make_service_with_connect_info::<SocketAddr>()` not in atlas)_ — The listener configuration in daemon::start_daemon must invoke .into_make_service_with_connect_info::<SocketAddr>().** _(commonwealth-ai-system-overview sec_00042)_  
> The listener must use `.into_make_service_with_connect_infor::<SocketAddr()`

_Next step:_ Search the codebase for `.into_make_service_with_connect_info::<SocketAddr>()`; if present, the structural atlas is missing this symbol (separate fix). If absent, the principle is stale — revise or implement.

**8. normative claim _(no anchor)_ — Sensitive folders never contribute to pre-turn ambient context but remain searchable on explicit query and in Inner Work…** _(commonwealth-ai-system-overview sec_00025)_  
> soa sensitiv efoldernevercontributestopre-t urnambientcontext，whileremainingsearchableonexplicitqueryandinInnerWorkmode(§ 4.15).

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**9. normative claim _(anchor `SYSTEM_OVERVIEW.md` not in atlas)_ — SYSTEM_OVERVIEW.md must describe what exists today, not what was planned or what's aspirational.** _(commonwealth-ai-arch-principles sec_00003)_  
> It must describe what exists today, notwhat was planned or what'saspirational.

_Next step:_ Search the codebase for `SYSTEM_OVERVIEW.md`; if present, the structural atlas is missing this symbol (separate fix). If absent, the principle is stale — revise or implement.

**10. normative claim _(anchor `FILE PATHS` not in atlas)_ — File paths are assertions that must verify in SYSTEM_OVERVIEW.md.** _(commonwealth-ai-arch-principles sec_00003)_  
> File paths, tool counts, enum variants, CLI subcommands, HTTP routes — allare assertions, allmustverify.

_Next step:_ Search the codebase for `FILE PATHS`; if present, the structural atlas is missing this symbol (separate fix). If absent, the principle is stale — revise or implement.

**11. normative claim _(anchor `TOOL COUNTS` not in atlas)_ — Tool counts are assertions that must verify in the document.** _(commonwealth-ai-arch-principles sec_00003)_  
> File paths, tool counts,enumvariants,CLIsubcommands,HTTProutes—allareassertions,allmustverify.

_Next step:_ Search the codebase for `TOOL COUNTS`; if present, the structural atlas is missing this symbol (separate fix). If absent, the principle is stale — revise or implement.

**12. normative claim _(anchor `ENUM VARIANTS` not in atlas)_ — Enum variants are assertions that must verify.** _(commonwealth-ai-arch-principles sec_00003)_  
> Filepaths,toolcounts,enumsvariants,CLIsubcommands,HTTProutes—allasertions,allmustverify.

_Next step:_ Search the codebase for `ENUM VARIANTS`; if present, the structural atlas is missing this symbol (separate fix). If absent, the principle is stale — revise or implement.

**13. normative claim _(anchor `CLI SUBCOMMANDS` not in atlas)_ — CLI subcommands are assertions that must verify.** _(commonwealth-ai-arch-principles sec_00003)_  
> File paths,tool counts,enum variants, CLI subcommands,HTTP routes—allareassertions, allmustverify.

_Next step:_ Search the codebase for `CLI SUBCOMMANDS`; if present, the structural atlas is missing this symbol (separate fix). If absent, the principle is stale — revise or implement.

**14. normative claim _(anchor `HTTP ROUTES` not in atlas)_ — HTTP routes are assertions that must verify.** _(commonwealth-ai-arch-principles sec_00003)_  
> Filepaths,toolcounts,enumvariants,CLIsubcommands，HTTProutes—allasertions，allmustverify.

_Next step:_ Search the codebase for `HTTP ROUTES`; if present, the structural atlas is missing this symbol (separate fix). If absent, the principle is stale — revise or implement.

**15. normative claim _(no anchor)_ — Feature docs should be phrased as prose honest about tradeoffs for a human, and must not cram implementation detail in t…** _(commonwealth-ai-arch-principles sec_00004)_  
> 

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**16. normative claim _(anchor `// Hold the per-view mutex across the entire enrichment. Prevents two overlapping enrichment runs from racing on the skeleton.json write or the LanceDB checkpoint.` not in atlas)_ — Good inline comments must name an invariant, a mechanism, and the consequence of breaking it.** _(commonwealth-ai-arch-principles sec_00005)_  
> Good comment: `// Hold the per-view mutex...` — names an invariant, a mechanism, and consequence...

_Next step:_ Search the codebase for `// Hold the per-view mutex across the entire enrichment. Prevents two overlapping enrichment runs from racing on the skeleton.json write or the LanceDB checkpoint.`; if present, the structural atlas is missing this symbol (separate fix). If absent, the principle is stale — revise or implement.

**17. normative claim _(no anchor)_ — Tests must be landed before moving code during refactoring.** _(commonwealth-ai-arch-principles sec_00009)_  
> Land the test first, then move.

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**18. normative claim _(anchor `SYSTEM_OVERVIEW.md` not in atlas)_ — Files with more than 1200 lines must be split, unless an exception is documented in §12 of SYSTEM_OVERVIEW.md.** _(commonwealth-ai-arch-principles sec_00031)_  
> > 1200 Split. No exceptions that aren't already documented in §12 of SYSTEM_overview.md.

_Next step:_ Search the codebase for `SYSTEM_OVERVIEW.md`; if present, the structural atlas is missing this symbol (separate fix). If absent, the principle is stale — revise or implement.

**19. normative claim _(no anchor)_ — The KnowledgeView feature promises that the three-map corpora never leave the user's machine.** _(commonwealth-ai-arch-principles sec_00045)_  
> The KnowledgeView feature promises that the four-map corpora never leave

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**20. normative claim _(anchor `tracing::debug!` not in atlas)_ — Every non-obvious decision must emit a tracing event.** _(commonwealth-ai-arch-principles sec_00054)_  
> 9.1 Every non-obvious decision emits a tracing event

_Next step:_ Search the codebase for `tracing::debug!`; if present, the structural atlas is missing this symbol (separate fix). If absent, the principle is stale — revise or implement.

## Provenance & Evolution (6114 of 6230 atoms enriched)

_Repo `/Users/alexsbryan/dev/commonwealth-ai` · 73 co-evolution pairs · 6114 fresh / 0 moved · renames not followed in v1._

**Stability highlights** _(load-bearing — held longest unchanged)_

- `sovereign/crates/sovereign-cli/src/main.rs` · 41 days · 50 commits · alexsbryan@gmail.com, alexbryan01@gmail.com, alexsbryan@Alexs-MacBook-Pro-2.local
- `sovereign/crates/sovereign-core/src/runtime.rs` · 41 days · 74 commits · alexbryan01@gmail.com, alexsbryan@gmail.com
- `sovereign/crates/sovereign-core/src/traits.rs` · 41 days · 34 commits · alexbryan01@gmail.com, alexsbryan@gmail.com
- `sovereign/crates/sovereign-inference/src/lib.rs` · 41 days · 11 commits · alexsbryan@gmail.com, alexbryan01@gmail.com
- `sovereign/crates/sovereign-inference/src/embedded.rs` · 41 days · 57 commits · alexsbryan@gmail.com, alexbryan01@gmail.com

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

## Investigation queue (859)

Most are matcher-coverage gaps, not real drift. Promote any to Act-on if you disagree:

- **method/function** (5): `corpus_engine::extractors::html`, `corpus_engine::acquirers::http_api::template`, `corpus_engine::extractors::code`
- **constant/identifier** (854): `sovereign_core::traits::InferenceProvider`, `sovereign_core::traits::StateStore`, `corpus_engine::enrichment::domain::Domain`

---
_Per-finding detail in the JSON sidecar._
