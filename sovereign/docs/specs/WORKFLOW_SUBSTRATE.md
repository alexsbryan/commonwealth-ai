# Spec — Step · Artifact · Runner: a general workflow substrate

Status: **P0+P1 + cache shipped** (the `sovereign-workflow` crate + `sovereign
workflow run`; see its `README.md`), rest of P2+ design / RFC · Scope: factor
the duplicated workflow machinery into one substrate, proven by running a
user-authored model workflow. Not a rewrite.

> **P0+P1 landed** — `sovereign-workflow` (Step · Artifact · Runner: TOML graph,
> auto-derived edges, `model:`/`mcp:`/`tool:`/`transform:` steps, single-process
> runner) + the `sovereign workflow run` command (daemon-routed inference, MCP +
> tools injected). The `notes-digest` demo and a sealed-MCP e2e prove it.
>
> **First slice of P2 — the content-addressed cache — landed too**: each `Read`
> step is keyed by its resolved inputs + the source file's fingerprint and
> persisted under `~/.sovereign/workflow-cache`, so a re-run skips unchanged
> work and editing one file re-runs only that item (a `Write` step is never
> cached). Still in P2/P3: the pipeline tool as a *durable/distributed* outer
> loop, the inference-resource scheduler, and corpus/enrichment/executor
> convergence.
>
> **Collections + the generalization diff landed.** A step can now `for_each`
> over another step's JSON-array output (a chunker's `1→N`), running once per
> element (`{element.…}`) with each element caching independently. We proved the
> substrate generalizes by re-expressing corpus ingest's `chunk → embed` stage as
> a Workflow and diffing it **byte-for-byte** against the real `chunk_text` +
> embed run as an oracle — it matches, and re-runs free from cache
> (`chunk_then_embed_matches_the_real_corpus_pipeline`). The load-bearing
> finding: **the `Artifact` was already general** (a collection is just a JSON
> array); the only missing primitive was a Runner-level **map** (`for_each`). The
> data model held; the runner grew one verb. This is the discipline the spec
> calls for — discover the abstraction against a real fifth case, don't design
> the astronaut.
>
> **Dispatch is now type-safe, and `model:` speaks OICP.** Adding `embed:`
> exposed a stringly-typed smell (ARCH §2.1): the `uses` string was re-parsed by
> scattered `starts_with` probes — one of which (`needs_inference`) had already
> silently forgotten `embed:`. The `uses` string is now parsed once into a
> `StepKind` enum (`kind.rs`); `resources()` is a compiler-checked classifier, so
> a new inference-bearing kind cannot slip past the "needs the daemon?" gate. In
> the same pass the `model:` slot vocabulary became OICP's own `LatencyClass`
> (`fast`/`normal`/`extended`) and `ModelStep` now attaches a real
> `InferenceRequirements` envelope rather than leaning on the legacy `Speed`
> field + the provider's `Speed`→`latency_class` rescue. The chat command routes
> `model:`/`embed:` through `SplitInferenceProvider` so the embed slot is never
> sent a chat model id, and a real `tool:chunk` (the production `chunk_text`) +
> `examples/chunk-embed.toml` make the chunk→embed ingest runnable live.

## Context: we have five workflow engines, not zero

The system already runs workflows over local models in at least five places, and
each one independently re-implemented the runner. Verified interfaces:

