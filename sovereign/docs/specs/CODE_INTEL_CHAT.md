# Code Intelligence in Chat — Plain-English Questions → Code-Level Answers

**Status:** Thesis hardened at 172-function scale (3/3 top-3; PPR disambiguation +
patchability proven). **Inc 1 core BUILT + green** (composed enrichment pass:
enumerate → summarize → index, patchable) — **Inc 1 + step 1 verified 22/0** on
2026-06-25. **Step 1 (CLI verb `sovereign enrich code-intel`) BUILT.** **Inc 2
(deterministic code-trace augmentation) BUILT** — NOT a chat tool-loop: the §3.3
hardening found the chat answer path is single-shot retrieve → synthesize, so Inc 2
deterministically appends a call-graph trace to the synthesis evidence for code-intel
hits. The trace builder lives in the lean `corpus-engine-scip` crate (read-only SQL
over `scip_graph.db`, **no tree-sitter grammars in the chat runtime**). Inc-2
verification pending the full-suite run. Remaining: **Inc 3** (grade vs the §4
answer-key). (2026-06-25)
**Owner context:** born from the tool-call-leak investigation (see
`sovereign/.../pipeline/presenter.rs::present_answer`). This doc is the
durable plan to build the feature; per `ARCH_PRINCIPLES.md §1.1` the
*validated-findings* sections are contracts (reproducible against the
experiments), the *plan* sections are proposals.

---

## 1. The vision & the demo

Let a user **ask high-level questions of a SCIP-indexed codebase in plain
English and get code-level answers** (precise traces, impact, "how does X
work"). The code corpora already carry **both** `chunks.lance` (embeddings)
**and** `scip_graph.db` (call-graph) — e.g. `commonwealth-ai`, `corpus-engine`,
`sovereign` (all 1024-dim).

- **Audience:** the **startup CTO persona** — a technical buyer who can smell
  BS and signs the check. The buy-trigger is *"turn the subsystem I'm afraid to
  touch into one I can change with confidence,"* + **local/sovereign** (their
  code never leaves the laptop — the moat vs Cursor/Copilot).
- **Headline demo:** the **full distributed-inference trace** (§4). The scary,
  trait-heavy `inference`/`model_slot`/`mesh` code is *both* where a CTO most
  fears modifying *and* where the call-graph most decisively beats fuzzy
  retrieval (dynamic dispatch).
- **The "pop" test (the whole thesis):** the user must ask **without naming any
  function/symbol** and still get the code back. "Callers of `gate_answer`" has
  zero pop — you already named it.

---

## 2. The problem — the conceptual→symbol bridge

The SCIP call-graph is **precise and complete** (`callers`/`callees`/`blast`,
883K edges, compiler-resolved, catches trait dispatch) — *once you have the
symbol*. The unsolved piece is getting from plain-English intent **to** the
right entry symbol. Proven broken for **retrieval-only**:

- A chat run of 3 no-keyword questions retrieved **0/3** of the load-bearing
  functions. It drifted to **tests + prompts + config**:
  `is_daemon_live_false_when_port_answers_but_path_404s`, `CURATOR_SYSTEM`,
  `oicp_routing_selects_correct_model`, `daemon_is_running`, `chat_completions`.
- **Root cause:** *tests describe behavior in English*, so a plain-English
  question out-matches the terse implementation. The right function's **raw
  code** even loses to a near-but-wrong HTTP entry (`chat_completions`).
- The logic sits **one hop below** a retrievable seam — so the call-graph can
  *walk* to it, but retrieval never *lands* near it.

