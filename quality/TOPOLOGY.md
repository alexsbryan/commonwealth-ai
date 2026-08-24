# Topology — the top level, and what it forbids

**Status:** pre-registration. Every row below carries the check that would fail
if the state it forbids is still reachable. A row with no falsifier is not a
row (ARCH §18.1).

**This document is not a description of the system.** Descriptions of this
system already exist and most of their structural claims are false on the live
path — that is the finding that produced this file. What follows is a
*specification*: three axes, and thirteen states the architecture must make
unreachable.

---

## 1. Why this exists — the derivation tax, measured

On 2026-08-24 one seat made a single architectural judgement about the top
level of this repo. It cost three read-only subagents, ~440k subagent tokens
and roughly two hours, and three first-pass conclusions were still wrong:

- `commonwealth-daemon` was treated as the primary runtime. It is vestigial
  and ships in no release lane.
- Binaries were counted as the unit of topology. The topology is mode-shaped:
  `sovereign-desktop` is one executable with four process modes, and
  `--daemon-child` *is* `sovereign_cli_daemon::daemon_child_main()`.
- A contention thesis was built on interior-mutability counts. The real signal
  was deferred construction.

None of the three was careless. Each was the reasonable reading of the code in
front of it. **The system permitted all three**, and every agent that touches
this codebase pays a fraction of the same cost and gets a fraction of it
wrong.

That is the problem this topology exists to remove. The measure of success is
not that the architecture is documented. It is that the count of non-obvious
facts an agent must *hold* to make a correct top-level change approaches zero.
Baseline at authoring: ~15.

---

## 2. The one disease — a minted type is not an adopted type

Seven declared structural invariants, one failure mode:

| Declared | On the live path |
|---|---|
| Five deployment profiles (`TARGET_ARCHITECTURE.md §5`) | Zero occurrences anywhere outside that document |
| `retrieve` is the only producer of `Evidence` (`§3.1`) | `CorpusIndex::retrieve` has **zero production callers**; nine bypasses |
| No `Answer` without a `Judgement` (`§3`) | `Draft::release` is reached on 1 of ~15 gate exits |
| `acquire: Source → Vec<Record>` | `Record` does not exist; `Acquirer::acquire` returns `PathBuf` |
| `measure: Run → Measurement` | `Measurement` does not exist; `LaneBaseline` fuses measurement and baseline |
| `converge: Measurement × Baseline → Verdict` | Five parallel implementations; `render_and_exit_code` returns `i32` |
| Single-writer corpus index | An **unregistered** env var (`quality/baselines/env_unregistered.txt:52`) |

The register cannot see this. `quality/CONCEPTS.toml` carries `status` (how far
migration got) and `home` (does the destination path exist). Neither asks the
only question that decides whether an invariant holds: **is the alternative
path gone?** `home = minted` is true for `Evidence` and worth nothing, because
nine other doors into the prompt are open.

**Required register change:** an `adopted` column whose value is the count of
remaining constructors *other than* the canonical one. `adopted = 0` is the
only value that means the invariant holds. Anything else is a target wearing a
`holds` marker.

---

## 3. The three axes

The top level is not a set of runtimes. Two of the three obvious "runtimes"
own zero crates exclusively — `commonwealth-daemon` is 1,145 lines over a
516k-LOC closure (0.22% of its own build), `sovereign-server` 7,375 over 535k
(1.4%) — and 63% of their union is shared by all three. A runtime boundary
here is a *linking* boundary, not a work boundary.

What actually varies independently is three things.

### 3.1 Process role — ports, GPU, identity

```
Daemon        owns models and mesh identity; binds :9741 (client) + :9742 (peer)
ComputeChild  owns the weights for one slot; binds 127.0.0.1:0, stdout handshake
Shell         reaches a Daemon over HTTP; MAY supervise a Daemon, never BE one
Peer          another Daemon, over :9742 or iroh
```

`ComputeChild` is a deliberate process boundary for GPU/llama.cpp crash
isolation and is correct as it stands. Keep it.

Role is currently an emergent property of which builder methods a call site
happened to chain. It must become a closed type each binary declares.

### 3.2 Storage role — what a process may write

```
StorageRoot(~/.svrnmesh) × { CorpusWriter, EnrichmentWriter,
                             ScipWriter, AgentStateWriter, Reader }
```

This axis exists because the Shell/Daemon split is the inverse of the obvious
reading. `sovereign-cli-llm` builds an `InferenceClient` against
`127.0.0.1:9741`, calls `into_closures()`, and hands the resulting
`EmbedFn`/`ChatCompletionFn` to a **locally constructed `CorpusEngine`**. The
Shell borrows *compute* over HTTP and owns *state* directly on disk.

`CorpusEngine::new(recipes_dir, index_dir, embed)` takes two `PathBuf`s and a
closure. It acquires nothing, registers nothing, and cannot fail. Instances are
unbounded, and more than one is the normal case:
`scripts/code-intel-corpus-run.sh` is an ~11h launchd job that runs a
CLI-built engine deliberately while the daemon stays up.

