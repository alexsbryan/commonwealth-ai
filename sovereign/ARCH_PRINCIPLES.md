# Architecture Principles — Commonwealth AI

A practical field guide to how code is written and kept in this workspace.
Derived from the drift, rot, and refactor work actually done on `sovereign/`,
`commonwealth/`, `corpus-engine/`, and `oicp-types/` — not from theory. Every
rule cites code you can read and a specific failure it prevents.

**Read this on day one.** Keep it open on day 100. If a pull request violates
a numbered principle, call it out in review — rebaseline principles rarely,
so a violation is either a bug, a missing principle, or a deliberate exception
that deserves its own note.

---

## 0. Operating ethos

Four commitments that underlie every specific rule below.

1. **Glassbox, always.** The people running this system — the user, the
   operator, the next engineer — must be able to see *why* the program did
   what it did without attaching a debugger. If a decision is invisible from
   `tracing=debug`, the decision isn't finished.

2. **Empathy for the next reader.** You will not be at the keyboard when
   someone else has to modify this code. Write for them: name the constraint,
   surface the non-obvious, don't pun with variable names. A comment that
   names the *why* survives refactors; a comment that names the *what* is
   noise.

3. **Tell the truth in the docs.** `SYSTEM_OVERVIEW.md` is the canonical map
   and is expected to be up to date on the commit it appears in. When code
   and docs disagree, code wins at runtime but costs the next engineer a day.
   Update the doc in the same PR as the code change.

4. **Don't whack moles.** A failing test means something. Instrument, repro,
   understand, *then* fix. Disabling a test to get green is a last resort
   that requires a `todo`-kind note explaining what was deferred and why.

These aren't soft — they're load-bearing. Every concrete rule below descends
from one or more of them.

---

## 1. Documentation is an invariant, not a diary

The workspace carries two kinds of prose: the authoritative map
(`sovereign/SYSTEM_OVERVIEW.md`) and per-feature "why" docs (`sovereign/docs/`,
`commonwealth/docs/`). Different rules.

### 1.1 `SYSTEM_OVERVIEW.md` is a contract

- It must describe **what exists today**, not what was planned or what's
  aspirational.
- Anything a new contributor could derive from the doc **must still be true**.
  File paths, tool counts, enum variants, CLI subcommands, HTTP routes — all
  are assertions, all must verify.
- Update it **in the same PR** as the subsystem change. If you added a
  subsystem and didn't update the doc, you didn't finish.
- Failure mode this protects against: in one session we found `SYSTEM_OVERVIEW.md`
  silent on KnowledgeView and ATOS (two production subsystems), wrong about
  the MCP tool count (17 listed, 24 actually registered), missing three
  enrichment domains, and missing five CLI subcommands. Cost: a new engineer
  would have missed *half the surface area*.

### 1.2 Feature docs describe *intent*

Files like `sovereign/docs/knowledge-view.md`, `sovereign/docs/ATOS.md`,
`sovereign/docs/FEATURES.md` exist to tell the reader *why* a feature is the
shape it is. Style: prose, honest about tradeoffs, phrased for a human.
Don't cram implementation detail in them — that rots fast.

### 1.3 Keep inline comments about the *why*

Bad comment: `// loop over the views`. That's what the code says. Bad comment
in review: accepted, because it's cheap.

Good comment: `// Hold the per-view mutex across the entire enrichment.
Prevents two overlapping enrichment runs from racing on the skeleton.json
write or the LanceDB checkpoint.` — names an invariant, a mechanism, and the
consequence of breaking it.

If in doubt, ask: "will a comment here save the next reader from
re-deriving what I just figured out?" If no, drop it.

---

## 2. Type-safe dispatch over stringly-typed

### 2.1 Closed sets belong in an enum

When a symbol appears in N places as a `&str` constant and the full set of
valid values is known, it's a type trying to escape. Make it an enum.

