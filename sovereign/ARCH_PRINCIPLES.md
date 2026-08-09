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

## The eleven

Everything below is lookup. **These eleven are held, not looked up** — they are
what gets injected into every session, and they are the ones a violation of
should stop you mid-keystroke. Each names the section that carries its
evidence.

1. **Glassbox, always.** A decision invisible at `tracing=debug` is not
   finished. *(§0, §9)*
2. **Don't whack moles.** Instrument, reproduce, understand — *then* fix.
   *(§0)*
3. **Write for the next reader,** and land the doc change in the same commit as
   the code. *(§0, §1)*
4. **Cite, don't recall.** Verify before you claim it — from `grep`, from
   `symbols`, or from a run you just did. *(§11)*
5. **A gate you have not watched fail is not a gate.** Four verdicts, not two:
   passed, failed, could-not-judge, never-ran. *(§18.1, §18.2)*
6. **Never silently substitute.** Refuse, or name the substitution in the
   response. Absence is reported, never defaulted. *(§18.3)*
7. **Validate the instrument before the result.** One run is not a measurement.
   *(§18.4, §18.5)*
8. **One decider, one name.** One implementation per threshold, scorer, schema
   and key; one accessor per path; identity from essence, never a counter or an
   address. *(§10.6, §7.5)*
9. **Closed sets are enums, open sets are registries, open text is a centroid.**
   *(§2, §4)*
10. **Make it structural, not remembered.** Encode the invariant so it cannot be
    forgotten — and never ask a model to guarantee what code can enforce.
    *(§7, §7.6)*
11. **The inventory outranks the plan.** Survey what already exists — corpora,
    seams, tools, scripts, prior art — and prove it cannot serve before you
    build new. A design that feels complicated is usually a missed reuse.
    *(§19)*

One through four are the operating ethos this workspace was built on. Five
through eight were earned: they are what six months of working notes say
actually goes wrong here, and §18 exists because the failure they describe —
a plausible, well-formed, exit-0 result that is wrong — is this system's
characteristic one. Nine and ten are the two structural moves that prevent the
most rework. Eleven was minted 2026-08-08, after the additive-bias pattern
recurred for a third documented time: an agent built, or funded building, what
already existed — and every catch came from the operator, never from the
builder's own process (§19).

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

### 2.4 Classifying open text is a centroid, not a keyword list

§2.1 bans stringly-typed *ids*. The same error one level up is a stringly-typed
*decision procedure*: `looks_like_*`, `.contains("today")`, a lead-word list.
It works on the examples you had in mind and fails on the ones you didn't.

`needs_current_info` substring-matched "today" inside "…from antiquity to
today" and put the user's phone into a refusal loop. A locator gated on nine
literal substrings missed "What was the first thing I asked?" and misrouted it
on four runs in five. Both were replaced by an embedding axis; `looks_like_*`
is now deprecated in `router_embed.rs`'s module doc.

The replacement is a calibrated centroid with **both** an abstain gate and a
margin gate, proven on held-out inputs of both classes. Two cautions that cost
real measurements:

- **Similarity is topic-dominated, not shape-dominated.** The same exemplar
  scores 1.000 on its own topic and 0.531 on near-identical phrasing about
  something else. Adding exemplar rows buys you the topics you add, nothing
  more — it does not generalise the *shape* you were trying to capture.
- **After re-filing exemplars between classes, re-check the abstain cases, not
  just the positives.** Margin is a relative quantity: one re-filing moved a
  margin 0.043 → 0.128 with `sim_positive` identical at 0.561, creating a false
  positive that had not existed. "Same winner ⇒ same verdict" is false for any
  gate with a margin term.

Not this trap: stripping a fixed protocol token (`<think>`, `<tool_code>`).
That is mechanical removal of a literal, not semantic judgement — keep it in
code and keep it narrow.

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

### 7.5 Identity derives from essence; shared paths have one accessor

A key built from a counter, a row count, or a network address will be reused
or will churn. The failure is never an error — it is a confident wrong answer.