| Engine | Unit of work | Composition | Resume | Retry | Concurrency | Per-step trace |
|---|---|---|---|---|---|---|
| **Corpus ingest** (`corpus-engine/engine/ingest.rs`) | `ExtractedDoc` through `Extractor`/`Chunker`/`Embedder` traits | Recipe TOML → factory (`ingest_factories.rs`) | `_source_manifest.json` + `_update_progress.json` (mtime/doc-id) | skip-and-continue | partition lock + embed batch | tracing spans |
| **Enrichment** (`enrichment/pipeline/`) | phase fns on `Pipeline` trait (`trait_def.rs:40`) | recipe `[enrichment]` → `PipelineRegistry.get` | `PhaseCache` mtime + upstream DAG (`phase_cache.rs:96`) | none (abort) | single-threaded | tracing spans |
| **Agent Planner→Executor** (`executor.rs`) | `StepKind` (Reason/Tool/Branch/ReasonWithTools) | `Plan { steps, edges }` DAG (`types/mod.rs:105` `topological_batches`) | in-memory + task-state per batch (`executor.rs:405`) | per-step idempotency gate (`:649`) | `join_all` within batch | — |
| **Pipeline tool** (`sovereign-pipeline/`) | `WorkUnit { recipe_id, key, payload }` (`worklist.rs:48`) | Recipe TOML (source/enrich/dispatch); enrich = a command template | SQLite worklist + lease sweep (`worklist.rs:283`) | bucketed classifier + attempts (`classifier.rs`) | adaptive ceiling (`adaptive.rs:80`) + mesh fan-out | status tick + cost ledger |
| **Retrieval pipeline** (`runtime/retrieval_pipeline.rs`) | `RetrievalStep { name, flag, run }` | ordered `Vec<RetrievalStep>` | — | — | sequential | **one event/step w/ before-after delta** (`:342`) |

The duplication is the point: **three different resume mechanisms** (SQLite
worklist / file manifest / mtime phase-cache), **three retry models** (bucketed /
skip / abort), **three concurrency models**, **three tracing shapes**. The
"general model" is the one runner these are all instances of — and every piece
it needs already exists as a shipping prototype.

## The three abstractions

### `Step` — a typed transform (generalize `Tool`)

`ToolDescriptor` (`types/routing.rs:184`) is already 80% of `StepDescriptor`: it
carries `effect`, `idempotency`, `latency`, `scope`, `output_schema`, and
`examples`. A `Step` is a `Tool` plus a declared **resource need** (the one thing
the scheduler consumes) and a **determinism** bit (the one thing the cache
consumes).

```rust
#[async_trait]
pub trait Step: Send + Sync {
    fn descriptor(&self) -> StepDescriptor;
    async fn run(&self, inputs: &ArtifactSet, ctx: &StepCtx) -> Result<Artifact>;
}

pub struct StepDescriptor {
    // ── identical to ToolDescriptor ──
    pub id: String, pub name: String, pub description: String,
    pub parameters: serde_json::Value, pub examples: Vec<StepExample>,
    pub effect: Effect, pub idempotency: Idempotency,
    pub latency: Latency, pub scope: Scope,
    pub output_schema: Option<serde_json::Value>,
    // ── new, load-bearing ──
    pub resources: ResourceNeed,   // what the scheduler places against
    pub deterministic: bool,       // pure transform → cache key omits model/prompt
}

pub enum ResourceNeed {
    None,                                  // CPU-only transform (chunk, parse, fold)
    Inference(oicp::InferenceRequirements),// a model call — REUSE the existing type
    Tool(Vec<Permission>),                 // a tool call (incl. MCP) — existing gate
}
```

The existing `Tool` trait becomes one `Step` impl; an `Extractor`/`Chunker` is a
`ResourceNeed::None` step; an `Embedder`/enrichment phase is a
`ResourceNeed::Inference` step; an MCP tool is a `ResourceNeed::Tool` step. No
new vocabulary — the four step *kinds* are exactly today's stage families.

### `Artifact` — a content-addressed typed value (generalize `StepOutput` + the asset store)

`StepOutput` (Text/Json/…) is the inline form; the content-addressed
`asset_store/` (sha256, sharded, ledgered) is the by-reference form for large
values (a Lance index, a corpus). Unify:

```rust
pub struct Artifact {
    pub id: ArtifactId,        // content hash = the cache key
    pub type_tag: String,      // "text" | "json" | "chunks" | "embeddings" | "corpus-index" | …
    pub body: ArtifactBody,    // Inline(serde_json::Value) | ByRef(AssetRef)
    pub lineage: Vec<ArtifactId>,  // provenance edges (free, from inputs)
    pub produced_by: String,   // step id
}
```

**The cache key is the whole game.** For a step output:
`hash(step_id, params, sorted(input artifact ids), model_id?, prompt_hash?, sampling?)`
— omitting the model/prompt terms when `deterministic`. This is Bazel/Nix for
model artifacts: resume is "skip steps whose key is already in the store," dedup
and provenance fall out for free. It subsumes all three existing resume
mechanisms.

### `Runner` — schedule, cache, retry, trace (synthesize the four existing runners)