```rust
// Before — knowledge_view/manager.rs, recipes.rs, cross_view.rs all
// hand-rolled string matches against these constants.
pub const VIEW_PERSONAL_KNOWLEDGE: &str = "personal-knowledge";
pub const VIEW_CONVERSATION_HISTORY: &str = "conversation-history";
// …three more files had to keep them in sync.

// After — single source of truth, const-fn accessors, no match cascades.
pub enum ViewKind { Personal, Conversational, Institutional, CrossView }
impl ViewKind {
    pub const fn id(&self) -> &'static str { /* … */ }
    pub const fn title(&self) -> &'static str { /* … */ }
    pub const fn default_budget_tokens(&self) -> usize { /* … */ }
    pub fn from_id(id: &str) -> Option<Self> { /* … */ }
}
```

Reference implementation: `sovereign/crates/sovereign-tools/src/knowledge_view/view_kind.rs`.

### 2.2 Keep string-id constants as *aliases* when they're part of the wire API

Legitimate exception: when the strings appear on disk, in gossip, or in
persisted config, they're a wire contract. Keep the `pub const &str` form as
an alias, but make it a `const fn` call so the enum stays the source of truth:

```rust
pub const VIEW_PERSONAL_KNOWLEDGE: &str = ViewKind::Personal.id();
```

A test pins the equivalence:

```rust
#[test]
fn legacy_view_id_constants_match_view_kind() {
    assert_eq!(VIEW_PERSONAL_KNOWLEDGE, ViewKind::Personal.id());
}
```

### 2.3 `match` over open sets belongs in a registry, not a trait object

If the set is open (third parties register handlers, the list grows with
features), use a registry. See §4.

---

## 3. Single-responsibility at the file level

A Rust file is the smallest unit where reviewers can easily see "does this
hold together?" Watch the size and the concern count — **both** matter.

### 3.1 Soft ceilings, hard triggers

| File size | Expectation                                        |
|-----------|----------------------------------------------------|
| < 400 lines | Fine.                                            |
| 400–800   | OK if it has one clear concern. Name the concern in the module doc. |
| 800–1200  | Has to justify itself. Either split or write a comment at the top arguing why the seams aren't natural. |
| > 1200    | Split. No exceptions that aren't already documented in §10 of SYSTEM_OVERVIEW.md. |

These aren't LOC fetishism — they're a proxy for concern count. An 1100-line
file doing one tight job (the SQL schema, the protobuf translator) is fine;
a 600-line file juggling lifecycle, observer, debouncer, digest assembly, and
token estimation is not.

### 3.2 What we split, and how

Recent concrete example: `sovereign-tools/src/knowledge_view/manager.rs`
(1332 lines, 5 concerns) → `manager.rs` (757, façade) + `debouncer.rs` (207,
timing + enrichment task) + `digest.rs` (125, pure formatter) + `tokens.rs`
(92, budget math) + `view_kind.rs` (128, type-safe dispatch).

The pattern:

1. **List the concerns** the file actually holds. If you name more than
   three, you have a splitting problem.
2. **Pick pure helpers first.** Move `estimate_tokens` and `format_landscape`
   before you move anything with I/O; they're the easiest to test in
   isolation and the safest to relocate.
3. **Keep the public façade intact.** External callers import
   `KnowledgeViewManager` from `manager.rs` — that never changed. Internal
   consumers use submodule imports.
4. **Move the tests with the code.** A test for `format_landscape` belongs
   in `digest.rs`, not in `manager.rs` after the move.
5. **Behaviour-preserving by default** — see §10. If the refactor changes
   observable behaviour, that's a separate PR with an explicit callout.

### 3.3 Files we haven't split *yet* are flagged, not hidden

Current outliers (the largest; `SYSTEM_OVERVIEW.md` §10 carries the
full set with per-file deferral rationale):

