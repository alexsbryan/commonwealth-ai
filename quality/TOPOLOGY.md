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
| 1 | A prompt fed by content that never passed `retrieve` | `CorpusIndex::retrieve` has **zero production callers**; 9 sites build `ScoredChunk` straight into the prompt, one injecting model-authored prose at `score: 1.0` (`retrieval_pipeline.rs:951`) | Verb axis | type |
| 2 | An `Answer` released without a `Judgement` | `Draft::release` reached at `grounding/mod.rs:1645` only — 1 of ~15 gate exits; flattened back to `String` at `:1665` | Verb axis | type |
| 3 | Two daemon implementations binding `:9741` | **Closed 2026-08-24 — deleted, not gated.** `commonwealth-daemon` (vestigial: `current_thread`, `NodeId::generate()` per boot, empty `Mesh`, no gossip; and per the 2026-08-05 review, no `join` subcommand and no `model pull`, so an inference plan could never arrive) is gone, with its three contrib packaging artifacts and the five shipped strings that pointed users at it. `EmbeddedDaemon` is the only implementation that binds the port. Consequences: `quality/TOPOLOGY.toml`'s `one-implementation-per-process` invariant flips to `holds = true`, `routing-field-guide.md §1` collapses from two daemon shapes to one, and `state.local_inference` loses its sole `None` producer (`main.rs:828`) so it can stop being an `Option` | Role axis | type |
| 4 | A binary whose role cannot be named | `sovereign-desktop` has 4 modes; `--daemon-child` is the daemon. **Also `sovereign-server`** — spawned as a supervised child by `mobile_host_setup::start` and `exec`d by `mobile_cmd.rs:152`, and until 2026-08-24 named by no variant at all, which is why the number is 8 → 3 → 3 and not 7. `Launch::Server` now names it and `is_resident()` covers it. Naming it is the precondition, not the fix: the assembler built for §10's acceptance criterion still has to own its lifecycle (hazard 10) | Role axis | type |
| 5 | `:9741` meaning different things by host | **Re-measured 2026-08-24 at `fc709d94`: desktop 5 of 7 routers, CLI daemon 7 of 7 — a delta of exactly two (`knowledge_view_http`, `solve_http`), not the "4 vs 6" first recorded.** Both original counts missed `corpus_watch_http`, which each host installs indirectly through `WatchedSubsystem::install`. The three-setters half was correct: `set_provider_factory` / `set_mesh_store` / `set_convergence_recorder` were CLI-daemon-only. **Closed** — the delta is now a field of `DaemonServices::Headless`, and mesh/admin/reading are built by the daemon from its own `Weak<Self>`, so no host can differ on them | Root construction | type |
| 6 | A shell that is conditionally a daemon | `SOVEREIGN_USE_SUPERVISOR` picks in-process vs supervised at runtime. Declared correctly in `quality/env-flags.toml` 2026-08-24 (it had been filed under `mesh` as "route distributed-inference workers through the supervisor", which it has never done) and the in-process branch now claims the data root's run lock — but a flag still selects which process is the daemon, so the hazard stands until Phase 10 | Environment | type |
| 7 | A subsystem reachable only by `exec` | `enrich_cmd` private in a leaf binary; `sovereign-tools/src/enrich.rs:204` shells out and parses stdout banners | Verb axis | type |
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
| 5 | The daemon serves the turn | not started |
| 6 | Hosts become surfaces | not started — the payoff: the bench measures the shipped assembly by construction |
| 7 | Make the second assembly uncompilable | not started |
| 8 | Delete `commonwealth-daemon` | **DONE 2026-08-24**, out of order on operator direction. See hazard 3 |
| 4a | Enrichment reach-throughs 35 → 0 | **DONE 2026-08-25** — `Lane` is a value stages receive; falsifier `tests/lane_reach_through_census.rs`. **State made unrepresentable:** a stage that enriches from a provider its caller did not resolve for the turn |
| 4b | Measure, then assemble | **DONE 2026-08-25** — measured (no variants; totality, not sum), `Runtime::new` total over the enrichment stack with eight builders deleted, and `sovereign_mesh::assemble` is the one exhaustive `Launch` match all four sites go through. Falsifiers 1 and 3 both met. **States made unrepresentable:** a host that means to wire a provider and silently does not; a crate outside `sovereign-mesh` composing a serving daemon at all |
| 9 | **Verbs — `retrieve` is the only door** | not started. Added 2026-08-24; hazards 1, 2, 7 had no phase |
| 10 | **Environment — 249 reads become profile variants or construction-time fields** | **STARTED 2026-08-25**, opened by 4b landing. First pair done: `SOVEREIGN_USE_SUPERVISOR` + `SOVEREIGN_FORCE_LOCAL` are now `sovereign_contracts::launch::DaemonHost`, resolved once by the desktop's `main` beside `Launch::parse`. Three points of use became one construction-time read, and a §10.6 duplicate closed on the way (`bootstrap::detect` and `supervisor_setup::is_enabled` each parsed `SOVEREIGN_FORCE_LOCAL` independently). The in-process shape now carries WHY — `ForceLocal` vs `KillSwitch` — where the predicate could only report a bare `false`. **State made unrepresentable:** two call sites disagreeing about what the launch-topology flags mean |

