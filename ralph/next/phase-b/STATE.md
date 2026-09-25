# phase-b — the ralph queue (STAGED, not started)

Designed by the seat with the operator on 2026-09-25 (ralph/decisions/phase-b-1.md).
It replaces the single plan-and-mint row staged on 2026-09-24 (five-programs-39):
the rows below are OUTCOMES (ralph/PROMPT.base.md §2), each with its own proof, and
there is no mint step. Nothing runs this queue until the operator launches it at
five-programs' `HUMAN-phase-b` row. Launch line: this directory's queue.toml header.

What Phase B is: making every program takeable ALONE. "The next developer who wants
THIS but not THAT" (operator, 2026-09-25). It builds the owners the five-programs
exceptions stood in for, and it never adds one. Design: docs/FIVE_PROGRAMS.md §1,
§2 (six programs: `serve` is split out of `cmnwlth`), §2c (distributions, the host
kit, the compose rule), §4 rules 6-7, and §12 3a (the ladder, with the mechanism
rung). Finish: FIVE_PROGRAMS §12 "Done".
- boundary-gate exits 0;
- no `package = "svrn"` exception remains;
- all six lift sandboxes pass;
- each §2c drive has one implementation.

Order: collapse before split (principle 8). The host kit, the one MCP dispatcher
and the one serving assembly land before any host is stood up. Hosts come before
their clients. Every row names the owner it extends; the appendix maps every red
edge to exactly one row.

**Pre-registered bars (set 2026-09-25, before any data; principle 7).** They apply
to pb-svrn-dials-serve.
- Serving over loopback vs in-process, n ≥ 5 runs each, same host, same model:
  first-token latency p50 at most 10% slower, and embedding throughput at batch 32
  at least 90% of in-process.
- `svrn quality check --lane retrieval-prod` and the synth lane stay inside their
  noise bands (sovereign/docs/RUNBOOK.md §6).

A miss is NEEDS_HUMAN with the numbers, never a re-tuned bar.

## Rows

- [ ] REVIEW-pb-census — depends [] — OUTCOME: every red edge on the merged tree has exactly ONE owner row below, and the known defects are reproduced or struck. No code, apart from the TSV repair.
  (1) Run the gate on `cut` after the origin/main merge. Reconcile this file's appendix against it: an edge the merge added gets an owner row by the §12 3a ladder, and an edge that closed is struck with the count.
  (2) Repair docs/FIVE_PROGRAMS_DECISIONS.tsv rows 31-44. They carry 11 fields, so `decision_needed` holds the delta text and the real question sits in column 10.
  (3) Reproduce each defect in the appendix's "Defects" list by reading or running it, then confirm its owner row or strike it with the evidence.
  (4) Measure the baseline each later row reports against:
  - the copy counts of the §2c drives (locators, root locks, MCP loops, tool-set builders, engine assemblies, bring-up paths);
  - `sovereign-contracts`' fan-in, meaning manifests that name it;
  - bench's closure crate count.
  — read: this file whole, docs/FIVE_PROGRAMS.md §2c and §12 3a, ralph/decisions/phase-b-1.md, quality/ARCH_LAYERS.toml — check: BOUNDARY (paste the count and the owner histogram), DOCS
- [ ] pb-lift-instrument — depends [REVIEW-pb-census] — OUTCOME: ONE instrument proves any program builds and runs outside the monorepo: `scripts/program-lift.sh [--sandbox] <program>`.
  - Collapse the two twins `scripts/cw-rails-lift.sh` and `scripts/cw-work-lift.sh` onto it (principle 8). Keep both names as thin wrappers so existing callers still work.
  - Each program's closure seed and forbidden set are read from quality/ARCH_LAYERS.toml's package map and `[[forbid]]` rows (one decider, principle 8). The regex copies go.
  - `workspace-hack` is dropped from copied manifests, as fp-solo-lift did.
  - Four verdicts with distinct exits (principle 5). An absent precondition of the RUN step (an invite, a model file) is exit 3 naming it.
  - Programs: svrn, ingest, cmnwlth, serve, code, bench. Until pb-serve-program lands, `serve` is the serving crates' closure.
  - Record every program's verdict at this commit as the baseline. Most are 0, and that is the honest scoreboard, not a failure of this row.
  - PROOF: cmnwlth passes (fp-solo-lift made it pass). A PLANT that adds a sovereign-* dep to commonwealth-rails turns cmnwlth red.
  LIFT ~500 lines, scripts only. — read: scripts/cw-rails-lift.sh, scripts/cw-work-lift.sh, scripts/co-lineage.py `measure_bar` (the verdict contract), quality/ARCH_LAYERS.toml package map — check: LIFT(cmnwlth) passes; `scripts/cw-rails-lift.sh --sandbox` unchanged verdict; PLANT(add `sovereign-time` to commonwealth-rails [dependencies] → LIFT(cmnwlth) value 0); paste the six-program verdict table
- [ ] pb-hostkit — depends [pb-lift-instrument] — OUTCOME: every program claims its data root through ONE lock, from a neutrally named leaf that cw-rails can take (FIVE_PROGRAMS §2c, §12 3a rung 4).
  - REUSE `sovereign-cli-base` (852 lines, principle 11). Its contracts uses are exactly host-kit and dialer items: dirs.rs:26,33, guest_link.rs:54, urls.rs:31.
  - Name it with `svrn code converge noun <Name>` first, rename it neutrally, and keep a re-export crate or path so no consumer breaks.
  - Move its program vocabulary to the owners: guest_link and the rail helpers go to sovereign-cli-mesh; `client_daemon_base` goes to sovereign-turn-client (the dialer).
  - MOVE sovereign-contracts/src/run_lock.rs into it and keep the historical path as a re-export. Then, in its OWN commit because Windows gains enforcement, re-base the lock on std `File::try_lock`, which drops libc.
  - Collapse these onto it: sovereign-core/src/deep_research/state.rs:208 (a second type named RunLock); cw-rails' fp-solo-lift lock; corpus-engine-scip's fs4 lock only if its semantics match (else name why not).
  - ARCH_LAYERS: the leaf row with `allow = ["workspace-hack"]` and a size cap of 2,500 code lines. The cap is the operator's to change and is never ratcheted. The prose of the 3a rung is already in FIVE_PROGRAMS.
  - PROOF: LIFT(cmnwlth) still passes with the kit in cw-rails' closure. Two processes on one root refuse, for the daemon and for cw-rails. Copy counts go down, with the numbers in the body.
  LIFT ~1,200 lines, mostly renames. — read: sovereign-cli-base/src, sovereign-contracts/src/run_lock.rs, sovereign-core/src/deep_research/state.rs:190-240, commonwealth-rails/src/cli.rs, quality/ARCH_LAYERS.toml leaf rows — check: CLEAN, LINT, TEST(<the kit>), TEST(commonwealth-rails), TEST(sovereign-daemon), LAYER, LIFT(cmnwlth), PLANT(remove the try_lock claim → TEST(<the kit>) red), BOUNDARY