- `sovereign-inference/src/embedded/model_slot.rs` (~3475 lines) — slot state machine + decode loops + MTP; the residual of the former 9,669-line `embedded.rs` after its per-concern split (engine / rpc_distribution / grammar / prompt_helpers / sampler / embed_slot / rerank_slot / rpc_warm_cache). One tight unsafe-FFI concern; further seam = an alternate backend at the `InferenceProvider` trait.
- `sovereign-desktop/src-tauri/src/state.rs` (~1430 lines, down from 2347) — desktop `AppState` + `bootstrap_with_progress`. **Decomposition in progress:** config / built-in skills and four bootstrap sub-phases (`health`, `store`, `inference`, `knowledge_view`) are extracted to `state/` + `state/builders/` with mock-backed tests; `embedded_daemon` remains, `tools` stays inline (mutated across the whole bootstrap). (The former `commands.rs` monolith was already split into `commands/*.rs`.)
- `commonwealth-api/src/frontdoor.rs` (~5758 lines) — harness-protocol → model-native normalizer.
- `corpus-engine-notes/src/notes.rs` (~5634 lines) — NoteStore (carved-out crate; still wants an in-file split).
- `corpus-engine/src/enrichment/atlas/resolution.rs` (~5189 lines) — atlas URI resolution + scoring.
- `sovereign-cli-dev/src/atos_cmd/run.rs` (~4659 lines) — ATOS lifecycle dispatcher.
- `sovereign-cli-daemon/src/daemon_cmd/` (was ~3746 lines; `mod.rs` ~2378 + `lifecycle`/`workspace`/`provider`/`worker`/`tool_registry` submodules) — daemon Runtime construction. **Partially split (2026-06-09):** separable lifecycle / workspace / provider / worker / tool-registry concerns extracted; the `run_daemon` bootstrap's two self-contained early phases (VRAM `preflight` + `inference` provider load) were extracted to `daemon_cmd/build/`. The remaining ~22 phases (~1990 lines) are interleaved (shared locals + ordering constraints) and stay inline — the accepted end state, mirroring the desktop `state.rs` `tools`/`embedded_daemon` call.

All are listed in `SYSTEM_OVERVIEW.md` §10 Architecture Roadmap with
their deferral rationale. Big files without a roadmap entry are
*bugs*. Big files with one are work that's intentionally sequenced.

History: `runtime.rs` (15,024 lines) was decomposed in the 2026-05-23
refactor pass into `runtime/` (13 helper modules + 10 per-intent
handler modules); the 2026-06-10 pass finished the job — the residual
dispatch monolith split into `runtime/prompts.rs` (~733, the pure
prompt/budget/refusal policy layer), `runtime/streaming.rs` (~1,950,
streaming dispatch), and `runtime/turn.rs` (~680, non-streaming
dispatch), leaving `runtime.rs` at ~745 lines holding the `Runtime`
struct, builders, lifecycle, and the module façade.
`atos_cmd.rs` (2673 lines) and `local.rs` (1183 lines) were split into
folders in the spring 2026 refactor pass — they were the prior
occupants of this list. `sovereign-core/src/types.rs` (3623 lines, 17
type families, 228 importers) was decomposed 2026-06-02 into `types/`
(`mod.rs` façade at ~1194 + `completion` / `routing` / `conversation` /
`narration` / `document` / `ui` submodules) behind `pub use` re-exports —
zero importer churn, workspace `cargo check` green.

---

## 4. Registry pattern for pluggable dispatch

When new implementations of a trait are expected to appear over time —
domains, middleware, tools, exporters — dispatch through a registry, not a
match on an id string. You never edit a match arm to add a thing; you call
`register`.

### 4.1 Canonical shape

```rust
pub struct DomainRegistry {
    factories: HashMap<&'static str, Box<dyn Fn(…) -> Arc<dyn Domain> + …>>,
}

impl DomainRegistry {
    pub fn register(&mut self, id: &'static str, factory: F) { … }
    pub fn build(&self, id: &str) -> Option<Arc<dyn Domain>> { … }
}
```

### 4.2 Where this already lives (copy from these, don't reinvent)

- `corpus-engine/src/enrichment/domain_registry.rs` — enrichment domains
- `commonwealth/crates/commonwealth-api/src/middleware/mod.rs` — pipeline middleware (see `MiddlewareRegistry`)
- `sovereign-core::registry::ToolRegistry` — runtime tools

### 4.3 Unknown-id handling is *explicit* and *loud*

