# The code-intel package boundary

`code-intel/` holds the **code intelligence package** — the stack a third party
could lift out of this monorepo and run against any host that speaks an
OpenAI-compatible HTTP surface. For that to stay true, the package must never
reach back into the monolith. This document is the contract;
`cargo run -p xtask -- boundary-gate` enforces it in CI (blocking).

It is the second package to be cut this way. The first is `studio/`
([studio/BOUNDARY.md](../studio/BOUNDARY.md)), and everything structural here —
the two tiers, the shared-leaf budget, the dev/build-dependency rule — is that
document's pattern applied a second time. Where the two differ, the difference
is noted.

Per `ARCH_PRINCIPLES.md §1.1` this file is a contract: every claim below is
verifiable against the code on the commit it appears in. Measurements are dated.

---

## 1. What the package is

Two entry points over one substrate.

**For a person:** *point it at a codebase and a spec; get back every place they
disagree, pinned to a file and a line.* No spec, and it tells you what the code
actually does — cited, in plain English. The product description is
[CHECK_CODE_AGAINST_SPEC.md](./CHECK_CODE_AGAINST_SPEC.md); the one-command
entry is `code map <path> [--spec <file>]`.

**For an agent:** an MCP server exposing exact symbol lookup, compiler-resolved
call graphs, a deterministic tree-sitter fact base, live lint/test status,
durable cross-session notes, and index-health posture — so a coding agent stops
guessing from grep. The design record is
[CODE_INTEL_CHAT.md](../sovereign/docs/specs/CODE_INTEL_CHAT.md).

Both run on the same four layers:

| Layer | What it is | Determinism |
|---|---|---|
| Call graph | SCIP, ingested from any standard indexer | exact, compiler-resolved |
| Fact base | tree-sitter facts in a per-file-keyed SQLite store | exact, cited |
| Summaries | intent-forced per-symbol purpose + questions | LLM, body-hash cached |
| Adjudication | claim-to-capability verdicts | LLM, evidence-gated |

The bottom two carry a line number or they do not ship. The top two are labeled
as the softer answer in every report that prints them
(CHECK_CODE_AGAINST_SPEC "Two kinds of answer, honestly labeled").

**The LLM layers ship.** They are the product, not an internal convenience, and
they are already injectable: `corpus-engine/src/enrichment/code_intel/mod.rs`
takes a `ChatCompletionFn` and knows nothing about where it points. The prior
posture recorded in `sovereign/SYSTEM_OVERVIEW.md` §4 — "the LLM-bound layers
stay mesh-side" — was a statement about which gates run in *this repo's public
CI*, and is not a distribution constraint. Update that sentence when Phase 6
lands so the two documents do not disagree.

---

## 2. The two tiers

**Package crates** (`code-intel/crates/`):

| Crate | Role | Source today | LOC |
|---|---|---|---|
| `yield-hook` | The cooperative foreground-yield contract — one trait, zero deps | `corpus-engine-yield` | 110 |
| `scip-graph` | SCIP call-graph store, per-language exporter dispatch, the read-only trace builder, capability map, arch metrics | `corpus-engine-scip` | 7,093 |
| `code-facts` | The deterministic tree-sitter fact base + fact store + claim checks | `corpus-engine/src/{facts,facts_check,facts_store}.rs` | 1,710 |
| `code-notes` | NoteStore + project-docs index | `corpus-engine-notes` | 8,981 |
| `code-watchers` | Lint/test watchers, their SQLite result stores, the coordinator + heartbeat | `corpus-engine-watchers` | 3,294 |
| `code-archaeology` | Git history mining, rough-edge surfacing, provenance eval | `corpus-engine-archaeology` | 2,514 |
| `code-enrich` | Intent-forced symbol summarisation, incremental pass, body-hash cache | `corpus-engine/src/enrichment/code_intel/` | 1,776 |
| `code-tools` | The `Tool` implementations + registry assembly + MCP surface | `sovereign-tools/src/code/` minus §4 | 16,550 |
| `code-intel-cli` | `code map`, `check-spec`, `spec-intel`, `spec-reconcile`, `capability-doc`, `capability-reconcile`, the MCP server entry | `sovereign-cli-dev/src/code_*.rs` + `sovereign-cli-llm/src/enrich_cmd/{spec_*,capability_*,code_intel}.rs` | 8,734 |