- Chunk ids allocated from `chunk_count()` were reused after any delete.
  Wikipedia carried **31,432 duplicate rows**, and the citation "2026 Lebanon
  war" opened "Gold can be used in food and has the E number 175." Retrieval
  was correct; only the id-keyed read-back was ambiguous.
- An iroh bridge's loopback port used as peer identity produced 14 rebuilds in
  21 minutes for one peer that had not moved, and read a single stable worker
  as a stream of new ones until quarantine compounded to permanent exclusion.
  When a stable thing is keyed by a volatile address, **move the key** — don't
  dampen the volatility.
- Atlas rebuilds re-mint `EdgeId`s sequentially, so edge-id-keyed governance
  adjudications would have re-opened every settled conflict weekly.

Key on content hash or on a stable id. The address is a mutable *attribute* of
the thing, never its name.

The same rule applies to paths. A path or id derived by hand in two processes
is a split-brain, and it presents as "the write succeeded and the read found
nothing": `lint_status` reported `running` indefinitely against an orphaned DB
because the reader resolved `indexes/` while the writer used the root, and
`SOVEREIGN_DATA_DIR` carried four divergent unset-fallbacks across five crates,
two of them *relative* paths that wrote into the current directory. Resolve
through one named accessor; `clippy.toml`'s `disallowed-methods` entry for
`dirs::home_dir` is how that is enforced here.

Store the pre-image beside anything derived. A `MeasurementRecord` kept only a
`placement_digest`; an exhaustive search over every split, both range orders,
both head placements and five `total_blocks` values could not reproduce it. The
number was still on disk and what it was a number *for* was gone. Re-derive at
the point of use, and treat a witness that fails to explain its digest as
**absent** rather than quoting it.

### 7.6 Structure over instruction

§7.3 says structural encoding is for threats. Extend it: **a model's behaviour
belongs in the threat category, not the bug category.** It cannot be fixed by
asking more firmly, and a prompt imperative relied on for correctness is a
gamble you re-run on every request.

Instruction-caveat compliance measured **~60%** on the 4B this repo runs
(honesty 0.64 against a 0.91 counterfactual). Prose prohibitions fare worse:
"absent-from-reference is NOT a hallucination" was ignored, and the first run
flagged 11 hallucinations, mostly absence-flags. The structural fixes worked
where four rounds of prompt language had not:

- Force the output shape. Requiring `{claim, contradicts_item: <ref #>}` and
  dropping entries with no valid cited item took hallucinations **11 → 2**.
- Commit the constraint at decode time rather than requesting it. A structural
  caveat prefix removed the honesty *variance*, which was the actual defect.
- Enforce mechanically after the fact. Dropping uncited bullets and re-asking
  once took frame recall **17% → 88%**.
- Remove the fuel instead of forbidding the use. On zero atlas-atom matches the
  append round is skipped entirely, so there is nothing to fabricate over.

A bright line is also cheaper than a fuzzy one, and not only for safety. One
unambiguous clause took hand-confirmed breaches 2 → 0 *and* moved unrelated
quality: filler-only questions 19.5% → 7.2%, anchoring 75.9% → 85.4%, with the
curious-read rate flat. The working hypothesis is that the model had been
spending turns hedging around a limit it could not cleanly obey.

Corollary for agents: adding more imperative language will not beat a model
that has discovered it can no-op. Reach for a stronger verify gate (§18.1).

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
- `svrn code arch-report` checks **SCIP-observed** symbol references —
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

### 9.5 A probe must not ride the resource it monitors

A health check that contends with the work it is checking reports "dead"
precisely when the system is busiest — and the failure names the wrong culprit.

`/v1/mesh/status` sampled per-device memory through an FFI call that is a
register read locally and a **synchronous network round-trip** for an RPC
device. It tested fine, because it was tested against an idle worker. Under a
serving 122B the same call went from ~2 ms to hanging past 70 s, and since that
endpoint is how `mesh bench` identifies the mesh, the bench died with "daemon
not reachable" **while the daemon was healthy**. The fix — serve the reading
the loader already captured — was also more correct, because the memory the
loader saw when it chose a split is precisely what explains that split.