- [ ] pb-mcp — depends [pb-hostkit] — OUTCOME: ONE MCP implementation. It is one dispatcher and one tool set built from bundles, and every host mounts it.
  - The dispatcher: lift `McpService` from sovereign-daemon/src/mcp_router.rs:408-639, plus the version, alias and exposure logic in sovereign-tools/src/mcp_surface.rs, into the host kit.
  - Framings: stdio, in the corpus-mcp mcp.rs shape, and HTTP+SSE with axum behind a feature.
  - The MCP method set becomes ONE enum beside oicp-types' jsonrpc (principle 9). There are three string matches today.
  - A call-log PORT replaces mcp_router's `Arc<NoteStore>` (mcp_router.rs:31,168,182). `ToolPatternMatcher` becomes the code mount's observer; five-programs-39 names this delta.
  - Tool exposure becomes a manifest field. `MCP_TOOLS_ALWAYS`, `SPEC_GATED`, `RETIRED` and `ALIASES` become data.
  - The tool set is built from `ToolBundle`s (sovereign-contracts tool_bundle.rs:67). The three hand builders collapse onto it: daemon tool_registry.rs (38 register calls), cli-dev project_cmd/serve.rs:389-588 (30) and tools_cmd/registry.rs (35). The bundles `CodeIntelTools`/`NotesTools` (sovereign-code/src/bundle.rs) become the one assembly. Today they have zero production constructors.
  - Adopters: the daemon, corpus-mcp and `project serve`.
  - Deltas, each in its own commit: corpus-mcp moves to protocol-version negotiation and isError results (a behaviour change: name it). Registration drift between the three builders resolves to the union, and the body lists every tool that changes host.
  - Closes sovereign-cli-dev → sovereign-daemon. cli-dev's only daemon imports are mcp_router's (serve.rs:666,675,696).
  LIFT ~1,500 lines. — read: mcp_router.rs, mcp_surface.rs, corpus-mcp/src/{mcp.rs,tools.rs}, tool_bundle.rs, sovereign-code/src/bundle.rs, the three builders, ralph/decisions/five-programs-39.md — check: CLEAN, LINT, TEST(<the kit>), TEST(sovereign-daemon), TEST(corpus-mcp), TEST(sovereign-cli-dev), `sovereign-cli-dev/tests/mcp_surface_e2e.rs`, PLANT(register one tool in a host outside the bundle → the surface test red), LAYER, BOUNDARY (paste; expect −1)
- [ ] pb-shell — depends [pb-hostkit] — OUTCOME: every program's HTTP server is ONE shell plus the routes it registers.
  - One `serve(listeners, bundles, shutdown: impl Future)` in the host kit. It owns: peer addresses (the "bare axum::serve drops ConnectInfo" comment becomes code), the loopback guard, body limits (server.rs:30-46), bind retry and mount tracing.
  - The loopback guard: lift the daemon's `pub(crate)` LoopbackRouter/LocalOnly (loopback_guard.rs:126) and retire mcp_router's hand-rolled `is_localhost`.
  - Route bundles return `(name, Router)` as ONE value. The daemon's 18 `mounted.push`/`mount_names.push` pairs (daemon.rs:3500-3633) and the two parallel matches `host_routers`/`host_router_names` (daemon_services.rs:434-466) collapse onto it.
  - Adopters: the daemon's six binds, cw-rails (api.rs:119-133, internal.rs:50-75), the compute child, and the meshapp dev and ring dev near-twins (sovereign-cli-mesh meshapp_cmd.rs:246-268, ring_cmd/dev.rs:88-110).
  - Delete `server::serve` (server.rs:667-712) if the census confirms it has no caller.
  - Health spellings stay as they are, because they are wires. Name them in the body.
  LIFT ~1,200 lines. — read: sovereign-daemon/src/{daemon.rs:3478-3922,loopback_guard.rs,daemon_services.rs:434-466,server.rs}, commonwealth-rails/src/{api.rs,internal.rs}, sovereign-compute/src/child_main.rs:223-330 — check: CLEAN, LINT, TEST(<the kit>), TEST(sovereign-daemon), TEST(commonwealth-rails), LIFT(cmnwlth), PLANT(mount a route outside the bundle list → the mount-trace test red), LAYER, BOUNDARY
- [ ] pb-serving-assembly — depends [REVIEW-pb-census] — OUTCOME: a hot reload builds exactly what cold start builds, from ONE engine assembly that the serving package owns.
  - Today three paths build the engine, and they have drifted:
    - daemon build/inference.rs `load_provider` (:67-490);
    - daemon provider.rs `LlamaCppFactory` (:36-164), which calls `EmbeddedLlamaCpp::load_full_with_families` directly, never reads `[engine] kind`, installs no extra/edit/rerank slots and no compute layer, and passes `ModelFamily::Unknown`;
    - sovereign-compute child_main.rs:354/374/385.
  - One `assemble_serving(cfg) -> ServingParts { provider, reload_factory, child_trampoline }` in the serving package, beside `engine_factory::register_engine` (sovereign-inference/src/engine_factory.rs:141). All three paths call it. Serving admission (containment, fast≠primary; engine_factory.rs:15-20 handed it to the daemon) moves with it.
  - Deltas (the fix), each in its own commit:
    - a reload on a `kind="remote"` node stops loading GGUFs;
    - a reload under `[compute] distributed_primary` stops loading the withheld primary in-process;
    - embed keeps its manifest family.
  - PROOF: a test builds the provider through the cold path and the reload path for the configs {llama, remote, distributed_primary} and asserts identical slot sets. PLANT: restore the direct load in the reload path, and the test goes red.
  LIFT ~800 lines. — read: sovereign-daemon/src/{build/inference.rs,provider.rs}, sovereign-inference/src/engine_factory.rs, sovereign-compute/src/child_main.rs — check: CLEAN, LINT, TEST(sovereign-inference), TEST(sovereign-compute), TEST(sovereign-daemon), PLANT, LAYER, BOUNDARY
- [ ] pb-serving-kinds — depends [pb-serving-assembly] — OUTCOME: a new model kind is ONE registration, and rerank and NER are served kinds.
  - Served kinds get a registry on register_engine's precedent. A kind registers its manifest role, in-process loader, OICP route, oicp-client method and compute-child role once. Slot roles stay a closed enum (SLOT_ALIAS_POLICY, principle 9).
  - Rerank:
    - Today it is env-only (`SOVEREIGN_RERANK_MODEL_PATH`, inference.rs:369), with two loaders on one env var (reranker_standalone.rs:119).
    - It has no route and no client method, and `SplitInferenceProvider` falls through to NotImplemented (traits.rs:475).
    - It gains a `[models]` key, `/v1/rerank`, the client method and a child role.
  - NER: GLiNER is served through the existing `EntityExtractor` port (traits.rs:207) as a kind. The double load goes: daemon boot.rs:595 and sovereign-runtime-recipe lib.rs:835-859 load two different models whenever the env var is set.
  - runtime-recipe takes both as PORTS: `rerank: Option<Arc<dyn InferenceProvider>>`, since `inference_to_rerank_fn` is provider-agnostic (reranker_standalone.rs:20-24), and the boot extractor. This closes sovereign-runtime-recipe → sovereign-inference (retires the fp-69 exception) and → sovereign-gliner.
  - Delta, in its own commit: daemon turns GAIN rerank (`RerankWiring::AlreadyInProvider` recorded none, lib.rs:250-256). Measure it with `svrn quality check --lane retrieval-prod`, which must stay in or above its band.
  LIFT ~1,200 lines. — read: engine_factory.rs, sovereign-inference/src/{engine.rs:1556-1760,reranker_standalone.rs}, sovereign-contracts/src/traits.rs:200-480, sovereign-runtime-recipe/src/lib.rs:240-260,820-1010, sovereign-daemon/src/daemon_cmd/boot.rs:580-600 — check: CLEAN, LINT, TEST(sovereign-inference), TEST(sovereign-runtime-recipe), TEST(sovereign-daemon), PLANT(register rerank without its route → the kind-registry test red), `cargo xtask env-gate`, LAYER, BOUNDARY (expect −2 and the fp-69 exception row deleted)
