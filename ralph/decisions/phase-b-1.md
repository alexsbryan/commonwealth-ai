<!-- ledger -->

**phase-b-1 · 2026-09-25 · Phase B design · operator** — this commit
- Needed:
  - Phase B's staged plan built new hosts, one per red-edge class: a serving binary, an ingest server and a code server. It minted them through REVIEW-mint rows.
  - The operator named the smell: "a second owner of core capability". They set a test: "the next developer who wants THIS but not THAT".
  - Seven read-only inventories and persona walks followed. They found one owner already exists for each capability, and that the current hosts bypass it. Phase B as staged would have added the next copy of each drive.
- Chose:
  - (1) `serve` becomes a sixth program, split out of `cmnwlth`. It has one engine assembly, kinds by registration, and placement per kind (in-process, child or dial). Any OpenAI URL is a venue. cw-rails adverts inference origins and never ranks.
  - (2) A **host kit**, one neutrally named mechanism leaf (§12 3a rung 4), reusing sovereign-cli-base. It holds each program's lock, data root, server shell and MCP dispatch.
  - (3) **Distributions** are wiring-only composition roots, declared in `[[distribution]]` rows. They may link declared library faces, and every program still runs alone, proven by its own lift sandbox.
  - (4) The 3a ladder is re-applied to sovereign-contracts itself. `SetupConfig` splits into per-program files, with the migration in the same commit.
  - (5) Notes split by owner. svrn's memory is svrn's; decision notes go to code; the call log goes to the MCP host.
  - (6) Ingest is a library plus one CLI, and the work plane is an optional caller. There is no ingest server.
  - (7) Bench dials three URLs: model, subject and judge.
  - (8) The work atlas is KEPT, as an optional code bundle that dials cw-rails directly.
  - (9) cw-rails founds, joins and admits, and owns the one node key. svrn dials cw-rails' work doors.
  - (10) `pipeline pod` becomes `svrn mesh pod`.
  - (11) Grants hands merges to ingest as work units.
  - (12) The compose rule: extend, never re-own, and collapse the duplicates before splitting a process.
  - The Phase B queue is rewritten as 29 outcome rows with no mint step.
- Because:
  - Principles 12, 8, 11, 9, 6 and 10.
  - FIVE_PROGRAMS §1 "composition is by process" is kept for programs. Distributions give heterogeneous deployments (a one-process stock install, a phone, a pod) a place that is not a program.
  - Boundary gate: 51 at 55546ac07, unchanged. There is no code in this commit.

<!-- appendix -->

## phase-b-1 · 2026-09-25 — six programs, the host kit, distributions, and compose-never-re-own

<details><summary>reasoning, evidence, package</summary>

**What already exists and is bypassed.** Every claim below was re-checked by the seat against source unless marked.

- **Bring-up.** `ServingHost::ensure_reachable` (sovereign-turn-client/src/reach.rs:276) is the declared `bring_up_decider` (quality/ARCH_LAYERS.toml:1501), and it replaced thirteen private copies. Copies remaining: 7 identical sibling locators, cli-daemon `start_daemon`'s private ready-poll, and `serve_cmd` `spawn_background`, which is today's code-server connect-or-spawn. There are three root-lock mechanisms: run_lock (libc flock), deep_research's second `RunLock` (std try_lock) and scip's fs4.
- **Serving.** It has one owner: the serving crates behind `InferenceProvider` (sovereign-contracts/src/traits.rs:298), with a registry of engine kinds (`register_engine`, sovereign-inference/src/engine_factory.rs:141). The engine is assembled three times, and the reload path has drifted. `LlamaCppFactory` (sovereign-daemon/src/provider.rs:36) calls `load_full_with_families` directly and makes zero `install_*` or compute-layer calls. The compute child is off by default (setup_config.rs:750).
- **MCP.** `ToolRegistry` (sovereign-contracts/src/registry.rs:13) and `ToolBundle` (tool_bundle.rs:67) exist. The code bundles have zero production constructors. Three hand builders register 38, 30 and 35 tools and have already drifted, and three MCP loops make different protocol decisions.
- **Jobs.** Job execution is owned by commonwealth-work's `JobExecutorRegistry` (executor.rs:374), and `IngestExecutor` already implements `JobExecutor` (sovereign-daemon/src/ingest_executor.rs:430). FIVE_PROGRAMS §2 gives ingest the wire "CLI only".

**The persona walks.**

- **P1, a local model server with no mesh.** Today it needs the whole daemon, which pulls in 37 in-repo crates, and `/v1/models` reads cw-rails' ledger.
- **P2, a mesh in front of vLLM.** Impossible today: cw-rails gossips `inference_capable: false` (gossip.rs:79), and `InferenceVenue` is mesh-only and lives in sovereign-contracts (venue.rs:20).
- **P3, svrn on a hosted API.** Mostly works through `[engine] kind="remote"` (engine_config.rs:42).
- **P4, in-process serving.** Shut out by a dial-only default. A phone can't build llama-cpp-4 today (per the manifest; not built).
- **P5, code with no LLM.** Code search falls back to full text, but cli-dev links the daemon. The atlas is broken standalone.
- **P6, ingest in CI.** Works only through `corpus-mcp ingest`.
- **P7, svrn without code.** Broken by the earlier "notes owned by code" recommendation. notes.db holds four populations, and there are three `notes_db_path` deciders (backlog item.rs:30, awareness store_open.rs:60, contracts middleware.rs:165). One query returns 68 notes from one directory and 6,811 from another (item.rs:27). svrn already owns `save_memory` (traits.rs:1277).
- **P8, bench against my own endpoint.** 32 `port-listening:9741` preconditions, and judges share the URL under test.

**The shared layer is the ball.** sovereign-contracts is 42,441 lines and 58 modules, and 51 manifests name it. A bench-only developer pays for a ~342-crate closure (an upper bound from Cargo.lock) to use 4 items. 14 modules have one consumer program. cw-rails, the one program forbidden to link it, has had to copy four things.

**Alternatives rejected.**

- A new serving binary: a second owner.
- Serving under cmnwlth: fails P1, and cw-rails may not link it.
- Serving as a module of svrn: a model server would carry corpus-engine.
- An ingest server: a second owner of job execution.
- MCP dispatch in sovereign-contracts: no axum there today, it sits inside bench's budget, and cw-rails can't reach it.
- A shared notes program: no reader needs more than one population, so it would make the accident permanent.
- Exec-only distributions: rules out on-device serving (principle 12's last paragraph).
- One crate per mechanism: three more leaves.

**Pre-registered bars** (pb-svrn-dials-serve), set before any data:
- first-token latency p50 over loopback at most 10% slower than in-process;
- embedding throughput at batch 32 at least 90% of in-process;
- n ≥ 5 runs each;
- retrieval-prod and the synth lane inside their noise bands.

The host-kit size cap is 2,500 code lines; both numbers are the operator's to change.

**Falsified if:**
- a program's lift sandbox cannot pass without linking another program's crates, so the six-way line is drawn wrong;
- the host kit needs program vocabulary to serve any program;
- the one serving assembly cannot express the reload path without daemon state that no port carries;
- the loopback bars fail, which makes the dial-default wrong for the stock distribution and needs an operator decision on in-process placement.

</details>