```rust
#[async_trait]
pub trait Runner {
    async fn run(&self, wf: &Workflow, inputs: ArtifactSet) -> Result<RunOutcome>;
}
```

The runner topologically orders the graph (`Plan::topological_batches` already
does this), and for each ready step: computes the cache key → on hit, skip; on
miss, **schedule** the step against its `ResourceNeed`, run, content-address the
output, record lineage, and **emit one trace event** (the
`retrieval_pipeline.rs:342` shape: `step`, `before/after`, `delta`, `note`).
Failures route through the **bucketed classifier** (`classifier.rs`) with the
adaptive ceiling (`adaptive.rs`). The cross-cutting concerns are pluggable
backends, each already prototyped:

| Runner backend | Backed by (existing) |
|---|---|
| Scheduler (`ResourceNeed` → placement) | `BackendSelector::select` + `score_with_adjustments` (`selector.rs:211`, the SSOT) |
| Artifact store (content cache) | `asset_store/` sha256 + a manifest table |
| Durable persistence (resume) | the pipeline tool's SQLite worklist (`worklist.rs:122`) |
| Tracer | `RetrievalPipeline::run`'s per-step event |

**Two runner profiles over the *same* Step/Artifact model:**
`InteractiveRunner` (in-memory, low-latency, single turn) **is today's
`Executor`**; `BatchRunner` (durable, distributed, resumable) **is today's
pipeline-tool driver**. We do not unify the *runtimes* — only the step and
artifact vocabularies beneath them.

### `Workflow` — the graph as data (a TOML sibling of `Plan`)

```toml
[[step]]
id = "actions"
uses = "model:thoughtful"          # registry namespace: model: | mcp: | tool: | transform: | workflow:
input = { transcript = "{transcribe.output}" }
resources = { latency_class = "extended", privacy = "local_only" }
prompt = "List the action items in this transcript as bullets."
```

`uses` resolves into one **step registry** that subsumes today's three (Tool
registry, recipe-stage factory, enrichment `PipelineRegistry`). `{step.key}`
templating is exactly `resolve_inputs` (`executor.rs:208`).

## The distinctive piece: the scheduler

Generic engines (Airflow/Temporal) assume cheap, stateless steps bottlenecked on
I/O. Here the bottleneck is a handful of GPU slots across a heterogeneous mesh,
and a step is an expensive model call. The substrate is *inference-aware*, and
**that scheduler already exists**: a step's `ResourceNeed::Inference(req)` is the
existing `oicp::InferenceRequirements { capability_hint, latency_class,
context_tokens, max_output_tokens, privacy }`; `BackendSelector::select(request,
backends)` scores every `(peer, model-claim)` pair via `score_with_adjustments`
(structural fit × affinity × observed load × locality × availability) and returns
a placement. The Runner calls it per Inference step. `privacy = local_only`
keeps a step on-box; `mesh_allowed` lets it fan to peers — the same gate that
governs chat today.

## Mapping the five engines onto the substrate

Each existing engine is the general model with specific fills:

- **Corpus ingest** = a fixed `Workflow` of `None`/`Inference` steps
  (acquire→extract→filter→chunk→embed→index) on the `BatchRunner`; the
  `_update_progress` manifest becomes the artifact cache.
- **Enrichment** = a `Workflow` whose steps are the `Pipeline` phases (the
  phase-DAG `PhaseCache` already tracks is the artifact lineage).
- **Agent task** = a `Workflow` the planner *generates* at runtime, on the
  `InteractiveRunner` (the `Executor`, unchanged).
- **Pipeline tool** = the `BatchRunner` itself; today each `WorkUnit` runs one
  command — generalized, each runs a `Workflow`.
- **Retrieval pipeline** = a `Workflow` of `None` steps; its per-step trace is
  the Runner's tracer prototype.

## Generalization example (the smallest real test)