- [ ] pb-venues — depends [pb-serving-assembly] — OUTCOME: any OpenAI-compatible endpoint is a venue, and where each kind runs is a setting.
  - `Venue` becomes an enum { OwnSlot, ChildSlot, MeshMember(NodeId), PinnedPod, Url{ endpoint, key, declared claims } } over today's `InferenceVenue` (sovereign-contracts/src/venue.rs:19-46, mesh-only).
  - Placement per kind becomes an enum { InProcess, Child, Dial }. All three arms exist today, as the engine, the compute child and the terminal dial.
  - `[engine] kind="remote"` (engine_config.rs) absorbs the terminal node's `[node] entry` door, a second key for one question (principle 8). That door hard-codes model "primary" and sends no bearer (build/inference.rs:189,204).
  - A remote engine no longer needs a placeholder `[models]` section (build/inference.rs:225).
  - Delta: a `[node] entry` config is migrated to `[engine] remote` in the same commit (the config migration rule).
  - PROOF: an e2e points svrn at a stub OpenAI server as a `Url` venue, with no `[models]`, and a chat turn and an embedding round-trip through it. PLANT: drop the Url arm from the router, and the e2e goes red.
  LIFT ~900 lines. — read: sovereign-contracts/src/{venue.rs,engine_config.rs}, sovereign-serving-host/src/peer_inference.rs:1300-1340, sovereign-daemon/src/build/inference.rs:60-240, sovereign-turn-client/src/reach.rs:80-90 — check: CLEAN, LINT, TEST(sovereign-serving-host), TEST(sovereign-daemon), PLANT, LAYER, BOUNDARY
- [ ] pb-serve-program — depends [pb-serving-kinds, pb-venues, pb-hostkit, pb-shell] — OUTCOME: `serve` runs ALONE. One binary answers `/v1/chat/completions`, `/v1/embeddings`, `/v1/rerank` and `/v1/models` with no mesh, no knowledge server and no cw-rails. This is the "local model server" developer (FIVE_PROGRAMS §2 row `serve`).
  - FIRST, rename the phrase collision: daemon code calls cw-rails "the mesh's serving process" (daemon.rs ~:5222, setup_config `rails_base` docs). Principle 8: one name.
  - ARCH_LAYERS: a `serve` package whose members are sovereign-inference, sovereign-compute, sovereign-serving-host and sovereign-serving-policy, moved from cmnwlth. Measure the gate before and after, and name every edge the re-map moves.
  - Its own `[[bin]]` in its own crate. It promotes the compute child's server (sovereign-compute server.rs, which already has a supervisor, a wire and 5 routes) and puts serving-host's `inference_adapter` (the OpenAI translation) in front of it.
  - It gets the host-kit lock and shell, plus its own config file with its sections (models, engine, compute, shared_model).
  - `/v1/models` and the OICP manifest are built from its OWN provider (`oicp_synthesis::build_self_manifest` reads `resident_slots()`). Never from cw-rails' ledger: today the daemon's `/v1/models` is a per-request round trip to cw-rails (routes_inference.rs:29,330,359; principles 1 and 12).
  - The rpc-worker and compute-child trampolines move with it (the serving crates spawn them via `current_exe()`: compute manager.rs:309/356/390, inference rpc_distribution.rs:2405).
  - PROOF: LIFT(serve) passes. An e2e starts the `serve` binary on a temp root with the mock engine kind, and chat, embeddings, rerank and `/v1/models` each answer. PLANT: point `/v1/models` back at a rails dial, and the e2e goes red with cw-rails absent.
  LIFT ~1,500 lines. — read: SERVING_BOUNDARY.md, sovereign-compute/src/{server.rs,wire.rs,child_main.rs,manager.rs}, sovereign-serving-host/src/{inference_adapter.rs,oicp_synthesis.rs}, sovereign-daemon/src/{routes_inference.rs,state.rs:1290-1320}, quality/ARCH_LAYERS.toml package map — check: CLEAN, LINT, TEST(sovereign-compute), TEST(sovereign-serving-host), LIFT(serve), PLANT, LAYER, `cargo xtask env-gate`, BOUNDARY (paste before/after)
- [ ] pb-svrn-dials-serve — depends [pb-serve-program] — OUTCOME: svrn answers chat with serving in another process. The fp-10 and fp-68 exceptions are RETIRED by the owner, not by exception. The stock distribution dials `serve`, and a phone or single-binary distribution may still link serve's library face (FIVE_PROGRAMS §2c).
  - The daemon's serving bootstrap is deleted.
  - svrn reaches `serve` through `[engine] remote` (pb-venues) and brings it up with `ServingHost::ensure_reachable` at user-action moments only (daemon start, a verb). It never brings it up on a refused dial (ARCH_LAYERS ~:1495). An unreachable serve is a named absence (§4 rule 3).
  - svrn's `/v1/models` answers from serve's.
  - The edges sovereign-daemon → sovereign-inference, → sovereign-compute and → sovereign-serving-host close. Delete their `[[exception]]` rows.
  - BEFORE the switch commit, run the header's pre-registered bars and paste them: first-token latency, embedding throughput, retrieval-prod and synth. A miss is NEEDS_HUMAN with the numbers.
  - Delta: a stock install runs two processes. Setup and `svrn daemon status` name both.
  LIFT ~1,500 lines. — read: sovereign-daemon/src/{build/inference.rs,boot.rs,state.rs}, the fp-10/fp-68 exception rows, sovereign-turn-client/src/reach.rs — check: CLEAN, LINT, TEST(sovereign-daemon), TEST(sovereign-cli-daemon), the bars above, PLANT(stop serve mid-test → the chat turn reports absence, never an empty answer), LIFT(svrn) (paste the verdict; it may still be 0 for other edges), LAYER, BOUNDARY (expect −3)
- [ ] pb-membership — depends [pb-hostkit] — OUTCOME: cw-rails is the node's ONE mesh endpoint and holds its ONE node key. Founding, joining, admission and mDNS move to cw-rails (reversing five-programs-21's disclaimer, operator 2026-09-25), and the fp-9 exception (sovereign-daemon → commonwealth-discovery) is RETIRED.
  - commonwealth-discovery is already on cw-rails' except list (ARCH_LAYERS ~:686).
  - Solo mode (fp-solo-lift) covers a lone node.
  - The daemon's `~/.svrnmesh/node_key` retires. cw-rails signs fp-74's attestation at the daemon's request.
  - Delta: an existing install's key migrates in the SAME commit as the flip, because peers' rosters hold the daemon's key (five-programs-38 (2)).
  - PROOF: an e2e founds a mesh with cw-rails alone, and a second cw-rails joins by invite. A svrn daemon on a joined node signs through rails. Migration test: a node with only the old daemon key keeps its roster identity after upgrade. PLANT: skip the migration, and the roster test goes red.
  LIFT ~1,500 lines. — read: commonwealth-rails/src/{lib.rs:20-40,identity.rs}, commonwealth-discovery, sovereign-contracts/src/node_identity.rs, sovereign-daemon mesh join/admission sites, ralph/decisions/five-programs-{21,38,39}.md — check: CLEAN, LINT, TEST(commonwealth-rails), TEST(commonwealth-discovery), TEST(sovereign-daemon), LIFT(cmnwlth), PLANT, LAYER, BOUNDARY (expect the fp-9 exception row deleted)
- [ ] pb-daemon-mesh-exit — depends [pb-membership] — OUTCOME: the svrn daemon is no longer a mesh endpoint. It dials cw-rails for everything mesh.
  - Delete the daemon's copies of routes cw-rails serves. Duplicate ownership is principle 8. The routes: `/v1/mesh/status` (mesh_http.rs:36), `/v1/mesh/kv/*` (routes_mesh_kv.rs:44-47), `/v1/rail/{append,log}` and `/internal/gossip` (server.rs:277-278, :349).
  - Close the transport residue: peer_contact, iroh, identity, mesh_proof and fanout (~42 src lines in 19 files). First check whether `identity::load_or_create_client_token`/`generate_bearer_token` are svrn's own client-API auth; if they are, they MOVE to svrn and are not dialed.
  - Close the inherited REVIEW-mint-fp-core-dial and REVIEW-mint-fp-mesh-dial residue (their five-programs rows carry the sites).
  - Edges: sovereign-daemon → commonwealth-transport, → commonwealth-core, → sovereign-mesh.
  - PROOF: the daemon e2e suite passes with cw-rails serving every mesh route, and each deleted route on the daemon answers a named pointer where a client might still call it. PLANT: re-mount one deleted route, and the duplicate-route test goes red.
  LIFT ~1,500 lines; split by edge if the census exceeds it. — read: five-programs STATE.md rows REVIEW-mint-fp-core-dial and REVIEW-mint-fp-mesh-dial, sovereign-daemon/src/{mesh_http.rs,routes_mesh_kv.rs,server.rs,daemon.rs}, commonwealth-rails/src/api.rs — check: CLEAN, LINT, TEST(sovereign-daemon), TEST(commonwealth-rails), PLANT, LAYER, BOUNDARY (expect −3)