**Phase numbers are stable IDs, not an order.** Notes and worker orders already cite them. The order is the critical path below.

Phase 4 has two halves and they are ordered: **4a** kills the enrichment reach-throughs (35 → 0), **4b** measures `Runtime`'s collapse and builds the assembler. You cannot measure field independence while stages reach around the fields, so 4a is a precondition for 4b rather than a sibling.

**4a — DONE 2026-08-25. 35 → 0, machine-checked.** The seven providers §3.5 groups as `Lane` are now a value (`sovereign-core/src/runtime/lane.rs`) that a stage receives; 26 live `self.<enrichment_field>` reads across `runtime/retrieval/*`, `evidence_loop`, `streaming.rs`, `turn.rs` and `handlers/` became zero. `PipelineState` carries the lane, so a pipeline step reads `st.lane`; every other stage takes `lane: &Lane` beside the `enabled_corpora` / `corpus_ceiling` it already took. The falsifier is `sovereign-core/tests/lane_reach_through_census.rs`, whose named failing input is a real reflex edit — write `self.rerank_fn.as_ref()` in any stage and it fails with file and line. It carries an instrument check (§18.4): the scan must first find the `self.inference` / `self.store` reads it is *not* looking for, or its zero would mean "read nothing" rather than "found nothing". That check earned itself immediately — the first scanner missed `self\n    .gliner`, the rustfmt-split form, and the compiler caught what the test did not; the scanner now joins each line to the next before matching. **One correctness fix rode along:** `meta_atlas` is read out of its lock ONCE per turn instead of per stage, so the desktop's background index warm can no longer land mid-pipeline and score two halves of one pool against two different indexes. **Deleted:** the duplicated `rerank_config.enabled && rerank_fn.is_some()` decider — `Rerank::active()` is the one place those two halves are read together (§10.6).