(Aside: the dev `code_search` CLI tool is separately broken/scoped — returns
only `oicp-types` for everything — but that is NOT what the chat uses; the chat
retrieval covers the code corpora. Don't be misled by the dev tool.)

---

## 3. The solution — intent-forced enrichment + SCIP call-graph

**Architecture (both halves now have proof):**

> **Enrich each symbol with an *intent-forced* purpose-summary + the questions
> it answers** → a conceptual question matches that → **the symbol** → **SCIP
> call-graph traces** from there → synthesize the answer.

Enrichment fixes *retrieval*; the call-graph does the *trace*.

### 3.1 The validated experiments (reproducible; scratch in `/tmp/embed_*.py`)

Method: cosine similarity against the daemon's `qwen-embedding-0.6b` via
`POST :9741/v1/embeddings`; chat summaries via `POST :9741/v1/chat/completions`.

| # | Test | Result |
|---|---|---|
| 1 | purpose-SUMMARY vs RAW code, vs 3 conceptual Qs | summary wins **+0.15 mean** (all 3) |
| 2 | summary vs the actual POLLUTION (tests/HTTP-entry) | summary **ranks #1** (0.582 vs 0.446) |
| 3 | right summary vs a pool of 6+ decoy summaries (incl. hard `select_peers_ranked`) | **3/3** correct discrimination |
| 4 | **AUTO**-generated summary (35B, default prompt) | **FAILS** (0.443, lost) — wrote jargon: *"model routing strategy, ranked peer candidates"* |
| 5 | AUTO summary with **intent-forced prompt** | **WINS 0.718** vs 0.513 next (+0.20) |

### 3.2 The load-bearing finding — **the enrichment prompt is the lever**

A summary in **code-vocabulary fails**; the **same model forced into
user-vocabulary wins.** The intent prompt that worked:

> "Write a search-index entry a NON-PROGRAMMER could match by asking a
> plain-English question. Use everyday words ('this computer', 'another machine
> in the cluster', 'the AI model'); NEVER code/type names ('peer', 'node',
> 'RouteDecision', 'inference'). Output `SUMMARY:` (one plain sentence on the
> real-world decision) + `ASKS:` (two plain-English questions a user might ask
> that this answers)."

Produced for `select_route`: *"The system decides whether to process a request
using a specific designated AI model, another computer in the network, or
locally as a backup. ASKS: Which machine will handle this task? What happens if
no other computers are available?"* — this is what won.

### 3.3 The reuse — **Atlas (System 2), code ontology**

This is **not net-new infrastructure.** The Atlas — System 2 in
`corpus-engine/ENRICHMENT.md` (see also `ENRICHMENT_V2.md`) —
already does LLM-driven typed atoms (Entity/Claim/**Question**/…) with
**custom-ontology** support (see the custom-atlas work). The `Question` atom
type **is** the intent-bridge validated above. A **code ontology** would emit,
per symbol: a Function/Module entity with the intent-summary + Question atoms
(what it answers). **SCIP supplies the precise edges** (don't re-derive calls
from the LLM). So: Atlas Question-atoms (retrieval) + SCIP (navigation).

**Reuse audit — RAPTOR / GLiNER / HippoRAG (all three already in-repo; decided
2026-06-25).** The corpus stack already composes GLiNER entities + RAPTOR nodes
→ an entity graph → Personalized PageRank (`sovereign-tools/src/entity_graph.rs`,
`conv_entity_graph.rs`, `attached_document_search.rs` — a faithful HippoRAG, with
`from_raptor_nodes` *and* `from_chunk_entities` builders + `personalized_pagerank`).
For a **code** ontology the verdict differs per piece, because **SCIP changes the
economics: we already own an exact, compiler-resolved, trait-aware call graph
(883K edges), so the lossy text-derived graph the corpus stack builds is
redundant for code.**

- **GLiNER → drop.** It guesses entities/relations from text via NER; SCIP gives
  exact symbols + exact edges for free. Worse, GLiNER's output is *code-vocabulary*
  (identifiers/types) — the very signal that *loses* the conceptual→symbol bridge
  (§2). Dominated by SCIP for code.
- **RAPTOR → keep, as the multi-resolution summary *generator*** (it already feeds
  the graph via `entity_graph::from_raptor_nodes`). Two adaptations: (1) **swap its
  cluster-summary prompt for the intent-forced user-vocabulary prompt** (§3.2 — the
  proven lever; a generic RAPTOR summary is jargon and *fails*, experiment #4); (2)
  drive the hierarchy off **SCIP/module structure**, not embedding clusters (code's
  real hierarchy is the call/module tree). Bonus: RAPTOR's selectivity is the answer
  to **scale** — the subsystem is ~2,130 functions; summarizing every leaf equally
  is wasteful, a tree summarizes clusters and spends the LLM only where it pays.
  Multi-resolution also unlocks *subsystem*-level questions ("how does the whole
  inference path work?") a single function summary can't answer.
- **HippoRAG / Personalized PageRank → keep, run over the SCIP graph.** The real
  gem. The §5 tool-loop's "walk the call-graph" step is naive 1-hop today; PPR
  seeded at the top-K retrieval anchors and diffused over the **SCIP** graph is the
  principled multi-hop version — exactly HippoRAG's seed→diffuse→rank, but over a
  *perfect* graph instead of an OpenIE-extracted one. It directly resolves the Q1
  case (seed at `locate_named_model`+`select_route`+`select_peers_ranked` → the hub
  `select_route` accrues the mass). `personalized_pagerank` is **already
  implemented** — point a `from_scip_nodes` builder at it.
  **VALIDATED (2026-06-25, `/tmp/ppr_over_scip_q1.py`):** PPR seeded with the Q1
  retrieval scores, diffused over the real 9-edge SCIP subgraph, promotes
  `select_route` from retrieval **rank 2 → rank 1** under both *undirected*
  (centrality: 0.267 vs 0.261) and *reversed*/callee→caller (ancestry: decisive
  0.354 vs 0.194). *Forward* (caller→callee) flows mass to leaf callees and leaves
  it at rank 3 — so the tool-loop must use undirected or reversed, **never forward**.
  Control: the Q1-seeded diffusion correctly *suppresses* the unrelated gate family
  (`gate_answer` 16→19) — PPR is a **per-question** re-ranker, re-seeded with each
  question's retrieval scores. Caveat: 20-node/9-edge sparse graph; the *direction*
  of the effect is proven, full-corpus magnitude is for the scale run.

**Net:** Atlas (System 2) is the home for the typed atoms (chosen 2026-06-25);
**RAPTOR** (re-prompted, structure-driven) generates the multi-resolution intent
summaries; **SCIP** supplies exact edges; **PPR over SCIP** is the multi-hop
retrieval. The one **non-negotiable across all of them is the intent-forced
prompt** — the infrastructure is reusable, but the user-vocabulary lever (§3.2)
is what makes any of it match a conceptual question.

### 3.4 Patchability — the constraint that separates a CODE ontology from a text one

A document corpus is static once ingested; **code changes in small targeted diffs
constantly.** An enrichment that needs a full re-pass per commit is dead on arrival
(the inference/mesh subsystem alone is ~2,130 functions ≈ ~100 min of 35B
summarization). The ontology must **patch at the symbol grain.** Grounded audit of
what the repo already provides:

- **The patch backbone EXISTS — in the chunk index.**
  `corpus-engine/src/engine/reindex.rs::reindex_file` is "the hot path the
  `CodeWatcher` [drives]": it diffs a changed file's chunks by **BLAKE3
  `content_hash`** and re-embeds only the delta — `reindex_file.noop` (every chunk
  hash-matched → skip), `delete_only`, `delta_applied`. Per-id eviction exists
  (`index/write.rs::delete_chunks_by_ids`). *Targeted, content-hash-gated
  re-enrichment is already built* for chunks.
- **SCIP self-patches the edges.** The call graph is regenerated by the compiler on
  each build — caller-only changes move edges for free, no LLM. (This is also why
  **dropping GLiNER/OpenIE graphs was right, §3.3:** an LLM-extracted code graph
  would be re-extracted per diff with churning, non-deterministic output — a
  patchability nightmare. SCIP edges are deterministic and self-maintained.)
- **The Atlas atom store is the GAP.** `atoms.json` is a whole-corpus **monolith**,
  fingerprinted as one blob (`atlas/summary.rs` keys a cached summary on atoms.json's
  SHA-256 + mtime + size). There is **no per-symbol atom upsert/evict** — a change
  means regenerating the whole atlas. Real tension with the Atlas-as-home decision
  (Q2): as built, the Atlas is a batch artifact, not an incrementally-patchable store.

**The cost model that makes it tractable.** Key each intent-summary by its symbol's
**body `content_hash`**, riding `reindex_file`'s delta path:
- body changed → one LLM re-summary (targeted, cheap);
- caller-only change → body unchanged → summary stands, only SCIP edges move (free);
- rename / move with identical body → body_hash matches → **summary carried over,
  zero LLM cost.**
Per-commit cost ≈ **number of changed function *bodies*** (typically 1–5), not the
corpus. That is the whole game.

**The remaining cost center is RAPTOR's hierarchy** (§3.3): parent/cluster summaries
aggregate children, so a leaf change wants to invalidate ancestors (cascade).
Mitigation = **tiered freshness**: leaf (per-function) summaries patch on-commit via
`reindex_file`; parent/subsystem summaries are eventually-consistent (batched / lazy
/ debounced, or structurally re-aggregated without an LLM until a churn threshold).
Match the freshness SLA to the change frequency at each resolution.

**Staleness must be observable (glassbox).** When a stored summary's `content_hash`
no longer matches the live symbol, retrieval must **down-weight/flag it**, never
silently trust it — an answer narrated from a stale summary is the failure mode to
prevent. Same discipline as the dev watcher's `live`/`stale` signals.

**Storage fork the constraint forces** (subordinate to Q2's "Atlas" choice):
- **(B, recommended)** emit the fast-patching per-symbol intent-summary + ASKS onto
  the **already-incremental chunk path** (keyed by symbol + body_hash, via
  `reindex_file`); reserve the monolithic Atlas for slower-moving typed-atom / RAPTOR
  structure with tiered freshness. Patchability for free on the load-bearing signal.
- **(A)** make the Atlas atom store itself incrementally patchable (atoms.json
  monolith → a keyed per-symbol store so one symbol's atoms upsert/evict). "Do it all
  in the chosen home," but the bigger lift.

---

## 4. The answer-key — the distributed-inference trace (demo + grading rubric)

Hand-traced; this is what the chat must reproduce. `--symbol` (not `--name`) is
the call-graph CLI param.

**Local leg:** `handle_message_stream_with_classification` (streaming.rs) →
**`InferenceProvider::complete_stream_with_id_and_finish`** (streaming.rs:905,
**dyn-dispatch #1**) → `EmbeddedLlamaCpp` → `select_slot_for_request`
(engine.rs:1452, Fast/Primary/Code) → acquire KV permit → `spawn_blocking` →
`generate_stream_sync` (model_slot.rs) → tokenize → decode → sample loop →
`token_to_piece` (llama.rs:94, FFI) → stream frames.

**Distributed leg:** diverges at the same trait call → `MeshInferenceProvider`
→ **`select_route`** (peer_inference.rs:1392) OICP-scores peers + builds a
fallback cascade → `provider_for_peer` → `RemoteApiProvider` →
`POST {peer}/v1/chat/completions` → **the peer runs its OWN
`Arc<dyn InferenceProvider>`** (the kicker — the trait followed across a network
boundary) → its own slot/model → SSE frames home → `run_synthesis_stream` gates
them → user.

**Five dyn-dispatch boundaries** (the punchline — grep returns nothing here):
(1) `Arc<dyn InferenceProvider>` @ streaming.rs:905; (2) `select_route`;
(3) `provider_for_peer`→`RemoteApiProvider`; (4) the **peer's**
`Arc<dyn InferenceProvider>`; (5) `token_to_piece` FFI. **Trust feature:** the
trace stops *honestly* at the boundaries it can't cross (C++ decode, TLS variant,
OICP scoring in `commonwealth-inference`) instead of hallucinating them.

Verified live on the call-graph: `callers(gate_answer)` → 8 exact sites incl.
`gate_held_answer` @ streaming.rs:279; `callers(select_route)` → the seam
methods; `blast(InferenceProvider, depth=2)` → dependents across crates.

---

## 5. The build plan

**The architecture is a tool-loop, not retrieval-only** (proven necessary):
*retrieve a neighborhood anchor → walk the call-graph toward the load-bearing
logic → synthesize.* Increments:

- **Inc 1 — Enrichment pass (the bridge).** A code ontology for the Atlas (or a
  dedicated `symbol_summary` tier): for each SCIP symbol, LLM-generate the
  **intent-forced** summary + Question atoms (§3.2 prompt), embed + index
  alongside the code chunks. SCIP provides edges. *De-risk first with the
  subsystem-scale prototype below.*
- **Inc 2 — Chat tool-loop.** When a **code corpus is scoped**, expose
  `symbols`/`callers`/`callees`/`blast` to the model as a real agentic loop:
  retrieve over the intent-enriched index → identify the entry symbol → traverse
  the call-graph → narrate (filter the `callees` type-ref noise; order hops; name
  the dyn-dispatch boundaries). This is *also* where the reflexive tool calls
  finally **land** instead of leaking — `present_answer` (already shipped) stays
  the guard for non-code chats.
- **Inc 3 — Grade vs the answer-key (§4).** Headless first (CLI harness), grade
  the trace against the rubric, *then* wire any UI.

### The immediate next iteration (the gate to Inc 1)

> **Subsystem-scale prototype, contained script, no production wiring:**
> SCIP-list the ~15–20 `inference`/`mesh` functions → read each → generate the
> intent-summary (§3.2 prompt) → embed + index → run the 3 conceptual questions
> through **actual top-K retrieval**. **Success bar:** the right function is
> **top-3** for each question. If it holds at scale with auto-generation, the
> pipeline is real → build Inc 1 properly.

**RESULT (2026-06-25): PASSED — 3/3 top-3.** Pool = **20 real functions** (the 3
targets + genuine hard distractors: `select_peers_ranked`/`select_peer`/
`locate_named_model` next to `select_route`; `gate_held_answer`/`gate_longform`/
`grounded_abstention` next to `gate_answer`; `pick_slot`/`has_code_slot` next to
`select_slot_for_request`; + organic neighbors). Every summary auto-generated by
the **same** §3.2 intent prompt — no per-function tuning, no mention of the
questions. Script: `/tmp/code_intel_subsystem_proto.py`; artifact:
`/tmp/code_intel_subsystem_proto.json`.

- **Q2 → `gate_answer`: rank 1, clean** (0.763 vs 0.725 next).
- **Q3 → `select_slot_for_request`: rank 1, clean** (0.779 vs 0.744 next).
- **Q1 → `select_route`: rank 2** — `locate_named_model` (0.835) edged it,
  `select_peers_ranked` (0.773) just behind; top-3 within 0.06.

**The Q1 near-tie is the load-bearing finding, and it *validates* the
architecture.** Those three are a call-graph family: `select_route` *calls*
both `locate_named_model` and `select_peers_ranked`. Retrieval surfaced a
**parent + its two children**, and the #1 hit is a **direct callee** of the true
entry. Proven end-to-end on the live SCIP graph:
`callers(locate_named_model)` → **`select_route` @ peer_inference.rs:1401** — the
#1 retrieval anchor climbs to the canonical entry in **one hop**. So retrieval
lands the right neighborhood and the call-graph disambiguates the entry — exactly
the *retrieve-anchor → walk-graph* design. **Implication for Inc 2:** the
tool-loop must not trust retrieval rank-1 as the answer; it must treat the top-K
as a *neighborhood* and use `callers`/`callees` to resolve the canonical entry.

Honest scope of this de-risk: cosine over **auto-summaries only** (no raw-chunk
or Question-atom blend yet — §7), a **20-way** pool (not the full multi-thousand-
symbol corpus). The stated bar (top-3 at subsystem scale with auto-generation)
is met; full-corpus scale + retrieval blend remain open.

The 3 canonical eval questions (no keywords):
1. *"how does the app decide whether the AI model runs on my own machine or gets
   sent to another computer in the cluster?"* → `select_route`
2. *"how does this system keep the AI from stating answers that aren't supported
   by the documents it pulled up?"* → `gate_answer`
3. *"when several AI models are loaded at once, what chooses which one handles my
   request?"* → `select_slot_for_request`

### Hardening results (harden-first path, 2026-06-25)

Per the "harden before production code" choice, three de-risks beyond the 20-way
gate — all contained scripts, no production wiring:

1. **PPR-over-SCIP (the HippoRAG borrow) — §3.3.** Q1's retrieval near-miss fixed:
   `select_route` rank 2 → 1 by diffusing the retrieval scores over the real SCIP
   graph. Proves *retrieve-neighborhood → graph-disambiguate-entry* end-to-end.
2. **Scale + retrieval-blend (172 non-test fns across the 7 core files;
   `/tmp/code_intel_scale_blend.py`).** Conceptual decision/seam questions **hold at
   scale: 3/3 top-3** against 168 real distractors (Q1 `select_route`, Q2
   `gate_answer`, Q3 `select_slot_for_request`). **Best signal = `summary+asks`** (the
   exact Atlas atom shape) — and the two sub-signals are **complementary**: `summary`
   alone misses Q3 (rank 4), `asks` alone misses Q1 (rank 6), but combined they are
   top-3 on all three. Direct evidence for the Atlas `Question`-atom design and the
   answer to the §7 blend question: index **both**, retrieve over the union.
   - **Q4 (`token_to_piece`, a deep FFI leaf) FAILED across all modes (rank 43–84)** —
     diagnosed honestly: the summary is *correct + user-voiced*, but the question hit
     two retrieval pathologies — a **directional-inverse collision** (detokenize
     "numbers→words" is cosine-near embed "words→numbers") and **near-duplicate
     flooding** (~8 near-identical `embed*` summaries crowd the top). Both are exactly
     what the **call-graph traverses past** (reach the leaf by walking *down* from the
     streaming loop) — so Q4 reproduces the §2/§5 thesis at scale: retrieval-alone is
     insufficient for leaves; the tool-loop is load-bearing, not optional.
3. **Patchability cost model — demonstrated against the real 172-summary cache.**
   No-op commit = 0 re-summaries (172/172 hash-cached); one edited body = 1; rename /
   move with identical body = 0 (carried over, keyed by body `content_hash`);
   caller-only change = 0 (SCIP self-patches the edge). **Per-commit cost = number of
   changed function bodies, not the corpus** (§3.4), empirically confirmed.

**Net: the thesis is de-risked at scale.** Retrieval (`summary+asks`) lands the
conceptual neighborhood; PPR-over-SCIP picks the entry; deep leaves need the
call-graph (Q4); enrichment patches at the symbol grain for the cost of a diff. The
remaining work is the production build (Inc 1–3), not more proof.

### Build log

- **2026-06-25 — Inc 1 slice 1 (storage-agnostic generation core): landed, green.**
  `corpus-engine/src/enrichment/code_intel/` — `SymbolMeta`/`SymbolEnrichment`, the
  injected-`ChatCompletionFn` generator (`enrich_symbol`), the patchable
  `enrich_symbols_incremental` driver (glassbox `IncrementalReport`), and the
  on-disk-overridable intent prompt (`prompts/symbol_enrichment_system.md`, faithful
  to §3.2, code-only input). 8 unit tests with a fake provider (no daemon); full
  workspace compiles + filtered suite green (11/0). Reuses existing seams only
  (`ChatCompletionFn`, `ChatPrompt`, `load_or_baked`, `blake3`, `Error::Extraction`).
  Storage decision still deferred ("decide during build") — this core is identical
  for Atlas atoms or chunk rows.
- **2026-06-25 — Inc 1 slices 2–4 (the composed pass): landed, green (20/0).**
  - **Slice 2 `scip_source.rs`** (`treesitter`): `enumerate_symbol_sources(scip,
    source_root)` — `symbols_in_crate("","")` (all corpus symbols) → keep
    functions/methods → read each body (0-based inclusive slice, matching
    `symbol_lookup.rs::read_symbol_body`) → `Vec<SymbolSource>`. Dedups SCIP's
    double-listed rows; skips trivially short bodies; glassbox counts.
  - **Slice 3 `store.rs`** (Path B chosen): `index_symbol_enrichments` upserts each
    `summary + asks` as a chunk in the existing `chunks.lance` (so existing retrieval
    picks it up — unlike RAPTOR's separate table). `source_doc_id =
    codeintel:<qualified_name>` (stable upsert handle), `content_hash = body_hash`
    (the delta gate: unchanged body skips embed+write). Carries symbol identity
    (name/file/lines) for call-graph trace-back.
  - **Slice 4 `pass.rs`** (`treesitter`): `run_code_intel[_for_corpus]` composes 1–3
    plus a body-hash sidecar cache (`code_intel_cache.json`) so an unchanged body
    skips the LLM too. Integration test proves a 2nd pass re-summarizes nothing and
    writes nothing — end-to-end patchability.
  - **Verified:** `sovereign-test.sh --filter code_intel` — full workspace compiles
    with `treesitter`, **20/0** (the four slices built without per-slice feedback,
    correct first pass).
- **2026-06-25 — Step 1 (production wiring): written.** CLI verb `sovereign enrich
  code-intel <corpus>` (`sovereign-cli-llm/src/enrich_cmd/code_intel.rs`) — resolves
  the corpus, builds daemon-backed embed+chat closures the same way `enrich extract`
  does (`EnrichConfig::require` → `DaemonInferenceClient::into_closures`), calls
  `run_code_intel_for_corpus`. That wrapper now resolves `source_root` from
  `_corpus_meta.json` internally (CLI passes only corpus_dir + id). Registered in the
  `enrich` dispatcher + help. **Inc 1 is now runnable end-to-end:** `sovereign enrich
  code-intel commonwealth-ai` summarizes every function + indexes the summaries
  (needs an enrich config via `enrich init`, and the daemon up). Unverified pending
  the end-of-initiative test run (the build-tests-at-end preference).
  - **Next: Inc 2 (code-trace augmentation) — seam mapped 2026-06-25.** Key finding:
    the chat answer path is **single-shot retrieve→synthesize** (no agentic tool-loop),
    so Inc 2 is a **deterministic evidence augmentation**, not a tool-loop. Injection
    point: `runtime/handlers/knowledge_query.rs::format_scored_chunks_with_kinds`
    (~:787) — after retrieval formats the evidence block, append a "Call-graph trace:"
    section for code-corpus hits, before the synthesis prompt is assembled (~:813).
    Slices:
    - **2a (self-contained, testable):** a trace builder in
      `corpus-engine/.../code_intel/trace.rs` — given a `ScipGraph` + a symbol name,
      return a structured trace (callers via `find_callers`, callees via
      `find_callees_qualified` filtering the §6 type-ref noise). Orientation rules from
      the hardening: surface the common-ancestor entry (callee→caller), never trust
      retrieval rank-1. Testable with an in-memory `ScipGraph`.
    - **2b (runtime wiring):** add `corpus-engine-scip` to `sovereign-core`
      (feature-gated), detect code-corpus hits via `CorpusKind::Code` (kinds map
      already built in the KQ pipeline), map each hit's `ScoredChunk` symbol metadata →
      open the corpus's `scip_graph.db` → call 2a → format + append to the evidence
      block at the injection point, budget-bounded.
    - **Wrinkles:** (i) confirm `ScoredChunk` exposes `symbol_name`+`file_path`
      post-retrieval (else deref via `chunk_id`); (ii) `commonwealth-ai` index is
      `kind:"knowledge"` despite having `scip_graph.db` — decide whether code-intel
      summaries imply `CorpusKind::Code` or detect code by `scip_graph.db` presence.
  - **2026-06-25 — Inc 2 slice 2a (trace builder): written, unverified.**
    `corpus-engine/.../code_intel/trace.rs` — `build_symbol_trace(scip, symbol,
    qualified_name)` returns a `SymbolTrace` (callers via `find_callers`, callees via
    `find_callees_qualified`, deduped + capped), and `render_trace` emits a compact
    "Call-graph trace:" block that flags trait/dyn dispatch boundaries. Two in-memory
    `ScipGraph` tests. Registered. Remaining Inc-2 work is **2b (runtime wiring)** —
    the integration that adds `corpus-engine-scip` to `sovereign-core` + injects the
    rendered trace into the evidence block at the `knowledge_query.rs` injection point.
  - **2026-06-25 — Inc 2 slice 2b (runtime wiring): built.** The chat runtime now
    augments synthesis evidence with a call-graph trace for code-intel hits.
    - **Dependency-architecture correction (the load-bearing decision).** Slice 2a's
      `trace.rs` was wrongly homed in `corpus-engine` behind the `treesitter` feature.
      Wiring it from there would have dragged the **5 tree-sitter grammar crates into
      every `sovereign-core` build** — i.e. the daemon, desktop, and CLI — purely to
      *read* a graph. But reading the call graph is SQL over `scip_graph.db`
      (`find_callers` / `find_callees_qualified`); it needs **zero** grammars. So the
      trace builder MOVED to the lean `corpus-engine-scip` crate
      (`corpus_engine_scip::trace`, re-exported `build_symbol_trace` / `render_trace` /
      `SymbolTrace` / `CallSite`), and `sovereign-core` depends on **that crate
      directly** — never on `corpus-engine/treesitter`. Net every-build cost: a SQLite
      reader + `prost`, no grammars. The grammars stay confined to the indexing path
      that WRITES the db (CLI `enrich`/`code index` + the daemon watcher). This is the
      read/write split made concrete — the read model must not depend on the write
      model's parser. (Manifests already document the same split: corpus-engine's
      `stores` vs `treesitter` features, carved out 2026-05-22 for exactly this reason.)
    - **Both wrinkles resolved — better than the spec's two options.** (i) A retrieved
      `ScoredChunk.metadata` is parsed straight from the chunk's JSON metadata column
      (`index/search.rs:271`), so a code-intel summary chunk already carries
      `source` / `symbol` / `qualified_name` — no `chunk_id` deref needed. (ii) Detect a
      code hit **per-chunk** via `metadata["source"] == "code_intel_summary"` (the
      marker `store::insert_chunk_for` stamps). Precise — only actual summary chunks get
      traced — and independent of whether the corpus is tagged `CorpusKind::Code`
      (`commonwealth-ai` is `kind:"knowledge"` yet has a graph), which was the open
      question. No corpus-level `scip_graph.db`-presence heuristic required.
    - **New module `sovereign-core/src/runtime/code_trace.rs`.**
      `build_code_trace_block(chunks)` distills the distinct code hits (deduped by
      qualified name, capped at 3 — the §5 "never trust rank-1" finding: trace the top
      few as evidence, don't bet on chunk #1), groups by corpus so each `scip_graph.db`
      opens once, builds + renders each symbol's trace, and returns one evidence block
      (empty string — zero overhead — for the common non-code corpus). Best-effort: a
      missing/corrupt db or an edge-less symbol yields no trace and never disturbs the
      answer. Glassbox `tracing::info!` when it fires (symbol count). 4 unit tests on the
      pure `code_hits` extraction + the empty-fast-path.
    - **Injection point (spec ref corrected).** The formatter
      `format_scored_chunks_with_kinds` actually lives in `runtime/formatters.rs:74`
      (the spec said `knowledge_query.rs`); it is *called* from
      `knowledge_query.rs:806`. The trace block is appended to `doc_context` at that
      async call site (where the corpus dir is resolvable + we can `.await`), not inside
      the pure sync formatter. `scip_graph.db` path =
      `<data>/indexes/<corpus_id>/scip_graph.db`, the one canonical layout the daemon
      writer and atlas reader already use.
    - **Verification (full workspace suite, 2026-06-25): 7000 pass, my 2b code green.**
      The only 2 reds were **pre-existing**, from a peer commit `5972b635` ("talk to
      code wired up", same day) that added a `dim_mismatch_disclosure` retrieval step
      but left `kq_and_deep_share_head_and_core` asserting the old count (18 vs actual
      19) and `docs/retrieval-pipeline.md` un-regenerated. Reconciled both (assertion →
      19/20 + slice indices; doc regenerated via `UPDATE_RETRIEVAL_PIPELINE_DOC=1`). Not
      code-intel work, but they blocked the green gate.
    - **Adjacent committed work — `5972b635` is COMPLEMENTARY, not a collision.** Its
      `presenter.rs` adds `present_answer` / `looks_like_phantom_tool_call`: Qwen3.6
      *reflexively* emits phantom tool calls (`:code_search(...)`, `:symbols(...)`,
      `:callers(...)`, `:callees(...)`, `<tool_code>`) for code questions even though
      chat wires no executable tools, so it strips them and falls back honestly. That is
      the OUTPUT-sanitize layer; Inc 2 is the INPUT-evidence layer (it hands the model
      the real call-graph trace so it can answer for real instead of phantom-calling).
      Different layers, they compose — and the phantom-call reflex is independent
      evidence the model wants exactly the SCIP navigation Inc 2 supplies. (A future
      iteration could close the loop: when a phantom `:callers(X)` is detected, resolve
      it against `scip_graph.db` and answer it for real rather than just stripping it.)
  - **2026-06-25 — Inc 3: live grade on `commonwealth-ai` (388 MB SCIP graph).** The
    real-corpus run flushed out two SCIP-data bugs, both fixed at the source (the
    user's "fix the input" steer), then graded the §4 answer-key.
    - **Input fix 1 — exporter `line_end` was an end-COLUMN, not end-line.**
      `scip_export.rs` read `occ.range[2]`; for a single-line *name* occurrence that
      is the end column, so every function's `line_end` was garbage (`gate_answer`
      end=31). Body extraction was broken for *every* consumer — code-intel AND
      `symbol_lookup::read_symbol_body` (same `line_end.max(line_start)` clamp → a
      1-line body). Fixed to use `occ.enclosing_range` (the whole-definition span;
      the caller-scope path already did). Re-exported via `project refresh --local`
      (in-process `export_all` — applies the fix without a daemon rebuild; the
      default refresh nudges the daemon = old code). **Verified:** `gate_answer`
      230→494, ground-truthed against its definition at source line 231.
    - **Input fix 2 — `kind` is unreliable** (rust-analyzer labels enums/modules but
      leaves Rust functions `unknown`/`trait`). Enumerate functions from the
      **call-graph caller-set** instead (`ScipGraph::caller_qualified_names` —
      `DISTINCT refs.caller_qualified`): the precise "has a body and calls things"
      signal. 234 real fns in the §4 scope vs 2,094 under a kind screen. Added a
      `--files=` substring scope to the enrich verb.
    - **Enriched 279 functions** across the 5 §4 inference files (`fast`/Qwopus-4B for
      throughput — `primary`/35B at 84-165 s/call would be hours), 0 failures.
    - **Retrieval-bridge grade** (`chat inspect --corpus commonwealth-ai`, scoped, no
      LLM): **neighborhood 5/5, exact rank-1 0/5, exact in top-12 1/5.** Every
      plain-English question retrieved the correct code *region* (streaming / slot /
      mesh / grounding clusters); the precise answer-key symbol was rarely rank-1.
      This **confirms the thesis** — "retrieve a neighborhood anchor → walk the
      call-graph → disambiguate; never trust rank-1." For Q1 the #8 hit
      (`handle_message_stream`) *calls* the target; for Q5 the #1 hit
      (`gate_held_answer`) is the *#1 caller* of `gate_answer` — one trace hop closes
      the gap. Trace substrate validated: `callers(gate_answer)` = the rubric's 8
      sites incl. `gate_held_answer@streaming.rs:279`, verbatim.
    - **End-to-end in live chat — the bridge works, the trace has a wiring gap.**
      `sovereign chat ask` runs the runtime in-process (so it *does* use the fresh
      index + my code, no daemon restart). Two phrasings, two lessons:
      - *"In this codebase, where is X"* → router picks **`MetalingualQuery`**
        (canned "no code corpus indexed" reply, no retrieval). The meta phrasing
        defeats it.
      - *"How does answer gating decide whether to show or withhold a reply…"* →
        router picks **`DeepQuery`** (REASONING, conf 1.00) → retrieval surfaces
        **`gate_held_answer`'s code-intel summary @ rank 5** → the 35B synthesizes an
        accurate, code-aware answer naming `gate_held_answer` with a
        `[Source: gate_held_answer]` citation. **Inc 1's bridge is validated through
        the full chat.**
      - But `code_trace` *still* didn't fire — because code questions route to
        **DeepQuery**, whose evidence is built at `retrieval.rs:3416`, while my Inc-2
        injection sits only at the **KnowledgeQuery** site (`knowledge_query.rs:815`).
        `Metalingual` (`metalingual.rs:181`) is also uncovered. The injection must be
        added at those async call sites (it can't hoist into the *sync*
        `format_scored_chunks_with_kinds`).
    - **Verdict:** the *mechanism* is validated end-to-end on a real codebase — the
      intent-summary bridge reaches the right neighborhood and produces a correct,
      cited answer in live chat; the call-graph trace data is exact. Three follow-ups
      for a shippable feature: (i) **extend the `code_trace` injection to the
      DeepQuery/Metalingual evidence sites** (`retrieval.rs:3416`,
      `metalingual.rs:181`), not just KnowledgeQuery; (ii) **router** — reliably send
      code questions to a retrieval path (clean phrasing already routes to DeepQuery;
      "in this codebase" mis-routes to Metalingual); (iii) **summary model** —
      `fast`/4B reached the neighborhood, a stronger model would sharpen exact-rank
      precision, and the caller-set enumeration should drop `*_tests` (Q4 surfaced
      test functions).
  - **2026-06-25 — Inc 4: trace on all paths + a first-class CODE route.**
    - **Fix #1 — `code_trace` on every retrieval path.** The Inc-2 injection lived
      only on the KnowledgeQuery evidence site; code questions route to DeepQuery
      (and sometimes Metalingual), whose evidence is built at `retrieval.rs:3416`
      and `metalingual.rs` — so the trace never fired there. Added
      `build_code_trace_block` at both (the sync formatter can't host an async
      call, so it goes at each async evidence site). **Verified live:** a
      knowledge-phrased gate question routed to DeepQuery, the trace fired, and the
      35B answer quoted *"the resolved call-graph trace"* and named
      `gate_held_answer`'s callers (`stream_knowledge_query_turn`@958,
      `stream_deep_query_turn`@1700) with line numbers.
    - **`Intent::CodeQuery` — a first-class route.** Per the user's steer (the
      router is where over-rotation bites, so keep it tight): new enum variant +
      `parse_intent` mapping + 12 **code-structural** exemplars in
      `router/exemplars.toml` ("what calls X", "where is Y implemented", "trace the
      flow", "call graph" — phrased to NOT steal general "how does X work" asks) +
      `turn.rs` dispatch → `handle_code_query` + the ~7 exhaustive `match intent`
      sites (compiler-enforced). `handlers/code_query.rs` is purely additive: it
      detects code corpora by on-disk `scip_graph.db` (robust — `commonwealth-ai`
      is `knowledge`-kind yet has a graph), narrows retrieval to them (kills the
      33-corpus dilution measured in Inc 3), and delegates to the knowledge path
      (reusing retrieval + the trace + synthesis). No code corpus installed ⇒ falls
      straight through to the knowledge path, so non-code deployments are unchanged.
    - **Over-rotation gate — `bench all --routing-only`: clean.** 6 pre-existing
      misroutes, all confusions *between existing intents*
      (deep↔comparison↔knowledge↔metalingual) vs. 16–42-day-old baselines; **zero
      routed to `code_query`.** Since k-NN only reclassifies when a code exemplar is
      the nearest neighbour, and none became one, the code cluster provably stole no
      non-code query — the guard you asked for held.
    - **Summary preference — the fix that makes the bridge + trace engage.** First
      cut: scoping to code corpora replaced cross-corpus dilution with a
      *within*-corpus one — the 279 summaries lost to tens of thousands of raw code
      chunks, so the bridge/trace didn't fire (gate summary fell to ~#11). A `score`
      boost did nothing: **`cross_corpus_sort_cmp` sorts by `vector_distance`, not
      `score`** (falls back to score only when both distances are `None`). The fix
      (`reweight_by_query_relevance`) pulls a code-intel summary's `vector_distance`
      toward the query (×0.6) — and boosts `score` ×3 for the score-based gates.
      Self-gating (only lifts a summary that already matches) and a no-op for
      non-code corpora. (`chat inspect` won't show it — it skips the reweight step;
      verify via full `chat ask` provenance.)
    - **Result — the headline §4 demo, live.** "Where is answer gating implemented,
      and what calls it?" → CodeQuery → 4 code corpora → **summaries rank #1
      (`gate_held_answer`) and #3 (`gate_answer`)**, above the raw chunks → the trace
      fires for both → the 35B answers BOTH halves with cited call-graph facts:
      *"Who calls it: `gate_held_answer` ← `stream_knowledge_query_turn`,
      `stream_deep_query_turn`; `gate_answer` ← `handle_complex_task`,
      `handle_knowledge_query`, `handle_simple`, `run_post_stream_refinement`,
      `gate_attached_doc_answer` [Source: Call-graph trace]"* — matching the §4
      answer-key. Plain-English → summary-bridge → call-graph-trace → cited answer,
      end-to-end.
    - **Polish (done 2026-06-25):**
      - **Code synthesis prompt** — `CODE_SYNTHESIS_DIRECTIVE` appended to the
        synthesis system prompt on CodeQuery turns (both route sites);
        `handle_code_query` passes `CodeQuery` so the branch fires. The answer now
        names callers at file:line and explains the architecture
        (`gate_held_answer` streaming gate → delegates to `gate_answer` engine;
        callers match the §4 key: `handle_knowledge_query`@1229, `handle_simple`@201,
        `handle_complex_task`@321, `gate_attached_doc_answer`@665).
      - **Drop `*_tests` from the caller-set + prune** — `mod tests` fns sit under a
        `/tests/` SCIP path segment; the pass partitions them out of enrichment AND
        deletes any prior summary for them (`symbol_source_key` refactored to key off
        `SymbolMeta`). Re-enrich: 279 → **260** (19 test summaries pruned, non-test
        all cache hits). Real-fn summaries (`gate_held_answer`/`gate_answer`) now
        cleanly rank #1/#2.
    - **Still open (not blockers):** stronger summary model for exact-rank precision
      (35B is too slow to re-enrich and there's no mid tier); broader tuning of the
      0.6 distance factor across more queries (works for the gate Q; routing bench
      clean; the boost is self-gating + a no-op for non-code corpora).

---

## 6. Gotchas & infra notes (will bite a fresh session)

- **The chaos/breaker `--spawn` reuses a PREBUILT `target/debug/sovereign-desktop`
  and does NOT rebuild.** Always `cargo build -p sovereign-desktop` (stop the
  daemon first to dodge the watcher's cargo lock) **before** any chat
  verification, and confirm the binary mtime is newer than your edits. This
  silently invalidated a batch of runs in this session.
- **Call-graph CLI param is `--symbol`** (e.g. `sovereign tools call callers
  --symbol=gate_answer`); `symbols` uses `--name`. The inconsistency silently
  no-ops queries — worth aligning, and the tool-loop must use `--symbol`.
- **SCIP staleness:** the call-graph ingest goes stale when toggled off during
  dev. `sovereign refresh <project>` rebuilds it (~213s for `sovereign`, 883K
  edges). `symbols` can resolve while edges are empty — confirm `callers`
  returns before trusting a run.
- **`callees` output is noisy** (surfaces type refs + stdlib — confirmed live:
  `callees(select_route)` interleaves `CompletionRequest`/`Result`/`Vec`/
  `RouteDecision` with real calls); the narration layer must filter it.
- **The SCIP graph double-lists some files under two path prefixes** (`crates/…`
  *and* `sovereign/crates/…` both appeared in `callees(select_route)`); the
  tool-loop's dedup must canonicalize paths or it will double-count edges.
- **Build call-graph adjacency from `callers`, not `callees`.** `callers` is clean
  (only functions call functions) and complete; `callees` is noisy (type-refs) *and*
  truncated ("…and 158 more") — it will silently drop edges.
- **PPR-over-SCIP orientation matters (proven, §3.3).** Seeded at retrieval anchors,
  *forward* (caller→callee) PageRank flows mass to leaf callees and buries the entry
  symbol; *undirected* (centrality) or *reversed* (callee→caller, ancestry) promotes
  the seam/entry. Default undirected; use reversed to surface the common ancestor
  above the retrieved implementation details.
- **`code_search` (dev CLI tool) is broken/scoped to `oicp-types`** — not the
  chat's retrieval; ignore it for this work.

---

## 7. Open questions

- **Cost/scale — largely answered (§3.4).** Key summaries by body `content_hash`,
  ride `reindex_file`; per-commit cost ≈ number of changed function *bodies*. Still
  open: cheaper summary model; RAPTOR parent-node re-aggregation cadence.
- **Storage home — DECIDED (2026-06-25): Atlas code-ontology** (Q2), with RAPTOR as
  the summary generator + PPR-over-SCIP retrieval (§3.3). **But patchability (§3.4)
  forces a sub-decision the Atlas-as-built doesn't support: per-symbol atom
  upsert/evict** — atoms.json is a monolith today. Recommended path B: emit the
  fast-patching intent-summaries on the incremental chunk path, reserve the Atlas for
  slow-moving structure. Confirm before Inc 1.
- **Narration quality** is where the demo lives (Inc 2) — the model must follow
  the *right* edges and stop honestly at boundaries. Grade hard vs §4.
- **Retrieval blend — ANSWERED (2026-06-25, §5 hardening).** `summary+asks` (the
  Atlas atom shape) is the most robust signal — `summary` and `asks` are
  complementary (each alone misses a different question; combined = top-3 on all three
  solvable Qs at 172-scale). Index both, retrieve over the union. **New sub-finding:
  near-duplicate flooding** — many near-identical summaries (e.g. `embed*` across
  crates) cluster and can crowd retrieval for a unique correct answer; needs a
  dedup/diversity step, or rely on the call-graph to route past it. Plus
  **directional-inverse collisions** (X→Y vs Y→X are cosine-near) — another reason
  deep/leaf answers should come from graph traversal, not retrieval alone.