Two consequences worth holding:

- **A status field must never be produced by a call that can block.** Publish a
  timestamped observation captured where the work already happens.
- **Absence of a response is not evidence of absence.** ggml's RPC server
  accepts one connection at a time, so a busy worker is indistinguishable from
  a dead one: a healthy 284B child was SIGTERM'd mid-load at 223 s, and it
  presented as "big models don't work distributed" rather than as a timeout.
  Distinguish "answered: no" from "did not answer", and prefer an independent
  signal — gossip membership is deliberately the liveness signal here, because
  the `/status` probe starves under decode load.

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

### 10.6 Duplicating a *decider* is worse than duplicating code

§10.3 tolerates a little duplication rather than trait gymnastics. That
allowance stops at anything that **decides**: a threshold, a schema, a scorer,
a policy table, a measurement key. Duplicated code diverges and you get a
compile error or a failing test. A duplicated decider diverges and you get a
plausible number, with nothing red anywhere.

- A third view of the slot-alias policy spelled `vec![stem]` by hand and
  omitted `commonwealth/primary`. **Every peer request for the shared 122B was
  answered by the 0.8B fast slot at HTTP 200** — 111 tok/s against ~14.8. Every
  measurement ever filed for that model was the small one under its name.
- The chaos bench re-read `SOVEREIGN_GV_THRESHOLD` through a private
  `.unwrap_or(0.5)` while production ships 0.9, so a gated run was 0.4 stricter
  than the shipped gate and its verdicts described a system nobody runs.
- The daemon capped a field in UTF-8 bytes and the client in UTF-16 chars.
  Either side 400s the request, and the offending unit **stays in the history
  window poisoning every later prediction, behind a green status bar**.
- A Python replica of a gate's centroid maths disagreed in the third decimal
  and returned the wrong verdict at +0.038 against a 0.040 gate.
  **Re-derive, never re-implement.**

The shape to copy is `sovereign-mesh/src/slot_aliases.rs`: one `const` policy
table, every derived view computed from it, and tests that pin the views
against each other. The module doc tells the next author to add a row rather
than a branch. If two implementations genuinely must exist, share the body and
add a golden equivalence test — conventional agreement decays, structural
inseparability does not.

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
`svrn project audit` is only as useful as the notes that feed it.

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
| A check with no failing input you can name                          | §18.1 |
| A guard asserting on a field the subject supplies or echoes back    | §18.1 |
| An `Err` collapsed into a success-shaped value                      | §18.3 |
| A single-run delta reported as a result                             | §18.5 |
| Two implementations of one threshold, formula, or key               | §10.6 |
| A key derived from a row count, sequence number, or network address | §7.5 |
| New capability added without citing the existing surface that was checked | §19 |

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

---

## 18. Unearned success

This section came out of clustering 818 working notes written across six
months. It is the single most repeated lesson in them, and it is worth stating
plainly:

> **This system rarely crashes. It reports success it has not earned.**

A gate that cannot fail, a fallback nobody announced, an instrument nobody
validated, and a bench measuring a path production never runs are four ways of
producing the same artifact — a plausible, well-formed, exit-0 result that is
wrong. Each rule below is cited to notes you can read with
`sovereign notes list --id <id>`.

### 18.1 A gate must be observed to fail before it is trusted

Before you land a check, name the input that makes it red — then make it red
and watch. A check that cannot fail is not a check.

- `doctor` asserted that an *unrelated* file existed and reported Passed. Nine
  test assertions pinned the wrong JSON shape, so for months **the suite
  defended the bug** (`f6d4c770`).
- The CLI journey verifier captured `2>&1`, and every `svrn` invocation prints
  a deprecation banner on stderr — so `stdout_non_empty` was satisfied by *any*
  command, including one that printed nothing. Every such assertion in the
  manifest was vacuous. Three controls were added and watched to fail first;
  three "passing" steps went red immediately (`f496d39e`).
- **Assert on something the subject cannot author.** An SSE `model` field is a
  verbatim echo of the client's request, so the wrong-slot guard passed cleanly
  on the exact failure it existed to catch (`a6ca12aa`).
