<!-- GENERATED FILE — do not edit by hand.
     Source: quality/env-flags.toml (the declared env-knob registry)
     Regenerate: cargo run -p xtask -- env-gate --update-doc -->

# Environment-variable knobs — the declared registry

One row per declared knob, grouped by subsystem cluster. Names use the
canonical `SOVEREIGN_` prefix; every one is mirrored to `SVRNMESH_*` by
the rebrand bridge (`sovereign-contracts/src/rebrand.rs`), so both
spellings work. `status` legend: **guard** = safety/kill-switch, keep;
**shipped** = default-on product behavior; **experiment** = A/B lever,
default-off unless noted; **deprecated** = scheduled for removal.

The registry is enforced by `cargo run -p xtask -- env-gate`: a NEW env
var read anywhere in the workspace must be declared here (or in the
gate's third-party allowlist); pre-registry debt rides the shrink-only
baseline `quality/baselines/env_unregistered.txt`. The historical
dead-codepath survey lives in `docs/ENV_VAR_AUDIT.md`.

## bench

| flag | default | status | purpose |
|---|---|---|---|
| `SOVEREIGN_CHAOS_EXTRACTION_SCORER` | on | shipped | Chaos scorer uses the extraction test (does a reader come away with an answer?) instead of decline-detection. =0 falls back. |
| `SOVEREIGN_CHAOS_TYPED_VERDICT` | on | shipped | Chaos scorer derives answer-vs-abstain from the typed epistemic verdict (parity-proven 43/43 vs the gate-action prefix, 2026-07-19). =0 forces legacy. |
| `SOVEREIGN_FRONTDOOR` | unset | deprecated | Backwards-compat alias for SOVEREIGN_HARNESS (tested + documented; the one legacy duplicate the 2026-07-13 audit named). *(alias of `SOVEREIGN_HARNESS`)* |
| `SOVEREIGN_HARNESS` | unset | shipped | Bench harness selector. |

## cli-binaries

| flag | default | status | purpose |
|---|---|---|---|
| `SOVEREIGN_BIN` | unset | shipped | Drift orchestrator's path to the sovereign CLI. Synonym-cluster member (see SOVEREIGN_CLI_PATH). |
| `SOVEREIGN_CLI` | unset | shipped | Enrichment tool's path to the sovereign CLI. Synonym-cluster member (see SOVEREIGN_CLI_PATH). |
| `SOVEREIGN_CLI_DAEMON_BIN` | sibling of dispatcher | shipped | Path override for the sovereign-cli-daemon sibling. |
| `SOVEREIGN_CLI_DEV_BIN` | sibling of dispatcher | shipped | Path override for the sovereign-cli-dev sibling. |
| `SOVEREIGN_CLI_LLM_BIN` | sibling of dispatcher | shipped | Path override for the sovereign-cli-llm sibling the dispatcher execs. |
| `SOVEREIGN_CLI_PATH` | unset | shipped | Desktop supervisor's path to the sovereign CLI. One of THREE names for 'where is the CLI' (with SOVEREIGN_BIN, SOVEREIGN_CLI) — the sibling-binary synonym cluster. |
| `SOVEREIGN_SERVER_PATH` | unset | shipped | Mobile host's path to the sovereign server binary. |

## enrichment

| flag | default | status | purpose |
|---|---|---|---|
| `SOVEREIGN_GLINER_MODEL_ID` | gliner_small-v2.1 | experiment | WHICH GLiNER model the ingest path loads; the generation (v1 gline-rs vs GLiNER2 bare-ort) is derived from it via KNOWN_MODELS. gliner2-base-v1-onnx routes ingest through GLiNER2 — MEASURED AND REJECTED for the vault path 2026-08-03 (no speedup on vault-length chunks, worse per-mention typing; DEFAULTS_LEDGER.md REJECTED). The knob stays because it is how that is re-tested. Sibling of SOVEREIGN_GLINER_MODEL_DIR, which says WHERE models live. |

## grounding

| flag | default | status | purpose |
|---|---|---|---|
| `SOVEREIGN_AGENTIC_KQ` | off | experiment | Round-0 → sufficiency-judge → sub-query formulation → round-2 retrieval loop on KnowledgeQuery (the agentic evidence loop). |
| `SOVEREIGN_AGENTIC_KQ_DEBUG` | off | experiment | Mirror gate + agentic-loop trace lines to stderr for bench/CLI surfaces with no tracing subscriber. |
| `SOVEREIGN_AGENTIC_KQ_THRESHOLD` | 0.5 | experiment | Insufficiency probability above which the agentic loop's round-2 formulation fires. |
| `SOVEREIGN_CITATION_BROAD` | on | shipped | Run quote-first citation grounding on ALL gated factual answers, not just entity-anchored ones (entity_anchored alone tripped 0 times on the chaos stream). =0 to A/B off. |
| `SOVEREIGN_CITATION_GROUNDING` | on | shipped | Quote-first citation grounding on entity-anchored fact queries: the model copies a verbatim supporting sentence before answering (7x confab reduction, 2026-06-24). =0 to A/B off. |
| `SOVEREIGN_CITATION_LOCATOR` | on | shipped | Name the SECTION a released quote came from ("CHAPTER VII — \"…\""), resolved through the corpus's chunk→section join. Purely additive: a locator appears only where the WHOLE quote is one contiguous span of one chunk AND the join can attribute that chunk, so a corpus without section structure, without a populated join, or a quote that matched only as a partial run releases exactly the text it always did. Display-only and outside the quote marks, so it can never make a claim pass a check. =0 is the CONTROL arm for the situated lane's `cites_a_source` criterion, which was 0/7 in both arms of arm C because the answer path had nothing per-passage to name (2026-08-05). |
| `SOVEREIGN_CITATION_MULTIQUOTE` | off | experiment | Multi-quote citation contract: ground a COMPOUND question one verified quote per sub-question and NAME the parts the passages do not answer. The single-sentence contract grounds 0/14 compound probes (measured 2x n=7, 2026-08-04) because no one sentence answers a two-part question. =1 to arm the A/B. |
| `SOVEREIGN_GATE_CLAIM_SEARCH` | on | guard | Per-claim corpus re-search that widens the audit's evidence beyond the prompt chunks (ClaimSearcher::search_corpus). Set =0 to audit against the prompt chunks alone — the documented no-searcher fallback, not a new behaviour. Exists to price the fan-out against its value: it runs one hybrid search per ALLOWED CORPUS per CLAIM and keeps only CLAIM_SEARCH_K=4 chunks in total. Measured 2026-08-05 on `bench sep/summarize --synth` at HEAD d3c5261d: 753 searches inside the gate window costing 608.9s over 14 questions, of which wikipedia alone was 247 calls at 2218ms = 547.9s — 25% of total run wall-clock, against a 33.8s median draft phase. |
| `SOVEREIGN_GATE_EXCLUDE_RAPTOR` | on | guard | Exclude RAPTOR summary chunks from the gate's evidence view (a summary is not verbatim source). =0 includes them. |
| `SOVEREIGN_GATE_SUMMARY_EVIDENCE` | on | shipped | P1.4 provenance-aware evidence: RAPTOR summary chunks stay in gate evidence marked Summary-class; factual claims still verify against Leaf text only, thematic/structural claims may use summaries. =0 restores the Fix B wholesale exclusion. |
| `SOVEREIGN_GROUNDING_GATE` | on | shipped | Global on/off for the hold→verify→retry→abstain gate on answer-producing surfaces (the Grounded-Everywhere contract). =0/false disables (naked benches, latency debugging). |
| `SOVEREIGN_GROUNDING_GATE_<SURFACE>` | unset | shipped | Per-surface override (=1 force on, =0 force off); SURFACE ∈ {KNOWLEDGE_QUERY, DEEP_QUERY, ATTACHED_DOC, COMPLEX_TASK, SIMPLE_QUERY, REFINEMENT, GOVERNANCE, PROXY_ARGUMENT}. Read dynamically (never appears in the census). |
| `SOVEREIGN_GV_THRESHOLD` | 0.9 | shipped | Violation-probability threshold τ for the external grounding verifier — ONE default for the production gate and the chaos bench (grounding_gate_threshold(); bench override: --gv-threshold). |
| `SOVEREIGN_SHORT_SPECIFICS_SCAN` | off | experiment | SHELVED short-path second-opinion specifics scan on released single-claim answers; dormant pending clean-evidence validation (target category proved ~90% measurement artifact). |
| `SOVEREIGN_SPECIFICS_SCAN` | on | shipped | Long-form holistic specifics scan inside gate_longform: one judge pass over the whole answer vs full evidence, catching fabricated specifics the per-claim audit misses. =0 disables. |
| `SOVEREIGN_SUFFICIENCY_CHUNKS` | 12 | experiment | How many round-0 chunks the agentic loop's sufficiency judge reads. |

## inference

| flag | default | status | purpose |
|---|---|---|---|
| `SOVEREIGN_ALLOW_INPROCESS_DISTRIBUTED_PRIMARY` | unset | guard | Override the containment guard that compute.distributed_primary arms (in-process distributed primary). *(shadows `SetupConfig.compute.distributed_primary`)* |
| `SOVEREIGN_ALTERNATION_GRAMMAR` | unset | guard | Grammar-constraint alternation mode — breaks tool-calling if wrong; do not change casually. *(shadows `SetupConfig.daemon.alternation_grammar`)* |
| `SOVEREIGN_BLOCK_NON_LATIN` | unset | guard | Unicode-crash denylist: block non-Latin scripts in generation. |
| `SOVEREIGN_CONTENT_TEMPERATURE` | unset | experiment | Sampler content-temperature override. Near-synonym of SOVEREIGN_SYNTH_TEMP (desktop synthesis temperature) — the temperature knob cluster. |
| `SOVEREIGN_EMBED_MODEL` | unset | shipped | Embed model override — takes a GGUF PATH. Near-synonym of SOVEREIGN_EMBED_MODEL_ID, which takes an id string; same meaning, incompatible values. *(shadows `SetupConfig.models.embed`)* |
| `SOVEREIGN_EMBED_MODEL_ID` | unset | shipped | Embed model override for notes embeddings — takes a model ID string (corpus-engine-notes). Near-synonym of SOVEREIGN_EMBED_MODEL (a path); same meaning, incompatible values. |
| `SOVEREIGN_EXACTVAL_FIX` | on | guard | Anti-fabrication exact-value fix in constrained decoding. |
| `SOVEREIGN_FORCE_CPU_CHAT` | unset | guard | Force chat inference onto CPU (Gemma4+Metal crash workaround). |
| `SOVEREIGN_FORCE_TOOL_CALLS` | unset | guard | Force the tool-call constraint path. *(shadows `SetupConfig.daemon.force_tool_calls`)* |
| `SOVEREIGN_GPU_LAYERS` | unset | shipped | GPU offload layer-count override for the embedded engine. *(shadows `SetupConfig.compute.slot[].n_gpu_layers`)* |
| `SOVEREIGN_LOCAL_FIT_CHECK_SKIP` | unset | deprecated | Word-order twin read in the SAME fallback expression as its alias target — one knob, two spellings; scheduled to collapse onto the alias target. *(alias of `SOVEREIGN_SKIP_LOCAL_FIT_CHECK`)* |
| `SOVEREIGN_MODEL` | unset | shipped | Primary chat model override (sovereign-server config env path). *(shadows `SetupConfig.models.primary`)* |
| `SOVEREIGN_MTP_DISABLE` | unset | guard | Kill-switch: disable multi-token-prediction speculative decoding. |
| `SOVEREIGN_MTP_QUARANTINE_DISABLE` | unset | guard | Kill-switch: disable the MTP quarantine (the guard that benches a model before trusting its MTP head). |
| `SOVEREIGN_PREFIX_STATE` | on | shipped | Caller-directed pinned-prefix full-state cache. Graduated default-on 2026-08-03 on a measured 1.30x through the production answer path (868.4s -> 669.0s, 2 reps/arm); the grounding gate is its only consumer. Opt out with =0. Worth most on archs where prefix_cache_gate vetoes ordinary partial-KV reuse. |
| `SOVEREIGN_PREFIX_STATE_MAX_MB` | 2048 | shipped | Per-slot byte cap on the pinned-state LRU. State files run ~64KB/token, so a 10K-token pin is ~650MB; this is what bounds the default-on pin's disk cost. |
| `SOVEREIGN_PREFIX_STATE_MIN` | code default | shipped | Minimum stable-prefix length (tokens) worth pinning. Below it, saving state costs more than the re-prefill it avoids. |
| `SOVEREIGN_RERANK_MODEL_PATH` | unset | experiment | Cross-encoder reranker GGUF path — installs the rerank slot [models.extra] otherwise declares. Default-inert but wired into the production daemon; one owner decision pending (docs/archive/RERANK_EXPERIMENT.md). *(shadows `SetupConfig.models.extra`)* |
| `SOVEREIGN_SKIP_LOCAL_FIT_CHECK` | unset | guard | Skip the local fit check before joining a distributed load (rpc_distribution.rs). |
| `SOVEREIGN_SKIP_PER_DEVICE_FIT` | unset | guard | Skip the per-device fit check in the RPC byte-aware shard split. |
| `SOVEREIGN_SKIP_VRAM_CHECK` | unset | guard | Skip the model-fit VRAM check before load. |
| `SOVEREIGN_STRICT_VRAM_CHECK` | unset | guard | Make the preflight VRAM check strict (fail instead of warn). Same family as SOVEREIGN_SKIP_VRAM_CHECK — the fit-check knob cluster. |
| `SOVEREIGN_SYNTH_TEMP` | unset | experiment | Desktop synthesis temperature override. Near-synonym of SOVEREIGN_CONTENT_TEMPERATURE. |

## infra-paths

| flag | default | status | purpose |
|---|---|---|---|
| `SOVEREIGN_BIND` | unset | shipped | Daemon bind address override. *(shadows `SetupConfig.daemon.client_bind`)* |
| `SOVEREIGN_CLIENT_TOKEN` | unset | shipped | Daemon client auth token. Env-only override of the SetupConfig field — an auth secret living in a shadow precedence chain (declared debt; see CLEANUP.md). *(shadows `SetupConfig.client_token`)* |
| `SOVEREIGN_COMMAND_BRIDGE_PORT` | 9745 | shipped | Desktop command-bridge port (the real-mode harness endpoint). |
| `SOVEREIGN_CORPORA_DIR` | under data root | shipped | Corpora storage directory override. |
| `SOVEREIGN_DAEMON_BASE` | unset | deprecated | Example-code spelling of the daemon base URL; use SOVEREIGN_DAEMON_URL. *(alias of `SOVEREIGN_DAEMON_URL`)* |
| `SOVEREIGN_DAEMON_URL` | http://localhost:9741 | shipped | Daemon base URL for CLI clients (one of the daemon-endpoint synonym cluster — see aliases). |
| `SOVEREIGN_DATA_DIR` | ~/.svrnmesh (legacy-aware) | shipped | Per-user data root override. ONE derivation: sovereign_contracts::rebrand::data_dir() — read sites must not re-derive the fallback chain (pre-2026-07-30 chains wrote into CWD when HOME was unset). *(shadows `SetupConfig.data.dir`)* |
| `SOVEREIGN_DB_PATH` | under data root | shipped | State-store DB path override (normally rebrand::state_db_path under the data root). |
| `SOVEREIGN_HOME` | unset | deprecated | THIRD spelling of the data root (hand-rolled ~/.sovereign in watched/enrich.rs) that bypasses rebrand::data_dir() entirely — scheduled to route through the SSOT accessor. *(shadows `SetupConfig.data.dir`)* |
| `SOVEREIGN_MODELS_DIR` | under data root | shipped | GGUF model directory override. *(shadows `SetupConfig.data.dir`)* |
| `SOVEREIGN_PADDLE_OCR_MODEL` | ppocr-en-v4v5 | shipped | PaddleOCR model-set id — the subdirectory under the models root. Read by the bake-off and ocr_images harnesses; the production engine uses paddle::DEFAULT_MODEL_ID. |
| `SOVEREIGN_PADDLE_OCR_MODEL_DIR` | ~/.sovereign/models/paddle-ocr (NOT rebrand-aware) | shipped | PaddleOCR models ROOT — the dir holding <model_id>/{det.onnx,rec.onnx,dict.txt}. The built-in fallback in paddle::models_root() is a hardcoded ~/.sovereign path that a ~/.svrnmesh install never hits, so any rebranded or staged deployment must set this. The daemon's ocr_install probes <data_dir>/models/paddle-ocr first and sets the var for the engine. |
| `SOVEREIGN_PDFIUM_LIB` | unset (pdfium-render bundled/system search) | shipped | Absolute path to libpdfium (.so/.dylib/.dll). Without it no PDF can be rasterized, so OCR silently yields nothing on a box with no system pdfium — which is every air-gapped Linux install. The daemon probes <data_dir>/lib first and warns naming every path it tried. |
| `SOVEREIGN_PORT` | 9741 | shipped | Daemon API port override (read via svrnmesh_env; also gates the rebrand migrator's daemon-liveness probe). *(shadows `SetupConfig.daemon.client_port`)* |
| `SOVEREIGN_RECIPES_DIR` | under data root | shipped | Recipe registry directory override. |
| `SOVEREIGN_SESSIONS_DIR` | under data root | shipped | Session-frame storage override (session continuity; read dynamically with both prefixes). |
| `SOVEREIGN_WORKSPACE_DIR` | unset | shipped | Agent workspace (shell/file tool sandbox) root override. |

## mesh

| flag | default | status | purpose |
|---|---|---|---|
| `SOVEREIGN_ADVERTISE_ADDR` | unset | shipped | Address this node advertises to mesh peers. |
| `SOVEREIGN_DISABLE_AUTO_COLLAB` | unset | guard | Kill-switch: disable auto-collaboration (peer ingest handoff) on the mesh. |
| `SOVEREIGN_DISABLE_MDNS` | unset | guard | Kill-switch: disable mDNS LAN peer discovery. *(shadows `SetupConfig.discovery.mdns`)* |
| `SOVEREIGN_DISABLE_PEER_INFERENCE` | unset | guard | Kill-switch: =1 keeps all inference local instead of load-balancing to mesh peers (solo bench runs, reproducibility). Read in sovereign-mesh/src/peer_inference.rs. |
| `SOVEREIGN_IROH` | unset | guard | Toggle for the iroh (no-VPN) mesh transport. *(shadows `SetupConfig.iroh.enabled`)* |
| `SOVEREIGN_IROH_RELAY_ONLY` | unset | guard | Force the iroh transport onto relay-only paths (what iroh.relay_urls/discovery express declaratively). *(shadows `SetupConfig.iroh.relay_urls`)* |
| `SOVEREIGN_JOIN_HOST` | unset | shipped | Mesh join target host override. |
| `SOVEREIGN_PRIMARY_SIBLINGS` | unset | shipped | Distributed-inference primary's sibling set override. *(shadows `SetupConfig.models.primary_pool.copies`)* |
| `SOVEREIGN_RPC_DISCOVER` | unset | shipped | Discover half of the shared-model role. *(shadows `SetupConfig.shared_model.role`)* |
| `SOVEREIGN_RPC_HEADROOM` | unset | shipped | Per-device VRAM headroom for the byte-aware shard split (env wins when pre-set, setup_config.rs). *(shadows `SetupConfig.shared_model.headroom`)* |
| `SOVEREIGN_RPC_MIN_POOLED_GB` | unset | shipped | Minimum pooled VRAM before the shared model loads. *(shadows `SetupConfig.shared_model.min_pooled_gb`)* |
| `SOVEREIGN_RPC_QUORUM_ANCHORS` | unset | shipped | Anchor quorum for the pooled shared model. *(shadows `SetupConfig.shared_model.quorum_anchors`)* |
| `SOVEREIGN_RPC_SERVE` | unset | shipped | Anchor/host half of the shared-model role (env half of the config's role field). *(shadows `SetupConfig.shared_model.role`)* |
| `SOVEREIGN_RPC_SHARD_FETCH` | unset | shipped | Shard-fetch strategy for distributed model load. *(shadows `SetupConfig.shared_model.shard_fetch`)* |
| `SOVEREIGN_SHARED_MODEL_HOST_NODE_ID` | unset | shipped | Distributed-inference host node id override. *(shadows `SetupConfig.shared_model.host_node_id`)* |
| `SOVEREIGN_SHARED_MODEL_ID` | unset | shipped | Distributed-inference shared model id override (bootstrap.rs env-or-config). *(shadows `SetupConfig.shared_model.model_id`)* |
| `SOVEREIGN_USE_SUPERVISOR` | unset | shipped | Route distributed-inference workers through the supervisor. |
| `SOVEREIGN_WORKER_RUNNER` | unset | shipped | Distributed-inference worker runner selector. |

## retrieval

| flag | default | status | purpose |
|---|---|---|---|
| `SOVEREIGN_ATLAS_GROUNDING` | on | shipped | Atlas graph-walk grounding: cosine seeds → BFS over typed edges → FTS-fetch evidence chunks. =0/false/off/no disables. |
| `SOVEREIGN_ATOM_ENUM` | off | experiment | Enumeration-class questions get top-degree typed atoms injected as virtual chunks. Net-negative on focused enumeration (2026-06-04 bench); keep gated. |
| `SOVEREIGN_ATOM_ENUM_NOFILTER` | off | experiment | Ablation hatch: disable the enumeration-question classifier filter. |
| `SOVEREIGN_ATOM_ENUM_OVERVIEW` | on | shipped | Overview/summary questions inject the scoped corpus's atlas Claim atoms as virtual chunks. Independent of SOVEREIGN_ATOM_ENUM; question-shape detected, no LLM call. |
| `SOVEREIGN_ATOM_ENUM_POOL` | see helper | experiment | Atom-enumeration candidate-pool cap before ranking. |
| `SOVEREIGN_ATOM_ENUM_RANK` | rrf | experiment | Atom-enumeration ranking mode. |
| `SOVEREIGN_ATOM_ENUM_RELATIONS` | off | experiment | Include relation atoms in the enumeration. |
| `SOVEREIGN_ATOM_ENUM_SCORE` | see helper | shipped | Score stamped on enumerated virtual chunks. Shared with the shipped overview path — load-bearing. |
| `SOVEREIGN_ATOM_ENUM_TOPIC_GRIP` | 0.5 | shipped | Fraction of an overview question's TOPIC tokens (content tokens minus the overview framing) that a corpus's best atlas Claim must cover before ANY of that corpus's claims may be injected. Admission is per CORPUS, not per claim. Raising it toward 1.0 injects only on near-exact topical matches; 0.0 restores the pre-2026-08-05 behaviour where pool presence alone granted injection rights — which put 89 ARCH_PRINCIPLES chunks and a personal Obsidian vault into 14/14 SEP philosophy answers (audit D1). |
| `SOVEREIGN_ATOM_ENUM_TOPK` | see helper | shipped | How many enumerated atoms become virtual chunks. SHARED between the experimental entity path and the shipped overview path — load-bearing. |
| `SOVEREIGN_COMPACTION_DISABLE` | off | guard | History layer escape hatch: =1 disables dropped-history compaction. *(shadows `SetupConfig.memory.compaction`)* |
| `SOVEREIGN_CONV_PPR_WEIGHT` | off (0.0) | deprecated | Post-pipeline PPR rerank weight for conversation-corpus chunks. DEFAULT FLIPPED 0.25 -> 0.0 (OFF) 2026-08-04: a 180-question paired bank could not separate it from off (49-31 p=0.0567 alone; 64-43 p=0.0527 under the strongest retrieval config). It re-ranks in place and never adds a document — B-in-pool and source_ratio were identical to 4dp with it on and off — while rebuilding a per-conversation entity graph from SQL on every query and forcing GLiNER to run eagerly at ingest. Code kept and working; set a non-zero weight to re-enable. See DEFAULTS_LEDGER.md. |
| `SOVEREIGN_COVERAGE_NEAR_SIM` | 0.55 | shipped | Similarity floor for the coverage probe's TopicUncovered/ClaimUncovered split. |
| `SOVEREIGN_COVERAGE_PROBE` | on | shipped | On gap/abstain turns: cross-corpus nearest-chunk-cosine probe classifying a gap as TopicUncovered vs ClaimUncovered. =0 disables. |
| `SOVEREIGN_DECOMP_DECAY` | 1.0 | experiment | Score decay applied to fanned-out sub-query hits (<1 = augment, never displace). |
| `SOVEREIGN_DEMAND_PLAN` | off | experiment | One fast-slot structured-output call plans the turn's demands (sub_queries, entities, stance contrast); feeds the epistemic demand set. |
| `SOVEREIGN_DEMAND_PLAN_FANOUT` | off | experiment | Fan the demand plan's sub_queries out into corpus search. Off after the 2026-07-19 A/B (2-3x slower for flat recall). |
| `SOVEREIGN_EPISTEMIC_STATE` | on | shipped | Post-pipeline per-turn epistemic ledger assembled into message metadata (pure collation, no model calls). =0 disables. |
| `SOVEREIGN_FORENSIC` | off | experiment | Debug hatch: =1 enables audit_pipeline_stage composition snapshots between retrieval steps. |
| `SOVEREIGN_GRAPH_NEIGHBOR_EXPAND` | off | experiment | Axis-aware structural-graph one-hop expansion (per-entity axis neighbors + co-citation bridges). Wikipedia-specific, never promoted. |
| `SOVEREIGN_HISTORY_RETRIEVAL` | on | shipped | History layer: retrieval over prior conversation turns. =0 disables. |
| `SOVEREIGN_MERGE_SELECT` | on | shipped | Demand-aware merge composition (pins + per-entity demand slots + greedy diminishing-returns selector) replacing the cap/reserve/truncate heuristic pile. =0 restores the legacy stack. |
| `SOVEREIGN_META_BRIDGE` | off | experiment | Cross-corpus bridge boost: question entities matching a bridge topic pull the linked corpus's framing via typed edges. Built by `sovereign meta-atlas align`. |
| `SOVEREIGN_PPR_EXPAND` | on (dark without a reranker) | shipped | PPR walk + typed edges over the link graph propose answer-side articles; cross-encoder admission gate injects only CE-yes candidates. |
| `SOVEREIGN_QUERY_DECOMP` | off | experiment | Pure-Rust question decomposition; each sub-query gets its own focused retrieval pass. |
| `SOVEREIGN_RAPTOR_DEDUPE` | see helper | shipped | Collapse one entry's multi-level RAPTOR nodes to its best. |
| `SOVEREIGN_RAPTOR_GROUNDING` | on | shipped | RAPTOR collapsed-tree summary nodes injected as virtual chunks (position picked by SOVEREIGN_RAPTOR_LATE). |
| `SOVEREIGN_RAPTOR_LATE` | on | shipped | Inject RAPTOR summaries AFTER the leaf pipeline (QA-neutral) instead of pre-merge. |
| `SOVEREIGN_RAPTOR_MIN_LEVEL` | see helper | shipped | Minimum RAPTOR tree level for injected summaries. |
| `SOVEREIGN_RAPTOR_TOP_M` | see helper | shipped | Top-M RAPTOR summary nodes injected. |
| `SOVEREIGN_TITLE_EXPAND` | off | experiment | Fast-slot LLM names explicit article titles for abstract questions; titles are fan-out-searched and reserved through the merge. |
