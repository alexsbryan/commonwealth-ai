# Topology — making the types tell the truth

**Status:** pre-registration. The claim this document makes is a *soundness and
completeness* claim: after the work below, the states the types can represent
and the states the system can be in are the same set — so a maintainer
reasoning from the types is reasoning correctly. Small cardinality is a
consequence, and deliberately not the target. Every section carries the check
that falsifies it (ARCH §18.1).

**This document is not a description of the system.** Descriptions already
exist and most of their structural claims are false on the live path — that is
the finding that produced this file.

---

## 1. Why this exists — the derivation tax, measured

On 2026-08-24 one seat made a single architectural judgement about the top
level. It cost three read-only subagents, ~440k subagent tokens and about two
hours, and three first-pass conclusions were still wrong:

- `commonwealth-daemon` was treated as the primary runtime. It was vestigial,
  and was deleted on 2026-08-24. (Even the correction was half wrong: "ships in
  no release lane" was the seat's phrasing, and it shipped a systemd unit, a
  launchd plist and a `curl | sh` installer — nothing *built* the binary they
  installed, which is a different defect and a worse one.)
- Binaries were counted as the unit of topology. The topology is mode-shaped:
  `sovereign-desktop` is one executable with four process modes, and
  `--daemon-child` *is* `sovereign_cli_daemon::daemon_child_main()`.
- A lock-contention thesis was built on interior-mutability counts. The real
  signal was deferred construction.

None was careless; each was the reasonable reading of the code in front of it.
**The system permitted all three**, and every agent that touches this codebase
pays a fraction of the same cost and gets a fraction of it wrong.

The measure of success is not that the architecture is documented. It is that
the number of non-obvious facts an agent must *hold* to make a correct
top-level change approaches zero. Baseline at authoring: ~15.

---

## 2. The problem is not a big number. It is an unknowable one.

Measured at authoring:

| Composition root | fields | `Option<…>` | nominal configurations |
|---|---:|---:|---:|
| `sovereign-mesh::EmbeddedDaemon` | 22 | 17 | 131,072 |
| `sovereign-core::Runtime` | 32 | 15 | 32,768 |
| `sovereign-desktop::AppState` | 22 | 16 | 65,536 |
| `commonwealth-api::AppStateInner` | 60 | 14 | 16,384 |
| **combined** | | **62** | **≈ 4.6 × 10¹⁸** |

**`EmbeddedDaemon` is now collapsed** (2026-08-24, daemon-convergence Phase 2):
10 fields, and its 17 `RwLock<Option<T>>` became one `DaemonServices` sum with
three variants — `MeshAdmin`, `Desktop`, `Headless` — named by the pair-
independence pass of §4 over the live construction sites, not chosen in
advance. 2¹⁷ → 3. Two `Option`s remain and neither is host-settable: the
serving `InferenceProvider` and (inside the variants) the named-absence types
`McpSurface` / `EmbedAdvertisement`, whose absence is a disk or probe failure
carrying its reason rather than a hole. `SetupConfig` stopped being optional
entirely — `SetupConfig::unconfigured()` is byte-identical to the fallbacks the
`None` arm used to apply, so "no config" was never a distinct state, only an
unnamed one. The row above is left at its authoring measurement because §2 is
the baseline this document is judged against; the delta is the result.

Plus **249 distinct `SOVEREIGN_*` environment reads** in first-party code —
158 registered in `quality/env-flags.toml`, **157 unregistered**
(`quality/baselines/env_unregistered.txt`). Each is an independent binary
choice, and because they are read at the point of use rather than at
construction, they are not configuration at all. They are *state*.

The reachable subset is certainly far smaller than the nominal one — handlers
touch small disjoint slices, and most combinations never occur. **But nobody
can compute it.** Not this seat, not three subagents with two hours, not a
maintainer on their first week. That is the actual defect: not that the space
is large, but that it is *uncountable by any available means*, so it cannot be
enumerated, tested, or explained.

**A blocklist of known-bad states is what you write when you cannot
enumerate.** It never composes: there is always one more. The goal is the
opposite — an allowlist with a cardinality proof.

---

## 3. The move: product to sum

62 independent `Option`s is a **product** type: 2⁶². Five named profiles is a
**sum** type: 5. The state space did not explode because the system is
complicated. It exploded because a single closed choice was decomposed into
sixty-two independent flags.

`TARGET_ARCHITECTURE.md §5` already declares that closed choice — profiles
`assistant-local`, `assistant-mesh`, `daemon-headless`, `server-multitenant`,
`bench`. It calls them "declared and validated, so *which capabilities are
wired* is a value you can print, test and diff — not an emergent property of
which builder methods a call site happened to chain."

That is exactly right, and it was never built. Those five names occur **zero
times anywhere in the tree** outside the document declaring them.

Four techniques carry the collapse. Each is ordinary Rust, none is type-level
astronautics:

1. **Sum, not product.** `EmbeddedDaemon`'s 17 `RwLock<Option<T>>` slots become
   one enum whose variants are the profiles that actually install them.
   2¹⁷ → 3.
2. **Typestate for phases.** Construction is a sequence, not a set of flags:
   `Spec → Wired → Running`. An install out of order does not compile.
   *(The ordering constraint this was written for — `set_corpus_engine` must
   precede `try_resume`, enforced by a comment at `state/builders/mod.rs:14-24`
   — was retired on 2026-08-24 by technique 3 instead: the engine is a
   constructor argument, so there is no ordering left to get wrong. Typestate
   is still the answer where a phase order genuinely survives; the daemon was
   not such a case.)*
3. **The profile is the constructor.** `Profile::build(Config) -> Running` is
   total. There is no partial system between call and return, so there is no
   window in which a request can observe a half-wired root.
4. **Capabilities are tokens, not booleans.** `CorpusWriter` is a value
   obtainable only from a lease. Holding it *is* the proof; there is no flag
   to check and no flag to forget.

---

## 3.5 The destination — daemon topology

Measured, not sketched. The variant set below comes from the pair-independence
pass over `EmbeddedDaemon`'s 17 slots (note `258e5319`, receiver-proven by grep
over every `.set_*` / `.install_*_router(` call site outside `target/`); the ring
membership comes from the receiver-scoped `Runtime` mutator matrix of the same
day. Anything here that is not yet true is marked `TARGET`.

### One process assembles; everything else is a surface

```
   desktop        svrn chat        server         bench / eval
      │               │               │                │
      └───────────────┴───────┬───────┴────────────────┘
                              │  turn protocol — tokens · narration ·
                              │  interpretation · clarification · metadata
                              ▼
  ╔══════════════════════════════════════════════════════════════════════╗
  ║ DAEMON — the only process that assembles a Runtime      TARGET       ║
  ║                                                                      ║
  ║   Runtime  =  CORE ONLY                                              ║
  ║     inference · store · corpus_engine · router · planner             ║
  ║                                                                      ║
  ║   owns exclusively: compute children · LocalCorpusManager ·          ║
  ║   KnowledgeViewManager · NoteStore · SCIP graph + Reindexer          ║
  ╚══════════════════════════════════════════════════════════════════════╝

  Everything else is a VALUE BUILT PER TURN and passed down — not a member
  the Runtime holds and stages reach back into:

     request ──▶ Scope          what this turn MAY SEE
                   corpus_ceiling (airtight) · enabled_corpora (forgeable)
                   · sensitive_corpora · folder labels
                           │
                           ├──▶ Capabilities   what this turn MAY DO
                           │      tools · skill · approval
                           │
                           └──▶ Lane           what enriches THIS stage
                                  atlas_context · wikipedia_graph ·
                                  meta_atlas · bridge · rerank ·
                                  gliner · conv_tiered
                           │
                           ▼
                    RetrievalPipeline  (steps receive Scope + Lane;
                                        they do not hold a Runtime)
```

**The composition is not a new invention — it already exists twice, and was
never applied consistently.** That is the whole finding:

- `PipelineState<'ctx>` (`retrieval_pipeline.rs:321`) already carries policy as
  per-request VALUES, and already draws the distinction that matters:
  `enabled_corpora` is *"CLIENT-CONTROLLED and forgeable — NOT a security
  boundary"* while `corpus_ceiling` is *"the airtight upper bound … INDEPENDENT
  of the forgeable selection"*. That is a `Scope`, resolved once per turn.
- `PprLane { graph, engine, rerank_fn, gliner }`
  (`retrieval/query_expansion.rs:256`) already bundles the enrichment providers
  one stage needs and hands them down. That is a `Lane`.

What has not happened is the other **35 reach-throughs** of the form
`self.<enrichment_field>` scattered across `runtime/retrieval/*`,
`evidence_loop`, `streaming.rs` and `turn.rs`, where a stage closes over `&self`
and pulls what it wants out of the Runtime. Those are what make the Runtime fat,
and grouping them into sub-structs does nothing about it: `self.gliner` merely
becomes `self.enrichment.gliner` — the same coupling down a longer path.

**The falsifiable bar, therefore, is a count, not a shape:** reach-throughs from
a pipeline stage into a Runtime enrichment field go **35 → 0**. When it is zero,
the Runtime is core-only because nothing else can reach it, and `Scope`,
`Capabilities` and `Lane` are values a caller constructs — which is also what
makes a turn testable without building a Runtime at all.

The ring a capability sits in is still decided by **what absence costs** — core
cannot answer, policy answers *wrongly*, capability cannot act, enrichment
answers less well — and that remains the placement rule for anything added
later. But the ring decides WHERE THE VALUE IS BUILT, not merely how the field
is grouped. Two corrections the rule forces: `corpus_engine` is `Option`
today and all three hosts set it unconditionally, so it was never optional; and
`sensitive_corpora: None` currently means *"no sensitivity gate applied, all
corpora eligible"* — a privacy control defaulting to permissive, which is §7's
structural-invariant rule inverted.

### Five capabilities leave the Runtime entirely

Not demoted to an outer ring — **gone**:

```
  mesh_knowledge  ─→  dissolves.  MeshKnowledgeClient posts to
                      127.0.0.1:9741/v1/knowledge/search — "the client API port
                      of our own embedded daemon" — and that route already
                      merges local + peer with (corpus_id, content) dedupe.
                      Inside the daemon it is a loopback call to itself.
  compaction      ─→  a thing that RUNS; belongs to whatever owns background
                      tasks, not to the object that answers questions.
  routing_events  ─→  per-connection wire concern. A surface subscribes;
  landscape_digests   the core holds no sink.
  corpus_principal ─→ a per-request field. A tenant is a property of a
                      request, not of a process.
```

### The daemon's construction variants — three, and they nest

```
        Headless                         Desktop
     svrn daemon run                  embedded / child
       A + B + C                          A + B
            └────────────────┬───────────────┘
                          A + B
                             │
                        MeshAdmin
                 svrn mesh create / join
                    (no construction slots)

  A  corpus_engine · inference_provider · embed_model · state_store
  B  mcp · setup_config · mesh_http · admin_http · project_http ·
     reading_http · corpus_watch_http
  C  provider_factory · mesh_store · convergence_recorder ·
     knowledge_view_http · solve_http                    Headless only
```

It nests as of 2026-08-25. `state_store` was class D — Desktop only — which is
what made Headless and Desktop incomparable rather than nested; Phase 3 moved it
into A. There is no class D.

~~Class D crosses what would otherwise be a clean nesting~~ — **closed 2026-08-25.**
The crossing was a bug, not a shape: the CLI daemon installed `reading_http`
while owning no `state_store`, so conversation-history chunks rendered
title-less on a headless daemon. Phase 3 gave the daemon its own
`<data_root>/sovereign.db` and moved the store into `ServingCore`, so class D is
empty and the lattice nests: **Desktop = A + B, Headless = A + B + C.** The
desktop variant is now literally `Desktop(Box<ServingProfile>)` — the nesting is
in the type rather than in this drawing.

A fourth apparent variant was measured and rejected. `cli_setup_wiring`
(`state.rs:1007`) gates the desktop's whole HTTP surface on the `ConfigSource`
captured at *probe* time, but every path that reaches a corpus engine has already
loaded `config.toml` from disk — so the wizard path yields a daemon with an
engine, a provider and a store but **no `/v1/mesh/*`, no `/mcp`, and no model-slot
registration**. That is a stale-snapshot bug, not a deployment shape, and it
collapses into Desktop.

### What the topology forbids

The point of the drawing is the states it removes. Each is unrepresentable —
not detected, not gated:

| Forbidden | Because |
|---|---|
| Two hosts with different capability sets | one process assembles; `Runtime` is not public |
| A bench measuring a configuration no user runs | the bench is a surface, not an assembler |
| A daemon serving a route it has no slot for | each ring is total; no `None` to 404 on |
| A second per-user data root | one accessor; `mesh_data_dir()` deleted |
| A capability wired in 2 of 3 hosts | there is one host |

Three states remain runtime-refusable rather than type-forbidden, and are
reported loudly (§18.3) rather than defaulted: a legacy data root that still
holds live data, two processes writing one corpus index, and a cancel that must
cross a process boundary.

---

## 4. The property to hold — not a number to hit

The temptation here is to name a target cardinality: *N states, printed on one
screen*. Resist it. A count is the wrong shape of claim, for two reasons. It
is unstable — the honest number changes the first time a legitimate profile is
added, and an architecture whose guarantee expires on the next feature is not a
guarantee. And it is not what legibility actually requires: a maintainer does
not hold a list of states, they answer a short series of questions. **The
property is factorization, not smallness.**

State it as two halves, which together are the whole claim:

> **Sound** — every state the types can represent is one the system can
> actually be in. No representable-but-dead configurations.
>
> **Complete** — every state the system can be in is one the types can
> represent. No reachable-but-unnameable configurations.

Today both fail, in opposite directions and for the same reason. Soundness
fails at 2⁶²: overwhelmingly most representable configurations are dead, which
is why reading the struct teaches you nothing. Completeness fails at 249
environment reads: real behaviour is selected by values no type mentions, which
is why reading the struct *misleads* you. When both hold, reasoning from the
types is reasoning correctly — and that, not a small number, is what removes
the derivation tax.