- **A zero count is not a positive control.** `canary_hits: 0` is the expected
  *good* result, so it proves nothing. Planting a real colliding document
  revealed that the counter incremented on only one of the two paths, and the
  two halves of the report had been silently disagreeing (`72b3ab47`).

The lint adapter is the cautionary case for gates that are also *narrow*: it
counted rustc error records only, so build-script failures, bad feature flags
and link errors were invisible, and it printed `pass: 1 fail: 0` while cargo
exited 101 (`e752b13a`).

### 18.2 Four verdicts, not two

`passed`, `failed`, `could-not-judge`, `never-ran`. Collapsing the third or
fourth into either of the first two is how a gate reports the opposite of the
truth.

The adapter gate reported an **all-NaN diverged training run as "not
trained"** — Python's `max()` keeps the running value when compared against
NaN. Diverged and never-started demand opposite responses, and the gate was the
single artifact the whole handoff rested on (`edbfabb8`). Elsewhere: a strict
balanced accuracy of 10.88 is below chance *by design*, because a parse failure
scores the wrong label rather than abstaining — read it as "no measurement",
never as "worse than a coin" (`e5c02e64`). `shard_fits` returns `None` for
"cannot judge" specifically so a fit check can never clear every device on the
strength of a table of zeros (`143acf9f`).

Give each verdict its own exit code. `sovereign-test.sh` exiting 4 on a
zero-test run is this principle already in force.

### 18.3 Never silently substitute, and never substitute for absence

If you cannot do what was asked, refuse — or name the substitution in the
response. Degrading quietly is worse than failing, because it spends the
caller's trust instead of their attention.

- With the distributed primary unavailable, the non-streaming path returned a
  clean error while **the streaming path, same model string, seconds apart,
  returned 200** served by a 0.8B. The label, the shape and the finish reason
  were all correct, so no client could tell (`d45489a3`).
- The desktop updater collapsed **every** `Err` into `Ok(None)` = "up to date".
  That mask is why two other update bugs stayed invisible for weeks
  (`c17ba1ff`).
- A store that grows and never answers is worse than a refusal, because it
  looks like it worked (`143f57b8`).

Absence gets reported, never defaulted. A guessed rate is a fabricated fact
with a unit attached: `Unpredictable` is never collapsed into a number, and
`Unpredictable` and `Infeasible` point in opposite directions so they must not
collapse into one `Option` (`963a8d88`). Reading a metric under the wrong key
and defaulting to 0 returned 0 for *every* historical run and made the current
one look like a fresh regression from zero (`9345fb89`).

Unknown fields are errors, not no-ops — §4.3 one level down. A wrong parameter
shape produced `{created: true, sections_updated: []}`; 10 of 111 calls used
it, and three frames on disk were empty husks, one of them live in the boot
index (`30251dcf`). A pluralised TOML key silently dropped a threshold while
validation still passed (`665e8cd5`).

### 18.4 Validate the instrument before the result

When a new harness's first run reports a regression, **suspect the instrument
before the system.** Diff the harness's parameters and evidence against
production before you open a bug.

The decisive case: an oracle judged answers against chunks truncated to 1500
characters and a top-12 slice, while the gate under test grounded on the full
set. A composite reported at **60% was really ~90%** — 85-90% of the apparent
gap was the capture artifact, and a re-audit found 14 to 17 of 22 "broken"
turns were correct behaviours mis-scored (`8bfc177c`). A judge that treats a
length target as a hard requirement **cannot see honest improvement at all**: a
length-blind re-judge scored the same change +17.3 points while the live
composite went down (`d6b55fcc`).

So: **score against the same evidence the system consumed**, and resolve
thresholds through the shipped resolver rather than a private `.unwrap_or`
(§10.6). Validate that the path you are measuring actually executed — four days
of training runs looked healthy while the gradient sum was exactly 0.0, so
every number came from an unmodified base model, and detecting it cost two
seconds (`3d9a9ce4`). A bench whose classifier stack is never constructed in
the served daemon is measuring something else entirely, and that is the
mechanism behind "the product feels worse even as the benches improve"
(`762a98c0`).