- [ ] pb-inference-origin — depends [pb-membership, pb-venues, pb-serve-program] — OUTCOME: the mesh fronts ANY OpenAI-compatible server (the "mesh in front of vLLM/ollama" developer).
  - `OriginKind::Inference` in oicp-types (origin.rs:21-26: "a new kind is a new variant beside a new route").
  - cw-rails adverts it and never ranks (its charter). cw-rails gossips `inference_capable: false` today (gossip.rs:79).
  - The origin's manifest comes from the origin itself (oicp-conformance exists) or from an operator-declared claims file that cw-rails serves verbatim (data, principle 9).
  - serve's router ranks venues. Its `VenueSource` reads cw-rails' HTTP roster in place of the in-process DeferredDaemon.
  - PROOF: an e2e puts a stub OpenAI server behind cw-rails as an Inference origin with a claims file, and a second node's `serve` routes a chat turn to it. PLANT: drop the claims file, and the venue drops out with a named reason.
  LIFT ~1,000 lines. — read: oicp-types/src/origin.rs, commonwealth-rails/src/gossip.rs, sovereign-serving-host/src/peer_inference.rs:1300-1340 — check: CLEAN, LINT, TEST(oicp-types), TEST(commonwealth-rails), TEST(sovereign-serving-host), LIFT(cmnwlth), PLANT, LAYER, BOUNDARY
- [ ] pb-work-doors — depends [pb-membership] — OUTCOME: svrn submits and takes work through cw-rails' own doors. cw-rails has owned the journal and the fold since fp-45 (49578c1e2).
  - cw-rails gets two doors: submit, and take/complete. Acts are sealed with the one node key (pb-membership).
  - Act DTOs go into oicp-types beside JobKind, JobRequirements and JobUnit (§12 3a rung 2, federation wire).
  - sovereign-cli distribute.rs (15 lines) and the daemon's work_donor.rs/ingest_executor.rs (28 src lines in 7 files) dial the doors. Executors stay program-side.
  - Rejected: §12's daemon submitter route, which is a proxy (five-programs-39).
  - Edges: sovereign-cli → commonwealth-work, sovereign-daemon → commonwealth-work.
  - PROOF: an e2e submits a job from `svrn`, a donor takes it through the door, and the fold shows it complete. PLANT: an unsealed act is refused.
  LIFT ~900 lines. — read: commonwealth-work/src/{executor.rs,process.rs}, commonwealth-rails/src/api.rs:98, sovereign-cli/src/distribute.rs, sovereign-daemon/src/{work_donor.rs,ingest_executor.rs} — check: CLEAN, LINT, TEST(commonwealth-rails), TEST(commonwealth-work), TEST(sovereign-daemon), PLANT, LAYER, BOUNDARY (expect −2)
- [ ] pb-code-server — depends [pb-mcp, pb-shell] — OUTCOME: code intelligence runs ALONE as an MCP server, with no LLM, no knowledge server and no mesh (the "code intel for my agent" developer).
  - `svrn code mcp` = the host kit + CodeIntelTools + NotesTools (decision notes) + the work-atlas bundle. The atlas is optional: it dials cw-rails' KV directly, never the daemon's `/v1/mesh/kv` proxy. When cw-rails is down it is absent by name (the null object `Withheld`, tool_bundle.rs:298).
  - Connect-or-spawn goes through `ServingHost`.
  - One SCIP loader (tool_registry.rs:384 and sovereign-cli-shared/src/scip.rs:31 collapse).
  - One freshness path: the `Reindexer`. The 30 s mtime poll (serve.rs:731) goes.
  - The watcher runtime is owned here, which closes sovereign-daemon → corpus-engine-watchers.
  - SpecWatcher (380 lines) and the spec-gated list move to sovereign-code, which closes sovereign-cli-dev → sovereign-tools.
  - Defects, fixed in their own commits:
    - `code_search`'s `inf.embed(query).await.unwrap_or_default()` (code_search.rs:123) becomes a named fall-back, never a silent swap to FTS;
    - the in-memory notes fallback (serve.rs:251) becomes a refusal that names the path.
  - Delta: legacy `project serve` stops refusing while the daemon runs (serve.rs:41-62), because it no longer shares the daemon's surface.
  - PROOF: LIFT(code) passes. An e2e runs `svrn code mcp` on a fixture repo with NO daemon and no model, and `symbols`/`callers` answer. PLANT: make the atlas dial the daemon proxy, and with the daemon absent the atlas test goes red.
  LIFT ~1,500 lines. — read: sovereign-code/src/bundle.rs, sovereign-cli-dev/src/project_cmd/serve.rs, sovereign-tools/src/code/code_search.rs, corpus-engine-watchers/src/reindexer.rs, sovereign-work-atlas — check: CLEAN, LINT, TEST(sovereign-code), TEST(sovereign-cli-dev), TEST(sovereign-work-atlas), LIFT(code), PLANT, LAYER, BOUNDARY (expect −2)
- [ ] pb-ingest — depends [REVIEW-pb-census] — OUTCOME: an index builds in CI with only an embeddings endpoint (the "ingest in CI" developer). Ingest is a library plus ONE CLI (FIVE_PROGRAMS §2 row `svrn ingest`).
  - The working precedent is `corpus-mcp ingest` (corpus-mcp/src/ingest.rs:319-329; its `tests/no_inference_stack.rs` pins the closure).
  - Fold `svrn corpus ingest`'s workflow path into that one CLI. That path posts to the daemon and runs the notebook workflow (corpus_cmd/ingest.rs:108, workflow_cmd.rs:595), and it is a second ingest implementation (principle 8).
  - ONE endpoint-resolution decider for embedder and chat: corpus-mcp's host.rs discovery ladder, reused. `code index` and bench use it later.
  - Fixed in their own commits:
    - the chat model is no longer resolved under `--no-enrich` (corpus-mcp/src/ingest.rs:225-242);
    - GLiNER absence is named, not silently skipped (corpus-engine/src/engine/ingest.rs:1927).
  - The work plane is one OPTIONAL caller, never a requirement.
  - PROOF: LIFT(ingest) passes. An e2e ingests a fixture folder against a stub embeddings server with no chat model, and the index opens. PLANT: resolve chat under `--no-enrich`, and the e2e goes red.
  LIFT ~1,000 lines. — read: corpus-mcp/src/{ingest.rs,host.rs}, sovereign-cli/src/corpus_cmd/ingest.rs, workflow_cmd.rs, corpus-engine/src/engine/ingest.rs:100-130,1400-1420,1920-1935 — check: CLEAN, LINT, TEST(corpus-mcp), TEST(corpus-engine), LIFT(ingest), PLANT, LAYER, BOUNDARY