A user-authored workflow on the existing durable runner — **not** the corpus
pipeline (proving generality), reusing the pipeline tool's worklist + scheduler
(proving it's incremental). `memos-to-actions.toml`:

```toml
[workflow]
name = "memos-to-actions"

[source]                 # reuses the pipeline tool's source enumerator (folder/SlugList/Command)
type = "folder"
path = "{memos_dir}"
glob = "*.m4a"

[[step]]                 # 1. an MCP tool step — a local Whisper server
id = "transcribe"
uses = "mcp:whisper:transcribe_audio"
input = { path = "{item.path}" }

[[step]]                 # 2. a local-model step, mesh-distributable
id = "actions"
uses = "model:thoughtful"
resources = { latency_class = "extended", privacy = "local_only" }
prompt = "Transcript:\n{transcribe.output}\n\nList the action items as bullets."

[[step]]                 # 3. a built-in tool step
id = "save"
uses = "tool:write_note"
params = { title = "{item.name} — action items", body = "{actions.output}" }
```

```
sovereign pipeline run memos-to-actions.toml --concurrency 4
```

The `BatchRunner` enumerates the folder → one `WorkUnit` per file → per unit runs
the three-step `Workflow` **in-process via the step registry** (instead of one
opaque `enrich.command`) → content-addresses each step output (re-run skips
already-transcribed files for free) → schedules step 2 across mesh peers via
`BackendSelector` → buckets and retries failures. One file = mixing an ecosystem
MCP tool, a local model, and a built-in tool, authored as data by a
non-developer. That is the payoff of the whole MCP→attach→substrate arc.

## Roadmap — where we stand (updated 2026-06-23)

Progress lives on **three independent axes** — conflating them is the easiest way
to overstate where we are.

**1. Capability — can the substrate *express* the work?** Furthest along.

```
read → chunk → embed → store → search
 ✅      ✅      ✅       ✅       ✅      ← full ingest stage runs e2e
```

The read→chunk→embed half is proven byte-identical to the real corpus engine
(`chunk_then_embed_matches_the_real_corpus_pipeline`); `tool:corpus_store` writes
the `(chunk, embedding)` pairs into the real LanceDB index via the engine's own
`insert_batch` (idempotent per `source_doc_id`), and a `corpus search` command
closes the loop. The whole `ingest.toml` (`chunk → embed → store`) runs live —
real 1024-dim vectors, FTS indices built, queryable.

**2. Adoption — does *production* run on it?** Started. `sovereign corpus ingest
<folder>` is a production `corpus` subcommand whose *mechanism is the Runner* — it
builds a real, searchable corpus by running `chunk → embed → store`, not a bespoke
loop (plain-text sources; the engine's own `ingest()` is untouched). That's the
first rung: a real entrypoint on the substrate, not a `workflow run` demo. The
deeper work — routing `LocalCorpusManager::ingest` (the desktop+CLI API) through
the Runner and converging the rich path (batched embed, resume, enrichment) before
retiring the bespoke loops — is the remaining prize.

**3. Scale — durable / distributed / scheduled?** Single-process today. Durable
resume, mesh-distribution, and Runner-native scheduling are unbuilt (with a
caveat — see deviation 2).

### Built (core, hardened)

`Step·Artifact·Runner` crate; TOML graph + auto-derived edges; topo execution;
per-step tracing. Five step kinds behind a type-safe `StepKind` (one parse
boundary, compiler-checked `resources()`). Content-addressed cache (effect-aware,
file-fingerprint invalidation, per-element granularity). `for_each` collection-map
(not in the original plan — discovered when chunk→embed needed it). OICP-native
`model:` requests. The generalization diff + live chunk→embed e2e.

### Two deviations from the original P0–P3 plan

1. **The Runner is standalone, not bolted onto the pipeline tool.** P0 imagined
   "the pipeline tool's WorkUnit runs a one-step Workflow." Research killed that:
   `sovereign-pipeline` has zero inference/tool deps and runs one `/bin/sh -c` per
   unit with no output threading. So durable/distributed becomes a clean *outer
   loop* later (the pipeline tool runs `sovereign workflow run-unit`), and the
   substrate stayed independent. Durability is therefore still entirely unbuilt.
2. **Scheduling is borrowed from the daemon, not Runner-native.** The "distinctive
   piece" above imagined the Runner calling `BackendSelector::select` per inference
   step. Instead `model:`/`embed:` route through the daemon over HTTP and the
   *daemon* does the OICP/mesh scheduling — thinner, and aligned with "leverage
   inference via request, don't own complexity." Treat the Runner-native scheduler
   as a **possible non-goal**, not a pending task.

### Phased path from here

| Phase | What | Status |
|---|---|---|
| Done | **Store step** — `tool:corpus_store` (reuses `CorpusIndex::insert_batch`, idempotent per doc); `ingest.toml` = chunk→embed→store; `corpus search` closes the loop. The `secret-agent` ingest runs live and is queryable | ✅ |
| Done | **Adoption, first rung** — `sovereign corpus ingest <folder>` runs chunk→embed→store on the Runner (plain-text; bespoke `ingest()` untouched). First production path on the substrate; shares `run_assembled` with `workflow run` | ✅ |
| **Now** | **Deeper adoption** — route `LocalCorpusManager::ingest` (desktop+CLI API) through the Runner for plain-text; converge batched-embed / resume / enrichment; then retire the bespoke loops | ⬜ the prize |
| Then | **Durable/distributed** — pipeline tool as the outer loop over `run-unit` | ⬜ |
| Later | **Executor convergence** — a `Plan` *is* a `Workflow`; agent steps = workflow steps; ATOS keeps its own human-in-the-loop Runner profile, sharing only the vocabulary | ⬜ |
| Maybe-not | Runner-native `BackendSelector` scheduling | ❓ daemon may already own this correctly |

### The store step, scoped

The zip (`store` needs `(chunk, embedding)` pairs) is already expressible: `store`
is a normal **non-`for_each`** tool taking *both whole collections* as params
(`chunks = "{chunk.output}"`, `embeddings = "{embed.output}"`) and pairing them
internally — no new Runner primitive. The real work is corpus-engine-side: writing
the LanceDB index in the **exact schema the retrieval path reads**. A vector
written but not in searchable shape is a silent half-success, so DoD is `search`
returning the chunk, not "rows written." Open fork to settle first: **reuse the
corpus-engine's real index writer** (faithful, keeps this on the adoption path) vs.
a minimal parallel table (cleaner demo, risks schema drift) — lean reuse.

## What NOT to do (the over-build risks)

- **Don't unify the runtimes.** Interactive (latency, in-memory) and batch
  (throughput, durable) are genuinely different schedulers; force them together
  and you get a Procrustean bed. Unify the `Step`/`Artifact` vocabulary beneath
  them, nothing more.
- **Don't design ahead of instance #5.** The corpus/enrichment/executor/pipeline
  engines are instances 1–4; the clean seam *emerges* from making the
  `memos-to-actions` instance and refactoring the overlap — the same discipline
  as the corpus-engine carve-outs and the runtime decomposition.
- **Don't reinvent the scheduler.** `BackendSelector` + `score_with_adjustments`
  is the SSOT and the hardest part; the substrate *calls* it, never replaces it.
- **Don't inline large artifacts.** A corpus index or an embedding set is
  by-reference (asset store); hash the manifest, not the bytes.
- **Don't make this a marketplace.** Step *types* are registered in-process
  (Rust) or are MCP tools (config); workflows are authored as data. No plugin
  runtime — consistent with `ARCHITECTURE.md`'s no-marketplace stance.

## Key seams to build on

| Concern | File:line |
|---|---|
| `StepDescriptor` prototype | `sovereign-core/src/types/routing.rs:184` (`ToolDescriptor`) |
| DAG + topo order | `sovereign-core/src/types/mod.rs:105` (`Plan::topological_batches`) |
| `{step.key}` templating | `sovereign-core/src/executor.rs:208` (`resolve_inputs`) |
| Per-step tracer | `sovereign-core/src/runtime/retrieval_pipeline.rs:342` |
| Scheduler (SSOT) | `sovereign-inference/src/selector.rs:211` + `oicp-types/src/lib.rs` `score_with_adjustments` |
| `ResourceNeed::Inference` type | `oicp-types/src/lib.rs:389` (`InferenceRequirements`) |
| Durable worklist | `sovereign-pipeline/src/worklist.rs:48,122` + `driver.rs:105` |
| Bucketed retry / adaptive | `sovereign-pipeline/src/classifier.rs`, `adaptive.rs:80` |
| Content-addressed store | `corpus-engine/src/asset_store/` |
| Existing stage traits | `corpus-engine/src/{extractors,chunkers,acquirers}/mod.rs` |