Package total: **~51k LOC**, about 6% of the repo's ~865k Rust (measured
2026-07-27).

**Shared leaves** (repo root, unchanged — these are the same leaves `studio/`
pins):

| Crate | Allowed internal deps |
|---|---|
| `oicp-types` | *(none)* — the wire vocabulary, a pure leaf |
| `sovereign-contracts` | `oicp-types` only |
| `oicp-client` | `sovereign-contracts`, `oicp-types` |

`sovereign-contracts` is where `Tool`, `ToolDescriptor`, `ToolResult`, `Error`,
`ToolRegistry`, `types` and `slot_policy` actually live;
`sovereign-core/src/lib.rs:56-65` re-exports them verbatim. Every
`use sovereign_core::{error,types,traits}` in the package is that re-export, not
a dependency on the runtime hub — see §6.

---

## 3. The rules

1. **A package crate may depend only on other package crates + the shared
   leaves.** No `corpus-engine`, `sovereign-core`, `sovereign-tools`,
   `sovereign-mesh`, `commonwealth-*`, `sovereign-atos`. Where the package needs
   something the monolith owns — chunk search, atlas atoms — it declares a trait
   and the monolith injects an impl at the call site (§6).
2. **The shared leaves keep the budget in the table above.** Widening a leaf
   widens both packages' contract surface at once, so the gate pins them by
   hand. This is the same `allowed_leaf_deps` table `studio/` uses; do not fork
   it.
3. **No `build.rs` in any package crate, and no `include_str!` /
   `include_bytes!` that escapes the crate root.** A build script or an escaping
   embed is a source-tree reach-in no boundary survives. The package's one baked
   asset — the intent-forced enrichment prompt
   (`prompts/symbol_enrichment_system.md`) — lives inside `code-enrich` and is
   loaded through the existing on-disk-overridable path.
4. **The read model does not depend on the write model's parser.** Reading a
   call graph is SQL over `scip_graph.db` and needs zero grammars; only the
   indexing path that *writes* the db links tree-sitter. This rule was learned
   the expensive way — see CODE_INTEL_CHAT.md's Inc-2 build log, where
   `trace.rs` was moved out of `corpus-engine` because wiring it from behind the
   `treesitter` feature would have pulled five grammar crates into every daemon,
   desktop and CLI build purely to read a graph. `code-facts` and `code-enrich`
   link grammars; `scip-graph` and `code-tools` must not.
5. **No required host.** Every LLM-bound path takes an injected completion
   function and a base URL. The package must run to completion against a bare
   Ollama (§5). A path that only works against the Sovereign daemon is a bug in
   this package, not a feature of that daemon.

The rules count **dev- and build-dependencies too**: a crate a third party lifts
carries its tests and its build scripts.

---

## 4. Deliberately excluded

Two things that look like they belong and do not.

**The eight ATOS tools** — `provision_feature`, `archive_feature`,
`record_atos_event`, `atos_plan_emit`, `atos_utils`, `atos_verify`,
`write_redteam_finding`, `spec` (2,287 LOC in `sovereign-tools/src/code/`).
ATOS is an opt-in experiment behind a Cargo feature with zero default-build
consumers (`SYSTEM_OVERVIEW.md` §2). It is already a separate project living in
the tree; it does not become part of this one by adjacency.

**`sovereign-work-atlas`** — the cross-agent scope-claim surface. It depends on
`commonwealth-state`, `commonwealth-core` *and* `sovereign-core`, so lifting it
drags the mesh in, and its value (peer coordination) only exists on a mesh
anyway. Excluded from v1. It is the natural first addition if the package ever
grows a fleet story, and the seam for that is `declare_scope`/`work_in_flight`
becoming a trait the package declares and a host satisfies — the same shape as
§5.

Also out of scope for v1, for a different reason: `cache-audit` and
`session frames` (`sovereign-cli`). They are agent-harness telemetry, not code
intelligence — a different dependency profile, a different audience, and a
transcript format owned by a third party. They belong in their own package or
none.

---

## 5. The host contract — standalone, Ollama, and the daemon

The package is designed to be **strictly better inside this system without ever
requiring it.** That is a contract, not an aspiration, and it has three rules.

