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

## corpus

| flag | default | status | purpose |
|---|---|---|---|
| `SOVEREIGN_CORPUS_MAINTENANCE_INTERVAL_MINS` | 60 | shipped | Minutes between self-healing corpus maintenance sweeps in the daemon (crate::corpus_maintenance). Each cycle asks every corpus how many rows sit OUTSIDE its indexes — a metadata read — and compacts/folds only those past the floor. Exists because lancedb serves a query by running the index over indexed data AND flat-scanning everything appended since, which is silent, correct and progressively slower: wikipedia reached 3,955 manifest versions and 2218ms per search while the static sep stayed at 100ms. 0 disables the sweep, which lets appended corpora decay again. |
| `SOVEREIGN_CORPUS_MAINTENANCE_PRUNE_DAYS` | 7 | shipped | Age below which superseded dataset versions are KEPT by the maintenance sweep. Compaction is non-destructive — superseded fragments stay readable under old manifests — so without pruning the directory grows without bound, which on a desktop install is its own product failure. Seven days is far outside any in-flight reader while still bounding growth. 0 disables pruning and retains every version. |
| `SOVEREIGN_CORPUS_MAINTENANCE_UNINDEXED_FLOOR` | 5000 | shipped | Rows-outside-the-index a corpus must exceed before the daemon's maintenance sweep rewrites it. Not zero: folding an index costs seconds-to-minutes on a large corpus and a few hundred straggler rows are genuinely cheap to flat-scan; the cost only becomes visible in the thousands. |

## dev-gates