`MiddlewareRegistry::build_pipeline` returns `MiddlewareError::Infra` for an
unknown id. **Do not** silently skip; a typo in a pipeline alias file should
fail the request at startup, not shave off a behaviour at runtime.

---

## 5. Interface segregation — the StateStore model

### 5.1 Trait surfaces are costs

Every method on a trait is a method every implementor has to provide and
every caller can depend on. A 30-method god-trait forces mocks, test
fixtures, and alternate backends to carry weight they don't need.

### 5.2 The `StateStore` decomposition is the target

`sovereign-core::traits.rs` exposes `StateStore` as a blanket supertrait of
focused sub-traits: `ConversationStore`, `TaskStore`, `MemoryStore`,
`RoutingStore`, `DocumentStore`, `CorpusStateStore`, `BudgetStore`,
`PermissionStore`, `HealthStore`, `DocumentSessionStore`,
`DocumentAssetStore`.

New code **narrows its bounds**:

```rust
// Don't:
fn x<S: StateStore>(s: &S) { … }

// Do:
fn x<S: ConversationStore + MemoryStore>(s: &S) { … }
```

The narrow bound tells the reader exactly what data the function touches and
lets tests pass a fixture that implements only those traits.

### 5.3 Don't widen single-method traits

Some traits are already minimal and should stay that way:

- `LandscapeDigestProvider` (one async method)
- `StateStoreObserver` (three methods, each a write-path event)
- `ApprovalChannel` (four methods, each a well-defined interaction)

If you're tempted to add a method to these, check: is it a new concern? It
probably belongs in a sibling trait, not on this one.

### 5.4 Pipeline stages parameterize on data, not source identity

When a stage in a multi-step pipeline (enrichment, retrieval,
indexing) takes a source-shaped handle in its signature
(`&DocumentAsset`, `&CorpusEntry`, `&ConversationRecord`), it
silently couples to that source. Adding a second source kind means
either an `enum` parameter, a wrapper trait, or a parallel
implementation — none of them free.

The tiered-retrieval port surfaced this. `build_raptor_atlas`,
`EntityGraph::build`, `extract_motif_candidates`, and
`detect_segment_boundaries` all take *chunks + embeddings +
inference + store handles* — never a `DocumentAsset`. Porting from
the attached-doc surface to a conversation corpus was a recipe + a
state-machine adapter; the algorithmic stages were unchanged.

Three concrete commitments that earn portability:

1. **Builder signatures are corpus-free.** Pass primitives
   (chunks, embeddings, an `EmbedFn` or `InferenceFn`, store
   handles), not source aggregates.
2. **Storage tables key on string IDs in the source's own
   namespace**, not on document-specific identifiers. Same
   schema serves conversation, vault, attached doc, encyclopedia
   — under their own `asset_id`-shaped namespaces.
3. **State machines are per-source, not per-document.** The
   variant set (`Pending → Indexing → PartiallyReady → … →
   Ready | Failed`) is universal; *where it lives* is per-corpus.

Reference: `sovereign-core::document_asset::AssetState` +
`sovereign-tools/src/raptor_atlas.rs` (canonical
shape). Counter-example: any function that needs an `if let Some(doc)
= asset { … } else { … }` switch is reaching across this boundary
and should be split.

---

## 6. Data vs. program — the SICP separation

Prompts, templates, lookup tables, and configuration are **data**. Rust
source is **program**. Conflating them is a recurring smell.

### 6.1 What counts as data

- Agent-facing preambles and instruction blocks
- Report section headings and labels
- Q&A catalogs (e.g., ATOS amend-time adversarial prompts)
- Default configuration that an operator might reasonably tune
- Model families, hardware profiles, corpus tiers

### 6.2 Rule of thumb

If the string or table might change **without requiring a code change in the
same commit**, it's data and belongs in an asset file loaded via `include_str!`:

```rust
const ATOS_INSTRUCTIONS: &str = include_str!("../../assets/atos_instructions.md");
```

Reference: `commonwealth/crates/commonwealth-api/assets/atos_instructions.md`.
The asset lives alongside its consumer, not in a distant `config/` directory,
so the grep distance from "where does this string come from?" to "here it
is" is one hop.