**4b — measured 2026-08-25; the totality half landed, the `Launch` assembler did not.** The Phase 2 pair-independence pass, re-run over all 19 `Runtime` builders across the three live `Runtime::new` sites (desktop `state.rs:1629`, server `main.rs:623`, chat `bootstrap.rs:397` — the daemon builds none, which is the phase). **It does not factor, and that is the result:** eight distinct column vectors, with three mutually incomparable two-host classes (`Y.Y` conv_tiered/mesh_knowledge, `YY.` landscape_digests/routing_events, `.YY` meta_atlas). A lattice cannot look like that. The raggedness is **omission, not topology**, and the code says so in its own comments — `with_rerank` carries *"Until 2026-08-03 the ONLY surface that installed one was the `svrn chat` CLI, so the hub server and the desktop shipped baseline fusion ordering while the ledger recorded the capability as available."* Both historical divergences were closed by copying, which is the signature of a missing constructor rather than a profile. And the ragged remainder is **exactly §3.5's five departures**, arrived at independently: every non-`YYY`, non-enrichment builder is one of `mesh_knowledge`, `compaction`, `routing_events`, `landscape_digests`, `corpus_principal`. The measurement did not know that claim; it reproduced it.

  So the corrected 4b move is **totality, not product-to-sum** — `Runtime` has no variants; the three-ness in this program belongs to `DaemonServices`. Landed: `LaneSources` is a **required argument** to `Runtime::new`, and the **eight `with_*` enrichment builders are deleted** (`with_gliner`, `with_rerank`, `with_rerank_config`, `with_meta_atlas`, `with_bridge`, `with_atlas_context_provider`, `with_wikipedia_graph`, `with_conv_tiered_reader`). A builder never could enforce installation: from inside the Runtime a forgotten call and a host with no such provider are the same state. All three hosts now gather their stack and commission once. `install_meta_atlas` survives as the one member that legitimately arrives after construction, backed by a cell rather than a second storage. **Also retired:** `seam_count_is_stable`, which asserted `ENRICHMENT_SEAM_COUNT == 8` against a const three lines above it — no input could make it fail (§18.1); its replacement takes the count from the reader, and the reader now takes `&LaneSources` so it can actually be *run* rather than only compiled.

  **The assembler landed the same day.** `sovereign_mesh::assemble(&Launch, LaunchParts) -> Result<DaemonServices, AssemblyRefusal>` is the one exhaustive match, and all four sites that used to name their own variant now hand it parts: `daemon_cmd/mod.rs`, desktop `state.rs`, `mesh_cmd.rs` ×2. It deliberately does NOT build the parts — a host still opens its own corpus engine and provider, because those need the host's own I/O. What moved is the DECISION, so the illegal pairs are refused in one place rather than being unrepresented anywhere: a desktop launch carrying headless rails, a verb launch carrying a serving profile, a launch mode that assembles nothing being handed daemon parts. Refusals name both sides and are fatal — a daemon that came up as the wrong shape is the hazard itself, so there is nothing to degrade to (§18.3).

  **Home: `sovereign-mesh`, not `sovereign-cli-daemon` as §10 settled on 2026-08-24.** The reasoning for cli-daemon was that the desktop already path-depends on it; that is true and was enough for a two-host assembler, but `svrn mesh create` / `join` are the third construction site and they live in `sovereign-cli-llm`, which does not depend on cli-daemon and should not start. `sovereign-mesh` already owns `DaemonServices`, already depends on `sovereign-contracts` (so it can name `Launch`), and is already below all three hosts. Putting the match beside the type it constructs also lets the two composite constructors go `pub(crate)`, which is the part the compiler can hold.

  **Two doors shut, one left for Phase 7.** `DaemonServices::desktop` and `::headless` are now `pub(crate)`, so no crate outside `sovereign-mesh` can compose a serving daemon at all. `MeshAdmin` is a bare variant and stays nameable until it carries a private witness — that is Phase 7. `sovereign-mesh/tests/launch_assembler_census.rs` covers the gap meanwhile, with its exemption (this crate's own tree) stated rather than assumed and an instrument check that must first find the three host sites it expects.

  **Falsifier 1 closed on the way past.** Threading the `Launch` into `daemon_cmd::run` killed the last surviving reader — `daemon_cmd/mod.rs:172`'s `args.iter().any(|a| a == "--worker-mode")` — which §10 had deferred to its own commit because `Launch::Worker` carries argv *including* the `run` subcommand while `run_worker_daemon` wants it stripped. Passing the `Launch` value rather than re-deriving from `args` sidesteps that entirely: `matches!(launch, Launch::Worker { .. })` reads the decision, and `args` keeps coming from `daemon_cmd::run`'s own routing. **Readers 3 → 1 → 0; writers 6 → 0.**

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

The other two, both **MET 2026-08-25**: Option-returning accessors on `DaemonServices` at exactly the real ones (now two, not three — Phase 3 retired `state_store()`); and exactly one exhaustive match over `Launch` that constructs anything (`sovereign_mesh::assemble`, with `launch_assembler_census.rs` holding the half the compiler cannot).

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