| flag | default | status | purpose |
|---|---|---|---|
| `SOVEREIGN_LINT_JOBS` | unset (derived from cores + free memory) | guard | Caps the cargo job fan for scripts/sovereign-lint.sh. Unset resolves through lib/cargo-jobs.sh, which budgets from cores AND free memory: `cargo check` is lighter than the test gate, but it is the same unbounded fan against the same RAM, so it obeys one budget rather than a second rule. This is the per-machine lever — a box holding a resident big model pins itself low in a shell profile; --jobs overrides it per run. |
| `SOVEREIGN_TEST_JOBS` | unset (derived from cores + free memory) | guard | Caps the cargo job fan for scripts/sovereign-test.sh — build+link+run, the heaviest fan in the repo. Twin of SOVEREIGN_LINT_JOBS on the same lib/cargo-jobs.sh budget; --jobs overrides it per run. The resolved value is what the banner's `jobs:` line reports, so a run that self-throttled on low free memory says so instead of just being slow. |

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
| `SOVEREIGN_CITATION_MULTIQUOTE` | on | shipped | Multi-quote citation contract: ground a COMPOUND question one verified quote per sub-question and NAME the parts the passages do not answer. The single-sentence contract grounds 0/14 compound probes (measured 2x n=7, 2026-08-04) because no one sentence answers a two-part question. Flipped ON 2026-08-05 on a matched control (saltgrass_compound n=7, same HEAD/day/topology, 0 extraction failures both arms): citation releases 0->3, competence-when-present 0.14->0.43, gate-attributed misses 4->2, blatant-confab-rate 0.00 unchanged. =0 for the control arm. |
| `SOVEREIGN_GATE_CLAIM_SEARCH` | on | guard | Per-claim corpus re-search that widens the audit's evidence beyond the prompt chunks (ClaimSearcher::search_corpus). Set =0 to audit against the prompt chunks alone — the documented no-searcher fallback, not a new behaviour. Exists to price the fan-out against its value: it runs one hybrid search per ALLOWED CORPUS per CLAIM and keeps only CLAIM_SEARCH_K=4 chunks in total. Measured 2026-08-05 on `bench sep/summarize --synth` at HEAD d3c5261d: 753 searches inside the gate window costing 608.9s over 14 questions, of which wikipedia alone was 247 calls at 2218ms = 547.9s — 25% of total run wall-clock, against a 33.8s median draft phase. |
| `SOVEREIGN_GATE_CLAIM_SEARCH_LADDER` | off | experiment | Two-stage claim audit: judge each claim against the prompt window FIRST and issue the per-claim corpus fan-out only for claims that fail without it. Targets the fan-out measured at 25% of wall-clock on `bench sep/summarize --synth` (one hybrid search per allowed corpus per claim; 2218ms per wikipedia call). Lossless for rescues BY CONSTRUCTION — a rescue is by definition a claim that fails without re-search and passes with it, so every rescue has stage-1 vp >= tau and always reaches stage 2; measured 7/7 rescues kept while searching 11 of 18 claims on `summary_cosmological_argument` (2026-08-05). Named behaviour change: a re-searched hit can currently dilute a claim the prompt window alone supported (the longform judge scores all passages in ONE joint forced-choice — no per-chunk max, no rescue floor), and the ladder releases such claims on stage 1 instead; measured newly_failed=0 on the specimen. Default OFF pending the multi-question confirmation and a net wall-clock number. |
| `SOVEREIGN_GATE_CLAIM_SEARCH_SHADOW` | off | experiment | Shadow-measure the per-claim corpus re-search without changing any answer (same pattern as SOVEREIGN_GATE_BATCH_SHADOW). Emits `claim_search_shadow` per claim: the production verdict WITH re-searched hits, plus the counterfactual vp on the prompt chunks alone, plus best-extra vs best-chunk support. Both come from the same judge pass — the only added inference is scoring past the 0.95 early break, which shadow mode relaxes for data while still returning the value production would have stopped at. Purpose: get the (vp_production, vp_chunks_only) pairs that say WHICH claims the fan-out actually rescues, so it can fire selectively instead of on every claim. |
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
| `SOVEREIGN_GROUNDING_JOURNAL` | on | guard | =off/0/false stops the GROUNDING journal stream only, leaving other streams recording (one decision line per gated answer: verdict, score, tau, action, and (corpus, chunk-id) evidence handles — never claim/answer/chunk text; VERIFIER_V0.md §6.1 phase 0). Per-stream twin of SOVEREIGN_JOURNAL; the marker-file equivalent is `svrn journal grounding off`. Every stream declares its own such var via JournalStream::disable_env. |
| `SOVEREIGN_JOURNAL` | on | guard | =off/0/false stops EVERY local journal stream in this process. A journal is one feature's append-only metadata-only record of how it behaved on the developer's real work, under ~/.svrnmesh/journal/<stream>-<date>.jsonl (today: next-edit; the layer is feature-agnostic — sovereign-contracts/src/types/journal.rs). This is the CI/harness switch; the developer-facing opt-out is `svrn journal off`, which drops a DISABLED marker needing no daemon restart. Global env, global marker, per-stream env and per-stream marker are all read by ONE decider, JournalStream::enabled. Local write is default-ON and sending is NEVER: no network path exists in the module, and `svrn journal bundle` writes a file plus a manifest of every field in it. Ledger row: sovereign/DEFAULTS_LEDGER.md. |
| `SOVEREIGN_JOURNAL_DIR` | unset | shipped | Overrides the journal directory for every stream. Tests and operators only — unset resolves through rebrand::journal_dir() so the ~/.svrnmesh rebrand and its legacy ~/.sovereign fallback are honoured (clippy.toml's path-SSOT ban). |
| `SOVEREIGN_LOCAL_FIT_CHECK_SKIP` | unset | deprecated | Word-order twin read in the SAME fallback expression as its alias target — one knob, two spellings; scheduled to collapse onto the alias target. *(alias of `SOVEREIGN_SKIP_LOCAL_FIT_CHECK`)* |
| `SOVEREIGN_MAX_QUEUE_WAIT_SECS` | 30 | shipped | Ceiling on how long a caller may wait SILENTLY for a model permit before the host sheds with a structured 503 + Retry-After (MESH_N4_TOPOLOGY M5). Bounds PREDICTED WAIT, not queue depth: measured 2026-08-06, an identical depth of 8 cost 6.2s when callers shared a prompt prefix and 90.7s when they did not. =0 restores the pre-M5 unbounded wait. Raise it for a hub deployment with no alternative holder, where a shed means no answer rather than a slow one. |
| `SOVEREIGN_MODEL` | unset | shipped | Primary chat model override (sovereign-server config env path). *(shadows `SetupConfig.models.primary`)* |
| `SOVEREIGN_MTP_DISABLE` | unset | guard | Kill-switch: disable multi-token-prediction speculative decoding. |
| `SOVEREIGN_MTP_QUARANTINE_DISABLE` | unset | guard | Kill-switch: disable the MTP quarantine (the guard that benches a model before trusting its MTP head). |
| `SOVEREIGN_NEXT_EDIT_FALLBACK` | off | experiment | =1/true serves the next-edit lane (POST /v1/edit_predictions) off the already-resident FAST slot when NO [models.edit] is configured, instead of serving nothing (install_fallback_next_edit_slot). Next-edit needs only a prompt dialect, not FIM marker tokens, so an ordinary chat model can serve it. MEASURED AND IT DID NOT HOLD (2026-08-07): the flag resolves through fast_path(), so on a box with an explicit [models].fast the answering model is the fast slot — a 4B there scored 14/30 useful (GM4 FAIL) at p95 2194ms, against 21/30 for the 35B-A3B chat primary and 19/30 / 828ms for a 1.5B specialist, same 60-case gym/next-edit/gen bank. Safe but not useful: 0 wrong edits in 17 fires. A bench number does not transfer down a model class. Marks the slot degraded=true, which drives the advice nudge on /status.inference.edit. An explicit [models.edit] always wins. STAYS OFF — opt-in only; users are pointed at `svrn setup --fim`. Ledger row: sovereign/DEFAULTS_LEDGER.md. |
| `SOVEREIGN_NEXT_EDIT_JOURNAL` | on | guard | =off/0/false stops the NEXT-EDIT journal stream only, leaving other streams recording (one record per POST /v1/edit_predictions, plus the editor's outcome report). Per-stream twin of SOVEREIGN_JOURNAL; the marker-file equivalent is `svrn journal next-edit off`. Every stream declares its own such var via JournalStream::disable_env. |
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
| `SOVEREIGN_MODELS_DIR` | under data root | shipped | GGUF model directory override. *(shadows `SetupConfig.data.dir`)* |
| `SOVEREIGN_PADDLE_OCR_MODEL` | ppocr-en-v4v5 | shipped | PaddleOCR model-set id — the subdirectory under the models root. Read by the bake-off and ocr_images harnesses; the production engine uses paddle::DEFAULT_MODEL_ID. |
| `SOVEREIGN_PADDLE_OCR_MODEL_DIR` | <svrnmesh root>/models/paddle-ocr | shipped | PaddleOCR models ROOT — the dir holding <model_id>/{det.onnx,rec.onnx,dict.txt}. Since 2026-08-10 paddle::models_root() falls back through the path SSOT (rebrand::svrnmesh_root()), so it resolves correctly on both a rebranded and a not-yet-migrated install and setting this is no longer required to work around a hardcoded path — it is now purely a staging/relocation override. The daemon's ocr_install probes <data_dir>/models/paddle-ocr first and sets the var for the engine. |
| `SOVEREIGN_PDFIUM_LIB` | unset (pdfium-render bundled/system search) | shipped | Absolute path to libpdfium (.so/.dylib/.dll). Without it no PDF can be rasterized, so OCR silently yields nothing on a box with no system pdfium — which is every air-gapped Linux install. The daemon probes <data_dir>/lib first and warns naming every path it tried. |
| `SOVEREIGN_PORT` | 9741 | shipped | Daemon API port override (read via svrnmesh_env; also gates the rebrand migrator's daemon-liveness probe). *(shadows `SetupConfig.daemon.client_port`)* |
| `SOVEREIGN_PROBE_GGUF` | unset | experiment | First shard of the GGUF that the #[ignore]d device-memory probe measures (sovereign-inference/tests/device_memory_probe.rs). Manual, opt-in: the test panics with this instruction when unset, and never runs in CI. |
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
| `SOVEREIGN_RAPTOR_LATE` | on | shipped | Inject RAPTOR summaries AFTER the leaf pipeline (QA-neutral) instead of pre-merge. TIMING only — since 2026-08-10 both late sites also reserve the summaries to the head of the pool, because tail placement admitted 0 of 8 into the deep prompt (invariant 3035f3a4). |
| `SOVEREIGN_RAPTOR_MIN_LEVEL` | see helper | shipped | Minimum RAPTOR tree level for injected summaries. |
| `SOVEREIGN_RAPTOR_TOP_M` | see helper | shipped | Top-M RAPTOR summary nodes injected. |
| `SOVEREIGN_TITLE_EXPAND` | off | experiment | Fast-slot LLM names explicit article titles for abstract questions; titles are fan-out-searched and reserved through the merge. |