- [ ] pb-code-index — depends [pb-code-server, pb-ingest] — OUTCOME: indexing a project needs only the code program, and `project init` works with no daemon.
  - `code_index` plus incremental (1,495 lines) and `code_refresh` (561) move from sovereign-cli-shared into the code program. D5 places them there. fp-5's refusal falls, because code now ships in the default distribution.
  - `code index` and `project refresh` stop refusing without the daemon (code_index.rs:260-271,451-455).
  - `project init`'s index step dials code by connect-or-spawn with an explicit `--fts-only` when there is no embedder. This closes D6, sovereign-cli → corpus-engine (project_init/mod.rs:30,516).
  - Embedder through pb-ingest's one endpoint decider.
  - Defect, fixed in its own commit: with no embedder, write `vector=false` and say so in the banner. Never zero vectors stamped with `configured_embed_model_name()` (project_init/mod.rs:505-513).
  - Edges: sovereign-cli-shared → corpus-engine, sovereign-cli-dev → sovereign-cli-shared, sovereign-cli-dev → corpus-engine (fp-34 residue), sovereign-cli → corpus-engine.
  - PROOF: an e2e runs `svrn project init` then `svrn code index` on a fixture repo with no daemon and no embedder, and `code_search` answers from FTS with the banner naming it. PLANT: stamp the model name, and the metadata test goes red.
  LIFT ~1,500 lines. — read: sovereign-cli-shared/src/{code_index.rs,scip.rs}, sovereign-cli/src/project_init/mod.rs, sovereign-cli-dev code_refresh — check: CLEAN, LINT, TEST(sovereign-cli), TEST(sovereign-cli-dev), TEST(sovereign-code), LIFT(code), PLANT, LAYER, BOUNDARY (expect −4)
- [ ] pb-code-clean — depends [pb-code-server, pb-code-index] — OUTCOME: the code program links nothing of svrn, and svrn serves no code tool, so `symbols` is served in exactly one place.
  - The daemon stops registering code tools. svrn's in-process Runtime (tool_registry.rs:43) dials code's MCP and reports absence when code is unreachable (§4 rule 3).
  - A moved tool called on svrn answers a named pointer: "moved to svrn code; `svrn project init` updates your config" (five-programs-39).
  - The scaffold (sovereign-cli/src/project_init/scaffold.rs:509-536) writes both servers. This repo's .mcp.json and .opencode/opencode.json change with it.
  - The daemon stops constructing the work atlas (bootstrap.rs, tool_registry.rs). The mesh test tree's atlas test moves to the work-atlas crate's own tests.
  - sovereign-cli-dev backlog_cmd/score.rs:15,19 repoints to oicp-client, a package_leaf (ARCH_LAYERS ~:914) that already sends json_schema (oicp-client/src/lib.rs:758-765).
  - Edges: sovereign-daemon → sovereign-code (fp-11), sovereign-daemon → sovereign-work-atlas, sovereign-mesh → sovereign-work-atlas (dev), sovereign-cli-dev → sovereign-enrichment-build.
  - PROOF: calling `symbols` on the svrn MCP surface returns the pointer, and on code's it answers. LIFT(code) passes. PLANT: re-register one code tool on the daemon, and the one-home test goes red.
  LIFT ~1,000 lines. — read: sovereign-daemon/src/{tool_registry.rs,bootstrap.rs}, scaffold.rs, backlog_cmd/score.rs — check: CLEAN, LINT, TEST(sovereign-daemon), TEST(sovereign-cli), TEST(sovereign-cli-dev), TEST(sovereign-mesh), LIFT(code), PLANT, LAYER, BOUNDARY (expect −4)
- [ ] pb-notes-split — depends [pb-mcp, pb-code-server] — OUTCOME: svrn keeps its memory with NO code program, and code keeps its decision notes (the "svrn without code" developer).
  - notes.db holds four populations by `kind`, in three files. There are three path deciders: sovereign-cli-dev backlog_cmd/item.rs:30, sovereign-cli-llm awareness_cmd/store_open.rs:60 and sovereign-contracts middleware.rs:165. One query returns 68 notes from one directory and 6,811 from another (item.rs:27).
  - Split by OWNER (principle 12):
    - svrn's memory (lessons Global; the tool_decision dossier and commitments Session: memory.rs:1606, lessons.rs:623, commissive.rs:80) goes to svrn's own store, converging on the StateStore memory port (`save_memory`, sovereign-contracts traits.rs:1277);
    - decision notes go to code;
    - the tool-call log goes to the MCP host's call-log port (pb-mcp);
    - recipe-author notes (Feature scope) go to ingest.
  - One path decider per store.
  - Delta: existing rows MIGRATE in the same commit as the switch.
  - Fixed in their own commits:
    - chat's `knowledge_lookup` stops treating every kind as evidence, and its `unwrap_or_default` error collapse becomes a named absence (knowledge_lookup/mod.rs:294-300);
    - lessons are not gossip-eligible unless the user opts in. Verify the desktop's `private: false` write first.
  - Closes the four notes-factory edges: sovereign-cli, sovereign-cli-llm, sovereign-daemon and sovereign-tools → corpus-engine-notes.
  - PROOF: an e2e runs svrn chat with the code program absent, and a lesson is written, recalled after restart, and not visible to code's `notes` tool. Migration test: a fixture notes.db splits with row counts conserved. PLANT: skip the migration, and the count test goes red.
  LIFT ~1,500 lines. — read: sovereign-contracts/src/{notes.rs,middleware.rs,traits.rs:1260-1290}, corpus-engine-notes/src/port.rs, sovereign-core memory/lessons/commissive, knowledge_lookup/mod.rs — check: CLEAN, LINT, TEST(sovereign-core), TEST(corpus-engine-notes), TEST(sovereign-daemon), PLANT, LAYER, BOUNDARY (expect −4)
- [ ] pb-ingest-dial — depends [pb-ingest, pb-work-doors] — OUTCOME: svrn executes no ingest in-process. Its residue dials the ingest CLI.
  - sovereign-tools' MCP tools that EXECUTE ingest (235 lines in 56 files, knowledge_view/manager.rs and conv_tiered_provider.rs) submit jobs or exec the ingest CLI. The daemon's corpus-engine residue (104 src lines in 33 files) does the same.
  - The legacy pull loop is deleted ("replaced, not yet deleted", ingest_executor.rs:18-24).
  - `IngestExecutor` retires ONLY after `process:v1` has been watched carrying real ingest traffic. Paste the run.
  - sovereign-mesh → corpus-engine (6) and sovereign-runtime-recipe → corpus-engine (11, lib.rs) take ports or dial.
  - sovereign-tools → sovereign-recipe-author (bundles.rs, fp-29 RecipeProjectStore) is re-placed by the ladder.
  - Edges: sovereign-daemon, sovereign-tools, sovereign-mesh and sovereign-runtime-recipe → corpus-engine; sovereign-tools → sovereign-recipe-author.
  - PROOF: an e2e runs a svrn chat turn that triggers an ingest, and it completes through the CLI with the daemon linking no corpus-engine writer. PLANT: call the engine in-process, and LAYER goes red.
  LIFT ~1,500 lines; split by edge if the census exceeds it. — read: sovereign-daemon/src/ingest_executor.rs, sovereign-tools knowledge_view, the phase-b appendix ingest-dial lines — check: CLEAN, LINT, TEST(sovereign-tools), TEST(sovereign-daemon), TEST(sovereign-runtime-recipe), PLANT, LAYER, BOUNDARY (expect −5)