### 5.1 The baseline every host must satisfy

```
POST {base_url}/v1/chat/completions      (streaming optional)
POST {base_url}/v1/embeddings
GET  {base_url}/v1/models
```

That is the whole requirement. Ollama, LM Studio, `llama-server`, vLLM, TGI and
any hosted OpenAI-compatible API all satisfy it unmodified. `--base-url` is the
only mandatory flag on the LLM-bound verbs. Your code and your spec never leave
whatever machine you pointed it at.

Note the symmetry already present in this repo: the daemon serves an
**Ollama-native shim** (`/api/{chat,generate,tags,…}`, `routes_ollama.rs`) so
Ollama clients can talk to Sovereign. This rule is the other direction — the
package talks to Ollama. Both directions work, and neither is privileged.

### 5.2 What a richer host adds, and how the package finds out

Enhancement is **detected, never configured**. The package probes
`GET /oicp/v1/capabilities`; a host that answers gets the enhanced path, a host
that 404s gets the baseline. No flag, no profile, no "sovereign mode."

| Capability advertised | What the package does with it | Baseline fallback |
|---|---|---|
| Multiple slots (fast / primary / code) | Bulk symbol summarisation on the fast slot, adjudication on the primary. Measured 2026-06-25: 279 functions enriched on a 4B fast slot; the same pass on a 35B primary at 84–165 s/call is hours | One model for both; print the estimate before starting |
| Live watcher | Read lint/test status from the running coordinator; the tree-sitter overlay keeps symbol defs fresh in milliseconds | Run the configured lint/test command in-process on demand |
| Mesh peers | Fan bulk enrichment out to peers, the way collaborative corpus ingest already does | Local only |
| Runtime + retrieval | Plain-English questions over the code corpus — the summary bridge plus call-graph trace, cited (CODE_INTEL_CHAT §4) | Tools and reports, no conversation |
| Corpus host (`CorpusHost`, §6.1) | Raw-chunk `code_search` over LanceDB + Tantivy, and `brief`'s atlas-atom section | Summary-index search only; both tools state the absence rather than returning empty |
| Shared note store | Notes and session frames replicate across machines | Local SQLite |

### 5.3 The three rules that keep this a community and not a lock-in

1. **Declare capability, do not name a vendor.** The package asks "is there a
   fast slot?", never "is this Sovereign?". OICP is the vocabulary for that
   question, and it is a shared leaf precisely so both sides can speak it
   without either depending on the other.
2. **Every enhancement states its absence.** Degradation is printed, never
   silent. The existing precedent is `watcher.live` — the watchers already
   report when their own answer is orphaned rather than letting a days-old run
   masquerade as current, and every enhanced path here owes the user the same
   sentence. This is `ARCH_PRINCIPLES` glassbox applied across a boundary.
3. **The daemon consumes the package the way a stranger does** — through the
   published crates and the same OICP surface. No private back-channel, no
   in-tree shortcut, no `#[cfg(sovereign)]`. If the daemon ever needs a path the
   package does not expose, that is a missing part of the public surface, and
   the fix is to expose it. This is the rule that keeps the standalone honest,
   and it is the same discipline `studio/`'s `run_workflow_with_provider`
   already runs on: the host injects its provider, the package does not reach
   for one.

The synergy is real and it is worth having — a fast slot turns a multi-hour
enrichment into minutes, and the mesh turns it into a fan-out. But the shape of
the win is *the same package, better fed*. Nothing in the package's behaviour
forks on who is feeding it.

---

## 6. The measured boundary today

Measured 2026-07-27 against `sovereign-tools/src/code/` (18,837 LOC, 48 tool
files). These are the edges the gate will fail on, in the order they should be
resolved.

**The `sovereign-core` edge is a re-export, not a dependency.** 128 imports of
`sovereign_core::{error, types, traits}` across the module, all of which resolve
through `sovereign-core/src/lib.rs:56-65` to `sovereign-contracts` — already a
shared leaf. Repointing them is mechanical and changes no behaviour.

**The `corpus-engine` edge is nine imports and two methods.** In full:

