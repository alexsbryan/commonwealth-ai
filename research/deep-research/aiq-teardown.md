# AIQ teardown, DRB-II harness cross-check, and control-arm design

Order deep-research-t6g (drafted/approved 2026-08-19), operator directives 726feee2
("absolutely do a teardown of AIQ") and 88237c19 (DRB-II re-target). Read-only
due-diligence; no code lands from this order.

Sources: the NVIDIA AI-Q Blueprint repository
(https://github.com/NVIDIA-AI-Blueprints/aiq), cloned 2026-08-19, commit
`4b0b931cb35bfcd23fb147190991b1212ebb7a25` (v2.2.0, pyproject.toml), all file
citations below are paths in that clone; the DRB-II leaderboard
(https://agentresearchlab.org/benchmarks/deepresearch-bench-ii/index.html,
fetched 2026-08-19); the DRB-II repository
(https://github.com/imlrz/DeepResearch-Bench-II, fetched 2026-08-19); the DRB-I
repository (https://github.com/Ayanami0730/deep_research_bench, fetched
2026-08-19); and this repo's own score reports and notes (cited in place).

## BLUF

AIQ is a genuinely different architecture from our loop in exactly the three
places the DRB-II leaderboard rewards: it decomposes every task into
independent research queries and runs them **concurrently** (up to 6 workers,
up to 20 queries, up to 100 source-tool calls per job), it **separates the
writer** (a no-search-tools synthesis stage over persisted plans and notes),
and it spends large-model tokens (Nemotron-3-Ultra 550B, or GPT Sol/Luna) on
every stage. Its grounding is **citation-identity only** — URL whitelisting
against a per-session source registry with five match strategies, plus
sanitization. There is **no content-level verification, no two-source floor,
no custody stamping** — nothing that checks whether the cited source actually
supports the claim. That is the single biggest architecture contrast with our
loop, and it is the whole of the "no recall tax": AIQ passes every single-origin
claim its model decides to pass, and the DRB-II rubric judge (GPT-5.5, score
{1,0,-1} with blocked-reference penalties) is the only honesty gate in the
system.

Critical-path answer (item 2), verified from source: **AIQ's NAT eval covers
DRB-I only (RACE + FACT, arXiv 2506.11763). There is no DRB-II support
anywhere in the repository** — zero hits for "InfoRecall", "rubric" (in the
eval sense), or the DRB-II paper id 2601.08536. NAT is a generic
workflow-runner; for DRB the scoring happens in the official DRB-I repo's
evaluator, outside AIQ. The rubric implementation and data pipeline are
therefore **not** candidates for reuse as our DRB-II scorer instrument. What
is reusable is NAT as the report-generation side of a control arm: its llm
blocks are endpoint-agnostic (`base_url` is configurable on `_type: nim` and
the `_type: openai` block; `embed_base_url` likewise), so it can be pointed at
our daemon's OpenAI-compatible endpoint with a config change, not a code
change.

On the leaderboard: nvidia-aiq (Nemotron 3, Opus 4.6) scores **TotalScore
54.50 — InfoRecall 49.23, Analysis 61.55, Presentation 93.15**, rank 9 of 17,
the open-source entry sitting inside the closed-source leader cluster
(54–64) and well above o3 (45.40) and Gemini-3-Pro (44.60). Perplexity
Research (38.58; InfoRecall 33.05) is rank 16. Ranking is driven by
InfoRecall and Analysis; Presentation is saturated (74.59–94.77). The
InfoRecall dimension is precisely the dimension where our corroboration floor
imposes a measured tax (the R-12 leg: gap sets grow because single-origin
audits cap at could-not-judge — 0/12 in both t6b and t6c reports). The
control-arm design in item 4 pre-registers the decomposition of that tax.

---

## 1. TEARDOWN — how AIQ actually works

### 1.1 Platform stack

AIQ is a Python/uv multi-agent research backend built on the **NVIDIA NeMo
Agent Toolkit (NAT)** (`nvidia-nat*==1.8.0`, pyproject.toml lines 34-38),
**LangChain Deep Agents** (`deepagents`), and **LangGraph** for the
orchestration state machine. The deep-research path runs on a Dask cluster
with an async job API (`docs/source/architecture/data-flow.md`: submit →
RUNNING → SUCCESS/FAILURE/INTERRUPTED, SSE streaming, reconnection via event
IDs). Frontends: CLI, web UI, REST API, and a standalone MCP server
(`mcp/`, `aiq_mcp` package, FastMCP over Streamable HTTP).

The three NAT-adjacent things worth separating for a teardown:

- **NAT** — the agent toolkit: YAML workflow configs, LLM blocks, function
  registration, the `nat eval` harness, the FastAPI frontend, telemetry.
- **AIQ's own agents** (`src/aiq_agent/agents/`) — intent classifier,
  clarifier, shallow researcher, deep researcher (orchestrator + subagents +
  researcher workers), report rewriter, plus the citation-verification and
  sandbox machinery.
- **The knowledge layer** (`sources/knowledge_layer/`) — pluggable document
  ingestion/retrieval.

### 1.2 Workflow composition — one classifier call decides the depth

Every query enters through the intent classifier
(`docs/source/architecture/agents/intent-classifier.md`): a single LLM call
that returns `{intent: meta|research, meta_response?, depth: shallow|deep}`.
Meta → direct answer and END. Research/shallow → the shallow researcher (one
bounded tool-calling loop, `max_llm_turns` 10-20, `max_tool_iterations` 5,
synthesis anchor when the budget is exhausted). Research/deep → clarifier
(gathers missing context and output-shape preferences, max 3 turns), then the
deep researcher. Shallow escalation ("unable to find", "need more research")
routes back through the clarifier, gated by `enable_escalation`
(`docs/source/architecture/overview.md` §Routing logic; the LangGraph graph
is `chat_researcher/agent.py`).

Our analog: the gap ledger and depth decisions in our loop are artifacts of
the compass/verdict machinery, not a separate classifier call. AIQ's depth
decision is a single model judgment on one call, made without any gap
evidence — there is no feedback from "the first round failed to cover" into
"escalate to deep". Adopt-later: a depth-ratchet (shallow → deep on a
could-not-judge-dense first round) is a cheap addition to our loop and does
not exist in AIQ either — both systems leave escalation to a model's
self-assessment.

### 1.3 The deep-research pipeline — the part that wins the leaderboard

Canonical description: `docs/source/architecture/agents/deep-researcher.md`.
Five phases:

1. **Advisory source routing** (`enable_source_router`, default true): the
   `source-router-agent` subagent picks one domain route from a data-only
   domain catalog (`configs/domain_catalogs/deep_research_domain_catalog.yml`)
   and writes `/shared/source_routing.json`. It does not research. The
   per-request `data_sources` list is a hard filter on registry-mapped tools.
2. **Planning**: the `planner-agent` subagent does bounded discovery (calls
   search tools only enough to shape the plan) and returns a structured
   `ResearchPlan` — answer strategy, required components, constraints, and
   self-contained `ResearchQuery` objects with `preferred_tools` /
   `fallback_tools` (prompt guidance only; workers stay bound to the full
   filtered tool set). Persisted to `/shared/plan.json`.
3. **Concurrent evidence collection**: the orchestrator calls
   `run_research_batch` with the plan's queries; each query is executed by a
   **reusable researcher worker** (a LangChain runnable, NOT a `task()`
   subagent) concurrently, capped by `max_research_concurrency` (default 6).
   Each worker returns structured `ResearchNotes` — findings with `source_ids`
   into a per-note `sources` list, `target_components`, and an
   `evidence_judgment` {0-100 usefulness score, confidence, rationale}.
   Persisted as `/shared/research_note_*.json`.
4. **Writer-first synthesis**: the `writer-agent` subagent reads the plan,
   all research notes, and the compact `get_verified_sources` output; it has
   **no source-search tools and performs no new research**; it writes the
   final answer to `/shared/output.md`. The runtime accepts the output only
   when its bytes match the digest recorded after the writer's commit —
   otherwise it fails closed with `writer_output_not_committed` (one bounded
   corrective turn).
5. **Citation verification + sanitization** (Phase 5, below).

The two design decisions that matter for the recall dimension:

- **Per-sub-question dispatch.** The planner decomposes; independent queries
  run concurrently with dedicated workers. No worker competes with other
  questions for context, and each query's notes carry their own source list
  and evidence judgment. This is the structural answer to the "breadth"
  problem our loop handles with round-budgeted serial search.
- **The budget is large and per-job.** `resource_limits` defaults: up to 20
  research queries, 100 source-tool calls per job, plus per-request,
  graph-time, plan, report, shared-state, note, todo ceilings
  (`docs/source/architecture/agents/deep-researcher.md` §Configuration).
  Compare our loop: the t6c battery's round-1 allowance is on the order of
  tens of search calls (see `arms/runs-t6c/` gap traces) — the whole-loop
  search budget is smaller than AIQ's per-job source-call ceiling, and AIQ
  pays for it with Tavily/NVIDIA API spend that our order explicitly does not
  have.

### 1.4 Retrieval and embedding choices

- **Web search**: Tavily (`_type: tavily_web_search`), `max_results` 2-5,
  `max_content_length` 1000, optional `advanced_search: true` — the shipped
  profiles pair a 5-result web search with a 2-result advanced search. Tool
  schema is shallow: query + result count + content length; no site
  allowlists, no domain filtering beyond the data-source registry's tool
  mapping.
- **Paper search**: `_type: paper_search`, Serper API key, `max_results` 5 —
  optional and commented out in every shipped profile except the DRB eval
  config (`configs/config_cli_default.yml`; DRB config has it enabled with
  `SERPER_API_KEY`).
- **Knowledge layer** (`sources/knowledge_layer/KNOWLEDGE-LAYER-SETUP.md`):
  pluggable backends — LlamaIndex + ChromaDB (local), NVIDIA Foundational RAG
  (hosted Milvus), Azure AI Search, OpenSearch. Embedding model:
  `nvidia/nemotron-3-embed-1b` by default, hosted at
  `https://integrate.api.nvidia.com/v1` (`AIQ_EMBED_BASE_URL` overridable
  "for local NIM" — the doc's own words). Default retrieval `top_k: 5`
  (frontier profile, `configs/config_frontier_models.yml`). Text-only
  ingestion by default; image extraction optional via VLM.
- **Source registry**: `SourceRegistryMiddleware` records URLs/citation keys
  returned by source tools into a per-session registry; `get_verified_sources`
  serves it to the writer as the citation whitelist
  (`src/aiq_agent/common/citation_verification.py`).

Contrast with our estate seam: AIQ's captured sources are a **per-session
registry, never persisted** — nothing compounds across runs, there is no
corpus, no custody class, no offline enrichment. The estate mechanism
(SPINE #3) has no AIQ analog at all. Adopt: the source-capture discipline
(every tool result contributes URL + title + type to a registry that the
writer must cite from). Skip: their retrieval depth (top_k 5, no reranking,
no corroboration-aware selection — the knowledge-search results enter the
window as-is).

### 1.5 Tool schemas

Shipped function types (`configs/`, `docs/source/customization/configuration-reference.md`):

| Tool | Config | Notes |
|---|---|---|
| `tavily_web_search` | max_results, max_content_length, advanced_search, api_base_url | api_base_url exists for proxies — one more pointer that this stack is endpoint-agnostic |
| `paper_search` | max_results, serper_api_key | scholar search via Serper |
| `knowledge_retrieval` | backend, collection_name, top_k (5), generate_summary, summary_model, chroma_dir, embed model | LlamaIndex/Azure/OpenSearch/RAG backends |
| `execute` (sandbox) | provider: Modal or OpenShell | one physical sandbox per deep-research job; policy-attested (OpenShell); durable artifacts (charts/CSVs) harvested with sha256 metadata; only generated code and job-workspace files cross the sandbox boundary — inference, source tools, credentials stay in-process |
| `think`, filesystem (`ls`/`read_file`/`write_file`/`edit_file`), `get_verified_sources`, `run_research_batch` | — | DeepAgents plumbing |

Notable absences for our purposes: **no search-now / fetch-pages affordance**
(the workers consume tool results directly; there is no separate
page-fetch-and-enter-window step — `max_content_length: 1000` on web search
means the window is snippets), **no gap ledger**, **no estate write path**,
**no blocked-reference or custody concept**. The sandboxed execution is real
engineering (attestation, artifact capture, MIME-from-bytes spoof rejection,
SVG sanitization) and is the strongest single piece of reusable machinery for
our estate/analysis ambitions — adopt-later, as the analysis-tooling seam
(visualization skills in AIQ depend on it).

### 1.6 The writer separation

The writer is the normative synthesis stage with four structural properties
(`src/aiq_agent/agents/deep_researcher/prompts/writer.j2`):

1. No source tools, no `task()` — it cannot go back out to research.
2. It reads the plan, every research-note file, and the verified-sources
   whitelist before drafting; a missing note must be named as a gap in the
   answer.
3. `evidence_judgment` scores from the notes drive synthesis: high-score
   notes are anchors, medium support/nuance, low-score notes are "mainly for
   gaps, caveats, conflicts, or clearly labeled weak evidence".
4. Citations are mandatory per material claim and must come from the
   whitelist; the final `## Sources` section lists each source on its own
   line; the runtime fails closed if the committed output does not match the
   writer's digest.

This is close in spirit to our R8 synthesis contract, with two deltas: (a)
the evidence judgment is **model-generated per note** (a 0-100 usefulness
score with a confidence) rather than our deterministic verdict pipeline — a
soft signal where we have a hard one; (b) the gap-naming obligation exists
("If evidence is thin, state the limitation in the final answer") but is a
**prompt-level instruction**, not a gate — nothing verifies that the named
gap was real, that the limitation was stated, or that a claim without support
was not passed. That is the difference between their honesty and ours:
theirs is instructed, ours is enforced (compass verdicts, custody veto,
witness downgrade, corroboration floor — `notes/corroboration.md`).

### 1.7 Grounding and citation — the whole contrast with our corroboration floor

`verify_citations()` (`src/aiq_agent/common/citation_verification.py`, 1554
lines) does exactly four things, all deterministic:

1. **Source capture**: URLs and citation keys from tool results → per-session
   `SourceRegistry`.
2. **Identity matching** of each report citation against the registry, five
   strategies: exact (raw/normalized), truncation (report URL is a prefix of
   exactly one registry URL), prefix, child-path, query-subset.
3. **Removal** of unmatched citations with an audit reason recorded; a report
   whose citations are all removed raises `CitationIntegrityError`
   (citation_integrity_lost) — fail-closed on zero verified citations.
4. **Sanitization**: shortened/garbled/IP/non-HTTP URLs removed; references
   renumbered. Knowledge-layer citations (`report.pdf, p.15`) match citation
   keys with lenient page-number comparison.

What it does **not** do — the list that matters for the comparison:

- No content check: nothing verifies that the cited source supports the
  claim it is attached to. The verifier checks the address, not the payload.
- No corroboration: a claim cited from a single page passes, five copies of
  one page pass.
- No custody: nothing stamps provenance at fetch, no refusal path for
  unknown-origin support.
- No witness: no specificity check (all-absent → could-not-judge), no
  negation/contradiction handling.
- No verdicts: there is no could-not-judge concept anywhere in the
  pipeline — a claim is either written by the model (and cited from the
  whitelist) or silently dropped. Absence is not reported; the writer's
  prompt says to state limitations, and nothing checks that it did.

Every one of these missing pieces is load-bearing in our loop: the witness
downgrade and the corroboration floor are exactly the gates whose cost shows
up in the R-12 leg (gap sets grow because single-origin audits cap at
could-not-judge — t6b bars; "growth mechanism identified — the strict-shape
re-draft spelled figures as words, invisible to the audit judge's citation
extraction (40/40 abstained)" — t6c landing note, git log 8457cf6e). AIQ has
**no recall tax by construction**: it cannot fail a claim for lack of a
second origin, because it never asks. The DRB-II rubric judge is the only
honesty gate, applied externally and only to the 132-task sample.

Verdict: **adopt the registry/whitelist discipline** (writer cites only from
captured sources — this is a cheap structural upgrade to our render step),
**skip their verification semantics** (identity-only), **keep our floor** —
and measure the tax (item 4).

### 1.8 Budgets, resource limits, observability

- `resource_limits`: hard ceilings per job — max 20 research queries, 100
  source-tool calls, plus per-request/graph-time/plan/report/note/todo
  bounds; "configurable downward only" — an honesty property (ceilings can
  only tighten) we don't have and could copy in the flight-recorder charter.
- Observability: SSE `IntermediateStepEvent` stream (llm/tool/job/artifact
  events, replay via event IDs), Phoenix tracing, Weave project tracking, a
  tokenomics post-eval module that attributes cost per phase
  (`src/aiq_agent/tokenomics/` — timing-window attribution over NAT traces).
  Their glassbox is event-shaped, ours is artifact-shaped (the artifact
  spine); both are real.

### 1.9 The eval harness (NAT)

`nat eval --config_file <yml>` runs the configured workflow over a dataset
and writes `workflow_output.json` (`docs/source/evaluation/`). The eval
config block is: `eval.general.{workflow_alias, output_dir, max_concurrency,
dataset}` plus optional `eval.evaluators.<name>` — custom evaluators are
registered as `nat.plugins` entry points (the FreshQA evaluator is one,
`frontends/benchmarks/freshqa/src/evaluator.py`, an LLM-judge implementing
FreshEval Relaxed; DeepSearch QA uses the official DeepMind LLM-as-judge
methodology with precision/recall/F1). For DRB the eval config carries **no
evaluator block**: reports are exported (`export_drb_jsonl.py`) and scored by
the official DRB repo's evaluator. So NAT's role in the DRB pipeline is
report generation; all scoring is external.

---

## 2. Harness cross-check (critical path) — does NAT cover DRB-II?

**No.** Verified from the cloned source at 4b0b931:

- The only research-bench eval in the repo is DRB-I: `docs/source/evaluation/benchmarks/deep-research-bench.md` and `frontends/benchmarks/deepresearch_bench/README.md` both name RACE (Comprehensiveness/Insight/Instruction-Following/Readability) and FACT (Effective Citations, Citation Accuracy), paper arXiv 2506.11763, 100 tasks (50 EN / 50 ZH).
- `grep` across all .py/.md/.yml/.toml for `inforecall`/`info_recall` → 0 hits; for the DRB-II paper id `2601.08536` → 0 hits; "rubric" appears only as DeepAgents RubricMiddleware state (`src/aiq_agent/agents/deep_researcher/models/state.py:55,67`), not evaluation.
- The dataset download script (`frontends/benchmarks/deepresearch_bench/scripts/download_drb_dataset.py`) pulls DRB-I's query/reference/criteria files from `Ayanami0730/deep_research_bench` and builds a 100-question `drb_full_dataset.json` (question/expected_output per task).
- The official DRB-I repo is DRB-I-only by its own statement: its README points to DRB-II as a separate project (`imlrz/DeepResearch-Bench-II`, arXiv 2601.08536) and notes DRB-I "will continue to be maintained and updated" — the InfoRecall/Analysis/Presentation rubrics appear there only in a news pointer, not in code.

Consequences for the DRB-II window's instrument:

1. **The rubric implementation and data pipeline are not candidates for reuse or cross-validation of our own DRB-II scorer** — they do not exist in AIQ. Our scorer path stays `research/deep-research/drb/drb-score.py` and the DRB-II scorer work the window needs.
2. **NAT is still the right report-generation harness for the control arm**: it runs any workflow over a json dataset and exports outputs; the DRB config's dataset schema (question_key/answer_key/generated_answer_key) is exactly the shape a DRB-II task sample would take.
3. The DRB-II project itself is where the rubric machinery lives: 132 expert tasks, 9,430 binary rubrics, judge GPT-5.5, `score ∈ {1, 0, -1}` (1 = rubric satisfied with valid evidence and no blocked references; 0 = not mentioned; -1 = mentioned but evidence relies on explicitly blocked references), `run_evaluation.py` + `aggregate_scores.py` producing per-dimension CSVs. That is the external instrument our DRB-II scorer window cross-validates against (read-only reference, same-questions plan in item 4).

**Endpoint constraint check** (the order's "not worth continuing" condition): AIQ's harness CAN be pointed at a non-NVIDIA OpenAI-compatible endpoint. The llm blocks take `base_url` explicitly (`_type: nim` blocks carry `base_url: https://integrate.api.nvidia.com/v1` in every config; `_type: openai` blocks take api_key/model_name and the configuration reference documents `base_url` for NIM endpoints); the knowledge layer's `AIQ_EMBED_BASE_URL` is documented "for local NIM"; Tavily search takes `api_base_url`. Nothing in the config layer is hardwired to NVIDIA. The control arm proceeds.

---

## 3. Lesson-mining — where the leaderboard delta actually is

DRB-II leaderboard (fetched 2026-08-19 from
https://agentresearchlab.org/benchmarks/deepresearch-bench-ii/index.html;
TotalScore = weighted rubric pass rates, 9,430 rubrics, 132 tasks):

| Model | InfoRecall | Analysis | Presentation | TotalScore |
|---|---|---|---|---|
| AI21-DeepResearch | 60.35 | 71.00 | 92.89 | 64.38 |
| Dalpha | 58.62 | 61.36 | 93.41 | 61.01 |
| WhaleCloud-DocChain | 57.20 | 64.91 | 92.59 | 60.94 |
| iFlow-Researcher | 54.99 | 69.54 | 92.56 | 59.91 |
| Xiaoyi 6.0 | 53.05 | 69.90 | 91.12 | 58.72 |
| **nvidia-aiq (Nemotron 3, Opus 4.6)** | **49.23** | **61.55** | **93.15** | **54.50** (rank 9) |
| OpenAI-GPT-o3 DR | 39.98 | 49.85 | 89.16 | 45.40 |
| Gemini-3-Pro DR | 39.09 | 48.94 | 91.85 | 44.60 |
| Gemini-2.5-Pro DR | 34.91 | 51.91 | 90.24 | 41.98 |
| Qwen3-Max DR | 34.18 | 48.04 | 74.59 | 39.25 |
| Grok Deep Search | 33.52 | 42.50 | 91.42 | 39.23 |
| Perplexity Research | 33.05 | 44.47 | 79.34 | 38.58 (rank 16) |

Reads:

1. **The ranking is the InfoRecall ranking.** Presentation is saturated
   (74.59–94.77, most entries > 90) and Analysis is mid (35.89–71.00), so
   the 22.95→60.35 spread in InfoRecall decides everything. The dimension's
   rubric definition ("identify, retrieve, and cross-check all key
   information needed to answer the task") is a retrieval-breadth measure —
   which is exactly the axis our loop's R-12 leg and the corroboration floor
   constrain.
2. **nvidia-aiq is a top-half system with an open recipe**: its 49.23
   InfoRecall is 1.5× Perplexity's and beats every frontier-lab entry on the
   board — o3 (39.98), Gemini-3-Pro (39.09), Gemini-2.5-Pro (34.91),
   Qwen3-Max (34.18), Grok (33.52). The recipe per section 1.3:
   per-sub-question concurrent dispatch, ~100 source-call budget, writer
   separation, and a 550B-parameter agent model. All three are things our
   loop does differently (serial rounds, round-budgeted search, in-loop
   synthesis), and none of them is a scorer trick — they are structural
   breadth decisions.
3. **Perplexity's 33.05 InfoRecall is the "no floor tax" reference point**:
   Perplexity also has no corroboration floor, and still lands 16 points
   below AIQ and 27 below the leader. The floor cannot be the whole
   explanation for recall gaps; dispatch structure and retrieval depth are.
   The control arm's floor-off comparison (item 4) is what decomposes the
   tax from the breadth gap.
4. **DRB-II's honesty mechanism is external and rubric-shaped**: blocked
   references score -1, not-mentioned scores 0. A system with no internal
   honesty machinery can still do well on Analysis/Presentation (both are
   saturated) — the check that AIQ's system would fail is InfoRecall's
   "cross-check" phrasing, where our floor's single-origin caps are the same
   phenomenon measured from inside the loop.

Mapped to our seams:

- **t6f gap closure**: AIQ's planner does a bounded discovery pass (search
  only until the plan is stable) and each researcher is told to "stop when
  you can answer confidently" — the stop rule is model-judgment. Their gap
  loop is internal to a single worker turn; ours is the cross-round gap
  ledger. The AIQ delta is the **initial decomposition**, not the loop: a
  task-level decomposition into 20 self-contained queries is where their
  breadth comes from. Adopt-later: planner-shaped decomposition feeding our
  gap ledger (the gap texts are already search-shaped per t6f's frozen bar).
- **Estate assembly**: AIQ persists nothing across runs (per-session source
  registry only). The estate remains a structural advantage with no AIQ
  analog; their source-capture discipline (URL + title + type on every tool
  result) is the one pattern to copy into the estate write path.
- **The recall tax**: AIQ's no-floor stance buys them unblocked single-origin
  claims but costs them nothing on the leaderboard because DRB-II never
  checks internal honesty — the -1 blocked-reference penalty is the closest
  thing, and it applies to citations, not claims. Our floor buys claim-level
  honesty at a measured recall cost (R-12 0/12 in t6b and t6c). The right
  response is not to remove the floor but to know its magnitude — item 4's
  decomposition.

---

## 4. Control-arm design (pre-registered; no flights from this order)

The control arm is the answer to "same questions, same models, different
harnesses — where does their harness beat ours and vice versa, and how much
of the recall gap does the floor explain?"

### 4.1 Design

- **Harness A (theirs)**: AIQ's shipped workflow, run through `nat eval`
  with the llm blocks pointed at our daemon's OpenAI-compatible endpoint
  (`base_url: http://127.0.0.1:9741/v1`, `api_key: <local>`) — the frontier
  profile shape (`configs/config_frontier_models.yml`) or the Nemotron
  profile shape, model names swapped for our pinned local models (the
  daemon's pinned deep-research draft model per `research/deep-research/drb/README.md`
  §The judge pin). This is a config change only — no AIQ code changes.
- **Harness B (ours)**: our loop as flown by the t6b/t6c arms
  (`arms/run-arms.sh`), same inputs.
- **Questions**: (a) our frozen bank v1 (the report-class question + 16
  coverage keys, per SPINE.md) and the frozen t2b DRB subset
  (`drb/query.subset.jsonl`, 10 English tasks); (b) a DRB-II task sample —
  drawn content-blind from `tasks_and_rubrics.jsonl` in the DRB-II repo
  (132 tasks, 9,430 rubrics; CC BY 4.0 / CC0 / CC BY-NC 4.0 — the two
  NC-licensed tasks excluded, idx 26 and 110, per the repo's license fields).
- **Scorers**: our scorer (`drb/drb-score.py` family) on both arms'
  outputs; the DRB-II rubric judge (GPT-5.5 pipeline) on both arms' outputs
  for the sample; the t6b/t6c coverage/density instrumentation on both.
- **Same models**: each arm runs with the same model configuration —
  Harness A's worker/orchestrator llm mapped to our local pin, Harness B as
  flown. The point is harness delta, not model delta.

### 4.2 Pre-registered outputs (the four numbers the window needs)

1. **Per-dimension delta** on the DRB-II sample: InfoRecall/Analysis/
   Presentation pass rates for Harness A vs Harness B under the same rubric
   judge — where their harness beats ours (expected: InfoRecall, via
   concurrent dispatch and the 100-call budget) and where ours beats theirs
   (expected: claim-level verification shows up as rubric -1/0 differences
   on blocked/unsupported references, and as coverage of the "cross-check"
   phrasing).
2. **Recall-tax decomposition on bank v1**: Harness A's claims scored by our
   scorer twice — floor ON (as-is) and floor OFF (the scorer's floor is a
   pure function of the recorded windows + evidence ids; `notes/corroboration.md`
   §Golden regeneration shows the re-scoring is deterministic, never a
   re-run). The tax = coverage(floor OFF) − coverage(floor ON) for Harness
   A's outputs, versus the same decomposition for Harness B — i.e. how much
   of each harness's recall gap the two-origin rule explains. This is the
   number that answers "should the floor move" with data instead of
   conviction. (The floor-off comparison is a named instrument toggle,
   journaled per §18.6, never silent.)
3. **Where the harnesses agree/disagree**: claim-level agreement matrix on
   the DRB-II sample (rubric pass / fail / not-mentioned vs our four
   verdicts) — the honest-vs-fluent axis made measurable.
4. **Cost/throughput ledger**: source-tool calls, tokens, wall time per arm
   on the same questions — the "is their breadth affordable" number, which
   decides whether the concurrent-dispatch adoption is even in our budget
   envelope.

### 4.3 Pre-registration discipline

The design above is fixed before any run; the sample selection (content-blind
seed), the floor-toggle journal entry, the scorer invocation, and the delta
tables are all registered in `adversarial/pre-registration.md` style before
execution. Execution is a follow-up order; this order changes no instruments
and no bars.

---

## 5. Integration surface report (the §19 inventory answer)

**Adopt (cheap, high-value, no dependency questions):**

- The **citation whitelist discipline**: writer cites only from captured
  sources; `get_verified_sources` as a render-stage input. A structural
  upgrade to our render step, independent of the floor.
- The **source-capture record shape**: URL + title + source_type + tool_name
  per source entry (`citation_verification.py` SourceEntry) — the natural
  wire format into our estate's stamped corpus.
- The **"configurable downward only"** resource-limit rule — an honesty
  property for the flight-recorder charter.
- The **write-path digest proof** (`writer_output_not_committed` fails
  closed) — a byte-exactness discipline our artifact spine could adopt for
  the report boundary.

**Adopt-later (needs an owner, a budget, or a follow-up order):**

- **Concurrent per-sub-question dispatch** (max 6 workers, 20 queries, 100
  calls) — the InfoRecall-relevant structural delta; gated on the control
  arm's cost ledger (4.2.4). This is the t6f gap-closure seam's complement:
  decomposition breadth first, then gap-targeted rounds.
- **Sandboxed execution** (OpenShell/Modal with attestation, artifact
  capture, spoof rejection) — the analysis-tooling seam (charts, tables,
  verification runs); the strongest single piece of engineering in the repo.
- **Scholar search** (Serper-based paper search) — one tool schema, low
  cost, directly reusable for the estate's academic intake.
- **Planner-shaped decomposition**: the bounded-discovery planner contract
  (plan.json structure: answer strategy, required components, constraints,
  queries with target_components) as a schema for our plan boundary.

**Skip (do not port):**

- **Their verification semantics** (URL-identity only) — we have strictly
  stronger machinery; adopting theirs would be a downgrade.
- **Cloud storage backends** (Azure AI Search, hosted Milvus, cloud artifact
  stores) — the estate runs locally.
- **The NVIDIA API dependency** — nothing in the config layer requires it
  (base_url is configurable), and our daemon replaces it.
- **The missing honesty machinery** — no gap ledger, no custody, no
  witness, no floor, absence not reported. These are the features our loop
  exists to have; AIQ's lack of them is the teardown's headline finding, not
  a feature.
- **The per-session-only source registry** — a step backward from the
  compounding estate.

**Not worth continuing check (order condition)**: false — the harness is
endpoint-agnostic (section 2). The order continues; this document is the
deliverable.

---

## Sources

- AIQ repository: https://github.com/NVIDIA-AI-Blueprints/aiq, clone commit
  4b0b931cb35bfcd23fb147190991b1212ebb7a25 (2026-08-19), v2.2.0. Cited files:
  `docs/source/architecture/{overview,data-flow}.md`,
  `docs/source/architecture/agents/{deep-researcher,intent-classifier,shallow-researcher,clarifier,sandbox}.md`,
  `docs/source/evaluation/benchmarks/{deep-research-bench,freshqa,deepsearch-qa}.md`,
  `docs/source/customization/{configuration-reference,knowledge-layer}.md`,
  `configs/{config_cli_default.yml,config_frontier_models.yml,config_domain_routing_and_skills.yml}`,
  `configs/domain_catalogs/deep_research_domain_catalog.yml`,
  `frontends/benchmarks/deepresearch_bench/{README.md,configs/config_deep_research_bench.yml,scripts/download_drb_dataset.py,scripts/export_drb_jsonl.py}`,
  `frontends/benchmarks/freshqa/{configs/config_shallow_research_only.yml,src/evaluator.py}`,
  `src/aiq_agent/common/citation_verification.py`,
  `src/aiq_agent/agents/deep_researcher/{README.md,factory.py,models/state.py,prompts/{orchestrator,planner,researcher,writer}.j2}`,
  `src/aiq_agent/agents/chat_researcher/{agent.py,nodes/context_aware_intent_router.py}`,
  `sources/knowledge_layer/{README.md,KNOWLEDGE-LAYER-SETUP.md,src/llamaindex/adapter.py}`,
  `src/aiq_agent/tokenomics/README.md`, `mcp/README.md`, `pyproject.toml`.
- DRB-II leaderboard: https://agentresearchlab.org/benchmarks/deepresearch-bench-ii/index.html (fetched 2026-08-19).
- DRB-II repo: https://github.com/imlrz/DeepResearch-Bench-II (fetched 2026-08-19) — rubric definitions, judge pipeline, arXiv 2601.08536.
- DRB-I repo: https://github.com/Ayanami0730/deep_research_bench (fetched 2026-08-19) — RACE/FACT, arXiv 2506.11763.
- Our loop's measured position: `research/deep-research/arms/score-report-t6b.json` (P4-v0 70/72, P4-v1 13/16, P3 12/13, R-12 0/12, loop density 1.0) and `arms/score-report-t6c.json` (P4-v0 68/72, P4-v1 11/16, P3 12/13, R-12-nongrow 0/12, loop density 0.797), both scored 2026-08-14 by `score-arms.py`.
- Corroboration floor: `research/deep-research/notes/corroboration.md`; spine mechanisms: `research/deep-research/SPINE.md`; t6f seam: `.sovereign/features/deep-research-t6f/order.md`; frozen DRB holdout: `research/deep-research/drb/README.md`.