- [ ] pb-ingest-rehome — depends [pb-ingest-dial] — OUTCOME: every ingest capability lives in ingest.
  - corpus-mcp is split by verb: ask and search stay svrn; ingest, recipe and serve-pull go to ingest. This closes corpus-mcp → corpus-engine and → sovereign-enrichment-build.
  - "Watch a folder" becomes an ingest verb (§4 rule 1). sovereign-tools watched/enrich.rs and atlas_*'s `EnrichConfig::load` dial it, which closes sovereign-tools → sovereign-enrichment-catalog.
  - Grants keeps deciding and hands the merge to ingest as a work unit (auto_recover.rs:574,600, shard_manager.rs:703), which closes sovereign-grants → corpus-engine.
  - fp-43, the authoring harness's drive home, closes sovereign-cli-llm → and sovereign-daemon → sovereign-authoring-harness.
  - corpus-engine's bundled recipe source (recipe_source/bundled.rs, five-programs-52) lifts to svrn's composition roots.
  - PROOF: LIFT(ingest) and LIFT(svrn) are pasted (svrn may still be 0 for edges owned by later rows). A grants merge e2e completes as a work unit. PLANT: link corpus-engine from grants, and LAYER goes red.
  LIFT ~1,500 lines. — read: corpus-mcp/src, sovereign-tools watched/enrich.rs, sovereign-grants auto_recover.rs, shard_manager.rs, five-programs STATE.md row fp-43, corpus-engine/src/recipe_source/bundled.rs — check: CLEAN, LINT, TEST(corpus-mcp), TEST(sovereign-grants), TEST(sovereign-tools), LIFT(ingest), PLANT, LAYER, BOUNDARY (expect −6)
- [ ] pb-bench-dials — depends [REVIEW-pb-census] — OUTCOME: bench judges ANY endpoint (the "bench my own server" developer).
  - Three named dials: the model (plain OpenAI API), the subject (svrn, for the turn and verdict lanes) and a SEPARATE judge (principle 7). Today judges share the URL under test, and only `bench/external/*` takes `--base-url`.
  - Every lane is tagged with the dials it needs. Against a foreign server, the model-only lanes run and the rest report could-not-judge with the reason.
  - The quality-check precondition `port-listening:9741` (32 uses in quality/instruments.toml) honours the subject dial and `SOVEREIGN_DAEMON_URL` (quality_check_cmd/exec.rs:33-37).
  - `eval run` lanes dial svrn in place of building an in-process Runtime (chat_cmd/bootstrap.rs:205-394).
  - Defect, fixed in its own commit: the forced-choice judge's `.ok()?` fails OPEN on non-sovereign servers (grounding/judge.rs:157-173). It becomes could-not-judge.
  - PROOF: LIFT(bench) passes. A lane run against a stub OpenAI server yields model-lane verdicts plus named could-not-judge rows. PLANT: restore fail-open, and the judge test goes red.
  LIFT ~1,200 lines. — read: sovereign/bench/README.md, quality/instruments.toml, quality_check_cmd/exec.rs, grounding/judge.rs, chat_cmd/bootstrap.rs — check: CLEAN, LINT, TEST(sovereign-eval), TEST(sovereign-cli-llm), LIFT(bench), PLANT, LAYER, BOUNDARY
- [ ] pb-cli-llm — depends [pb-serve-program, pb-bench-dials, pb-ingest] — OUTCOME: sovereign-cli-llm is a client. It links no serving, ingest or mesh internals.
  - RE-CENSUS FIRST, because five-programs-23's own falsifier is now true: `help` lives in sovereign-cli-base since fp-98; `sovereign_core::{types, setup_config, traits, tool_manifest, error, rebrand}` re-export contracts (sovereign-core/src/lib.rs:66-69); the 25 `::remote` refs re-export oicp-client. Repoint those (paths only).
  - Bench's turn drive (chat_cmd, 32 refs in 15 files) dials the subject.
  - The three local GGUF loads dial serve's embedder: router_fit_cmd.rs:403, router_cache_cmd.rs:231, inner_chaos/recall.rs:752. That leaves one embedder decider.
  - Node identity repoints to `sovereign_contracts::node_identity` (portfolio_cmd/mod.rs:59, partitions.rs:863/865). The hand re-implementation of `resolve_self_node_id`'s precedence (partitions.rs:863-879) goes.
  - The ingest half moves to the ingest CLI.
  - Edges: cli-llm → sovereign-enrichment-catalog, sovereign-enrichment-build (normal and dev), sovereign-gliner, sovereign-pipeline, sovereign-eval, corpus-engine, sovereign-inference, sovereign-mesh.
  - PROOF: LAYER passes with those deps removed from cli-llm's Cargo.toml, and the cli-llm e2e journeys pass. PLANT: re-add one local GGUF load, and LAYER goes red.
  LIFT ~1,500 lines; split by edge if the census exceeds it. — read: FIVE_PROGRAMS §11 "The cli-llm split", ralph/decisions/five-programs-{11,23}.md, sovereign-cli-llm/src — check: CLEAN, LINT, TEST(sovereign-cli-llm), PLANT, LAYER, BOUNDARY (expect −9)
- [ ] pb-pods-verb — depends [pb-membership] — OUTCOME: provisioning pods is a cmnwlth verb.
  - `svrn pipeline pod {up,pool,list,down}` (pipeline_cmd.rs:674 onward) plus worker_pod_provider.rs (391 lines) move to sovereign-cli-mesh as `svrn mesh pod …`.
  - `sovereign_pipeline::pod` (340 lines, with no ingest consumer) moves to sovereign-pods. cli-mesh already declares sovereign-pods (Cargo.toml:50) and uses it nowhere.
  - Delta: the old spelling answers with a named pointer to the new one (principle 6).
  - Closes sovereign-cli-llm → sovereign-pods and 5 of the mesh sites.
  - PROOF: `svrn mesh pod list` answers against the pod provider test double, and `svrn pipeline pod list` prints the pointer and exits non-zero. PLANT: remove the pointer, and the CLI contract test goes red.
  LIFT ~1,400 lines, mostly moves. — read: sovereign-cli-llm/src/{pipeline_cmd.rs,worker_pod_provider.rs}, sovereign-pipeline/src/pod.rs, sovereign-cli-mesh/Cargo.toml — check: CLEAN, LINT, TEST(sovereign-cli-mesh), TEST(sovereign-pods), `sovereign contract census`, PLANT, LAYER, BOUNDARY
- [ ] pb-meshapp-rehome — depends [pb-membership, pb-serving-kinds] — OUTCOME: the svrn daemon embeds no mesh application.
  - The seven fp-12 pairs are re-homed by the §12 3a ladder: sovereign-daemon → sovereign-meshapp-registry, sovereign-meshapp, sovereign-grants, code-next-edit, sovereign-tdd, sovereign-pods and sovereign-gliner.
  - gliner is closed by pb-serving-kinds; confirm it.
  - code-next-edit goes to the program §2 says serves FIM.
  - pods follows the worker exec split (TSV:23).
  - fp-47's app registry gets its one owner (sovereign-daemon → commonwealth-media).
  - The five-programs rows fp-12 and fp-47 carry the census and the HUMAN answers.
  - PROOF: each moved app's journey passes on its new host. PLANT: re-add one dependency, and LAYER goes red.
  LIFT ~1,500 lines; split by pair if the census exceeds it. — read: five-programs STATE.md rows fp-12, fp-47, HUMAN-fp12-daemon-embeds, HUMAN-fp47-app-registry — check: CLEAN, LINT, TEST(sovereign-daemon), TEST(sovereign-meshapp), TEST(sovereign-grants), PLANT, LAYER, BOUNDARY (expect −7)
- [ ] pb-contracts — depends [pb-serve-program, pb-notes-split, pb-hostkit] — OUTCOME: a program that takes the shared layer takes only shared vocabulary. The §12 3a ladder is re-applied to sovereign-contracts itself (42,441 lines, 58 modules, named by 51 manifests).
  - Single-program modules MOVE to their owners (re-exported at their historical paths until the last consumer repoints):
    - svrn: skills, intent_policy, data_roots, tool_result_cache, guest_pages, observer, memory_config, mcp_config, lessons; types' grounding_journal, stage_attribution and grounding_verdict; daemon_wire's svrn-only half.
    - serve/cmnwlth: fim, worker_pod, local_inference, build_stamp.
  - Traits whose implementer and consumer are both svrn (~14: TaskStore, MemoryStore, DocumentStore, BudgetStore, InsightStore, the oracles…) move into svrn.
  - DELETE mobile_host.rs (it targets the deleted sovereign-server) and `Launch::Server`. mobile_cmd.rs:108 loses its caller or gets a named pointer.
  - `egress` (reqwest; 125 crates of closure) moves to its owner.
  - Fix the stale "24,619 lines" leaf comment (ARCH_LAYERS ~:899-903).
  - PROOF: bench's closure crate count, before and after (measured by REVIEW-pb-census). LIFT(bench) and LIFT(serve) pass. PLANT: re-add a moved module to contracts' lib.rs, and the single-owner census test fails. Add that census as a test: a contracts module named by one program only is red.
  LIFT ~1,500 lines, mostly moves; split by owner if the census exceeds it. — read: sovereign-contracts/src/lib.rs, quality/ARCH_LAYERS.toml contracts leaf row, FIVE_PROGRAMS §12 3a — check: CLEAN, LINT, TEST(sovereign-contracts), TESTALL, LIFT(bench), PLANT, LAYER, BOUNDARY