### 6.3 The anti-pattern we removed

A comment that said *"baked into the source so operators can edit it without
a config redeploy"* next to a 17-line const. If the text wants to be edited
without touching Rust, it should live as data. Either move it out, or update
the comment to be truthful ("baked into the binary; edit here").

---

## 7. Privacy and critical invariants must be *structural*

### 7.1 Encode invariants so they can't be forgotten

The KnowledgeView feature promises that the three-map corpora never leave
the user's machine. That promise lives as three hardcoded fields on every
recipe builder:

```rust
// sovereign-tools/src/knowledge_view/recipes.rs
scope: Some("local".into()),
mesh_sharing: false,
query_sharing: Some(false),
```

These are not parameterised. A caller cannot flip them via config, a CLI
flag, or a remote request. The invariant is expressed as *code that cannot
compile the violation*.

### 7.2 Back it with a test that pins the invariant

```rust
#[test]
fn knowledge_view_recipes_are_structurally_local() {
    let r = personal_knowledge_recipe(&tmp_db());
    assert_eq!(r.corpus.scope.as_deref(), Some("local"));
    assert_eq!(r.corpus.mesh_sharing, false);
}
```

Now if someone refactors the recipe builder and accidentally makes
`mesh_sharing` tunable, the test fails before the PR lands.

### 7.3 Runtime asserts are for bugs; structural encoding is for threats

A `debug_assert!` or a runtime check is fine for "I expect this vector to be
non-empty here." It's **insufficient** for "user data must not leak." The
latter wants to be impossible to express in source, not impossible to
observe at runtime.

### 7.4 Layer the enforcement

KnowledgeView enforces privacy at *three* layers:

1. **Recipe level** — `mesh_sharing = false` hardcoded.
2. **Acquirer level** — the SQL `WHERE skill_id NOT IN (…)` clause excludes
   `privacy = "local_only"` conversations at ingest time.
3. **Splice level** — when the active skill is `local_only`, the
   conversational + institutional digests are not assembled at all.

Any single layer slipping doesn't compromise the invariant. **Defence in
depth** is the default for anything the user would consider sensitive.

---

## 8. Dependency hygiene

### 8.1 Centralise third-party deps in `[workspace.dependencies]`

One version of `tokio`, one version of `serde`, one version of `reqwest` per
workspace. No inline re-declarations in crate `Cargo.toml` files unless the
crate genuinely needs a feature the workspace default doesn't carry — in
which case *document why* next to the override.

### 8.2 Cross-crate version skew is a correctness risk

Hypothetical-sounding but real: `sovereign-tools/Cargo.toml` once carried
`arrow = "55"` while `corpus-engine` was at `arrow = "57"`, with
`arrow-array = "57"` in the same sovereign-tools file. A `RecordBatch`
constructed with 57 semantics and passed to a 55-compiled helper is a
runtime time bomb. Fix was two character edits; finding it required
reading the full workspace.

Periodic hygiene: `cargo tree | grep -E '^(arrow|parquet|tokio|serde) v' |
sort -u` should return exactly one version per crate.

### 8.3 Respect re-export boundaries

`oicp-types` is re-exported as `sovereign_core::oicp` and
`commonwealth_core::oicp`. Downstream crates depend on the *core* crates and
reach in via the re-export. They do **not** take a direct path-dep on
`oicp-types`.

The reason: a direct dep creates a second version seam. If two crates
transitively depend on two different versions of `oicp-types`, serde
round-trip silently succeeds while the compiler sees two distinct types.
That bug is genuinely miserable to diagnose.

Before adding a direct cross-workspace dep, check whether it goes through an
existing re-export. If it does, use the re-export. If it doesn't, and you
think it should, extend the re-export rather than bypassing it.

### 8.4 Feature flags are part of the dep contract

Declaring `tokio = { features = ["rt"] }` in one crate and `tokio = {
features = ["full"] }` in another works until one binary links both. Default
to the workspace-level feature set unless there's a concrete reason to
diverge; when you diverge, comment the reason.

