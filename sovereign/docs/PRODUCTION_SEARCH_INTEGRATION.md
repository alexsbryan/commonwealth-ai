# Production web-search integration — plan

**Status**: gym-validated at 90% on the existing mock backend (search-gym
v8, 2026-05-19). Productionization is the next workstream — wire the
gym-proven pieces (URL constraint, fast-slot routing, judge prompt) onto
a real backend (Tavily first), behind the abstractions ARCH_PRINCIPLES
calls for, and validate end-to-end against live queries.

## What this is for

The gym proved the search-during-inference behaviors hold under mock
backends:

- Routing decisions (search vs. skip)
- Citation faithfulness (URLs match tool results)
- Anti-fabrication (grammar constraint blocks invented URLs)
- Anti-stale-knowledge (model uses tool results over training data)
- Zero-results honesty (model says "no results" rather than confabulating)

Productionization is about (a) swapping mocks for real backends, (b)
wiring the URL-allowlist accumulation into the production agent loop
(currently only the gym runner does it), (c) consolidating the search
system prompt to one asset that both gym and production read from, and
(d) ensuring privacy + cost are enforced structurally per ARCH §7.

## What's landed

| Piece | Location | State |
|---|---|---|
| URL allowlist constraint | `sovereign-inference/src/url_constraint.rs` | shipped (2026-05-19) |
| Constraint wired to sampler | `sovereign-inference/src/embedded.rs::build_sampler` | shipped |
| Request field plumbing | `sovereign-core/src/types.rs::CompletionRequest.url_allowlist` | shipped |
| HTTP wire field | `commonwealth-api/src/openai_types.rs::ChatCompletionRequest.url_allowlist` | shipped |
| Gym runner accumulation | `sovereign-cli/src/search_gym_cmd/runner.rs` | shipped |
| Fast-slot mesh alias | `sovereign-mesh/src/oicp_synthesis.rs::build_self_manifest` | shipped |
| Judge T=0 + grounding | `sovereign-cli/src/search_gym_cmd/judge.rs` | shipped |
| `SearchBackend` enum (4 variants) | `sovereign-tools/src/web/search.rs:18` | exists; needs refactor |
| Tavily implementation | `sovereign-tools/src/web/search.rs::search_tavily` | exists; not productionized |
| Desktop wiring | `sovereign-desktop/src-tauri/src/state.rs:968` | exists; predates orchestrator |

## What needs to change