| Import | Files | Resolution |
|---|---|---|
| `corpus_engine::CorpusEngine` | `callers`, `callees`, `symbol_lookup`, `recent_changes`, `code_search`, `mod` | Trait, §6.1 — only `open_index` (2 call sites) and `installed_indexes` (1) are ever called |
| `corpus_engine::index::CorpusIndex` | `mod`, `dry_report` | Trait, §6.1 |
| `corpus_engine::facts_store::FactStore` | `facts_tool` | Moves with `code-facts` (Phase 2) |
| `corpus_engine::enrichment::atlas::{read_atlas_atoms, AtomEnvelope}` | `brief` | Trait, §6.1 |
| `corpus_engine::Error as CorpusError` | `mod` | Maps to `sovereign_contracts::Error` |

**The other five crates have no edge at all.** `corpus-engine-scip`,
`-notes`, `-watchers`, `-archaeology` and `-yield` depend on nothing beyond
rusqlite, prost, notify, and ordinary utility crates. Roughly 22k of the 51k
package is already extracted and is simply misnamed — the `corpus-engine-`
prefix is carve-out history, and none of those crates has anything to do with
corpora.

### 6.1 The seam — two indexes, and only one of them comes along

The load-bearing decision in this whole package. There are **two vector indexes
in play and they are different problems**; conflating them is what makes the
extraction look impossible.

| | Summary index | Raw-chunk index |
|---|---|---|
| Contents | One row per function: the intent-forced summary + ASKS | Every chunk of source text |
| Size (this repo, measured 2026-06-25 / 2026-07-25) | 279 rows for the 5-file inference subsystem, 260 after test pruning — one per function | **41,691 chunks** for `commonwealth-ai` |
| Who needs it | The conceptual→symbol bridge. CODE_INTEL_CHAT's entire thesis | `code_search` |
| Backing | Anything. A flat cosine scan is milliseconds at this scale | LanceDB IVF-PQ + Tantivy FTS, 6,956 LOC in `corpus-engine/src/index/` |
| Verdict | **The package owns it** | **Host-only** |

The summary index is the one that matters and it is *tiny* — a 5,000-function
codebase is 5,000 vectors. A brute-force cosine scan over that is single-digit
milliseconds, and the repo already accepts exactly this tradeoff elsewhere: the
RAPTOR grounding path falls back to a brute-force `conv_raptor_nodes` cosine
scan when its ANN index is absent (`SYSTEM_OVERVIEW.md` §3).

Raw-chunk `code_search` is the weakest tool in the set regardless —
CODE_INTEL_CHAT §6 records it as "broken/scoped to `oicp-types`… ignore it for
this work," and Inc 4 found that scoping to code corpora made raw chunks *drown*
the summaries until `reweight_by_query_relevance` pulled the summaries back up.
Dropping it standalone costs little and removes the only reason to care about
LanceDB.