The crate says this about itself, at `corpus-engine/src/sharding.rs:29-37`:

> The canonical directory is a shared address. […] Which of them lands before
> the promote is a race.

and at `sovereign-enrichment-catalog/src/lib.rs:4-6`:

> Three processes touch `<data-root>/enrichment/<corpus-id>/`.

There is exactly **one** real cross-process lock in the subsystem — an flock on
`scip_graph.db`, and it lives in a satellite crate, not in corpus-engine.
Everything else is SQLite `busy_timeout`, atomic rename, mtime heuristics, and
a merge-tolerance table (`sharding.rs:50-57`) whose default arm is *newest
mtime wins, loser preserved as `.superseded`*. That is conflict **resolution**,
which is the honest admission that there is no conflict **prevention**.

With this axis the true statements become sayable: the Daemon holds every
writer mode and **no exclusive claim on any of them**; the Shell legitimately
holds `EnrichmentWriter + AgentStateWriter + Reader`, and — whenever
`SOVEREIGN_ENRICH_SKIP_INDEX` is unset — illegally also `CorpusWriter`.

### 3.3 Verb ownership — one implementation, reachable from any role

`enrich` is ~129,000 lines across five crates whose orchestrator
(`sovereign-cli-llm/src/enrich_cmd/`, ~34k lines) is a **private module in a
leaf binary**. `sovereign-tools/src/enrich.rs:204` therefore drives it by
`Command::new(bin).arg("enrich").arg("build")` and parses stdout banners back
into progress events.

The consequence is the one that matters for this axis: **a trapped subsystem
does not stay trapped, it duplicates.** The desktop's six shell-outs exited 127
in shipped builds because `sovereign-cli` is not bundled with the desktop, so
they were deleted (`sovereign-desktop/src-tauri/src/enrich_commands.rs:3-12`)
and the desktop now runs a second, weaker enrichment in-process.

---

## 4. The forbidden-state table

Thirteen states reachable today. **Tier** distinguishes what a type can forbid
from what only a runtime check can catch and report loudly (ARCH §18.3) —
conflating the two turns "impossible" into a slogan.

| # | Forbidden state | Evidence today | Mechanism | Tier | Falsifier |
|---|---|---|---|---|---|
| 1 | A prompt fed by content that never passed `retrieve` | 9 production `ScoredChunk` construction sites outside the door; `retrieval_pipeline.rs:951` injects model-authored prose at `score: 1.0` | `ScoredChunk` loses public fields and `Deserialize`; `EvidenceSet` is the only prompt input | type | Count of `ScoredChunk` literal constructions outside `corpus-engine` == 0 |
| 2 | An `Answer` released without a `Judgement` | `Draft::release` called at `grounding/mod.rs:1645` only — 1 of ~15 gate exits; result flattened to `String` at `:1665` | Delete `GateOutcome`; every gate exit returns `Answer` | type | Count of gate exit paths whose return type is `String` == 0 |
| 3 | Two daemon implementations binding `:9741` | `commonwealth-daemon` (vestigial: `current_thread` runtime, `NodeId::generate()` per boot, empty `Mesh`, no gossip, no release lane) and `EmbeddedDaemon` both bind it | One `Daemon` role; the second implementation cannot be constructed | type | Count of types that bind the daemon ports == 1 |
| 4 | A binary whose role cannot be named | `sovereign-desktop` has 4 modes; `--daemon-child` is the daemon | Role as a closed enum the binary declares at its entry point | type | Every `[[bin]]` resolves to a declared `Role`; gate fails otherwise |
| 5 | `:9741` meaning different things by host | Desktop installs 4 of 7 routers; CLI daemon installs 6 plus `set_provider_factory` / `set_mesh_store` / `set_convergence_recorder` | `DaemonSpec` consumed by value in `start_daemon` | type | `DaemonSpec` has zero `Option<Router>` fields |
| 6 | A shell that is conditionally a daemon | `SOVEREIGN_USE_SUPERVISOR` picks in-process `EmbeddedDaemon` vs supervised child at runtime | `Shell` cannot reach the `Daemon` constructor | type | `EmbeddedDaemon::new` is not nameable from any `Shell` crate |
| 7 | A subsystem reachable only by `exec` | `enrich_cmd` private in a leaf binary; `sovereign-tools/src/enrich.rs:204` shells out and parses banners | `pub mod`; delete the subprocess driver | type | Count of `Command::new(..).arg("enrich")` == 0 |
| 8 | An injected hook nobody calls | `CorpusEngine::fast_inference` — setter `with_fast_inference_fn` has zero callers, field never read, doc claims "used for claim extraction" | A field with no reader fails `dead_code` | type | `cargo build` warning count for unread fields on root types == 0 |
| 9 | A spawner that spawns nothing | `spawn_auto_collaborate_loop` (`daemon.rs:2833`) contains no spawn; `auto_collaborate_loop` (~1000 lines) has zero callers repo-wide | A spawn that returns no handle is not a spawn | type | Every `spawn_*` returns a handle or a token |
| 10 | Cleanup that never runs | Every process exits via `fast_exit_skip_destructors`; `GossipHandle` / `BrowseHandle` / `WatchdogHandle` / `IrohAcceptor` / `ProjectHandle` Drop impls never fire. `sovereign-server` has zero signal handlers; desktop never calls `mesh.shutdown()` | Shutdown owned by the role, not by `Drop` | type | No `_exit` reachable before a completed `Shutdown` value |
| 11 | Two processes writing `chunks.lance` | `SOVEREIGN_ENRICH_SKIP_INDEX` is the only guard, set by hand in a shell script, and is unregistered | `CorpusWriter` lease on the `StorageRoot` | runtime | Opening a corpus writer without a held lease returns `Err`; env var deleted |
| 12 | A cancel that cancels nothing | `CancellationRegistry` doc claims "single source of truth … across the daemon"; it is process-local, so a desktop cancel cannot stop a CLI ingest of the same corpus | Cancellation scoped to the lease holder | runtime | Cross-process cancel test: CLI ingest stops on desktop cancel |
| 13 | Whichever file lands first wins | `sharding.rs:29-37` says "is a race"; default merge arm is newest-mtime-wins with `.superseded` | Write ordering owned by promote | runtime | Merge-tolerance default arm removed; unknown entry kinds refuse |