Corollary: **if a run cannot be re-scored without re-running the model, it is
not instrumented** (`e5c02e64`). Persist the raw artifact, not just the derived
verdict — frozen-transcript replay turned a 2-hour iteration into 3-15 minutes
(`d9fdd15e`).

### 18.5 A single run is not a measurement

Establish the noise floor at the sample size you are using, then decide what
counts as a delta.

Judge verdicts flipped on **37% of facts (104/284)** across trials on the same
transcript, and single-trial scoring inflated the result by ~17 points
(`485f9f05`). Chaos runs are not deterministic at temperature 0. A throughput
figure ranged 42% under one key on one box with one config (`72b2beaa`). At
n=2 a two-node comparison read backwards; at n=4 it inverted (`e39fa87d`).

Short runs are liveness checks, not stability tests. A 5-step gate passed one
step before the identical configuration diverged to NaN at step 11
(`c4851203`), and a 61-step-clean memory configuration was OOM-killed at step
63 — so any claim about that stack must name the step count it was measured
over (`12d363ea`).

Related discipline, which belongs to §10 but is cheapest to state here:
**measure the bar, and measure the fix as an arm, before building either.**
Designing four experimental arms against an unverified baseline cost an
evening; checking the baseline took five minutes and refuted it (`d39af2dc`).
The obvious scheduler fix, measured as an arm first, was a **235% regression**
(`5b315c5f`). And when the question is whether a thing is read at all, a static
census beats an ablation: a bank can only fail to *detect* a difference,
whereas an absent call site proves none can exist (`de25ebe9`).

---

## 19. Resourcefulness — the inventory outranks the plan

Bias toward reuse. Build new only when you have surveyed what already exists
and can say, with a citation, why it cannot serve. (Minted 2026-08-08 by
operator directive, after the pattern recurred a third documented time.)

The incidents:

- **2026-08-08, the SEP miss.** A drafted order funded enrichment runs to mint
  ~2,000 calibration claims while **59,100 already-enriched Claim atoms** sat
  in 1,665 installed `sep-*` corpora. The plan inherited a spec's substrate
  list; nobody ran `sovereign atlas list-corpora`. Caught by the operator
  (seat note `3836ec2d`).
- **2026-06-15, the desktop enrich rung.** An agent began replicating the
  CLI's `SplitInferenceProvider` + enrich pipeline inside the desktop; the fix
  was ~3 lines reusing the existing enrichment output through
  `state.corpus_engine`. Caught by the operator: "we kick off enrichments of
  corpora in plenty of places" (note `400649c9`).
- **2026-06-10, the transport seam audit.** `routes_knowledge.rs` carried a
  duplicate of gossip's address cache; `corpus_collaborate` carried a
  *drifted* inline copy of peer-address ranking. Additive copies decay — the
  drift was the finding.

The common shape: the capability existed, the builder never looked, and the
catch came from the operator or a later audit — never from the builder's own
process. §10.6 (one decider) is the downstream bill when this rule is skipped:
a fix to a duplicated body must be written twice (`998b68dd`).

**The survey is one command per resource class, not a research project:**

| Resource | Ask first |
|---|---|
| Data / corpora | `sovereign corpus status`, `sovereign atlas list-corpora` |
| Code paths, seams | `symbols`, `code_search`, `callers` — where does X already happen; §4.2's copy-from-these list |
| Tools / scripts | `sovereign tools list`, `ls scripts/` |
| Prior art, decisions | `notes(query: "…")` — was this built, tried, or rejected already |

**The review rule, falsifiable:** a plan, order, or PR that introduces a new
store, pass, corpus, harness, or subsystem must name the existing surface it
checked and why that surface cannot serve. No citation, no funding. "We need
to build X" is the smell; "X exists at Y but cannot serve because (measured
reason)" is the earned version.

The tell, from the 2026-06-15 incident: when a design starts feeling
complicated, treat it as a signal you missed the reuse — not that the problem
is hard.