### 8.5 Heavy deps stay in the crate that needs them

`llama-cpp-2` belongs in `sovereign-inference`. `tauri` belongs in
`sovereign-desktop`. `lancedb` belongs in `corpus-engine`. A headless
library crate should not pull in a GUI framework because one line imports
its types "for convenience."

### 8.6 The layer map is the contract

`quality/ARCH_LAYERS.toml` declares the workspace's dependency direction:
ordered layers (a crate depends only on its own or a lower layer),
`[[forbid]]` rules for what ordering can't express (the
commonwealth↔sovereign family seam), and `[[exception]]` entries — the
grandfathered-violation burn-down list. Every workspace member must appear
in exactly one layer; the map is total by construction.

Enforced two ways, one parser (the `quality/arch-layers` crate, so the
halves can't drift on semantics):

- `cargo xtask layer-gate` checks **Cargo-declared** edges in CI (<1s).
- `sovereign code arch-report` checks **SCIP-observed** symbol references —
  the coupling that re-export chains hide from Cargo — and persists the
  posture for the `arch_posture` tool.

Adding a violating edge requires adding an `[[exception]]` with a reason in
the same PR — a reviewable policy change, never silent accretion. A stale
exception (violation fixed) FAILS the gate until the entry is deleted:
removals are the celebration. Fan-in caps for the god-crates live in
`quality/baselines/fan_in.tsv` under the same ratchet.

**The ratchet lifecycle** (uniform across arch/layer/lock/lint gates):
baselines under `quality/baselines/` are machine-written only —
`--update-baseline` snapshots current state (defend the diff in review);
`--tighten` banks improvements and never raises (automated weekly by
`baseline-tighten.yml`). Every gate failure ends with the exact command
that fixes it. `cargo xtask quality` runs every local gate with one
summary table.

---

## 9. Observability — the glassbox principle

### 9.1 Every non-obvious decision emits a tracing event

"Non-obvious" means: the operator can't predict the decision from the
inputs + static analysis alone. Examples: which backend a request was routed
to, whether a cross-view match cleared the threshold, why a feature is
waiting for approval.

Baseline shape:

```rust
tracing::debug!(
    threshold,
    input_views = skeletons.len(),
    accepted_matches = matches.len(),
    "cross_view: match decisions"
);
```

Name the event with a **module:short_action** form so `grep "cross_view:
match"` across logs goes straight to the site.

### 9.2 Pick the level deliberately

- `error!` — a bug or an unrecoverable condition the operator must address.
- `warn!` — a recoverable degradation the operator should know about
  (e.g., drift detected, ingest failed softly).
- `info!` — lifecycle events: startup, shutdown, config reload, milestone
  pass/fail.
- `debug!` — routine decisions that are useful in an investigation but not
  during normal operation.
- `trace!` — full detail. One line per item in a loop is acceptable here and
  nowhere else.

A system that only emits `info!` is not observable; it's a press release.

### 9.3 Redact deliberately

Hash values, secrets, and anything user-sensitive get truncated or omitted.
The drift detector logs 12 hex chars of each SHA-256 — enough to correlate
across events, not enough to leak content.

### 9.4 Test-time silence is a smell

If a production code path is so noisy that tests have to special-case its
logs, the log level is wrong. Dial it down or move the event to a hook so
tests can observe without grepping stdout.

---

## 10. Refactor discipline

### 10.1 Behaviour-preserving by default

A refactor is behaviour-preserving if:

- All existing tests pass without modification.
- `cargo check --workspace` is green.
- An external caller cannot observe a difference in interface, output
  shape, or timing.

If your refactor changes *any* of those, it's not a refactor — it's a
behaviour change that happens to involve restructuring. Those land as
separate PRs with explicit notes, *especially* if they improve something.

### 10.2 Touch one dimension at a time

Splitting a 1300-line file *and* changing its error semantics in one PR is
uninviting to review and hard to revert cleanly. Pick one. Ship it. Then do
the next.

### 10.3 Prefer helpers over trait gymnastics for small duplication

Three copies of a 40-line block is a duplication smell. Extract a helper
with a small `Style` enum when the copies differ in tiny ways. Only reach
for a trait when you have *four* or more, or the callers genuinely need to
swap implementations at runtime. A `ReportRenderer` trait over three
renderers is overkill; a `render_redteam_findings_by_confidence(out,
findings, style)` helper is exactly right.

### 10.4 Write the test *before* the extraction when the code under
    refactor lacks tests

You're about to move 200 lines of code. If there isn't already a test
exercising them, the refactor is a leap of faith. Land the test first,
*then* move. Five minutes of test cost sometimes saves a whole day of
forensics.

### 10.5 Refactors earn back their review cost only if someone else reviews

A refactor that ships without review is a refactor that may have silently
broken a caller you don't know about. If no reviewer is available, at
minimum run the full workspace test suite twice (once before, once after)
and post both outputs to the PR.

---

## 11. Verify before claiming

### 11.1 Don't cite from memory — cite from `grep`

Before claiming "function X exists" or "the trait has method Y", verify it.
The codebase evolves faster than memory, and Explore-agent reports can be
partially wrong. In a recent pass:

- An exploration claimed `MiddlewareRegistry` didn't exist. It did —
  `middleware/mod.rs:292`.
- The same exploration claimed `FeatureState` didn't exist. It did —
  re-exported from `corpus-engine` at `sovereign-atos/src/local.rs:16`.

Cost of verifying: one `Grep` call. Cost of acting on a false claim: a
duplicate implementation and a code review that reveals the duplication.

### 11.2 `cargo check` is a correctness witness, `cargo test` is a behaviour
    witness, and neither replaces the other

`cargo check` tells you the types line up. `cargo test` tells you the
semantics hold. A refactor that passes `check` but fails a test you didn't
run is worse than one that fails to compile, because it's in the repository
before anyone notices.

### 11.3 Before touching a signature, know the blast radius

For Rust code, `find_callers` on the symbol (or `blast_radius` for
transitive callers) from the sovereign MCP server. Changing a function with
20 callers is a different strategy than changing one with 2.

---

## 12. Testing — what earns coverage

### 12.1 End-to-end for critical user-visible flows

KnowledgeView's `splice_into` path has an E2E test that creates a temp
database, ingests conversations, enriches a skeleton, and verifies the
spliced digests. It costs more to maintain than a unit test and catches
things unit tests cannot: the SQL query, the acquirer, the recipe builder,
and the formatter all have to cooperate.

Reference: `sovereign/crates/sovereign-tools/tests/knowledge_view_e2e.rs`.

### 12.2 Unit tests at the *seams* between concerns

When you split a file along a concern boundary (§3.2), the boundary is a
natural unit-test target. `format_landscape` is pure; testing it with a
fixture skeleton is fast and catches formatting regressions without
spinning up a database.

### 12.3 Pin invariants with tests, not comments

A comment that says "must be true" is a wish. A test that says
`assert!(…)` is an invariant. See §7.2.

### 12.4 Tests must not require GPU, network, or real model weights

Every test in this workspace runs on a CI box with no GPU, no internet, and
no model files on disk. If your test needs any of those, refactor the code
under test to accept a `Mock*` or a `DeterministicInference` implementation
via the existing trait boundaries.

### 12.5 `DeterministicInference` and friends exist — use them

`sovereign-inference::DeterministicInference`, `commonwealth-test-harness::
MockLlamaServer`, the in-memory `StateStore`, and the zero-vector `EmbedFn`
in tests are the standard mock set. You should not need to invent a new
mock unless you're adding a new trait.

---

## 13. MCP tooling — use the precise tools, not grep

A Sovereign code-intelligence MCP server runs at `localhost:9741/mcp` with
24 tools. For tasks these tools exist to handle, they are faster and more
accurate than grep:

- `symbol_lookup("TypeName")` returns the exact definition with file:line.
- `find_callers("fn_name")` is compiler-resolved and catches trait dispatch
  that grep misses.
- `blast_radius("symbol", max_depth: 2)` before a non-trivial change.
- `lint_status` / `test_status` instead of running `cargo check` / `cargo
  test` when the watcher is already running — the watcher holds the Cargo
  lock; racing it just makes both slower.
- `read_notes(query: "…")` to find prior decisions before you re-litigate
  them.

CLAUDE.md has the full decision tree. The short form: if grep would be your
third guess anyway, skip ahead and use the precise tool.

---

## 14. Process — how work lands

### 14.1 Keep PRs small and focused

One concern per PR. A PR that touches ten subsystems reviews badly and
reverts worse. Phase A (docs), Phase D.1 (dep fix), Phase B (KV refactor),
Phase C (ATOS cleanup), and Phase D.2–D.5 (dep centralisation) from the
recent refactor pass were **five separate commits** for this reason.

### 14.2 Write notes at the moment of decision

The NoteStore (`corpus-engine::notes`) exists so future sessions can see
*why* you chose approach A over approach B. Use `write_note` when:

- You rule out an approach (`kind="attempt"`).
- You discover a constraint that must not be violated (`kind="invariant"`).
- You make a choice with real alternatives (`kind="decision"`).
- You defer work explicitly (`kind="todo"`).

Don't batch these at session end — by then you've forgotten the strongest
reasons.

### 14.3 Update the roadmap when you defer work

If your PR leaves known cleanup for later (e.g. the recent pass deferred
`atos_cmd.rs` and `local.rs` splits), update
`SYSTEM_OVERVIEW.md` §10 Architecture Roadmap in the same PR. The next
engineer inherits a todo list, not a surprise.

### 14.4 ATOS artifacts are the audit trail

When a feature lands under ATOS orchestration (`.sovereign/features/<id>/`),
the `milestone-N.md`, `red-team.md`, and `epistemic-report.md` are the
reviewer's reading list. Write them honestly. The audit rollup at
`sovereign project audit` is only as useful as the notes that feed it.

---

## 15. Patterns we've learned to recognise (and stop)

A checklist for code review. Any of these in a PR is a conversation, not
automatically a block.

| Smell                                                               | See |
|---------------------------------------------------------------------|-----|
| A `match` on string ids with more than 3 arms                       | §2.1 |
| A file that crossed 1200 lines since the last split                 | §3.1 |
| A trait with more than ~8 methods and no obvious sub-trait shape    | §5.1 |
| A large const string literal in a `.rs` file                        | §6.2 |
| Two crates depending on the same third-party crate at different versions | §8.2 |
| A non-`core` crate taking a direct dep on a re-exported shared type crate | §8.3 |
| A branch of production code with no tracing event                   | §9.1 |
| A refactor PR that also "just cleans up some nearby stuff"          | §10.2 |
| A claim in commit or PR body that a function exists, without a citation | §11.1 |
| An assertion in English prose rather than in a test                 | §7.2 |

If you see one in your own code while writing it, fix it then. If you see
one in review, call it out with a link to the relevant section of this file.

---

## 16. What this document is *not*

- **Not a style guide.** Formatting is `rustfmt`'s job.
- **Not a tutorial on SOLID or SICP.** Those principles are the air we
  breathe; this file shows how we apply them *here*.
- **Not exhaustive.** Patterns that haven't come up in production don't
  belong here. Add a section when the codebase teaches you a new one, not
  before.
- **Not frozen.** When a rule has to bend for a real reason, the fix is to
  update this file — not to quietly violate it and hope reviewers don't
  notice.

---

## 17. How to add to this document

1. The addition must come from a **real incident** in this workspace: a
   bug you fixed, a refactor you did, a drift you caught. Cite the PR,
   commit, or file.
2. It must be **specific**. "Prefer simple code" is not a principle;
   "prefer a helper over a trait when duplication appears in ≤3 places" is.
3. It must be **actionable by someone who didn't see the incident**. If
   reading the rule doesn't tell the reader what to do differently
   tomorrow, rewrite it until it does.
4. Principles replace prose, not the reverse. If a new rule obsoletes an
   old one, delete the old one; don't let them coexist.