Cardinality then falls out as a *consequence*, and can be computed rather than
promised. That is the right relationship: the count is a diagnostic readout,
never the goal.

### The methods

Four, each an algorithm applicable to any struct in the tree, none of them
type-level astronautics:

**Factor into orthogonal questions.** A top-level state should decompose into a
small set of independent factors — which profile, which role, which
capabilities are held — each a closed set readable in one sitting. Legibility
comes from the *number of questions*, not from the product of their answers.
Three questions with five answers each is legible; sixty-two yes/no flags is
not, even though the second space is smaller.

**Product to sum.** Where two fields never vary independently, they are one sum
type, and a field meaningful in only one variant belongs *inside* that variant.
Mechanically: for each pair of `Option` fields on a root, ask whether any live
path sets one without the other. If none does, they are one variant. This is
the algorithm that collapses `EmbeddedDaemon`, and it terminates.

**Totality over assembly.** A constructor is a total function from
configuration to a running system. Where a phase order genuinely exists, encode
it as typestate (`Spec → Wired → Running`) so that an out-of-order install does
not compile. The daemon turned out not to need it: once every dependency is a
constructor argument there is no order to violate, and the one constraint the
codebase enforced by comment (`set_corpus_engine` before `try_resume`,
`state/builders/mod.rs:14-24`) is gone rather than encoded.

**Witnesses over checks.** Replace "am I permitted?" with holding a value that
exists only if you are. The same algorithm applies at three altitudes here: a
lease witnesses `CorpusWriter`, a seal witnesses `Evidence`, a judgement
witnesses `Answer`. A witness cannot be forgotten the way a flag can, and it
travels with the value it licenses.

### The axes, and what each method buys

| Axis | Today's disorder | Method | Falsifier |
|---|---|---|---|
| **Root construction** | 2⁶² nominal across four roots | Product to sum, then totality | Soundness: every variant is constructed by some live path, and every construction path names a variant |
| **Process role** | 13 binaries × 1–4 undeclared modes | Factor: role is a question the entry point answers | Completeness: no binary reaches a runtime posture its declared role does not name |
| **Environment** | 249 reads (158 registered, 157 not) | Factor: structural flags fold into profile variants; tuning flags become fields read once | Completeness: no `env::var` read selects behaviour after construction |
| **Storage writers** | unbounded `CorpusEngine` instances per root | Witnesses | A writer obtained without a lease is not constructible |
| **Verb implementations** | `enrich` 4, `converge` 5, `retrieve` 2, `compose` 2 | Product to sum at the call site; one decider (§10.6) | Soundness: each canonical type has exactly one constructor and no live bypass |

The falsifiers are **differential**, which is what makes them methods rather
than thresholds: compare the set of states the types admit against the set of
states observed across a real run, and require the two to coincide. A variant
nobody constructs is unsound. A posture nobody can name is incomplete. Either
direction fails the gate, and neither depends on agreeing a number in advance.

---

## 5. Why a bounded space subsumes the hazard list

Thirteen hazardous states were enumerated while deriving this. They are not
the specification — they are the **verification suite**, because each one is
unreachable as a *consequence* of the budget above rather than as a separate
fix. Kept because §18.1 requires a failing input you can name.

| # | Hazard | Evidence today | Retired by | Tier |
|---|---|---|---|---|
| 1 | A prompt fed by content that never passed `retrieve` | `CorpusIndex::retrieve` still has **zero production callers**, and 47 `ScoredChunk` literals remain. **The worst instance is closed (2026-08-25, Phase 9 first rung):** `retrieval_pipeline.rs:951` injected model-authored prose at `score: 1.0` into the evidence pool, and both gate defences missed it by construction — the empty `metadata` map read as `Grain::Leaf` (quotable), and since the pool was empty whenever it fired, `custody_engaged` was false so the custody refusal never ran. It was citable. Deleted; the disclosure it carried now renders into the prompt from the typed `unavailable_corpora` that already travelled there. Pinned by `evidence_pool_census.rs` | Verb axis | type |
| 2 | An `Answer` released without a `Judgement` | `Draft::release` reached at `grounding/mod.rs:1645` only — 1 of ~15 gate exits; flattened back to `String` at `:1665` | Verb axis | type |
| 3 | Two daemon implementations binding `:9741` | **Closed 2026-08-24 — deleted, not gated.** `commonwealth-daemon` (vestigial: `current_thread`, `NodeId::generate()` per boot, empty `Mesh`, no gossip; and per the 2026-08-05 review, no `join` subcommand and no `model pull`, so an inference plan could never arrive) is gone, with its three contrib packaging artifacts and the five shipped strings that pointed users at it. `EmbeddedDaemon` is the only implementation that binds the port. Consequences: `quality/TOPOLOGY.toml`'s `one-implementation-per-process` invariant flips to `holds = true`, `routing-field-guide.md §1` collapses from two daemon shapes to one, and `state.local_inference` loses its sole `None` producer (`main.rs:828`) so it can stop being an `Option` | Role axis | type |
| 4 | A binary whose role cannot be named | `sovereign-desktop` has 4 modes; `--daemon-child` is the daemon. **Also `sovereign-server`** — spawned as a supervised child by `mobile_host_setup::start` and `exec`d by `mobile_cmd.rs:152`, and until 2026-08-24 named by no variant at all, which is why the number is 8 → 3 → 3 and not 7. `Launch::Server` now names it and `is_resident()` covers it. Naming it is the precondition, not the fix: the assembler built for §10's acceptance criterion still has to own its lifecycle (hazard 10) | Role axis | type |
| 5 | `:9741` meaning different things by host | **Re-measured 2026-08-24 at `fc709d94`: desktop 5 of 7 routers, CLI daemon 7 of 7 — a delta of exactly two (`knowledge_view_http`, `solve_http`), not the "4 vs 6" first recorded.** Both original counts missed `corpus_watch_http`, which each host installs indirectly through `WatchedSubsystem::install`. The three-setters half was correct: `set_provider_factory` / `set_mesh_store` / `set_convergence_recorder` were CLI-daemon-only. **Closed** — the delta is now a field of `DaemonServices::Headless`, and mesh/admin/reading are built by the daemon from its own `Weak<Self>`, so no host can differ on them | Root construction | type |
| 6 | A shell that is conditionally a daemon | `SOVEREIGN_USE_SUPERVISOR` picks in-process vs supervised at runtime. Declared correctly in `quality/env-flags.toml` 2026-08-24 (it had been filed under `mesh` as "route distributed-inference workers through the supervisor", which it has never done) and the in-process branch now claims the data root's run lock — but a flag still selects which process is the daemon, so the hazard stands until Phase 10 | Environment | type |
| 7 | A subsystem reachable only by `exec` | `enrich_cmd` still private in a leaf binary and still reached by `exec`. **The REPARSING half closed 2026-08-26:** `sovereign-tools/src/enrich.rs` no longer regex-matches the CLI's human banners — a nine-function parser plus an `is_noise` allowlist plus a `classify_reason` that keyword-matched free text back into a `failure_kind` the child had already computed as an enum and discarded. The child now encodes typed events through `corpus_engine::…::progress::wire`; one writer, one reader, one declaration. **Deleted:** all nine `parse_*`, `is_noise`, `classify_reason`. **State made unrepresentable:** a banner reworded for a human changing what the desktop believes about a running enrichment. What remains is the `exec` itself — see rung 9.3 | Verb axis | type |
| 8 | An injected hook nobody calls | `CorpusEngine::fast_inference`: setter has zero callers, field never read, doc claims otherwise | Root construction | type |
| 9 | A spawner that spawns nothing | `spawn_auto_collaborate_loop` (`daemon.rs:2833`); `auto_collaborate_loop` (~1000 lines) has zero callers | Role axis | type |
| 10 | Cleanup that never runs | All exits via `fast_exit_skip_destructors`, so every `impl Drop` abort is decorative; `sovereign-server` has zero signal handlers. **LIVE INSTANCE, root-caused 2026-08-24:** an orphaned `sovereign-server` (PPID 1, 6d17h old) was found LISTENING on `0.0.0.0:8080`. `mobile_host_setup.rs:11-14` documents its death mechanism as `kill_on_drop(true)`; `main.rs:829` exits via raw `_exit`, so that never fires; and `main.rs:823` reaps the *daemon* child (`stop_daemon_child`) with **no `stop_mobile_host` counterpart** — the handle is aborted only by the Settings toggle (`config_setup.rs:42`). One orphan per quit. `supervisor_setup.rs:200-204` already records this exact reasoning as false, having learned it for the daemon child in 2026-08 and never generalised it. The leak self-limits in count because the first orphan wins the port and later spawns fail to bind — which is worse than accumulation: a stale build serves mobile access on all interfaces while the desktop believes it started a fresh one | Role axis | type |
| 11 | Two processes writing `chunks.lance` | `SOVEREIGN_ENRICH_SKIP_INDEX` is the only guard — set by hand, and unregistered (`env_unregistered.txt:51`) | Storage axis | runtime |
| 12 | A cancel that cancels nothing | `CancellationRegistry` claims "single source of truth … across the daemon"; it is process-local | Storage axis | **type (reclassified 2026-08-24)** — it is process-local only because no daemon-side session owner exists, and Phase 5 creates one |
| 13 | Whichever file lands first wins | `sharding.rs:29-37` says "is a race"; default merge arm is newest-mtime-wins, loser kept as `.superseded` | Storage axis | runtime |

Eleven of thirteen are closed by types. Two are genuinely runtime and must fail
loudly (§18.3) — pretending otherwise makes "impossible" a slogan. (Hazard 12
moved into the type column on 2026-08-24: it was filed as unclosable when what
it actually described was a missing owner, which Phase 5 supplies. The two that
remain — concurrent writers to one corpus index, and last-write-wins on shard
merge — are contention over on-disk state that no type can arbitrate.)

---

## 6. The register cannot currently see any of this

`quality/CONCEPTS.toml` carries `status` (how far migration got) and `home`
(does the destination path exist). Neither asks the question that decides
whether an invariant holds: **is the alternative path gone?** `home = minted`
is true for `Evidence` and worth nothing, because nine other doors are open.

**Required:** an `adopted` column whose value is the count of remaining
constructors *other than* the canonical one. `adopted = 0` is the only value
that means the invariant holds. This is the same measurement the budget's
falsifier column uses, so the register and the topology agree by construction.

---

## 7. How this document stays honest

It describes a failure mode it is susceptible to — seven documents here
already declare invariants that no longer hold, and none of them knew.

1. **Every falsifier becomes a test or an `xtask` gate.** A falsifier that is
   not executable marks its row a target.
2. **`cargo xtask target-arch` joins the `quality` gate list.** It exists
   (`corpus-engine/xtask/src/target_arch.rs`, `STALE = 1`) and is absent from
   `quality_cmd.rs`'s gate table — which is precisely how §5's profiles reached
   zero adoption unnoticed.
3. **The differential is the human-facing proof.** `svrn topology states`
   renders both sides — the postures the types admit, and the postures observed
   across a real run — and the build fails when they disagree in either
   direction. A maintainer does not read it to memorise a list; they read it to
   confirm that the questions they would ask are the questions the system
   answers.

---

## 8. Corrections this forces elsewhere

Required, or the contradiction stands (ARCH §1.1):

- `TARGET_ARCHITECTURE.md §3.1` corollary 1 — "`retrieve` is the only door" —
  is marked `partial`. The honest marker is `target`.
- `TARGET_ARCHITECTURE.md §5` profiles are unimplemented. Mark `target`; they
  currently read as description. **They are also the correct design** — §3 of
  this document is their implementation plan, not a replacement.
- `corpus-engine/DECOMPOSITION.md` steps 5–8 never shipped, which is why 73.8k
  lines of `enrichment/` remain inside `corpus-engine`. Record the stall.
- `corpus-engine-notes/src/notes.rs` was flagged in that plan at 2,781 lines.
  It is now 7,794 — a store, a vector index and a replication log in one file.
- `SOVEREIGN_ENRICH_SKIP_INDEX` must be registered in `quality/env-flags.toml`
  regardless of what else happens, since it currently gates index corruption.

---

## 9. Ownership — three engineers, one axis each

Ownership follows the axes, not the processes: two of the three obvious
"runtimes" owned zero crates exclusively (`commonwealth-daemon` was 1,145 lines
over a 516k-LOC closure, `sovereign-server` 7,375 over 535k), and 63% of their
union is shared by all three. `commonwealth-daemon` was deleted on 2026-08-24 —
the measurement stands as the reason it went, and the axis-not-process framing
is what it demonstrates: a "runtime" owning no crates exclusively was never a
work boundary. A runtime boundary here is a linking boundary,
not a work boundary.

| Owner | Axis | Charter | Proof they are done |
|---|---|---|---|
| 1 | Process role + root construction | `Profile`, `Role`, typestate, one shutdown path per role | The role/profile differential closes: no variant goes unconstructed, no runtime posture goes unnamed |
| 2 | Storage | `CorpusWriter` lease, cancellation scoping, promote ordering; decomposes `corpus-engine` | A writer is not constructible without a lease; `SKIP_INDEX` deleted rather than registered |
| 3 | Verbs + environment | One decider per verb; structural env flags fold into profile variants | Each canonical type has one constructor and no live bypass; no `env::var` selects behaviour after construction |

The four god-crates — `corpus-engine` 157k, `sovereign-cli-llm` 138k,
`sovereign-core` 118k, `sovereign-tools` 90k, together 50.6% of the workspace —
are a chartered shared project with one owner each, funded before ownership
hardens. `layer-gate` currently fails on the fan-in ratchet (`corpus-engine`
16→18, `sovereign-contracts` 19→20, `sovereign-core` 15→17) with zero ordering
violations: the gate is correctly reporting that the shared core keeps growing.

---

## 10. Sequencing — the eight phases