The existing `SearchBackend` enum has 4 variants (`DuckDuckGo`, `Brave`,
`Tavily`, `Mock`) dispatched via a `match` in a free function. Per
ARCH §4 ("when new implementations of a trait are expected to appear over
time — domains, middleware, tools, exporters — dispatch through a
registry, not a match on an id string"), this needs to become a trait +
registry. The set is open (Kagi, Google CSE, internal corpus, BYOM
enterprise search will all want in over time).

There is no orchestrator layer. Selection of backend per call,
privacy filtering, budget gating, cache lookup, and fallback chain all
need to live somewhere. Currently they don't live anywhere — the gym
hardcodes `SearchBackend::Mock`, the desktop hardcodes Tavily.

The URL-allowlist accumulation logic only lives in the gym runner. The
production agent loop (whoever owns the multi-turn tool-call → synthesis
flow) doesn't accumulate URLs across turns or inject them into
subsequent requests. The constraint is structurally ready and proven; it
just isn't reached in production.

The search system prompt is duplicated inside every gym fixture's
`input.json`. Production chat assembly has no canonical copy. Anyone
tuning the prompt has to edit it in 10+ fixtures, and production drifts
from what the gym validated.

## Six phases — ordered

Each phase is a separate PR per ARCH §14.1. Each phase is
behavior-preserving against the gym (regression gate). Phase 0–2 build
the abstraction; Phase 3 wires the production agent loop (this is the
load-bearing piece — the constraint is useless until the loop accumulates
URLs); Phase 4 moves the prompt to data; Phase 5 ships the real backend
and its tests; Phase 6 migrates desktop.

### Phase 0 — refactor `SearchBackend` enum → trait + registry

ARCH §4 (registry pattern) + §5 (interface segregation) + §10.1
(behavior-preserving refactor).

**Files:**

- `sovereign-tools/src/web/search/mod.rs` (new) — trait definitions,
  `SearchResult`, `SearchError`, `SearchPrivacy`, `SearchCost`
- `sovereign-tools/src/web/search/registry.rs` (new) — `WebSearchRegistry`
  per the canonical shape at ARCH §4.1
- `sovereign-tools/src/web/search/backends/mod.rs` (new) — module barrel
- `sovereign-tools/src/web/search/backends/mock.rs` (new) — extracted from
  current `search_mock` + supporting `MockResponse`/`MockAliasEntry`/etc.
- `sovereign-tools/src/web/search/backends/duckduckgo.rs` (new) — extracted
  from current `search_duckduckgo` + parsers + fallback chain
- `sovereign-tools/src/web/search/backends/brave.rs` (new) — extracted from
  current `search_brave`
- `sovereign-tools/src/web/search/backends/tavily.rs` (new) — extracted from
  current `search_tavily`
- `sovereign-tools/src/web/search.rs` — `pub use` shim during migration so
  external callers (`sovereign-desktop/src-tauri/src/state.rs:968`,
  `sovereign-cli/src/search_gym_cmd/runner.rs`) don't break; removed in a
  later PR per ARCH §10.2

**Trait shape (minimal per ARCH §5.1):**

```rust
#[async_trait]
pub trait WebSearchBackend: Send + Sync {
    async fn search(&self, query: &str, max_results: usize)
        -> Result<Vec<SearchResult>, SearchError>;
    fn id(&self) -> &'static str;
    fn privacy(&self) -> SearchPrivacy;
    fn cost_estimate(&self) -> Option<SearchCost>;
}
```

`SearchPrivacy` + `SearchCost` may be `Local` / no-cost stubs for now;
Phase 1 fills them in concretely.

**Done when:**
- `cargo check --workspace --features corpus-engine/treesitter` is green
- `cargo test -p sovereign-tools --test e2e_web_search` (existing) passes
- `sovereign search-gym run --replays 5` matches v8 pass rate exactly
  (45/50 = 90.0%) — regression gate
- `cargo build -p sovereign-desktop` succeeds (the shim keeps it
  compiling without modification)

### Phase 1 — privacy + cost capabilities

ARCH §7.1 (structural privacy) + §7.2 (test-pinned invariants).

**Enums:**

```rust
pub enum SearchPrivacy {
    Local,                            // Mock, internal corpus
    Mesh,                             // federated to known peers
    External { provider: &'static str }, // Tavily, Brave, DDG
}

pub struct SearchCost {
    pub units_per_call: u32,
    pub denomination: &'static str,   // "tavily-credits", etc.
}
```

**Backend declarations** (in each impl file from Phase 0):

- `MockBackend::privacy() = Local`
- `InternalCorpusBackend::privacy() = Local` (when added in Phase 5+)
- `DuckDuckGoBackend::privacy() = External { provider: "duckduckgo" }`
- `BraveBackend::privacy() = External { provider: "brave" }`
- `TavilyBackend::privacy() = External { provider: "tavily" }`

**Test (ARCH §7.2):**

```rust
#[test]
fn external_backends_declare_correct_provider_id() {
    let b = TavilyBackend::new("dummy".into());
    assert!(matches!(
        b.privacy(),
        SearchPrivacy::External { provider: "tavily" }
    ));
}

#[test]
fn local_backends_declare_local_privacy() {
    let b = MockBackend::new(PathBuf::new());
    assert!(matches!(b.privacy(), SearchPrivacy::Local));
}
```

**Done when:** all backends implement `privacy()` correctly; both tests
pass; gym regression gate still 45/50.

### Phase 2 — orchestrator

`sovereign-tools/src/web/search/orchestrator.rs` (new). Holds the
selection policy. Single-responsibility (ARCH §3): pick a backend,
respect privacy and budget, emit tracing, return the chosen backend
plus the chosen result set.

**Selection inputs (struct passed to `select()`):**

```rust
pub struct SelectInputs<'a> {
    pub query: &'a str,
    pub max_results: usize,
    pub max_privacy: SearchPrivacy,   // from request OICP context
    pub budget: &'a BudgetView,       // per-backend remaining
    pub prefer: &'a [&'a str],        // operator-configured order
}
```

**Algorithm:**
1. Take the registry's full backend list.
2. Filter: `backend.privacy() <= inputs.max_privacy` (Local ≤ Mesh ≤
   External).
3. Filter: drop External backends whose remaining budget is 0.
4. Order by `inputs.prefer`; unranked backends sort last.
5. Try each in order. On `Err`, log + fall through to the next.
6. If all fail, return a synthetic 0-results response and emit `warn!`
   per ARCH §9.2 — this is a recoverable degradation.

**Per ARCH §9.1, every selection emits:**

```rust
tracing::debug!(
    backend = %chosen.id(),
    candidates = ?candidate_ids,
    privacy_filter = ?inputs.max_privacy,
    budget_remaining = ?budget_snapshot,
    "search: backend selected"
);
```

**Tests pinning the invariants (ARCH §7.2 + §12.2):**

```rust
#[test]
fn local_only_request_never_reaches_external_backend() {
    let r = registry_with(vec![tavily_stub(), local_internal_stub()]);
    let chosen = Orchestrator::new(r)
        .select(SelectInputs { max_privacy: SearchPrivacy::Local, .. })
        .unwrap();
    assert_eq!(chosen.id(), "internal");
}

#[test]
fn external_drop_when_budget_zero() {
    let r = registry_with(vec![tavily_stub()]);
    let budget = BudgetView::all_zero();
    let result = Orchestrator::new(r).select(SelectInputs { budget: &budget, .. });
    assert!(result.is_err() || result.unwrap().id() != "tavily");
}
```

**Done when:** orchestrator compiles + tests pass; gym still 45/50 (gym
keeps using direct backend dispatch; orchestrator isn't on the gym path
yet).

### Phase 3 — wire url_allowlist into the production agent loop

The load-bearing piece. The gym runner accumulates URLs at
`sovereign-cli/src/search_gym_cmd/runner.rs:343-356` and injects them at
`runner.rs:217-232`. Production needs the same.

**Where**: needs a precise read of the multi-turn agent path. Candidates:

- `commonwealth-api/src/routes_inference.rs` — the OpenAI chat handler.
  Tool calls return tool_results and the handler synthesizes the
  follow-up request. If this is where multi-turn loops live, it's the
  insertion point.
- `sovereign-desktop` — if the desktop owns its own loop (rather than
  delegating to the daemon's), it needs its own injection.
- Both, if the architecture is "daemon handles a single tool round-trip
  per request" with the client looping.

Read first; insert second. The wrong locus is a wasted PR.

**Shape (mirror the gym runner):**

```rust
// per-conversation state, persisted across turns
struct AgentLoopState {
    accumulated_urls: Vec<String>,
}

// after each tool result:
if let Some(urls) = extract_urls_from_search_result(&tool_result) {
    state.accumulated_urls.extend(urls);
}

// before next CompletionRequest:
next_request.url_allowlist = Some(state.accumulated_urls.clone());
```

**Done when:** a multi-turn curl test (POST → tool call → return synthetic
search result → POST → assert daemon log emits `url_allowlist constraint
constructed` with the right URL count) passes against the live daemon.

### Phase 4 — search system prompt as asset (ARCH §6)

Currently the prompt is duplicated inside every gym fixture's
`input.json`. Move to:

```
sovereign-tools/src/web/search/assets/system_prompt.md
sovereign-tools/src/web/search/assets/tool_description.md
sovereign-tools/src/web/search/assets/default_backends.toml
```

Loaded via `include_str!` per ARCH §6.2. Asset lives alongside its
consumer (one grep hop from "where does this string come from?").

**Gym fixture migration:** strip the system prompt from each
`fixtures/*/input.json` system message; runner injects the asset's
content before POSTing. A `--system-prompt-from-asset` flag (default on)
controls the behavior so historical fixtures with inline prompts still
work for back-compat.

**default_backends.toml** — operator-tunable selection rules per ARCH
§6.1 ("default configuration that an operator might reasonably tune"):

```toml
[selection]
prefer = ["internal", "tavily", "brave", "duckduckgo"]

[budget]
tavily.daily_calls = 100
brave.daily_calls = 500

[privacy]
default_max = "external"
```

**Done when:** gym pass rate unchanged (asset content == old inline
content for the regression baseline); operator can edit the asset and
see the new prompt on next daemon restart.

### Phase 5 — Tavily real-network e2e test

The validation gate for the workstream. Per the recap message: "real
queries against the production system, with data to back the
robustness of the system."

ARCH §12.4 forbids GPU/network/model-weight requirements in tests. The
compromise: feature-flag + env-var double gate.

**File:** `sovereign-tools/tests/tavily_real_e2e.rs`

**Gate:**

```rust
#[tokio::test]
async fn tavily_real_query_returns_citable_results() {
    let key = match std::env::var("SOVEREIGN_TAVILY_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => {
            tracing::info!("tavily_real_e2e: skipped — no SOVEREIGN_TAVILY_API_KEY");
            return;
        }
    };
    // ... actual test ...
}
```

No feature flag needed if the env-var gate is reliable — every dev box
that wants the test runs `export SOVEREIGN_TAVILY_API_KEY=...`; CI doesn't
set it and the test skips silently with an `info!`. Per ARCH §9.4 the
silence is observable in logs.

**Test shape:**

```rust
let backend = TavilyBackend::new(key);

// 1. Real API call, real network.
let results = backend.search("rust programming language", 5).await
    .expect("tavily call should succeed with valid key");
assert!(!results.is_empty(), "tavily returned empty for a popular query");
for r in &results {
    assert!(r.url.starts_with("https://"), "url not https: {}", r.url);
    assert!(!r.title.is_empty());
    assert!(!r.snippet.is_empty());
}

// 2. End-to-end: feed results to orchestrator + url_allowlist
//    path, run a single round-trip against the local daemon, verify
//    the model only cites URLs from the real results.
let urls: Vec<String> = results.iter().map(|r| r.url.clone()).collect();
let synthesis = post_to_daemon_with_tool_result_and_allowlist(
    &results, &urls
).await.unwrap();
for cited in extract_urls(&synthesis) {
    assert!(
        urls.iter().any(|u| u == &cited),
        "model cited URL not in real-Tavily allowlist: {cited}"
    );
}
```

**Sister test that runs ALWAYS** (no key needed):
`tavily_request_shape_matches_api` in the same file. Uses `mockito` or
`httpmock` to spin up a stub that returns canned Tavily JSON; asserts our
backend serializes the request correctly and parses the response shape.
This catches API drift without burning credits.

**Done when:** sister test passes on CI; real-network test passes locally
with a key and skips silently without one. The real-network test is the
"data to back robustness" deliverable.

### Phase 6 — desktop wiring

Migrate `sovereign-desktop/src-tauri/src/state.rs:968` from constructing
`SearchBackend::Tavily { api_key }` directly to constructing the registry
from `default_backends.toml` + user API keys + the orchestrator. The
url_allowlist threading from Phase 3 needs to live in whatever desktop
multi-turn loop exists.

**Done when:** desktop opens a chat, asks a question, the daemon log
shows `search: backend selected backend=tavily` followed by
`url_allowlist constraint constructed`, and the final answer cites only
Tavily-returned URLs.

## Risk register

| Risk | Mitigation |
|---|---|
| **Phase 0 silently breaks `sovereign-desktop`** (the desktop imports `SearchBackend` directly today) | Keep `sovereign-tools/src/web/search.rs` as a `pub use` shim during Phase 0; remove in a separate PR per ARCH §10.2. `cargo build -p sovereign-desktop` is part of the Phase 0 DoD. |
| **Orchestrator selection diverges from gym expectations** when backends added | Gym uses MockBackend by id `"mock"`; orchestrator's `prefer = ["mock"]` for gym runs keeps it deterministic. New backends added behind operator config don't accidentally win on gym. |
| **Privacy invariant slips when adding a new backend** | Phase 1 test `local_only_request_never_reaches_external_backend` runs on every new backend impl (parametrize the test). Defense in depth per ARCH §7.4: backend declares privacy, orchestrator filters, request OICP gates. |
| **Real-network test flakes on Tavily side** | Sister canned test catches our regressions independently. When real test fails, check canned first — if canned passes, the failure is real (Tavily API changed or our key is bad), not our code. Per ARCH §11.2 separation of "types line up" vs "semantics hold". |
| **URL accumulation grows unboundedly across long conversations** | Cap at N most recent (e.g. 50). Older URLs drop. Either still-valid (model can re-cite the canonical N) or no longer relevant (model needs new search anyway). Pin in Phase 3 with a test. |
| **Budget exhaustion silently degrades to Local** | Per ARCH §9 emit `warn!` on every silent degradation so the operator can correlate "search quality dropped" with "Tavily budget exhausted". |
| **Mock backend leaks into production** | `MockBackend::new` takes a `PathBuf` — in production code paths there's nothing to pass. Per ARCH §7.1 the invariant is structurally enforced. Test pins it: `production_registry_excludes_mock_backend`. |
| **Tavily API key in logs / error messages** | Backend constructor takes the key; `Debug` impl redacts it (`api_key: <redacted>`); error messages from the backend never echo the key; only the provider id (`"tavily"`) is logged. |

## Validation criteria

**Per-phase regression gate:** `sovereign search-gym run --replays 5`
must match v8 pass rate (45/50 = 90.0%) at the end of each phase. No
phase is allowed to regress the gym.

**End-to-end validation (Phase 5):** real Tavily query returns ≥3
results; daemon synthesizes an answer citing only Tavily-returned URLs
(zero fabrications); orchestrator log shows correct backend selected;
budget log shows correct decrement.

**Production smoke (Phase 6):** desktop user types a question; sees a
synthesized answer; can click each citation and reach the cited page;
operator log shows the right backend was selected for the request's
privacy posture.

## File map

```
sovereign-tools/src/web/search/
├── mod.rs              ← Phase 0: trait + types
├── registry.rs         ← Phase 0: WebSearchRegistry
├── orchestrator.rs     ← Phase 2: selection logic
├── backends/
│   ├── mod.rs          ← Phase 0
│   ├── mock.rs         ← Phase 0 (extracted)
│   ├── duckduckgo.rs   ← Phase 0 (extracted)
│   ├── brave.rs        ← Phase 0 (extracted)
│   └── tavily.rs       ← Phase 0 (extracted), Phase 5 (real-network test paired)
└── assets/
    ├── system_prompt.md     ← Phase 4
    ├── tool_description.md  ← Phase 4
    └── default_backends.toml← Phase 4

sovereign-tools/src/web/search.rs  ← Phase 0: pub use shim (removed in a follow-up PR)

commonwealth-api/src/routes_inference.rs OR sovereign-desktop chat path
                                   ← Phase 3: url_allowlist accumulation

sovereign-tools/tests/tavily_real_e2e.rs  ← Phase 5: dual-gated real-network test

sovereign-desktop/src-tauri/src/state.rs:968
                                   ← Phase 6: migrate to registry + orchestrator
```

## Open decisions for the operator

1. **Backend launch order**: Tavily first (matches integration doc's
   prompt shape, generous free tier) or DuckDuckGo first (zero-config but
   HTML-scraping is fragile and rate-limited)?
2. **Cache layer (deferred)**: SQLite-backed query→results cache with
   TTL, or in-memory LRU per-session? Refinement; not a foundation.
3. **Budget granularity**: per-user, per-conversation, or per-node? The
   simplest first cut is per-node global counter that resets daily.
4. **Hard budget exhaustion behavior**: silently fall through to a Local
   backend (graceful degradation) or surface a tool-result error so the
   model can tell the user "search budget exhausted"?

## Reference — related work

- `URL_CONSTRAINT_INTEGRATION.md` — the constraint integration that
  preceded this. Many of the same shape patterns (CompletionRequest
  field, sampler wiring, runner accumulation) apply.
- ARCH_PRINCIPLES §4 (registry), §5 (interface segregation), §6 (data vs
  program), §7 (structural privacy), §9 (observability), §10
  (refactor discipline) — the principles this plan descends from.
- `project_url_constraint_eos_bypass`, `project_fast_slot_alias_advertisement`,
  `project_code_intel_kind_filter` — the three session-of-2026-05-19
  memories the gym validation produced.