Eleven of thirteen are compile-time closable. Two are genuinely runtime.

---

## 5. How this document stays honest

This file describes a failure mode it is itself susceptible to: seven documents
in this repo already declare invariants that no longer hold, and none of them
knew. Three mechanisms, in order of strength:

1. **Every row's falsifier becomes a test or an `xtask` gate.** A row whose
   falsifier is not executable is a target, and must be marked as one.
2. **`cargo xtask target-arch` joins the `quality` gate list.** It exists
   (`corpus-engine/xtask/src/target_arch.rs`, `STALE = 1`) and is *not* in
   `quality_cmd.rs`'s gate table, which is why the profiles in
   `TARGET_ARCHITECTURE.md §5` could reach zero adoption unnoticed.
3. **The `adopted` column** (§2) makes "the alternative is gone" a number the
   register carries rather than a claim a document makes.

---

## 6. Corrections this forces elsewhere

Landing this document requires the following, or the contradiction stands
(ARCH §1.1):

- `TARGET_ARCHITECTURE.md §3.1` corollary 1 — "`retrieve` is the only door" —
  is marked `partial`. The honest marker is `target`: the door has zero
  production callers.
- `TARGET_ARCHITECTURE.md §5` deployment profiles are unimplemented. Mark
  `target` or delete; today they read as description.
- `corpus-engine/DECOMPOSITION.md` steps 5–8 never shipped, which is why 73.8k
  lines of `enrichment/` remain inside `corpus-engine`. Record the stall.
- `corpus-engine-notes/src/notes.rs` was flagged in that plan as a size
  violation at 2,781 lines. It is now 7,794 — a store, a vector index and a
  replication log in one file, 2.8× past the line where someone said stop.
- `SOVEREIGN_ENRICH_SKIP_INDEX` must be registered in `quality/env-flags.toml`
  regardless of what else happens, since it currently gates index corruption.

---

## 7. Ownership — three engineers

Ownership follows the axes, not the processes, because two of the three
processes own no crates exclusively and no crate-level partition divides the
four god-crates (`corpus-engine` 157k, `sovereign-cli-llm` 138k,
`sovereign-core` 118k, `sovereign-tools` 90k — 50.6% of the workspace, three of
them in the all-three-shared set).

| Owner | Axis | Charter |
|---|---|---|
| 1 | Process role | Rows 3–6, 9, 10. `DaemonSpec`, the `Role` enum, one shutdown path per role. |
| 2 | Storage role | Rows 11–13. The `CorpusWriter` lease, cancellation scoping, promote ordering. Decomposes `corpus-engine`. |
| 3 | Verb ownership | Rows 1, 2, 7, 8. `retrieve` as the only door, `Answer` at every exit, `enrich` out of the leaf binary. |

The four god-crates are a chartered shared project with one owner each, funded
before ownership hardens. `layer-gate` currently fails on the fan-in ratchet
(`corpus-engine` 16→18, `sovereign-contracts` 19→20, `sovereign-core` 15→17)
with zero ordering violations — the gate is correctly reporting that the shared
core keeps growing.

---

## 8. Sequencing

Cheapest first coercion is **row 5** (`DaemonSpec`): mechanical, self-contained,
and it converts a runtime divergence into a missing struct field.

Highest-value first coercion is **row 1** (`retrieve` as the only door): it is
the invariant the entire answer-quality story rests on, it is the one this
codebase already tried to declare, and closing it is the proof that the
method — make the alternative unrepresentable, then count constructors —
actually works.