- [ ] pb-provider-split — depends [pb-svrn-dials-serve, pb-contracts] — OUTCOME: a client of a model server names only the dial surface.
  - `InferenceProvider` (traits.rs:298, 29 methods, 114 impl lines in 75 files) splits by who needs what:
    - the 10 dial methods (the `complete` family, embed, rerank) go to the wire side, beside oicp-client;
    - the 10 metadata methods stay with them if a dialer reads them, otherwise they go to serve;
    - the 7 slot-administration methods (warmup, load/unload_extra_slot, compute_children, …) go to serve;
    - the 2 mesh methods (peer_manifests, lender_manifest) go to serve's router.
  - One dimension per commit: introduce the narrow trait, repoint consumers, then shrink.
  - PROOF: sovereign-cli-llm, sovereign-eval and sovereign-core name only the dial trait (grep in the body). PLANT: call a slot-admin method from sovereign-core, and it fails to compile.
  LIFT ~1,500 lines. — read: sovereign-contracts/src/traits.rs:290-700, the 75 impl files' census — check: CLEAN, LINT, TESTALL, PLANT, LAYER, BOUNDARY
- [ ] pb-config-split — depends [pb-serve-program, pb-membership, pb-contracts] — OUTCOME: each program reads ONLY its own config file (FIVE_PROGRAMS §1, §4 rule 7). Today `SetupConfig` (setup_config.rs:32) is one 13-section schema across programs, loaded in 13 crates.
  - The split:
    - serve: models, engine, compute, shared_model;
    - cmnwlth: node, iroh, discovery;
    - svrn: daemon, data, memory, search, mcp_servers, watched_folders.
    cw-rails already has its own (commonwealth-rails/src/config.rs:129).
  - Setup, the distribution, writes all of them.
  - `client_daemon_base` moves to sovereign-turn-client, the dialer. Its parse-error-becomes-default-port substitution becomes a named error (principle 6).
  - Delta: an existing `~/.svrnmesh/config.toml` MIGRATES in the same commit as the switch. The old file is kept as `.migrated` and the migration is idempotent.
  - PROOF: a migration test takes a fixture full config and produces per-program files that each program loads, with no section lost. Each program also starts with only its own file. PLANT: drop one section in the migration, and the conservation test goes red.
  LIFT ~1,500 lines. — read: sovereign-contracts/src/setup_config.rs, commonwealth-rails/src/config.rs, the 13 `SetupConfig::load` crates — check: CLEAN, LINT, TESTALL, `cargo xtask env-gate`, PLANT, LAYER, BOUNDARY
- [ ] pb-distribution — depends [pb-svrn-dials-serve, pb-daemon-mesh-exit, pb-inference-origin, pb-code-clean, pb-notes-split, pb-ingest-rehome, pb-cli-llm, pb-pods-verb, pb-meshapp-rehome, pb-provider-split, pb-config-split] — OUTCOME: distributions are declared, every program runs alone, and Phase B is done.
  - `[[distribution]]` rows in quality/ARCH_LAYERS.toml, extending `[thin_surfaces]`, for: the `svrn` dispatcher (sovereign-cli), the setup wizard (sovereign-cli-daemon setup), sovereign-service and the desktop shell.
  - Each distribution may exec program binaries and link the wire leaves, the host kit, sovereign-turn-client and declared library faces. Its own code is capped as wiring (the operator sets the cap). layer-gate enforces it.
  - fp-25, setup's exec phase (sovereign-cli-daemon → sovereign-inference), closes: setup execs `serve` for model validation and download. That changes the Windows sidecar build contract (stage-daemon-sidecar.sh:91, `SOVEREIGN_SIDECAR_FEATURES`), which five-programs-38 names; update it in the same commit.
  - FINISH:
    - boundary-gate exits 0;
    - no `package = "svrn"` exception remains;
    - all six LIFTs pass;
    - the §2c copy counts are 1, with the final table in the body;
    - full suite and lint --full are green.
  LIFT ~1,000 lines. — read: FIVE_PROGRAMS §2c and §12 "Done", quality/ARCH_LAYERS.toml `[thin_surfaces]`, five-programs STATE.md row fp-25 and HUMAN-fp25-setup-host — check: CLEAN, `./scripts/sovereign-lint.sh --human --full`, TESTALL, PREPUSH, LIFT(svrn), LIFT(ingest), LIFT(cmnwlth), LIFT(serve), LIFT(code), LIFT(bench), LAYER, BOUNDARY (exit 0)

## Appendix — the handoff list

Written by five-programs' `REVIEW-handoff-phase-b` on 2026-09-25, at the gate on `cut` after 83fbdc620:
**boundary-gate 51 violation(s)**, 51 dep edges (49 normal, 2 dev) and 0 structural. The build.rs line closed with fp-105.
Every edge has exactly one appendix line in ralph/next/five-programs/STATE.md. The per-edge census, with sites
and counts, lives on that line, and the class is repeated here.

Owner histogram: 16 edges to the 8 inherited rows (fp-12 7, fp-11 2, fp-43 2, fp-25 1, fp-34 1, fp-47 1,
REVIEW-mint-fp-core-dial 1, REVIEW-mint-fp-mesh-dial 1). 35 edges to `phase-b`: 32 were NEEDS-OPERATOR lines,
and 3 are the ingest-dial class that HUMAN-fp7 (a) folded fp-7 and fp-29 into. Unowned: 0. There are also 5
excepted edges that are not red and that Phase B retires.

### Inherited rows (re-mint into their campaigns)