**Embeddings were never the problem.** `code_intel/store.rs` already takes an
injected `EmbedFn` — the same injection contract `corpus-engine` has always had
(`SYSTEM_OVERVIEW.md` §3, "corpus-engine never embeds or generates text
itself"). The package declares `EmbedFn`; the host satisfies it from
`POST /v1/embeddings`, which is in the §5.1 baseline. Zero work.

So: **two traits**, one owned and one optional.

```rust
/// The summary index. The PACKAGE owns this — it ships a default
/// SQLite-backed flat-cosine impl. corpus-engine also satisfies it in-tree
/// (mapping onto chunks.lance) so the daemon keeps exactly one index.
///
/// Surface derived by reading enrichment/code_intel/store.rs::index_one —
/// these four are every call it makes.
pub trait SymbolIndex: Send + Sync {
    /// Content hashes already committed under this symbol key (the delta gate:
    /// an unchanged body skips both the embed and the write).
    async fn committed_hashes_for(&self, key: &str) -> Result<Vec<String>>;
    async fn remove(&self, key: &str) -> Result<()>;
    async fn upsert(&self, key: &str, entry: SummaryEntry, embedding: Vec<f32>) -> Result<()>;
    async fn search(&self, embedding: &[f32], k: usize) -> Result<Vec<Hit>>;
}

/// Everything the package can only get from a corpus host: raw-chunk search,
/// the installed-corpus list, atlas atoms. OPTIONAL — absent standalone.
/// Satisfied in-tree by corpus-engine; `open_index` and `installed_indexes`
/// are the only two CorpusEngine methods the tools ever call.
pub trait CorpusHost: Send + Sync {
    fn installed_indexes(&self) -> Vec<IndexInfo>;
    fn open_index(&self, corpus_id: &str) -> Result<Box<dyn ChunkSearch>>;
    fn read_atoms(&self, corpus_id: &str) -> Result<Vec<AtomEnvelope>>;
}
```

`CorpusHost` being `Option` rather than required is what makes rule 5 (§3)
hold: absent it, `code_search` and `brief`'s atom section are unavailable and
**say so** — they do not silently return nothing. `read_atoms` serves `brief`
alone.

### 6.2 Leaf summaries vs RAPTOR — one shipped, one never ran

These are routinely conflated and the difference decides what Phase 4 has to
carry. Both are "summaries over code"; only one exists.

**Per-symbol leaf summaries: shipped, demoed, and live on this machine.**
`enrich code-intel <corpus>` generates the intent-forced summary + ASKS for each
function, embeds it, and upserts it into the index under
`codeintel:<qualified_name>` marked `source = "code_intel_summary"`. Measured
2026-07-27: `~/.sovereign/indexes/commonwealth-ai/code_intel_cache.json` holds
**335 entries** — grown since the 279-then-260 recorded in CODE_INTEL_CHAT's
Inc-3/Inc-4 log. This is the artifact behind the Inc-4 demo, where *"Where is
answer gating implemented, and what calls it?"* put `gate_held_answer` at rank 1
and `gate_answer` at rank 3 above the raw chunks, fired the call-graph trace on
both, and produced the cited answer matching the §4 answer key. It is
`code-enrich` in §2 and it is **in the package**.

**RAPTOR — the hierarchical multi-resolution tree — has never been run on code.**
Verified three ways on 2026-07-27, because a two-directory grep is not evidence:

| Check | Result |
|---|---|
| `SELECT corpus_id, COUNT(*) FROM conv_raptor_nodes GROUP BY corpus_id` | conversations-anthropic, obsidian-vault, three watched folders, chaos-secret-agent. **No code corpus** |
| `find ~/.sovereign/indexes -name raptor_summaries.lance` | `chaos-secret-agent`, `sep`, one sep backup. **No code corpus** |
| Repo-wide grep, RAPTOR × code terms | Every hit is design intent in CODE_INTEL_CHAT §3.3, a contrast comment in `code_intel/store.rs:5`, or a spec listing the four enrichment systems side by side. **No implementation** |

So v1 ports what exists (flat leaf summaries) and owes RAPTOR nothing.

**And when RAPTOR is built for code it is a new builder, not a port — for a
measured reason, not a stylistic one.** `enrich raptor` is corpus-agnostic;
nothing ever stopped anyone pointing it at `commonwealth-ai`. What stops it is
that the output would be the artifact CODE_INTEL_CHAT already proved fails:
experiment #4 auto-generated a summary with the *default* prompt and it **lost**
the retrieval bridge at 0.443, writing jargon ("model routing strategy, ranked
peer candidates"); the same model under the intent-forced user-vocabulary prompt
**won at 0.718**. Stock RAPTOR's cluster-summary prompt is exactly that default.
§3.3 therefore calls for two adaptations — swap in the intent-forced prompt, and
"drive the hierarchy off SCIP/module structure, not embedding clusters (code's
real hierarchy is the call/module tree)."

The existing `sovereign-tools/src/raptor_atlas.rs` (1,389 LOC) k-means-clusters
embeddings, so it is the wrong algorithm on both counts. A structure-driven
builder walks the module tree and needs three things, all of which the package
already has or declares:

| Needs | Source |
|---|---|
| The module/call hierarchy | `scip-graph` — in the package |
| An LLM to summarise each node | injected `ChatCompletionFn` — §5.1 baseline |
| Somewhere to put the nodes | `SymbolIndex` — package-owned, one extra row kind |

For reference if any of the existing code is reused: `index/raptor.rs` (579 LOC)
is a storage primitive over arrow + lancedb + `crate::error` — portable but only
if the package took LanceDB, which §6.1 says it should not. `raptor_atlas.rs`'s
only real coupling is `corpus_engine::enrichment::state` (checkpointing); its
`sovereign_core::*` imports are the §6 contracts re-export again.

---

## 7. Phase queue

The gate goes first. It fails loudly on day one against roughly 130 edges, and
that failing list *is* the work queue — the same way `studio/`'s did.

| Phase | Work | Shape |
|---|---|---|
| 0 | Generalise `boundary_gate.rs` from one `PACKAGE_SET` to N named packages; register this package's set with an empty allowlist | ~50 LOC in `corpus-engine/xtask/src/boundary_gate.rs` |
| 1 | Repoint 128 imports `sovereign_core::{error,types,traits}` → `sovereign_contracts::` | Mechanical; no behaviour change; drops the runtime-hub edge entirely |
| 2 | Move `facts.rs` + `facts_check.rs` + `facts_store.rs` (1,710 LOC) into `code-facts`; resolve `crate::{error,types}` against contracts | Straight move; carries the `lang_packs` grammar table |
| 3 | Define `SymbolIndex` + `CorpusHost` (§6.1); write the package's default SQLite flat-cosine `SymbolIndex`; implement both over `corpus-engine` in-tree so the daemon keeps one index | The only real design work in the plan |
| 4 | Move `enrichment/code_intel/` (1,776 LOC) behind `SymbolIndex`. `store.rs::index_one` calls exactly `committed_chunks_for_doc`, `delete_chunks_by_source_doc`, `insert_batch` — a mechanical retarget once Phase 3 exists | Small, and blocked only on Phase 3 |
| 5 | Regroup under `code-intel/crates/`; rename off the `corpus-engine-` prefix | Cosmetic, but the prefix actively misleads |
| 6 | `code-intel-cli`: `code map` as the standalone entry, `--base-url`, OICP capability probe (§5.2), MCP server mode | The deliverable |

Phases 0–3 are the extraction and none is a rewrite. Phases 4–6 are the product.
Phase 1 alone is worth doing regardless of whether the package ever ships: it
removes a false dependency on the largest hub crate in the workspace.

---

## 8. When the gate fails

It names the offending edge (`crate → dep`) or file. Either the dependency
doesn't belong in the package — move the code that needs it monolith-side and
inject through a seam (a trait in `sovereign-contracts`, or a `CorpusLocator`
impl) — or, if a leaf genuinely must grow, widen `allowed_leaf_deps` in
`corpus-engine/xtask/src/boundary_gate.rs` deliberately, with the table in §2
updated to match. Widening a leaf widens `studio/` too; check that document
before you do.

---

## 9. Open items

- **Crate names are unverified against crates.io.** `scip-graph`, `code-facts`,
  `code-notes` and friends are the intent; availability is unchecked. Resolve
  before Phase 5, not after.
- **Fidelity is uneven across languages and the reports say so.** The SCIP
  exporter dispatch covers rust, go, typescript and python
  (`scip_export.rs`); the deterministic fact base
  (`lang_packs`) reads **rust and python** today. A new language is a grammar
  plus a few tree-sitter queries — CHECK_CODE_AGAINST_SPEC calls it a
  well-scoped afternoon — and the one judgment call is how that language spells
  "a typed value built with named fields."
- **`SYSTEM_OVERVIEW.md` §4 must be updated** when Phase 6 lands, to retire the
  "LLM-bound layers stay mesh-side" sentence (§1).
- **`read_atoms` may not deserve to be on the trait** — one caller (`brief`).
  Decide in Phase 3.
- **The package's default `SymbolIndex` backing is undecided.** Flat cosine over
  SQLite BLOBs is the obvious floor and is provably adequate at the sizes in
  §6.1; `sqlite-vec` is the obvious next rung if a user points this at something
  much larger than a monorepo. Whichever is chosen, **measure and record the
  scale at which it stops being adequate** — an unstated ceiling is how a
  brute-force scan becomes a mystery latency bug two years later. The in-tree
  impl over `chunks.lance` has no such ceiling and is the escape hatch.
- **Code RAPTOR is unbuilt** (§6.2). It is not a v1 obligation, and when built
  it is a structure-driven builder over the SCIP module tree, not a port of the
  embedding-clustering `raptor_atlas.rs`. If it lands *before* this extraction,
  it must be written against `SymbolIndex` from the start or it will re-create
  the coupling Phase 4 removes.