**This section absorbed `~/.claude/plans/design-the-daemon-convergence-wobbly-feigenbaum.md` on 2026-08-24.** That plan and this document had grown duplicate copies of the target shape, the ring test, the five departing capabilities and the bar — §10.6 in document form. Worse, it lived outside the repo, so no worker, peer or other harness could read it: two workers executed phases from a document they had no access to, and it had already drifted (it cites note `81e9f605` as settled; that note is retired, superseded by `b2aa9fb8`). One source, in the repo, or this happens again.

**Standing bar on every phase: name the state it makes unrepresentable.** A phase that cannot name one is scaffolding and is labelled as such, not counted as progress. Phase 0 is scaffolding by this test and is honest about it.

**Second bar, added 2026-08-24: name what you deleted.** Per §6, the count of remaining non-canonical constructors must reach zero; a phase that adds a type without closing the alternative doors has not landed an invariant.

| # | Phase | Status |
|---|---|---|
| 0 | Shrink the `Runtime` surface | **DONE** — `runtime.store` desktop 11 → 0, server 20 → 0; `runtime.tools` server 4 → 0. Desktop surface 9 methods + 5 fields → 9 + 4 (not the 5 → 2 estimated: `tools`/`skills` have no host handle to point at, a Phase 4 decision; `sessions` is a chat op, Phase 5) |
| 1 | One mesh identity, structurally | **DONE 2026-08-24** — `rebrand::mesh_data_dir()` deleted, zero callers; desktop `default_data_dir`/`default_skills_dir` → `rebrand::data_dir()`, which was the fresh-install split-brain source. Run lock re-keyed off `$HOME` onto the DATA ROOT and moved to `sovereign-contracts::run_lock` (one implementation; the desktop's in-process daemon, which never locked at all, now takes it). Loud refusal shipped as `data_roots::classify` — an empty root beside a live former root REFUSES, two live roots WARN. **Deleted:** `acquire_run_lock`/`try_flock_exclusive` (private copy), `ablate.rs::daemon_lock_path` (second derivation), `SOVEREIGN_ALLOW_MULTIPLE_DAEMONS` (the escape hatch the wrong key needed). `SOVEREIGN_USE_SUPERVISOR` re-declared: it is a desktop kill-switch, not distributed-inference routing |
| 2 | `EmbeddedDaemon`: product to sum | **DONE** — 22 fields → 10, 17 `RwLock<Option<T>>` → 2 (neither host-settable), zero `set_*`/`install_*_router` tree-wide. Variants named by the pair-independence pass, not chosen. Landed shape + the five things not to undo: note `b4b36597` |
| 3 | The daemon grows a state store | **DONE 2026-08-25** — `sovereign daemon run` opens `<data_root>/sovereign.db` and `state_store` moved from `DesktopServices` into `ServingCore`, so both serving variants carry one and the lattice nests. **Deleted:** the `DesktopServices` struct (once the store left it, it wrapped exactly one field, so `Desktop(Box<ServingProfile>)` now states the nesting in the type); `DaemonServices::state_store()` (artifactual the moment both variants had one — accessors 3 → 2); the second `InMemoryStateStore` the watched-folder subsystem was handed, whose `delete_corpus_state` was a no-op against an empty map, so removing a watched folder left its rows in the real db forever. **State made unrepresentable:** a serving daemon with no conversation store — i.e. a headless daemon that mounts `reading_http` and answers every conversation-history chunk with `title: null` |
| 4 | The daemon assembles the one `Runtime` | 4a **DONE**, 4b **DONE**. See the two rows below |
| 5 | The daemon serves the turn | **DONE 2026-08-25** — 5a, 5b and 5c all landed. Sizing it found two moves the plan never named, both preconditions rather than parts: the daemon cannot commission a `Runtime` because five of its nine arguments are built inside `sovereign-cli-llm` (5a), and the only turn protocol that exists is private to the `sovereign-server` **binary**, which nothing depends on (5b). See the three rows below |
| 6 | Hosts become surfaces | **THE CHAT SURFACE LANDED 2026-08-25 (session 5); the other hosts remain.** `svrn chat ask` and `svrn chat session` hold no `Runtime`, no store and no corpus engine — they are clients of `sovereign_mesh::turn_http` over `sovereign-turn-client`, a NEW crate in the **contract** layer beside `oicp-client` (protocol types plus the client that speaks them; its only non-leaf dep is `sovereign-contracts`, so a surface can depend on it without dragging a serving host's world along). `TurnClient::run_turn` is the client-side mirror of `serve_turn` — one implementation of "drive a turn and tell me what it did", where five ask commands each had their own. **Converting it required three protocol additions, and finding them is the phase's actual yield.** (1) **`--naked` had no wire form at all**, so the conversion would have silently dropped the flag and returned a grounded answer the client could not distinguish; it is `TurnMode::{Grounded,Naked}` now, a `serde(default)` field so every byte a client already sends still lands. (2) **The non-streamable-turn fallback existed twice and disagreed.** `chat_cmd/ask.rs` matched the refusal's error STRING and re-ran `handle_turn`; `eval_cmd/runner.rs` matched the VARIANT and re-ran `handle_message`; `serve_turn` did neither and emitted `StreamError`, so a document-attached question asked through the daemon FAILED where the same question asked in-process was answered. The two host copies are not interchangeable: the streaming path persists the user message BEFORE it bails, so the `handle_message` copy wrote the user's turn to the conversation twice. It is one decider now, inside `serve_turn`, taken BEFORE the call instead of caught after it — and the marker it turns on (`"[Document attached: "`, previously written out at six sites across four files) is `runtime::DOCUMENT_ATTACHED_PREFIX` with one predicate reading it. (3) **`Citation` carried neither `url` nor `provenance_tier`**, which are two of the five columns `svrn chat ask`'s sources footer prints — a footer whose own comment says its "whole point is diagnostic visibility". Both are `Option` + `skip_serializing_if`, so old citations serialize byte-identically. **A fourth capability was rescued rather than added:** `chat session` called `Runtime::end_conversation` on `quit` (the long-term-memory extraction pass) and the wire had no lifecycle call, so the conversion would have quietly stopped extracting memories on the one interactive CLI surface; `POST /v1/conversations/{id}/end` exists for it, and the CLI now REPORTS a failed pass instead of `let _ =`-ing it. **The census had to change, and that is a §18.4 result, not bookkeeping.** `COMMISSIONING_PROCESSES` counts files that build a `Runtime`; `chat_cmd/bootstrap.rs` stays on it while ANY of its thirteen callers needs an in-process assembly — including atlas backfills and the chaos harness, which are not turns and never become surfaces. Converting both real chat surfaces moved that number by **zero**. A number that cannot move under real progress does not measure it, so the bar is now `TURN_EXECUTION_SITES` — files outside the runtime that execute a turn, measured at **18 files / 32 call sites** before this session and **16 files** after, target empty. Each remaining entry carries what it would take to remove it, because "still on the list" and "cannot come off the list yet" are different states and only one is work: three report tools that force an intent AND read corpus indexes directly (neither is a turn, so neither belongs on the turn wire — they need their own surface); nine measurement harnesses that deliberately reach past the turn surface; the desktop, still blocked on the GLiNER fork; and the hub, which likely ends as a tenant-scoping front rather than a deletion. **SECOND WAVE, same day — and it started by disproving two of the three blockers the first wave named.** The first pass reported 16 remaining sites with reasons; on re-examination the reasons were wrong in a way worth recording, because both errors were the same error. **The bar was stated as one PROCESS and it should have been one DRIVER.** Nothing requires a host to speak HTTP; what §3.5 needs is a single implementation of "which handler runs, what happens to a turn that cannot stream, and how the result is projected". An in-process host reaches that through a `TurnSink`, an out-of-process one through `sovereign-turn-client`. (1) **The desktop was a category error.** GLiNER blocks the desktop adopting the shared RECIPE (phase 5a, `UNSHARED_RECIPES`); it has nothing to do with driving a turn. Since 5c the desktop hands its OWN `Runtime` to its in-process daemon, so `serve_turn` was reachable from its chat commands all along. Its real blocker is the Tauri event contract the Svelte frontend listens on by name, which a Rust build does not cover. (2) **The three report tools were blocked on ONE missing turn parameter, not on being un-turn-like.** `govern`, `portfolio` and `proxy ask` pin `Intent::KnowledgeQuery` to bypass the router; that is a turn parameter and `Intent` already lived in `sovereign-contracts`. It is `TurnRequest::Message.intent` now and all three drive `serve_turn` through a shared `StdoutTurnSink`. Their corpus-index reads — cited as the second blocker — never made them turn hosts; a tool can hold a corpus engine and not own a turn loop. **The server's REST route joined its own WebSocket route.** `POST /v1/conversations/{id}/messages` drove `handle_message_any`, which existed ONLY because REST wanted a non-streaming door and which re-implemented the recipe-author dispatch `handle_message_stream` already performs internally. It now drives `runtime::collect_turn` — `serve_turn` with a collecting sink instead of a forwarding one. **Named as a behaviour change, not a refactor:** REST used to run the non-streaming pipeline, so the same question over REST and over WS could be answered by different code; that divergence is what the phase exists to remove. **Deleted:** `Runtime::handle_message_any` and both `TenantRuntime` turn wrappers — what that type owns is tenancy, and running a turn only looked like its job because there was nowhere else to put the call. **A turn output that existed and was thrown away.** `Response.task` is set in exactly one place (the agentic path) and reached a client through exactly one door — the REST route reading the struct field. The STREAMING path called the same handler, received the same `Response`, and kept only `message.id` and `message.content`, so a streaming client could not learn a task existed at all. It travels in the persisted metadata blob now, like `provenance`, `citations` and `epistemic_state`, and both doors project the same value. **THE DOUBLE-WRITE WAS IN SIX PLACES.** The first wave found two hosts disagreeing about the non-streamable fallback; the real count of sites that caught the refusal and re-ran `handle_message` — writing the user's message to the conversation a second time, because the streaming path persists it BEFORE it bails — was `chat_cmd/ask.rs`, `eval_cmd/runner.rs`, `eval_cmd/runner_threads.rs`, `bench_cmd/live_runner.rs`, the three `ask` report tools, and the DESKTOP's `send_message_stream`, whose arm caught *every* stream-start error rather than just the refusal and whose comment cited the retired ComplexTask contract. All are closed: converted hosts cannot reach the fallback, and the rest choose `handle_turn` vs `handle_message` by whether the message was already persisted. **`TURN_EXECUTION_SITES`: 18 → 11**, and the eleven now carry reasons that survive examination. The nine harnesses are blocked on §18.4/§18.6, not effort: `collect_turn` runs the STREAMING pipeline while they call `handle_message`, which runs the non-streaming one — converting a bench silently changes what it measures, so it needs a pre-registered re-baseline rather than a build. **THIRD WAVE — the desktop and the harnesses, and a measured stop.** The desktop's structural blocker was real and is now solved rather than routed around: `send_message_stream` must return the message id SYNCHRONOUSLY (the UI puts its placeholder up while retrieval runs, which is most of a cold turn's wait) and `serve_turn` owns handle acquisition. `TurnSink::on_turn_started` fires at acquisition — the same moment the old code learned the id — so all three desktop commands (`send_message_stream`, `send_message`, `document_ask`) drive the one driver with **no TypeScript change**: the sink emits the same three events with the same payloads and reads the persisted metadata blob in-process, which a caller that owns the store is entitled to do. Two behaviours stopped being the desktop's private property in the process: the **graceful guards** (oversize paste, contentless message) are answered as a turn rather than errored — only this host got that right, every other rendered them as a crash — and `send_message` no longer runs a different pipeline from `send_message_stream`, where the answer used to depend on which door the user came through. **Three of the nine harnesses converted and six did not, on evidence rather than appetite.** `eval_cmd/runner.rs`, `eval_cmd/runner_threads.rs` and `bench_cmd/live_runner.rs` were ALREADY driving `handle_message_stream`, so `collect_turn` is instrument-neutral for them and the change is a deletion of a hand-rolled drain. The remaining six call `handle_message`/`handle_turn` — the NON-streaming pipeline — so converting them changes what every bank they score is measuring, which is a §18.6 re-baseline and not a build. **`TURN_EXECUTION_SITES`: 18 -> 6.** **Phase 7b now has a measured blocker instead of a guessed one.** Making `Runtime::new` uncompilable needs `UNSHARED_RECIPES` empty, and the obstacle is not plumbing — `common_parts` already names both remaining hosts as struct-update cases. It is that **the shared recipe's tool registry is not a superset of theirs**: counted by type name, `sovereign-server` registers **31** and the recipe **11**. The ~20 missing are whole families — code intel (five tools), notes (three), recipe authoring (five), plus compute, checkpoint, decision-log, capability-map, capability-request, research-finding, registry-browse, probe-url and document-operation. Adopting the recipe as it stands would DELETE them from the hub: a regression wearing a convergence badge, and the same shape `turn_tool_census.rs` recorded and deliberately declined to lower (union 33, common 7, divergent 26). The open question is therefore not *when* to convert but **which of those 20 belong in the registry every host shares** — and on a multi-tenant hub, code intel and shell over another tenant's workspace is a security decision, not a refactor. It wants an operator call before a diff. **Original sizing, still standing:** the surface it converts hosts ONTO exists as of phase 5c (`sovereign_mesh::turn_http`, driven end-to-end by `tests/turn_surface.rs`). Three processes to convert, in ascending difficulty: `svrn chat` (already daemon-backed and already on the shared recipe, so this is a deletion — but `build_session` has ~10 callers beyond chat itself: the chaos harness, the atlas backfills, `govern ask`, the portfolio and proxy asks); `sovereign-server` (its own reason to exist is the multi-tenant hub shape, so it may end as a tenant-scoping front over the daemon rather than a deletion); the desktop (embedded mode makes "talk to the daemon" and "be the daemon" the same sentence — though since 5c it already hands its ONE `Runtime` to its in-process daemon, so the two are no longer separate assemblies). **Three divergences the one recipe made visible on day one**, by giving the other hosts something to diff against — the argument for Phase 6 stated in defects rather than in principle. All three are fixed; the first two structurally, so they cannot re-diverge. (1) **The rerank capacity pre-flight ran on ONE of three surfaces.** `reranker_standalone::load_from_env`'s doc comment read "One loader for every shipping surface… so 'is a reranker installed?' has one answer per process instead of three (ARCH §10.6)" and it was FALSE — the CLI never called it, and the CLI's private copy was the only one carrying the VRAM fit check that note `b57b0cd5` (the 64 GB SIGTERM incident) exists to mandate. So the desktop and the hub could each load a rerank slot beside a resident primary with nothing checking whether the two fit, and learn from the OOM killer mid-turn — on the hub, taking every tenant's in-flight turn with it. A comment asserting a mirror that is not one is §7.2's smell, and this is the third time this program has found one (after `SOVEREIGN_RPC_CACHE_DIR` and `SOVEREIGN_WORKSPACE_DIR`). The pre-flight moved INTO the one loader, so all three inherit it; the return type is now `RerankLoad` — `NotConfigured` / `Loaded` / `Refused` / `Failed` — because "nobody asked for one" and "one was asked for and refused" read identically through an `Option`, and the second is the one an operator has to see (§18.3). (2) **The desktop never spawned the atlas bump-flusher.** The CLI and the server have since adaptive triage (Phase B2) landed. Without it `record_match` accumulates bumps in an in-memory map that is never written — so on the one long-lived interactive surface the triage signal was permanently dead and the map grew for the session's life, while the ledger reported the capability available. (3) **GLiNER loads eagerly in the recipe and lazily on the desktop** (`LazyGlinerExtractor`, a ~950ms load moved to a background thread, and the handle shared with document ingest). This one is NOT yet resolved: the desktop's is better and folding it in changes when the extractor is warm for the other two hosts, which is a retrieval-timing change and therefore a bench question, not a build-only one (§18.4). It is the reason the desktop is still on UNSHARED_RECIPES rather than converted in this pass. **A fourth was caught before it shipped, and it is the sharpest argument of the four.** `SOVEREIGN_RERANK_MODEL_PATH` names one GGUF and TWO different things load it: the daemon installs it as a slot INSIDE its embedded llama.cpp engine (`install_rerank_slot`), while `svrn chat` loads a STANDALONE reranker because its provider is remote HTTP and cannot rerank at all. The moment the daemon adopted the shared recipe it would have done both — the same weights resident twice in one process — and **the VRAM pre-flight just added would not have caught it**, because the pre-flight plans one rerank slot and there would have been two. A guard that cannot see the thing it exists to prevent is §18.1's "a check with no failing input you can name" arriving from the other direction. It is now `RerankWiring::{Standalone, AlreadyInProvider}`, a required host input like `ShellAccess` and `LaneWarmth`; the daemon's arm also records the gap it leaves — the daemon's turn gets no cross-encoder rerank until the lane can reach the host provider's own slot, which is a sentence a reader can find rather than a doubled resident model an operator infers from RSS. The payoff is unchanged: the bench measures the shipped assembly by construction. **The remaining three of the five ask commands converted 2026-08-26**, and they are the case that shows the bar is about the DRIVE, not about who holds a `Runtime`. `svrn govern ask`, `portfolio ask` and `proxy ask` each drove a turn by hand — call `handle_message_stream_as`, drain the chunks, print each delta, catch a refusal and re-run a different handler — and **all three copies carried the same bug**, which is what made them worth converting rather than tidying: each caught `Error::NotImplemented(_)` and fell back to `handle_message`, but the streaming path persists the user message BEFORE it refuses a document-attached turn while `handle_message` persists it and then runs the chain, so every one of them wrote the question to the conversation TWICE on that path. It is the same defect §10 records at `chat_cmd/ask.rs` and `eval_cmd/runner.rs`, in three more copies — a re-derived loop reproduces the bug it re-derives. They now call `serve_turn` with a `StdoutTurnSink` (`sovereign-cli-llm/src/turn_sink.rs`), which decides the document case up front so the fallback cannot be reached with a message already written. **These three deliberately KEEP their in-process `Runtime`** — they need a corpus engine to render a sources footer, which is not a turn concern and does not travel the turn wire — so they do not reduce `COMMISSIONING_PROCESSES`. The property is ONE implementation of the drive, reached through a sink by an in-process host and through `sovereign-turn-client` by an out-of-process one |
| 7 | Make the second assembly uncompilable | 7a **DONE 2026-08-25** — `MeshAdmin` carries a private `MeshAdminWitness`, so all three `DaemonServices` variants are now unconstructible outside `sovereign-mesh` and the compiler holds what a source census used to. **Deleted:** `tests/launch_assembler_census.rs`, whose module docs said it existed only to cover this gap. **7b UNBLOCKED 2026-08-26.** What held it was never plumbing: adopting the shared recipe would have DELETED ~20 tools from `sovereign-server` (31 registered by type name against the recipe's 11, and the sets nested in neither direction), so the remaining work read as a policy question about which tools every host should carry. It was open/closed stated as a defect — the recipe OWNED the list, so no host could add a family without editing a file every host shares. `sovereign_contracts::tool_bundle` inverts it: a host composes `Vec<Box<dyn ToolBundle>>` and the recipe folds it, naming no tool at all. **Falsifier:** `sovereign-runtime-recipe/tests/recipe_names_no_tool.rs`, watched to fail on a re-introduced `register(Box::new(ShellTool))`. **The capability question answers itself under the seam** — a bundle is built FROM the collaborators its tools need, so a host can only offer code intel over a SCIP handle it owns and a tenant-scoped host has no other tenant's handle to compose from; the hazard is unrepresentable rather than disallowed (§7). **Deleted:** `ShellAccess`, which could express exactly one policy fork while every other family stayed hardcoded; its replacement, `Withheld`, works for any family and keeps a withheld capability a written decision rather than a line missing from a file (§18.3). **State made unrepresentable:** a host that adopts the shared recipe and thereby loses a capability. **7b COMPLETE 2026-08-26. Both remaining hosts adopted, and `UNSHARED_RECIPES` is EMPTY.** The desktop's `bootstrap_with_progress` and `sovereign-server`'s `main` each held a near-copy of the router stack, the registry, the MCP loader and the enrichment lane; both now hand `RecipeInputs` over and struct-update the slots only they have — six for the desktop (compaction, landscape digests, mesh knowledge, the sensitivity and folder oracles, the Tauri routing sink), three for the server (tenancy principal, landscape digests, the narration sink). Turn-registry divergence **26 → 23** (`turn_tool_census.rs`, union 33, common 10): `KnowledgeLookupTool` and `AttachedDocumentSearchTool` were CLI-only and are now on all three, which is what adopting the baseline MEANS rather than a separate decision. **Near-copies had drifted in BOTH directions, and the drift is the argument:** the server never wired `lane.bridge` or `conv_tiered`; the desktop never spawned the adaptive-triage bump flusher, on the one long-lived interactive surface where it mattered; the daemon and the CLI ran `knowledge_lookup` with its notes channel dark while the desktop, which wired it by hand, did not; and `sovereign-server` read MCP servers only from its own `[mcp]` section, so `svrn mcp add` reached every surface except the one serving tenants. **Two §10.6 doors closed on the way:** `effective_search_registry` lived in `sovereign-desktop` and was reachable only from there, so `CoreTurnTools` built `search` through the legacy `SearchBackend::DuckDuckGo` enum and an operator-configured Tavily key reached the desktop and nothing else; and `CapabilityRequestTool`'s inbox dir was wired only by the desktop, so a request submitted through the server was written and then unreadable. **Split:** `WebTools` (any url) from `WikipediaTools` (en.wikipedia.org), because a host that reads Wikipedia from an installed corpus wants one without the other and could not say so. **New:** `ToolSwitches` — what a host CAN provide and what a person PERMITTED are orthogonal axes, composed rather than conflated; forcing bundles 1:1 with `enabled_tools` would have shattered every bundle into one tool each. **State made unrepresentable:** a host with a private dialect of the retrieval stack.

1. **`lane.bridge`.** The recipe loads a `BridgeIndex` unconditionally; `sovereign-server` never has. Adopting means the hub gains a retrieval enrichment it has never run. Nullable by struct-update on `CommonParts::parts.lane`, so the conversion CAN be behaviour-preserving — but "nulled to keep the diff honest" should be a decision someone takes, not one a night shift takes silently.
2. **MCP comes from a different file.** The recipe calls `sovereign_tools::mcp::load_from_setup_config` (the canonical config's `[[mcp_servers]]`); the server calls `McpServerManager::from_config(&config.mcp.servers, ..)`. Two doors onto one capability — a §10.6 duplicate the seam made visible — and adopting the recipe silently repoints which file decides what external tools the hub exposes. Closing it properly means MCP becoming a `ToolBundle`, which needs a keep-alive in `ToolBundle::register_into`'s return: the door yields a manager whose per-server statuses the boot banner prints, and the trait returns only a report. **That is a change to the seam rather than a use of it**, which is why it was named and not half-done when the seam landed.
3. **`conv_tiered` is NOT a delta** — it is a `RecipeInputs` field the host supplies, and the server supplies `None`. Checked rather than assumed.

The router is already identical on both sides (same `router_bootstrap::build_llm_router`, same `ExemplarOverrides::from_env_and_repo`, same authority probe), and the server's own bootstrap would have to be DELETED on conversion rather than left to run twice — re-embedding the exemplar set costs minutes on a cold CPU embed slot. What remains for 7b is per-host wiring — the desktop on GLiNER eager-vs-lazy (a bench question, §18.4), the server on `corpus_principal` (a struct-update override) |
| 8 | Delete `commonwealth-daemon` | **DONE 2026-08-24**, out of order on operator direction. See hazard 3 |
| 4a | Enrichment reach-throughs 35 → 0 | **DONE 2026-08-25** — `Lane` is a value stages receive; falsifier `tests/lane_reach_through_census.rs`. **State made unrepresentable:** a stage that enriches from a provider its caller did not resolve for the turn |
| 4b | Measure, then assemble | **DONE 2026-08-25** — measured (no variants; totality, not sum), `Runtime::new` total over the enrichment stack with eight builders deleted, and `sovereign_mesh::assemble` is the one exhaustive `Launch` match all four sites go through. Falsifiers 1 and 3 both met. **States made unrepresentable:** a host that means to wire a provider and silently does not; a crate outside `sovereign-mesh` composing a serving daemon at all |
| 9 | **Verbs — `retrieve` is the only door** | **RUNGS 9.1 AND 9.2 BOTH COMPLETE 2026-08-26** (hazard 2 closed — `GateOutcome` carries an `Answer`, so all 16 gate exits name a `Judgement`; falsifier `gate_release_census.rs`). First rung done 2026-08-25; the program is DESIGNED — see "Phase 9, designed" below, which sizes all three rungs and orders them 9.3 -> 9.1 -> 9.2.** Hazard 1's named failing input is closed: the `readiness_disclosure` step that pushed model-directed prose into the evidence pool at `score: 1.0` is **deleted**, and the signal it carried (`unavailable_corpora`, already typed and already reaching both consumers) now renders through `unavailability::unavailability_guidance` into the PROMPT. Falsifier `sovereign-core/tests/evidence_pool_census.rs`, watched to fail. **State made unrepresentable:** a pool entry nothing searched for; and, from 9.1's second half, **a chunk whose custody the reading site gets to decide** — the gate, the formatter, the merge pin and the glassbox projection all read one typed stamp, and the estate store's `Custody::Personal` is now a property of the door it comes through rather than a string this call site writes. **Deleted:** the `CUSTODY_META_KEY` write and both its production reads; `metadata["source"] == "raptor"` at all three compare sites. **`mesh_peer` closed the same day** by putting custody and grain on the knowledge wire, so the manufacturer ratchet is five — every one of them content this process genuinely builds. Remaining: 47 `ScoredChunk` literals, and the pool's element type is not yet `Evidence` |
| 5a | The runtime recipe becomes one shape | **DONE 2026-08-25** — `Runtime::new` takes one `RuntimeParts`; the ten surviving `with_*` builders are **deleted**. Measured first: no two hosts commissioned the same `Runtime` (desktop 11 builders, server 5, chat 3; only `with_corpus_engine` common to all three), and a builder chain records a call while recording nothing about a call not made. **State made unrepresentable:** a host that means to wire a slot and silently does not. Falsifier `sovereign-core/tests/runtime_commission_census.rs` — §6's `adopted` count, currently **3**, target 1 |
| 5b | The turn protocol leaves `sovereign-server` | **DONE 2026-08-25** — `ServerEvent` was one enum on two transports, and the rule keeping them apart was a doc comment. It is now two types in two crates: `sovereign_contracts::types::TurnFrame` (`Token` / `Complete` / `StreamError` / `Narration` / `QueuePosition` — per-turn, down the one socket that asked) plus `TurnRequest` for the inbound half, and a server-local `ExecutorEvent` (`StepDone` / `ApprovalReq` / `UserInput` — genuinely fan-out). `projection.rs` moved with them, so REST and WS project a message's metadata through one library function a daemon can also call. Both halves gained `Deserialize`: a frame only one side can parse is not a protocol, and that was the whole defect. **State made unrepresentable:** one tenant's tokens on the fan-out broadcast, i.e. an answer delivered to every other connected client — `event_tx.send(ServerEvent::Token { .. })` compiled before, and now reads `expected ExecutorEvent, found TurnFrame`. **Deleted:** `sovereign-server/src/projection.rs`, the private `ClientEvent`, and the `serde_json::Value` the server re-encoded every narration phase into (`phase` is `NarrationPhase` now, same bytes). Falsifier `sovereign-contracts/tests/turn_wire_form.rs` pins each variant's literal JSON — the extraction had to move zero bytes, and rustc is blind to encoding; sabotage-verified with a field rename, and the leak demonstrated as a compile error once, then reverted |
| 5c | The daemon commissions and serves | **THE TURN SERVICE LANDED 2026-08-25**; the daemon's own commissioning is what remains. `sovereign_core::runtime::serve_turn` is the one implementation of "drive a streamed turn, forward the narration, emit the terminal metadata frame", and `sovereign-server`'s WebSocket handler is now a caller of it rather than its owner (`ws.rs` 322 → 192 lines). **The terminal metadata frame this row said does not exist now does** — it is `TurnFrame::Complete`, and `serve_turn` fills it, so a turn's result is a value that serializes rather than a row the caller must be in-process to find. **§10.6 closed:** the re-read after drain was SIX hand-rolled copies, not three (server ws, desktop `commands/chat.rs` ×3, CLI `ask.rs` + `session.rs`); all six now call `runtime::message_metadata`. **Deleted:** `TenantRuntime::{handle_message_stream, message_metadata}` and `ws.rs`'s private `stream_turn`/`forward_narration`. Scoping is applied ONCE per turn at the host edge — the turn service takes an already-scoped id — where it used to be applied three times, and one disagreement between those was a cross-tenant read. **THE DAEMON NOW COMMISSIONS AND SERVES — 2026-08-25, second session.** `ServingCore` gained `runtime: Arc<Runtime>` (not an `Option`: a serving daemon that cannot answer is not a shape anybody deploys, and an `Option` would put back the crossing Phase 3 closed), and `sovereign_mesh::turn_http` mounts `POST /v1/conversations` + `GET /v1/conversations/{id}/stream` from `Arc<Self>` beside mesh/admin/reading — so a serving daemon cannot come up unable to serve a turn. The wire form is `sovereign-server`'s, deliberately: same paths, same frames, so a client cannot tell which it reached, which is the property that lets a host stop assembling and start connecting. Loopback-only, both layers, and added to `tests/loopback_parity.rs`'s every-router sweep. **The recipe stopped being three copies.** `Runtime::new` had three callers and each carried its own ~600-line recipe, because the recipe needs `sovereign-tools` + `sovereign-gliner` and every crate that could already see both was a host BINARY — the identical structural cause as `sovereign-enrichment-catalog`'s three copies of `config.json`. `sovereign-runtime-recipe` (capabilities layer) is now the one of them; `svrn chat`'s copy is deleted (911 -> 450 lines) and the daemon was built on the shared one rather than a fourth. **`commission` is the only `Runtime::new` in first-party production code.** **The census had to change with it (ARCH §18.4).** Counting `Runtime::new` files would now read **1** while four processes still commission — a green number for a target not met. It counts two things instead: hosts still carrying their own recipe (**2**: desktop, server; target 0) and processes that commission by either door (**4**; target 1, the daemon). **States made unrepresentable:** a serving daemon with no thing that answers; a host and its in-process daemon holding two different `Runtime`s (the desktop hands its own to `ServingCore`, so one process has one). **Falsifier** `sovereign-mesh/tests/turn_surface.rs` — a real listener, a real WebSocket, a real `TurnRequest` in and real `TurnFrame`s out terminating in `Complete`; sabotage-verified by renaming the route. It found a live defect on its first run: `StateStore::insert_empty_conversation` has a no-op `Ok(())` default, so `InMemoryStateStore` reported success for a write it never performed and every seed-then-turn path was untestable against it (§18.3, an absence dressed as a result). Overridden, with the trait default left as a named follow-up. **Proven on the deployed path, not only in a test.** `svrn daemon stop && start` with the new binary, then a real WebSocket client against `127.0.0.1:9741`: a grounded answer with **20 citations**, `routing_tier: LOOKUP`, `inference_backend: Qwopus3.5-4B-v3-MTP-Q8_0`, 235s wall on a host concurrently running a wikipedia-newsworthy reindex. Daemon boot 59s, RSS 20.0 GB (the three model slots dominate; the Runtime's own additions are the atlas caches and the GLiNER probe). **Two defects the deployed run found that no unit test would have.** (1) The meta-atlas is a **981 MB** JSON parse on this host and the recipe loaded it synchronously, so the daemon's boot would have blocked on it — the desktop had already refused to do that in 2026-06 and warmed it in the background by hand. That fork is now `LaneWarmth::{Eager,Deferred}`, a required field rather than a default, because getting it wrong is invisible in opposite directions: an eager service looks hung, a deferred one-shot silently answers its only question with a boost that had not landed. The `Deferred` arm fills the `ArcSwapOption` cell `LaneSources::meta_atlas` already was — the type had anticipated this fork and only the desktop used it. (2) **The turn socket answered no pings for the whole turn.** `serve_turn` was awaited inline in the receive loop — the shape `sovereign-server`'s handler still has — so the stream was never polled and the connection could not pong. The first real turn died at 20s with `keepalive ping timeout` while the daemon's log showed that same turn's retrieval completing normally: a client that would have waited 235s for a good answer instead got a dropped socket, and the server-side log said nothing was wrong. The turn is a task now and the receive loop keeps reading; one-turn-per-socket is held by an `in_flight` guard that refuses a second turn BY NAME rather than queueing it silently. Gated by `a_socket_still_reads_while_its_turn_is_running`, sabotage-verified against the inline shape. **`sovereign-server`'s handler had the identical defect** — this route was written from it — and is fixed the same way in the same commit, with its fair-scheduler permit MOVED into the task so the permit's lifetime is still exactly the turn's (that is what its old comment meant by "holding the receive loop here keeps the permit scoped to one in-flight turn", preserved by ownership instead of by blocking). Its seven `http_tests` including `ws_streams_tokens_then_complete` and both tenant-leak tests still pass. Remaining: narration frames (the daemon has no per-connection subscription yet, stated in `turn_http`'s module docs rather than discovered), and mid-turn approvals, which are refused loudly rather than accepted and dropped |
| 10 | **Environment — 249 reads become profile variants or construction-time fields** | **STARTED 2026-08-25**, opened by 4b landing. First pair done: `SOVEREIGN_USE_SUPERVISOR` + `SOVEREIGN_FORCE_LOCAL` are now `sovereign_contracts::launch::DaemonHost`, resolved once by the desktop's `main` beside `Launch::parse`. Three points of use became one construction-time read, and a §10.6 duplicate closed on the way (`bootstrap::detect` and `supervisor_setup::is_enabled` each parsed `SOVEREIGN_FORCE_LOCAL` independently). The in-process shape now carries WHY — `ForceLocal` vs `KillSwitch` — where the predicate could only report a bare `false`. **State made unrepresentable:** two call sites disagreeing about what the launch-topology flags mean. **Five more closed 2026-08-25**, all found by asking which `SOVEREIGN_*` names are read at more than one site — **26 of 243 are, and that list is the phase's work queue.** (An earlier revision of this row said 36 of 246. That number came from a regex anchored on `var(`, which `set_var(` also ends in, so it counted WRITES as reads — §18.4, validate the instrument before the result. The corrected instrument anchors on `env::var`, which is what `cargo xtask env-gate` already does; the two numbers are not comparable and the earlier one should not be read as a before. Separately, **36 names are WRITTEN by production code** — env used as an inter-module channel — and that is its own target class, larger than it looks.) **Two more closed 2026-08-25 (session 5), and the instrument was re-validated first.** A naive count said 48 names are read at more than one site against the 26 on record; the gap was the INSTRUMENT, not drift — "site" means distinct FILE, and counting occurrences double-counts a name read twice in one file. By file the number is **28**, which reconciles with 26 plus what has accumulated since. §18.4 again, and cheap to check before reporting a number as progress. **`SOVEREIGN_FORCE_CPU_CHAT` — four read sites, three files, now ONE reader** (`sovereign_inference::cpu_compat::force_cpu_chat`). The identical three-line parse (`"1"`, or `"true"` case-insensitively) was written out in the desktop's `model_compat` (whether to swap in a CPU-safe model), the desktop's `inference` builder (whether to run the GPU smoke test), and `embedded/model_slot` TWICE (the actual `n_gpu_layers` decision). The last is the one that moves the weights, so a disagreement between them means the app picks a model for a CPU it then does not run on. Note the flag is also SET at runtime by the desktop's GPU-crash fallback — environment used as an inter-module channel, which is the larger 36-name write class and is deliberately untouched here; this closes the read side. **`SOVEREIGN_DAEMON_URL` — the production duplicate is closed, and what remains is a LAYER fact rather than unfinished work.** `sovereign-cli-shared::urls::daemon_base_url()` was already THE accessor, and `sovereign-cli-dev`'s tool registry had its own read that had drifted twice, both toward the WRONG daemon rather than toward none: it never consulted `SVRNMESH_DAEMON_URL` (which the boot bridge maps the legacy name onto, so on a host configured that way every other reader followed the operator and this one silently took the default), and its default was `127.0.0.1` where the shared one is `localhost` — not the same address under IPv6-first resolution. The two remaining readers are EXAMPLES in `sovereign-inference`, which sits below `sovereign-cli-shared` in `ARCH_LAYERS.toml` and therefore cannot reach the accessor; consolidating those means moving the accessor down, which is a layer decision and not a tidy-up. **`SOVEREIGN_RPC_HEADROOM` — one parser and one default, where the promise had been held by two copies of a number agreeing.** `svrn mesh plan`'s stated contract is that "a previewed plan uses the headroom the load executes with", and it was kept by the literal `1.2`, the `>= 1.0` filter and the parse existing identically in `mesh_cmd` and in `rpc_headroom_factor` — the function that actually gates the load. (Worth recording that this one was NOT a live drift: a first read called `mesh_cmd` unfiltered, which was wrong — its filter sits after the `or_else` and a truncated view cut it off. §11.1, and the fix would have been written against a defect that was not there.) The split is `rpc_headroom_from_env() -> Option<f64>` plus `RPC_HEADROOM_DEFAULT`, because the CLI has a second source the daemon does not — it can run BEFORE bootstrap bridges `[shared_model] headroom` into the environment, and could not express that fallback against a function that had already substituted a default. `SharedModelFleet` (`SOVEREIGN_SHARED_MODEL_ID` + `SOVEREIGN_RPC_QUORUM_ANCHORS`, 3 readers) and `RpcServe` (`SOVEREIGN_RPC_SERVE`, 4 readers in 4 crates). Neither was a tidy-up: the readers had drifted apart on what counts as a value, and each drift was a node **advertising something it does not do**. Two readers of the model id accepted `"  "` and the third did not, so the node published a shared-model fleet its own inference provider declined to join; and `capabilities.rs` treated ANY value of `SOVEREIGN_RPC_SERVE` — empty string included — as `can_anchor: true`, gossiping full VRAM while nothing bound and no port was advertised. Both deciders are pure functions with the environment split off, so the exact inputs that produced the split are tests rather than a story. **State made unrepresentable:** a node advertising a capability it would refuse to provide. **Third rung, same day: `LlamaLogs`** (`SOVEREIGN_LLAMA_LOGS`, 5 readers). Not a preference knob — `install_log_tracing*` sets a **process-global** ggml callback and `void_logs()` disables it globally, so the LAST backend to initialise decided for every other one. Four of the five readers voided on anything but `"1"` while the primary engine defaulted to errors-only and alone honoured llama.cpp's own `GGML_RPC_DEBUG`. Net effect: a daemon that loads the primary and then the embed slot went silent everywhere, defeating the documented default that "a failed model load still explains itself instead of surfacing as a bare null result", and an operator debugging an RPC worker got output from one backend of four. One value resolved once and installed at every init makes the ordering irrelevant. **Six more closed 2026-08-26, and the queue is 27 -> 21 by the corrected instrument.** `SOVEREIGN_AGENTIC_KQ_DEBUG` (the gate's glassbox and the evidence loop's each had their own `OnceLock` and their own identical parse, so the two halves of one debug switch could disagree); `SOVEREIGN_STOP_SANDBOXED` (two functions, and this is the flag whose per-call-site shape let the service-manager leg get added ABOVE the guard — the incident that cost two lanes of "the daemon keeps dying"); `SOVEREIGN_RERANK_CANDIDATES_K` (the shared recipe's ablation branch re-parsed what `sovereign_tools::corpus` already read, so one tuned number had two answers depending on which host built the config); `SOVEREIGN_DISABLE_WIKI_GRAPH` (the desktop's own comment named this as a follow-up — "dedup to a shared crate" — and both hosts reach `sovereign-tools`, which is where the predicate now lives); `SOVEREIGN_RPC_WORKERS` (byte-identical comma-split in the CLI daemon's bootstrap and in `rpc_distribution`, feeding different things — discovery seeding versus the actual distribution); `SOVEREIGN_RPC_DISCOVER` (three presence checks, two of which feed a containment VERDICT, so a divergence meant `doctor` reporting a posture the daemon does not run under).

**What is left is mostly NOT more of the same, and that is worth stating so the next session does not price it as such.** Of the 21, several are the *write* class (env as an inter-module channel — `SOVEREIGN_ALTERNATION_GRAMMAR` and `SOVEREIGN_FORCE_TOOL_CALLS` are config propagated to process env at daemon boot and read per request, which is one writer and one reader, not two deciders). Several are examples rather than production. And three are LAYER decisions of the same shape as `daemon_base_url`, not tidy-ups: `SOVEREIGN_FORENSIC` is read in `sovereign-core` and in `sovereign-inference`, and **`sovereign-core` does not depend on `sovereign-inference`** — checked, not assumed — so one reader means moving an accessor across a layer; `SOVEREIGN_IROH_RELAY_ONLY` is read in `sovereign-mesh` and in `commonwealth-transport`, which are peer projects, so it needs a shared home below both. |

**Phase numbers are stable IDs, not an order.** Notes and worker orders already cite them. The order is the critical path below.

Phase 4 has two halves and they are ordered: **4a** kills the enrichment reach-throughs (35 → 0), **4b** measures `Runtime`'s collapse and builds the assembler. You cannot measure field independence while stages reach around the fields, so 4a is a precondition for 4b rather than a sibling.

**4a — DONE 2026-08-25. 35 → 0, machine-checked.** The seven providers §3.5 groups as `Lane` are now a value (`sovereign-core/src/runtime/lane.rs`) that a stage receives; 26 live `self.<enrichment_field>` reads across `runtime/retrieval/*`, `evidence_loop`, `streaming.rs`, `turn.rs` and `handlers/` became zero. `PipelineState` carries the lane, so a pipeline step reads `st.lane`; every other stage takes `lane: &Lane` beside the `enabled_corpora` / `corpus_ceiling` it already took. The falsifier is `sovereign-core/tests/lane_reach_through_census.rs`, whose named failing input is a real reflex edit — write `self.rerank_fn.as_ref()` in any stage and it fails with file and line. It carries an instrument check (§18.4): the scan must first find the `self.inference` / `self.store` reads it is *not* looking for, or its zero would mean "read nothing" rather than "found nothing". That check earned itself immediately — the first scanner missed `self\n    .gliner`, the rustfmt-split form, and the compiler caught what the test did not; the scanner now joins each line to the next before matching. **One correctness fix rode along:** `meta_atlas` is read out of its lock ONCE per turn instead of per stage, so the desktop's background index warm can no longer land mid-pipeline and score two halves of one pool against two different indexes. **Deleted:** the duplicated `rerank_config.enabled && rerank_fn.is_some()` decider — `Rerank::active()` is the one place those two halves are read together (§10.6).

**4b — measured 2026-08-25; the totality half landed, the `Launch` assembler did not.** The Phase 2 pair-independence pass, re-run over all 19 `Runtime` builders across the three live `Runtime::new` sites (desktop `state.rs:1629`, server `main.rs:623`, chat `bootstrap.rs:397` — the daemon builds none, which is the phase). **It does not factor, and that is the result:** eight distinct column vectors, with three mutually incomparable two-host classes (`Y.Y` conv_tiered/mesh_knowledge, `YY.` landscape_digests/routing_events, `.YY` meta_atlas). A lattice cannot look like that. The raggedness is **omission, not topology**, and the code says so in its own comments — `with_rerank` carries *"Until 2026-08-03 the ONLY surface that installed one was the `svrn chat` CLI, so the hub server and the desktop shipped baseline fusion ordering while the ledger recorded the capability as available."* Both historical divergences were closed by copying, which is the signature of a missing constructor rather than a profile. And the ragged remainder is **exactly §3.5's five departures**, arrived at independently: every non-`YYY`, non-enrichment builder is one of `mesh_knowledge`, `compaction`, `routing_events`, `landscape_digests`, `corpus_principal`. The measurement did not know that claim; it reproduced it.

  So the corrected 4b move is **totality, not product-to-sum** — `Runtime` has no variants; the three-ness in this program belongs to `DaemonServices`. Landed: `LaneSources` is a **required argument** to `Runtime::new`, and the **eight `with_*` enrichment builders are deleted** (`with_gliner`, `with_rerank`, `with_rerank_config`, `with_meta_atlas`, `with_bridge`, `with_atlas_context_provider`, `with_wikipedia_graph`, `with_conv_tiered_reader`). A builder never could enforce installation: from inside the Runtime a forgotten call and a host with no such provider are the same state. All three hosts now gather their stack and commission once. `install_meta_atlas` survives as the one member that legitimately arrives after construction, backed by a cell rather than a second storage. **Also retired:** `seam_count_is_stable`, which asserted `ENRICHMENT_SEAM_COUNT == 8` against a const three lines above it — no input could make it fail (§18.1); its replacement takes the count from the reader, and the reader now takes `&LaneSources` so it can actually be *run* rather than only compiled.

  **The assembler landed the same day.** `sovereign_mesh::assemble(&Launch, LaunchParts) -> Result<DaemonServices, AssemblyRefusal>` is the one exhaustive match, and all four sites that used to name their own variant now hand it parts: `daemon_cmd/mod.rs`, desktop `state.rs`, `mesh_cmd.rs` ×2. It deliberately does NOT build the parts — a host still opens its own corpus engine and provider, because those need the host's own I/O. What moved is the DECISION, so the illegal pairs are refused in one place rather than being unrepresented anywhere: a desktop launch carrying headless rails, a verb launch carrying a serving profile, a launch mode that assembles nothing being handed daemon parts. Refusals name both sides and are fatal — a daemon that came up as the wrong shape is the hazard itself, so there is nothing to degrade to (§18.3).

  **Home: `sovereign-mesh`, not `sovereign-cli-daemon` as §10 settled on 2026-08-24.** The reasoning for cli-daemon was that the desktop already path-depends on it; that is true and was enough for a two-host assembler, but `svrn mesh create` / `join` are the third construction site and they live in `sovereign-cli-llm`, which does not depend on cli-daemon and should not start. `sovereign-mesh` already owns `DaemonServices`, already depends on `sovereign-contracts` (so it can name `Launch`), and is already below all three hosts. Putting the match beside the type it constructs also lets the two composite constructors go `pub(crate)`, which is the part the compiler can hold.

  **Two doors shut 2026-08-25 in Phase 4b, the third the same week.** `DaemonServices::desktop` and `::headless` became `pub(crate)`, so no crate outside `sovereign-mesh` could compose a *serving* daemon. `MeshAdmin` was a bare variant and stayed nameable — a bare variant is constructible wherever the enum is — so it was covered by a source census meanwhile. **Phase 7a closed it:** the variant carries a private `MeshAdminWitness(())` whose only mint is `assemble`, demonstrated once against a throwaway external test (`cannot initialize a tuple struct which contains private fields`) and then deleted, per §10's own verification rule. `tests/launch_assembler_census.rs` is **retired** — the compiler now holds what it was scanning source for, and its module docs had named exactly that as its retirement condition. The 21 in-tree sites that used to write `DaemonServices::MeshAdmin` by hand now go through `assemble` via one shared fixture, so each also proves the assembler accepts a verb launch.

  **Falsifier 1 closed on the way past.** Threading the `Launch` into `daemon_cmd::run` killed the last surviving reader — `daemon_cmd/mod.rs:172`'s `args.iter().any(|a| a == "--worker-mode")` — which §10 had deferred to its own commit because `Launch::Worker` carries argv *including* the `run` subcommand while `run_worker_daemon` wants it stripped. Passing the `Launch` value rather than re-deriving from `args` sidesteps that entirely: `matches!(launch, Launch::Worker { .. })` reads the decision, and `args` keeps coming from `daemon_cmd::run`'s own routing. **Readers 3 → 1 → 0; writers 6 → 0.**

### Phase 9, designed (2026-08-26)

**The finding that sizes this phase: every type it needs is already minted, with the right doors.** `Evidence` has private fields, a `pub(crate)` constructor and deliberately no `Deserialize` (`corpus-engine/src/index/evidence.rs`). `Draft::release` cannot produce an `Answer` without `&[Judgement]`, and `kernel-types/tests/ui/answer_without_a_judgement.rs` is a compile-fail test proving it. `Citation` points into a sealed `EvidenceSet`. What is missing is **adoption** — which is exactly §6's point that `home = minted` is worth nothing while nine other doors are open. So Phase 9 is a door-closing program, and its unit of done is §6's `adopted` column reaching zero, not a new design.

Three rungs, ordered **9.2 -> 9.1 -> 9.3** and re-measured 2026-08-26 before starting. An earlier revision of this paragraph ordered them 9.3 -> 9.1 -> 9.2 on two claims that do not survive measurement, and both are recorded because a stated reason that is wrong is worse than none (§11.1). **(1) 9.3 is not the small one.** It reads that way because `EnrichProgressFn` already exists on both sides of the seam — but the in-process entry point (`enrich_cmd/build.rs:128 build_with_progress`) is 1,373 lines pulling twelve sibling modules out of a 34,122-line `enrich_cmd`, and making it reachable from the desktop means moving that subtree below `sovereign-tools`. That is a crate migration, so it goes last. **(2) 9.1 does not gate 9.2.** The claim was that re-typing the gate over `ScoredChunk` inputs means doing it twice; 9.2 changes the gate's OUTPUT type, and the one exit that already releases builds its `Draft` from `kernel_types::Citation` values (`grounding/mod.rs:1506`), not from the pool. They are independent, and 9.2 is the cheapest rung that forces a real invariant.

**9.1 — `retrieve` is the only door (hazard 1). COMPLETE 2026-08-26: the field, the repoint, and both remaining doors.** Seven manufacturers became five, and the two that came off are the two that were never manufactured — estate documents and peer-served chunks. Canonical: `CorpusIndex::retrieve -> EvidenceSet` (`evidence.rs:249`), **zero production callers**. The doors open beside it, measured 2026-08-26: `CorpusIndex::search -> Vec<ScoredChunk>` at **47 production call sites across 20 files**, and **44 production `ScoredChunk` literals** (64 with tests).

The blocker nobody had named is that **`Evidence` is immutable and the pipeline's whole job is mutation**. `ScoredChunk` is a mutable accumulator — sovereign reassigns `score` at 12 sites, overwrites `content`/`title`/`url` at 6, and calls `metadata.insert` at 16 (measured 2026-08-20, recorded in `evidence.rs`'s module docs). Swapping the pool's element type for `Evidence` therefore does not compile and should not: re-ranking is a per-turn fact, not a property of what was acquired.

The move is to split what `ScoredChunk` conflates:

```
Evidence    what was ACQUIRED.  Immutable, minted only at a door.
Candidate   what THIS TURN computed about it.  Mutable, per-turn,
            holds an Evidence by value and never reaches inside it.
```

`PipelineState`'s pool becomes `Vec<Candidate>`; every step mutates the `Candidate`, and no step can alter what was acquired. That conversion is what makes the six content-overwrite sites legible: a step that rewrites a chunk's text is MANUFACTURING content, which is hazard 1's failing input stated exactly, and it must become an acquisition door or an annotation beside the evidence — never a write through it.

The 44 literal sites are producers, and each resolves to one of three things: **an acquisition door** in corpus-engine that mints `Evidence` (atlas atoms, RAPTOR summaries, conv-briefing rows — these ARE knowledge and do have provenance); **an annotation** on an existing `Candidate` (boosts, excerpts, re-scores); or **deleted**, when it is model-authored prose entering the pool, which is what the first rung already removed once (`readiness_disclosure` at `score: 1.0`).

Order inside the rung: mint `Candidate` and swap the pool element while `ScoredChunk` stays the acquisition input; route production `search` calls through `retrieve` and make `search` `pub(crate)`; then close the 44 producers largest-file first. **What landed 2026-08-26.** Not the pool-element swap — that is 190 `ScoredChunk` type positions in `sovereign-core` alone and it changes retrieval behaviour, so it is bench-gated (§18.4). What landed is the half that carries the hazard: **provenance stopped being two keys in a string map.**

`corpus_engine::index::ChunkProvenance` is a required field on `ScoredChunk` with no `Default` and no `Deserialize` — both would be doors, the same argument `Evidence` makes. Its `Acquired` arm has no public constructor, so `sovereign` can read provenance and can only WRITE `Manufactured { producer }`. Because there is no `Default`, adding the field broke every construction site in the workspace and the compiler enumerated them: **7 production manufacturers**, the rest fixtures.

The seven are now named, and naming them is the finding. `atlas_context_entity`, `atlas_atom`, `atom_enum_claim`, `raptor_summary` are derived or model-authored surfaces that were entering the pool indistinguishable from indexed rows. `mesh_peer` is content a PEER's index vouched for, arriving over a wire that carries no custody or grain. `estate_document` comes from the estate store, which has no acquisition door at all. One site was converted outright rather than named: the atlas-grounding path was calling `get_chunks` and assembling a `ScoredChunk` by hand — real index content entering the pool with no provenance — and now goes through a new `CorpusIndex::acquire_chunks` door.

**The parse moved to one place and the field is load-bearing today.** `custody_of` / `grain_of` live beside the type and are called at the acquisition doors only; `evidence_from_hit` reads the typed stamp instead of re-parsing, and the three `LEGACY_*` constants in `evidence.rs` were **deleted** as dead. Behaviour is identical by construction — the stamp is computed from the same bag at the same moment — which is what makes this half shippable without a bench.

**The second half, measured rather than estimated (2026-08-26).** The earlier revision of this paragraph said the repoint was "sovereign's ~26 remaining `metadata["custody"]` / `["source"]` reads" and that it needed acquisition doors on **both** the estate store and the mesh reply path before it could move. Both claims were wrong in the direction of more work, and the correction is recorded because a stated reason that is wrong is worse than none (§11.1).

**The surface is five sites, not twenty-six.** The 26 was every `metadata.get` on a chunk; most of those keys are not provenance (`code_intel`, `atom-enum`, `rerank_score`, `articulation`). The reads that carry the two facts `ChunkProvenance` types are: custody at `grounding/mod.rs:453` (the gate's evidence builder, load-bearing) and `question_analysis.rs:827` (the glassbox projection); grain at `grounding/mod.rs:477`, `formatters.rs:273` and `merge_select.rs:118`. All five now read the typed stamp.

**Estate was the only break, and mesh was never one.** Chunk by chunk: a local corpus hit computes its stamp from the same bag at the same moment, so it is identical by construction; a mesh hit, an atlas atom, a RAPTOR rollup and an `acquire_chunks` result were ALL already unstamped on both sides. Only the estate document diverged — `Custody::Personal` in the bag, `Manufactured` in the type — and it is closed by `ChunkProvenance::acquired_from_estate`, a door named for its store so that no argument could ask it for another class. The bag write it replaces is **deleted**: `CUSTODY_META_KEY` now has no production writer and no production reader.

**What the repoint had to preserve, and nearly did not.** The gate carries `chunk_custodies: Vec<Option<Custody>>` where `None` means unstamped, and `custody_engaged` is `any(is_some())` — so a pool with nothing stamped leaves the custody machinery OFF. `ChunkProvenance::custody()` answers `Unknown` for manufactured content, which is right for `Evidence` and wrong here: it would have engaged the gate on every pre-custody turn and refused it. Hence `stamped_custody() -> Option<Custody>` beside it, with `custody()` defined in terms of it so the two cannot disagree. This is the rung's near-miss and the reason it is worth a bench rather than a build.

**Grain moved onto the `Manufactured` arm**, because `metadata["source"] == "raptor"` matched an indexed rollup row AND an in-process one through a shared tag, and a typed replacement has to answer for both arms or it silently reclassifies one of them. `manufactured_summary` is the constructor; `manufactured` stays `Grain::Leaf`. **The inverse case is why `atom-enum` did NOT move**: `retrieval/atom_enum.rs:687` re-tags an already-ACQUIRED chunk, so `source = "atom-enum"` is a per-turn routing annotation and reading it off `producer()` would have dropped every fetched atom-enum chunk from the merge pin. That is the `Candidate` / `Evidence` split arriving as a concrete case rather than as a plan.

**`mesh_peer` was the last producer, and closing it took a wire change.** It was two facts, not one, and only one of them needed the wire — which is why the estate door was a one-liner and this was not.

**Custody was knowable here all along; what blocked it was blast radius.** Content from another node is `Custody::Peer`, a fact this node holds without trusting anything the peer sent. But mesh hits were unstamped, so a turn carrying one left the gate's custody machinery disengaged, and stamping them would ENGAGE it — after which an unstamped chunk anywhere in the leaf view refuses (custody.md §4). The door resolves it by JOINING rather than taking: `join_custody([peer_claim, Custody::Peer])`, max-restrictiveness, with a peer that recorded no class joining as `Unknown` and poisoning the join. Since **nothing in the repo writes a custody key into indexed chunk metadata** (measured 2026-08-26), every peer sends nothing today, the join yields `Unknown`, `stamped_custody()` yields `None`, and the gate stays exactly as disengaged as it was. Behaviour-preserving by construction — and correct the moment an index starts stamping.

**Grain genuinely needed the wire, and the wire was dropping it.** `commonwealth-api`'s two knowledge routes built `KnowledgeResult { metadata: Default::default(), .. }`, discarding the stamp the serving index had just applied — so the field existed and one side emptied it. Both routes now forward `provenance.stamped_custody()` and `provenance.grain()`, and `oicp-types` carries them as their canonical wire spellings rather than as `kernel_types` values: the protocol crate is pinned to ZERO internal deps by its `[[package_leaf]]` budget in `quality/ARCH_LAYERS.toml` so a third party can implement OICP without our kernel, and `Custody::parse_wire` / the new `Grain::parse_wire` are the one parser each. The mesh client parses once at its boundary into a typed `MeshScoredChunk`.

**Absence is the refusing value on both facts, which is what makes an un-upgraded peer safe.** A missing grain reads as `Grain::Summary`, not `Leaf` — `Leaf` is the permissive one, and a rollup wrongly marked `Leaf` becomes quotable, which is the direction that fabricates. A peer that serves out of `index.search` CAN serve an indexed rollup, so this is not hypothetical. The result: an old peer's hits stay exactly as unquotable and as unstamped as they were while they were `Manufactured`, and a peer that says `leaf` unlocks quoting. Both new fields are `#[serde(default, skip_serializing_if)]`, so old and new peers interoperate in both directions.

**Falsifier:** `sovereign-core/tests/chunk_provenance_census.rs`, watched to fail — the manufacturers (seven, now six) are a ratchet that may shrink and not grow, and no crate outside corpus-engine may stamp an acquisition. **It grew the other half of the ledger 2026-08-26:** a `DOORS` list, because `Acquisition::stamped` being `pub(crate)` stops sovereign inventing a custody but does nothing about a door added INSIDE corpus-engine, and a door taking a `custody` argument is that public constructor wearing a door's name. Both new checks were watched to fail on real inputs — an undeclared `acquired_from_nowhere`, and the old needle list, which cannot see `manufactured_summary(` (the char after `manufactured` is `_`, not `(`) and reported a live producer as GONE. It caught two of its own instrument defects on the way in (a scan that missed a rustfmt line break and reported a live producer as gone, and a check that failed on its own source text). **Also to extend:** `evidence_pool_census.rs` — with `search` crate-private, `Evidence`'s `pub(crate)` constructor already holds the invariant, so the census only has to assert no caller outside corpus-engine. **Deletes:** `CorpusIndex::search` from the public API; `ScoredChunk`'s mutability from sovereign.

**9.2 — an `Answer` cannot exist without a `Judgement` (hazard 2). LANDED 2026-08-26.** Canonical: `Draft::release(Attribution, &[Judgement]) -> Answer` (`kernel-types/src/answer.rs:310`), with `release_ungated` for the disarmed-gate case, which must name its reason. The door beside it is `GateOutcome { text: String }` — **16 construction sites in `grounding/mod.rs`**, exactly one of which releases (`:1645`), and that one flattens the `Answer` straight back to `String` at `:1665`. (The hazard table's "~15 gate exits" was close; a first count here said 21 by including the struct definition and the four `-> GateOutcome` signatures. `router_calibration::GateOutcome` shares the NAME and is a different type — router axis-gate counts — so it is not part of this rung, though one noun spelled two ways in one crate is its own §10.6 smell.)

The whole rung is one type change: `GateOutcome.text: String` becomes `GateOutcome.answer: Answer`. It is forcing rather than plumbing — each of the 16 exits must then name a `Judgement`, and the exits that cannot are precisely the ones releasing text nobody judged. Expect `CouldNotJudge` and `NeverRan` among them; both are honest and both already exist on `Verdict` (§18.1), so no new type is minted. **State made unrepresentable:** a released answer with no verdict attached.

**What landing it found.** Sizing it named 16 exits; the conversion found that choosing the door required knowing how far the gate got, and that fact was being **re-derived from the spelling of the wire action string** — `!action.starts_with("abstained") && !action.starts_with("judge_failed")` at the old `:2297`, plus a four-arm `match action { "released" => .., a if a.starts_with("abstained") => .. }` computing the claim-check narration counts. §2.1's smell, and the deeper defect is §10.6: the verdict was decided upstream (where `action` is assigned, at twelve sites) and reconstructed downstream from a string. `action` is now a `GateAction { id, reach }` — the wire id unchanged byte-for-byte, plus a `GateReach` of `Held | Flawed | Declined | Unjudged`, so the two cannot disagree and a new action id cannot fall into a `_ => (0, 0)` arm unnoticed.

Two exits turn out to have been releasing text under a passed answer's shape that the gate never judged: `judge_failed_open` (claim extraction failed, the ladder fell open) and `retry_released_unverified` (the rewrite was released without re-audit). Both are `GateReach::Unjudged` now, which is §18.2's whole point arriving as a type rather than as a comment.

**One decider, including the site that was already right.** The exit at `:1645` was the one correct release before this rung, and leaving it alone would have made "one decider" mean "one decider plus the original" — so it goes through `release_held` like every other exit. The census below caught exactly that: its first run failed on the `Draft::composed` still standing at `:1822`.

**Falsifier:** `sovereign-core/tests/gate_release_census.rs`, watched to fail — the four named doors are the only places a gate answer is minted, `GateOutcome` carries an `Answer` and not a `String`, and every `GateReach` has an arm in the dispatch. `kernel-types/tests/ui/answer_without_a_judgement.rs` already held the type half.

**Deletes:** `GateOutcome::text`; the prefix-matching derivation of the gate verdict from the action string.

**The LONGFORM ladder was still outside this, and the compiler was the only thing saying so (closed 2026-08-26).** `GateReach::Flawed` was never constructed — a variant naming a state the gate could not be in — because the longform path kept a bare-string `action` and called `release_flawed` directly at three production exits (`grounding/mod.rs`). Hazard 2 was closed there (those exits did name a `Judgement`); §10.6 was not, because the wire id and the verdict were still chosen at two places. Its six ids are now `GateAction` constants, byte-identical, and every exit goes through the one dispatch — `release_as_because`, which is `release_as` with the reason stated rather than derived from the id, so the longform exits keep sentences like "3 claim(s) still flagged after rewrite" as the `Judgement`'s `Reason`. The census gained `every_gate_reach_is_produced_by_some_action`: the old check asserted each reach had an ARM in the dispatch, which the compiler already guarantees, and could not see that no `GateAction` produced one — a check with no failing input for one of its four states (§18.1). Drop the six new constants and the table goes 17 sites to 11, carrying `Held`, `Declined`, `Unjudged` and no `Flawed`.

**What it did NOT do, named rather than claimed:** the four handlers and `streaming.rs` still call `outcome.answer.text()` and carry a `String` onward, so the `Answer` stops at the gate's edge. The judgement now EXISTS on every exit, which is hazard 2; carrying it to the surface — so a client can see `could_not_judge` rather than infer it from `meta["action"]` — is the next step and is not done here.

**9.3 — `enrich` is a call, not a subprocess (hazard 7). HALF CLOSED 2026-08-26 — the reparsing, not the `exec`.** The only rung with something to mint. Today `sovereign-tools/src/enrich.rs:204` spawns `sovereign-cli enrich build <corpus>`, pipes stdout, and rebuilds progress by parsing banner lines through a `ParserState`; the subsystem itself is private to `sovereign-cli-llm`. Lift the build path into a library entry point returning typed progress events and let the CLI become one caller of it — `EnrichProgressFn` already fixes the callback shape, so half the seam exists. **Do this last** — see the ordering correction above; the seam looks half-built and the subsystem behind it is not. Its failure mode is nonetheless the silent one §7.2 names — a banner reworded for a human changes what the desktop believes about a running enrichment, with no compiler and no test in between. **Falsifier:** zero `Command::new` spawns of `sovereign-cli` / `svrn` outside the supervisor and compute-child paths.

**What landed 2026-08-26, and what did not.** Not the migration. The subtree was re-measured first (§18.4 — the earlier "1,373 lines pulling twelve sibling modules out of a 34,122-line `enrich_cmd`" quoted the size of the WHOLE `enrich_cmd`, not of what `build` transitively needs): the actual `super::` closure of `build_with_progress` is **14 modules, 9,109 lines**, and it drags `inference_client`, `providers`, `source_loader` and `corpus_io` below `sovereign-tools` — a layer decision, so still a separate initiative and still last.

What DID land is the half with no gate in front of it: the wire. `corpus_engine::enrichment::pipeline::progress::wire` is one declaration of a line protocol (`@progress {json}`) that the CLI encodes into and `sovereign-tools` decodes from. That module's own header had claimed since it was written that "a future headless mode can filter on `serde_json::Value` without string-matching" — the events were `Serialize` and `#[serde(tag = "kind")]` from the start and only the rendering was missing. **Deleted:** nine `parse_*` functions, the `is_noise` banner-decoration allowlist, and `classify_reason`, which keyword-matched a free-text reason back into a `failure_kind` the child had already computed as an enum and thrown away — so `<think> truncated: parse error at EOF` was classified by whichever substring the `if` chain tested first. **`ProgressWire::Silent`** names the version-skew case rather than defaulting it: `resolve_sovereign_cli` walks four ladders and can land on a binary from `$PATH` that predates `SOVEREIGN_ENRICH_PROGRESS`, and a build with no progress and a build whose progress we could not HEAR look identical to a UI (§18.3). That is also why the request is an env var and not a flag — an unknown flag makes an older binary exit 2 on a usage error, turning a working build into a failure. **Falsifiers, both watched to fail:** renaming the wire prefix on one side reds three tests (under the banner parser, rewording a banner reddened nothing); collapsing `Silent` into `Spoken { events: 0 }` reds `e2e_a_cli_that_speaks_only_banners_is_reported_as_silent`.

**The register gets its column here.** §6 requires an `adopted` count of non-canonical constructors, and 0 of 31 `CONCEPTS.toml` rows carry one today. Phase 9 is the phase that needs it — each rung's done-when IS `adopted = 0`. Add it with the three rows Phase 9 owns rather than backfilling all 31, so the column arrives with a live user (§19).

### Calls taken on the four gaps (2026-08-24)

These were open questions. They are decided, each against the section it answers to.

**The environment axis lands at Phase 10, gated on 4b — not deferred to "later".** The split is §6.2's: a flag whose value selects *which code path runs* is structural and folds into a profile variant (§2.1, a closed set belongs in an enum); a flag that *tunes a value the same path uses* is data and becomes a field read **once at construction**, never at point of use. "Profiles first" was right about order and was misread as "much later" — profiles exist the moment 4b lands, which is exactly when classification becomes possible. It cannot slip past that, because the acceptance criterion dies on it: `std::env::var` at point of use is invisible to go-to-definition, so 249 hidden branch points defeat a clean spine no matter how good the spine is. Falsifier already written in §4's axes table and executable today via `cargo xtask env-gate`: **no `env::var` read selects behaviour after construction.**

**The verb axis lands at Phase 9, and it must precede Phase 5.** Hazards 1, 2 and 7 are one shape — one decider, many doors (§10.6: duplicating a *decider* is worse than duplicating code). The measure is §6's `adopted` column: constructors other than the canonical one reach zero. Sequencing it before Phase 5 is the load-bearing part: Phase 5 makes the turn the daemon's public contract, and shipping that while nine sites can inject into a prompt without passing `retrieve` would standardise the bypass rather than close it. §18.1 is satisfied by a failing input we can already name — `retrieval_pipeline.rs:951`, model-authored prose entering at `score: 1.0`.

**`AppState` and `AppStateInner` attach to Phase 6, using the Phase 2 method, not a new one.** §19 — the algorithm already exists and already terminated on real code; inventing a second approach for the same problem is the smell. `AppState` should collapse as a *consequence* of hosts becoming surfaces: a surface holds a connection, not a composition root. `commonwealth-api::AppStateInner` is the honest exception — 60 fields, the largest root in §2's census, in a peer workspace — and it is flagged here as likely needing its own order rather than riding Phase 6. **Until both land, §2's combined figure stays dishonest**, because it is the product of four roots and we will have collapsed two.

**`Runtime`'s collapse gets measured before 4b designs it.** What made Phase 2 trustworthy is that the pair-independence pass *produced* three variants nobody chose in advance. "Core only = five things" is a hypothesis, not a result, until the same algorithm says so over 32 fields and 15 `Option`s. §11.1 — do not cite from memory, cite from the run.

Two further calls the same review forced:

**Hazard 12 is reclassified from runtime-refusable to a Phase 5 deliverable.** `CancellationRegistry` is process-local only because no daemon-side session owner exists; Phase 5 creates one. Once the daemon owns the session, cancel is structural (§7.1), not a thing we promise to report loudly.

**The Phase 6 re-baseline pre-registration becomes structural.** It is currently a paragraph asking a future reader to remember something — §7.6, structure over instruction, and the exact failure the pre-registration exists to prevent. `sovereign-ci-bench.sh` must **refuse to run** post-Phase-6 without a pre-registration file naming the expected direction of movement.

### The acceptance criterion (operator, 2026-08-24)

This governs every phase and outranks any per-phase done-when. It is a **navigability** criterion, not a cardinality one:

> a glanceable path to click through a few files in an IDE and follow the implementation of the daemon runtime in each of its invocations to its actual implementation, with a remarkably finite amount of deviations possible, but still squeaky clean SOLID architecture.

**The number is 8 → 3 → 3.** Eight ways to start (`Launch`: `Daemon`, `Worker`, `ComputeChild`, `Smoketest`, `Desktop`, `Server`, `Verb`, `Bare`); three of them assemble a daemon runtime; three assembled shapes (`DaemonServices`: `MeshAdmin`, `Desktop`, `Headless`).

*It was 7 until 2026-08-24.* Wiring `launch.rs` — compiling it for the first time — made the omission legible: `sovereign-server` binds a long-lived listener and owns tenant state, but no variant named it, so `Launch::is_resident()` returned false for it. That predicate is what the run lock, the panic hook and the OOM watchdog key on, which is exactly why an orphaned server sat on `0.0.0.0:8080` for six days with no lock, no crash reporting and no refusal (hazards 4 and 10). **Resident and assembler are different questions**: `Server` is resident but is a *surface* in the target, so it widens the first number and not the second. Two paths reach it — the desktop supervises one, and `svrn mobile` `exec`s it, replacing its own process image.

The lesson generalises past this one variant: the closed set was wrong in a way nobody could see while it did not compile. Anything else claimed but unwired should be treated as unverified.

**The spine — three files, clicked in order:**

1. `sovereign-contracts/src/launch.rs` — `Launch::parse`. **Wired and green 2026-08-24** (`pub mod launch;` in `lib.rs`; 9 tests pass). Total, and its tests pin the equivalence whose absence produced §1's wrong conclusion: `--daemon-child` *is* `daemon run`.
2. **The assembler — one total function `Launch -> DaemonServices`**, exhaustive match, one arm per invocation. **LANDED 2026-08-25 as `sovereign_mesh::assemble`** (`sovereign-mesh/src/daemon_services.rs`). The home moved from the 2026-08-24 call of `sovereign-cli-daemon`: that was reasoned from the desktop's existing path-dependency, which holds, but `svrn mesh create` / `join` are the third construction site and live in `sovereign-cli-llm`, which does not depend on cli-daemon. `sovereign-mesh` owns `DaemonServices`, can name `Launch` (it already depends on `sovereign-contracts`), and sits below all three hosts — and putting the match beside its type is what lets `desktop`/`headless` become `pub(crate)`.
   **Adopted at the first entry point 2026-08-24** — `sovereign-cli-daemon::run_with_args` + `dispatch` now decide once via `Launch::parse` and match exhaustively, replacing three separate re-derivations (`first() == Some("--compute-child")`, `first() == Some("daemon")` for the panic hook, and `match cmd`). 178/178 package tests green. That adoption fixed a live divergence: this entry point matched `--compute-child` only at `args[0]` while the desktop matched it at any position, so a child re-exec carrying leading args fell through to verb dispatch and printed "unknown subcommand". Still to adopt: the desktop.
3. `sovereign-mesh/src/daemon_services.rs` — what got built. **Accessor bar met 2026-08-24 (10 → 3).**

**Why finiteness and SOLID do not fight.** They fight only when dependency inversion is done by *runtime registration* — setters, builders, `Option` slots filled elsewhere — because then go-to-definition lands on a trait and "which impl?" lives in another file. The resolution is `dyn` at the **seam**, concrete at the **assembly**, and exactly one assembly file. `RetrievalPipeline` should take `Arc<dyn InferenceProvider>`; the place that *creates* it must be one exhaustive match naming the concrete type. The sum type is itself the open-closed mechanism — add a variant, the compiler walks every match. OCP without a registry.

**The accessor bar: `DaemonServices` Option-returning accessors 10 → 3 (2026-08-24), then → 2 (2026-08-25).** The seven artifactual ones went first. `state_store()` was the third real fork and Phase 3 is what made it artifactual rather than "fixing" it early: while Desktop had a store and Headless did not, that `Option` was the type telling the truth, and deleting it would have hidden the crossing instead of closing it. Once both serving variants carry one it degenerated into `self.serving().map(..)` over a non-optional field — the exact shape of the seven — and went the same way. Two remain: `serving()` (`MeshAdmin` has no serving role; the discriminator every other read goes through) and `rails()` (`Headless` only).

Each deleted accessor was a one-line `self.serving().map(..)` or `self.rails().map(..)` over a field that is **not** optional one level down, so the `Option` it returned carried no information. Two of them stacked, which is the worse defect: `mcp()` returned `Option<&McpSurface>` where `McpSurface` is itself a two-state absence-carrying-a-reason (§18.3), so `services.mcp().and_then(|m| m.mount())` put a meaningless outer `Option` on top of a meaningful inner one. Both stacked sites (`daemon.rs:2270`, `:2462`) now read one real absence.

Eight call sites in `daemon.rs` were repointed; the compiler found every one. **No crate outside `sovereign-mesh` used them** — full workspace lint exit 0, `sovereign-mesh` 704/704.

**Two tests shrank, and the shrinkage is the proof.** The variant census asserted `corpus_engine`, `inference_provider` and `serving` separately against the same `core` column, and `provider_factory`, `mesh_store` and `convergence_recorder` separately against the same `rails` column — seven checks that could not disagree, because each read one variant through a wrapper. They are now two. And `a_serving_variant_never_has_half_a_core` was **retired**: with both accessors gone, half a core is not writable, so the test had no nameable failing input and by §18.1 had stopped being a gate. The property it guarded is carried by the type instead — which is the whole thesis of this document, arriving on one of its own tests.

The defect was never the count, and the count is a poor proxy: `services.` reads in `daemon.rs` moved only 20 → 19, because `serving()` and `label()` are reads too. What changed is that a call site now matches **once** on a real fork and then reads plain struct fields — so a click lands on a field, not a `.map()`.

**Three falsifiers.** The first was stated loosely on 2026-08-24 ("zero argv string literals outside `launch.rs`") and sharpened the same day when measuring it swept up every `--json` and `--help` parse in the tree — a check that cannot fail cleanly is worse than none (§18.1). It counts **launch-mode tokens only** (`--daemon-child`, `--compute-child`, `--smoketest`, `--worker-mode`), and it has two halves that are not the same defect:

- **Readers → 0. MET 2026-08-25.** Code deciding what *this* process is. Only `Launch::parse` may answer that. 3 → 1 on 2026-08-24 (both desktop sites gone; `main.rs` runs one exhaustive `Launch` match in place of three independent argv scans), then 1 → 0 on 2026-08-25. The survivor was `daemon_cmd/mod.rs:172` (`--worker-mode`), a §10.6 duplicate created by that very refactor — `dispatch` collapsed `Launch::Daemon` and `Launch::Worker` into one `daemon_cmd::run` call, so `Launch` answered and `daemon_cmd` asked again.
  It was deferred, not overlooked, on a real objection: `Launch::Worker` carries the args *including* the `run` subcommand, while `run_worker_daemon` receives them with `run` already stripped by `daemon_cmd::run`'s own routing, so routing `Worker` straight through meant either a signature change across two functions or silently altering what the worker parses. **The objection dissolved once the fix was the `Launch` value rather than its args**: `run` and `run_daemon` take `&Launch`, the branch is `matches!(launch, Launch::Worker { .. })`, and `args` keeps coming from the existing routing untouched. The same threading is what lets `daemon_cmd` call the assembler, so it stopped being a second dimension and became the same one.
- **Writers → named constants. 6 → 0 on 2026-08-24.** `launch.rs` now exports `DAEMON_CHILD_FLAG`, `COMPUTE_CHILD_FLAG` and `WORKER_MODE_FLAG`, `parse` reads them, and all six spawn sites name them (`supervisor_setup.rs` ×2, `compute/manager.rs` ×3, `mesh/tests/local_pod_smoke.rs`).
  The smoketest token is deliberately **not** duplicated into `launch.rs`: `sovereign-inference/src/smoketest.rs:53` already owns it next to the implementation, and `sovereign-contracts` sits below `sovereign-inference` so it cannot name that constant. Declaring a second copy would be the very §10.6 smell this work removes. The two are pinned equal instead, by a test in a crate that can see both — `sovereign-cli-daemon::launch_smoketest_flag_matches_owner`, which feeds the owner's constant into `Launch::parse`.

The other two, both **MET 2026-08-25**: Option-returning accessors on `DaemonServices` at exactly the real ones (now two, not three — Phase 3 retired `state_store()`); and exactly one exhaustive match over `Launch` that constructs anything (`sovereign_mesh::assemble` — and since Phase 7a the compiler holds this outright, so the source census that used to hold the half it could not is deleted rather than kept as a second opinion).

Baseline measured 2026-08-24: two `main.rs` files, argv string-matched at six-plus positions across two crates in disagreeing order (`desktop main.rs:92/104/114`, `cli-daemon lib.rs:151`), an 856-line desktop `main.rs` whose assembly lives inside a Tauri setup closure, and `bootstrap::detect()` deciding wiring from a snapshot taken before `config.toml` is written.

### Verification, in order of what each proves

1. **Phase 0/1** — `sovereign-lint.sh --human --full` and `sovereign-test.sh --human`, both exit 0. Gate on the exit code, never the summary line.
2. **Phase 2** — the constructor is total: every variant reachable from a live path, every live path names a variant (`tests/daemon_variant_census.rs`, watched to fail on real inputs before being kept).
3. **Phase 4** — one resolver per port, proven by construction: a grep for a second `open_wikipedia_graph`-shaped probe returns one site.
4. **Phase 5/6** — drive a real turn through the daemon from the CLI, then the desktop, and assert streamed token text and the terminal metadata frame match. `scripts/desktop-soak.py` is the instrument.
5. **Phase 6, the payoff** — `./scripts/sovereign-ci-bench.sh --quick` against the pre-convergence baseline. Expect movement and treat it as a **re-baseline, not a regression**: the harness was measuring a different port set, so old and new numbers are not comparable (§18.4). **Pre-register before the run**, or the first bench after Phase 6 reads as a quality regression and gets "fixed".
6. **Phase 7** — a throwaway crate calling `Runtime::new` must **fail to compile**. Demonstrate once, then delete it; the point is the demonstration, not the artifact.

**What the gate does not cover.** `sovereign-ci-bench.sh` scores outcomes and asserts nothing about wiring, which is what makes it safe to gut internals — implementation-opinionated unit tests breaking is the work queue, not a warning. But **conversation retrieval is not gated at all** (`bench/README.md:19`): `RETRIEVAL_CORPORA=(sep wikipedia)`, `conversation-private/` is gitignored so it can never hold a shareable baseline, and `conversation/` is a scaffold. Phase 0 landed inside that hole. Anything it changed there is accepted, not measured.

### Decisions taken

1. **`ShellTool` is scoped out of the daemon profile.** Shell execution does not move into a long-lived daemon running as a different user with a different cwd. Phase 4 splits the tool registry; Phase 6 confirms no host silently regains shell via the daemon.
2. **Desktop embedded mode is kept, as a transport variant.** Embedded versus supervised is only *where* the daemon runs — same construction, same routes. The second topology dissolves as a consequence of Phase 2 rather than as a deletion.
3. **`Runtime` does not become public under a `harness` feature.** An earlier draft allowed that; it was a hole, not a safeguard. The bench is precisely the thing that would enable such a feature, and a bench assembling its own divergent `Runtime` is the defect this program exists to kill. A door someone can open is not an invariant (§7.1).
4. **Typed turn shapes live in `sovereign-contracts`, not `kernel-types`** (note `d91de4b1`). The kernel's declared rule is that it names nothing from a product domain; `Scope`/`Capabilities`/`Lane` are sovereign domain concepts. `sovereign-contracts` is the DTO crate, already depends on `kernel-types`, and sits below all three hosts.

### Ownership and critical path

Follows §9's three-owner model, whose axes map onto the phases cleanly.

| Owner | Axis | Phases | Proof they are done |
|---|---|---|---|
| **A** | Process role + root construction | 2, 4, 7, 8 | A crate calling `Runtime::new` fails to compile; every variant reachable, every path names one |
| **B** | Storage | 1, 3 | One resolved data root honoured by every process, the run lock keyed on it, the daemon owning `sovereign.db` with no second opener |
| **C** | Surfaces + protocol | 0, 5, 6 | No host names `sovereign_core::Runtime`; bench and desktop drive the same turn endpoint |

**Critical path: 1 → 3 → 4a → 4b → 9 → 5 → 6 → 7.** Phase 2 gates 3 and 4. Phase 0 is parallel work that shrinks Phase 6. Phase 10 (environment) opens once 4b lands and must close before 7 — an `env::var` that still selects behaviour after construction means the second assembly is reachable by configuration even if it is uncompilable by type.

The ownership table above predates Phases 9 and 10 in one respect worth naming: §9 declares a third owner for **verbs + environment**, and the A/B/C mapping had quietly dropped that axis — owner C is *surfaces + protocol*. Phases 9 and 10 are that owner's, and their absence is why four of the thirteen hazards had no phase driving them.

Phases 0 and 1 are the only ones that are behaviour-neutral and reversible. Everything from Phase 3 onward changes where user data lives or where a turn executes, so each needs its own landing verdict rather than being bundled.

**Do not start with the environment axis.** 249 reads is the largest number here and the most tempting, but classifying a flag as structural or tuning requires knowing which profile owns it. Profiles first, or the classification has nothing to key on.