- [svrn] sovereign-cli-daemon → sovereign-inference — fp-25 (setup's exec phase, HUMAN-fp25 (a))
- [code] sovereign-cli-dev → sovereign-daemon — fp-11
- [svrn] sovereign-daemon → sovereign-code — fp-11 (the code MCP host; five-programs-39's MCP rule)
- [code] sovereign-cli-dev → corpus-engine — fp-34 (read mounts after fp-11's /mcp root; `code finalize`/`code watch` residue)
- [svrn] sovereign-cli-llm → sovereign-authoring-harness — fp-43 (the drive home, now an ingest dial)
- [svrn] sovereign-daemon → sovereign-authoring-harness — fp-43
- [svrn] sovereign-daemon → commonwealth-media — fp-47 (app registry's one owner)
- [svrn] sovereign-daemon → sovereign-meshapp-registry / sovereign-meshapp / sovereign-grants / code-next-edit / sovereign-tdd / sovereign-pods / sovereign-gliner — fp-12 (seven pairs)
- [svrn] sovereign-daemon → commonwealth-core — REVIEW-mint-fp-core-dial (fp-6 roster landed; residue)
- [svrn] sovereign-daemon → sovereign-mesh — REVIEW-mint-fp-mesh-dial (fp-6..8 verbs landed; residue)

### phase-b (open classes: each needs an operator decision or a Phase B host)

- ingest-dial class (corpus-mcp membership; HUMAN-fp7 (a) keeps canonical pull and the executor daemon-side):
  [svrn] corpus-mcp → corpus-engine, [svrn] corpus-mcp → sovereign-enrichment-build,
  [svrn] sovereign-daemon → corpus-engine (183 residue leaves), [cmnwlth] sovereign-mesh → corpus-engine (6),
  [svrn] sovereign-tools → sovereign-recipe-author (fp-29: RecipeProjectStore, the shim is load-bearing),
  [svrn] sovereign-tools → corpus-engine (170 residue refs that EXECUTE ingest as MCP tools),
  [svrn] sovereign-runtime-recipe → corpus-engine
- cli-llm split (five-programs-23; shared CLI-helper home, bench turn drive): [svrn] sovereign-cli-llm →
  sovereign-enrichment-catalog, → sovereign-enrichment-build (normal and dev), → sovereign-gliner,
  → sovereign-pipeline, → sovereign-eval, → corpus-engine (397 residue leaves), → sovereign-inference (3 local GGUF loads)
- node identity (five-programs-38/-39, lands with fp-9/fp-10's retirement): [svrn] sovereign-cli-llm → sovereign-mesh
- provisioning: [svrn] sovereign-cli-llm → sovereign-pods
- notes factory: [svrn] sovereign-cli → corpus-engine-notes, [svrn] sovereign-cli-llm → corpus-engine-notes,
  [svrn] sovereign-daemon → corpus-engine-notes, [svrn] sovereign-tools → corpus-engine-notes
- code_index's home (fp-5's refusal stands, REVIEW-mint-fp-cli-shared-leaf 59c38d4b8): [svrn] sovereign-cli-shared →
  corpus-engine, [code] sovereign-cli-dev → sovereign-cli-shared
- wire boundary (§12 recommendation untaken; fp-45's `process` executor seam): [svrn] sovereign-cli → commonwealth-work,
  [svrn] sovereign-daemon → commonwealth-work
- D6 keep, daemon-route closing condition untaken: [svrn] sovereign-cli → corpus-engine
- transport leaf question: [svrn] sovereign-daemon → commonwealth-transport
- work-atlas question (five-programs-54): [svrn] sovereign-daemon → sovereign-work-atlas, [cmnwlth] sovereign-mesh →
  sovereign-work-atlas (dev, fp-84's)
- watcher runtime owner (TSV:12): [svrn] sovereign-daemon → corpus-engine-watchers
- [svrn] sovereign-runtime-recipe → sovereign-gliner — the runtime lane's entity-extractor probe (load_gliner)
- [svrn] sovereign-tools → sovereign-enrichment-catalog — the watched-folder config writer's owner
- [code] sovereign-cli-dev → sovereign-tools — SpecWatcher + the MCP surface list
- [code] sovereign-cli-dev → sovereign-enrichment-build — scoring types (ChatPrompt)
- [cmnwlth] sovereign-grants → corpus-engine — dial or drop

### Excepted edges Phase B retires (not red; quality/ARCH_LAYERS.toml [[exception]] rows with `package = "svrn"`)

- sovereign-daemon → commonwealth-discovery — fp-9
- sovereign-daemon → sovereign-inference, sovereign-daemon → sovereign-compute — fp-10
- sovereign-daemon → sovereign-serving-host — fp-68
- sovereign-runtime-recipe → sovereign-inference — fp-69

### Non-edge item

- corpus-engine's default recipe/asset source (five-programs-52): `corpus-engine/src/recipe_source/bundled.rs` is the
  one corpus-engine module that names the `sovereign-recipes` data crate (`corpus_engine_recipes`), behind
  `default_source()` / `default_assets()`. It lifts out to svrn's composition roots once svrn dials ingest. Owner:
  the ingest serving campaign.

### Owner rows (phase-b-1, 2026-09-25; REVIEW-pb-census reconciles against the merged tree)

Each red edge has exactly one owner row:

- **pb-mcp:** cli-dev → daemon (fp-11).
- **pb-code-clean:** daemon → sovereign-code (fp-11); daemon → work-atlas; mesh → work-atlas (dev); cli-dev → enrichment-build.
- **pb-code-index:** cli-dev → corpus-engine (fp-34); cli-shared → corpus-engine; cli-dev → cli-shared; cli → corpus-engine (D6).
- **pb-code-server:** daemon → corpus-engine-watchers; cli-dev → sovereign-tools.
- **pb-ingest-rehome:** cli-llm → authoring-harness and daemon → authoring-harness (fp-43); corpus-mcp → corpus-engine; corpus-mcp → enrichment-build; tools → enrichment-catalog; grants → corpus-engine; the recipes default source (the non-edge item).
- **pb-ingest-dial:** daemon, tools, mesh and runtime-recipe → corpus-engine; tools → recipe-author.
- **pb-meshapp-rehome:** daemon → commonwealth-media (fp-47); the seven fp-12 pairs (gliner via pb-serving-kinds).
- **pb-daemon-mesh-exit:** daemon → commonwealth-core; daemon → sovereign-mesh; daemon → commonwealth-transport.
- **pb-cli-llm:** the cli-llm split class (8 edges) and cli-llm → sovereign-mesh (node identity).
- **pb-pods-verb:** cli-llm → sovereign-pods.
- **pb-notes-split:** the notes factory (4 edges).
- **pb-work-doors:** cli → commonwealth-work; daemon → commonwealth-work.
- **pb-serving-kinds:** runtime-recipe → gliner; runtime-recipe → inference (the fp-69 exception).
- **pb-distribution:** cli-daemon → inference (fp-25).

Excepted edges and their owner rows:

- **pb-membership:** daemon → commonwealth-discovery (fp-9).
- **pb-svrn-dials-serve:** daemon → inference and daemon → compute (fp-10); daemon → serving-host (fp-68).

### Defects found in the 2026-09-25 diligence (REVIEW-pb-census reproduces or strikes each)

- **Hot reload bypasses the engine factory.** On a `kind="remote"` node it loads GGUFs, and under `distributed_primary` it loads the withheld primary in-process (provider.rs:50-52). Owner: pb-serving-assembly. The seat verified this by reading.
- **Daemon `/v1/models` makes a per-request round trip to cw-rails,** which means a self-fact stored in another process and a status produced by a blocking call. Owner: pb-serve-program.
- **GLiNER is loaded twice in one daemon,** with different models when the env var is set (boot.rs:595, runtime-recipe lib.rs:835). Owner: pb-serving-kinds.
- **The daemon installs a rerank slot that nothing reads;** there is no route, and the recipe opts out. Owner: pb-serving-kinds.
- **`code_search` silently swaps to full-text search** on an embed error (code_search.rs:123). Owner: pb-code-server.
- **`project serve` falls back to an in-memory notes store,** so writes vanish at exit (serve.rs:251). Owner: pb-code-server.
- **`project init` stamps a model name on zero vectors** (project_init/mod.rs:505-513). Owner: pb-code-index.
- **The chat model is resolved under `--no-enrich`,** and GLiNER absence is skipped silently. Owner: pb-ingest.
- **`knowledge_lookup` collapses an error into an empty result,** and it treats every notes kind as evidence. Owner: pb-notes-split.
- **The forced-choice judge fails open on non-sovereign servers,** and the quality probe ignores `SOVEREIGN_DAEMON_URL`. Owner: pb-bench-dials.
- **A `client_daemon_base` parse error becomes the default port.** Owner: pb-config-split.
- **`mobile_host.rs` targets the deleted sovereign-server,** and so does `Launch::Server`. Owner: pb-contracts.
- **Stale citations:**
  - mcp_surface.rs:4-9;
  - mcp_router.rs:39;
  - corpus-mcp mcp.rs:3;
  - mcp_demo_server.rs:15;
  - tool_bundle.rs:10;
  - bin/sovereign-daemon.rs:2-4, which still calls itself "the cmnwlth binary's own main".

  Owner: the row that touches each file.
