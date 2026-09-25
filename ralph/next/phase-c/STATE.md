# phase-c — the ralph queue (STAGED, not started)

This is the follow-on to Phase B (ralph/decisions/phase-b-2.md, 2026-09-25). The rows below were designed in phase-b-1 and are still wanted. But none of them closes a red edge or makes a lift pass, so Phase B's finish does not need them, and they wait here rather than adding churn to it. The operator's rule is "no demo, no build": each row starts when a person is waiting on the use case it serves. Examples: a developer who fronts vLLM with the mesh (pc-inference-origin), a developer who takes one program and needs its own config file (pc-config-split), or a bench run against a foreign endpoint (pc-bench-dials).

Nothing runs this queue until the operator launches it, after phase-b's `pb-distribution`. At launch, write CHARTER.md, PROMPT.addendum.md and queue.toml from phase-b's (prefix `pc`), and re-verify every premise below against the tree of that day. They were cited at 55546ac07 plus the origin/main merge, and Phase B will have moved many of them.

## Rows

- [ ] pc-venues — depends [] — OUTCOME: any OpenAI-compatible endpoint is a venue, and where each kind runs is a setting.
  - `Venue` becomes an enum { OwnSlot, ChildSlot, MeshMember(NodeId), PinnedPod, Url{ endpoint, key, declared claims } } over today's `InferenceVenue` (sovereign-contracts/src/venue.rs:19-46, mesh-only).
  - Placement per kind becomes an enum { InProcess, Child, Dial }. All three arms exist today, as the engine, the compute child and the terminal dial.
  - `[engine] kind="remote"` (engine_config.rs) absorbs the terminal node's `[node] entry` door, a second key for one question (principle 8). That door hard-codes model "primary" and sends no bearer (build/inference.rs:189,204).
  - A remote engine no longer needs a placeholder `[models]` section (build/inference.rs:225).
  - Delta: a `[node] entry` config is migrated to `[engine] remote` in the same commit (the config migration rule).
  - PROOF: an e2e points svrn at a stub OpenAI server as a `Url` venue, with no `[models]`, and a chat turn and an embedding round-trip through it. PLANT: drop the Url arm from the router, and the e2e goes red.
  LIFT ~900 lines. — read: sovereign-contracts/src/{venue.rs,engine_config.rs}, sovereign-serving-host/src/peer_inference.rs:1300-1340, sovereign-daemon/src/build/inference.rs:60-240, sovereign-turn-client/src/reach.rs:80-90 — check: CLEAN, LINT, TEST(sovereign-serving-host), TEST(sovereign-daemon), PLANT, LAYER, BOUNDARY
- [ ] pc-inference-origin — depends [pc-venues] — OUTCOME: the mesh fronts ANY OpenAI-compatible server (the "mesh in front of vLLM/ollama" developer).
  - `OriginKind::Inference` in oicp-types (origin.rs:21-26: "a new kind is a new variant beside a new route").
  - cw-rails adverts it and never ranks (its charter). cw-rails gossips `inference_capable: false` today (gossip.rs:79).
  - The origin's manifest comes from the origin itself (oicp-conformance exists) or from an operator-declared claims file that cw-rails serves verbatim (data, principle 9).
  - serve's router ranks venues. Its `VenueSource` reads cw-rails' HTTP roster in place of the in-process DeferredDaemon.
  - PROOF: an e2e puts a stub OpenAI server behind cw-rails as an Inference origin with a claims file, and a second node's `serve` routes a chat turn to it. PLANT: drop the claims file, and the venue drops out with a named reason.
  LIFT ~1,000 lines. — read: oicp-types/src/origin.rs, commonwealth-rails/src/gossip.rs, sovereign-serving-host/src/peer_inference.rs:1300-1340 — check: CLEAN, LINT, TEST(oicp-types), TEST(commonwealth-rails), TEST(sovereign-serving-host), LIFT(cmnwlth), PLANT, LAYER, BOUNDARY
- [ ] pc-bench-dials — depends [] — OUTCOME: bench judges ANY endpoint (the "bench my own server" developer).
  - Three named dials: the model (plain OpenAI API), the subject (svrn, for the turn and verdict lanes) and a SEPARATE judge (principle 7). Today judges share the URL under test, and only `bench/external/*` takes `--base-url`.
  - Every lane is tagged with the dials it needs. Against a foreign server, the model-only lanes run and the rest report could-not-judge with the reason.
  - The quality-check precondition `port-listening:9741` (32 uses in quality/instruments.toml) honours the subject dial and `SOVEREIGN_DAEMON_URL` (quality_check_cmd/exec.rs:33-37).
  - `eval run` lanes dial svrn in place of building an in-process Runtime (chat_cmd/bootstrap.rs:205-394).
  - PROOF: LIFT(bench) passes. A lane run against a stub OpenAI server yields model-lane verdicts plus named could-not-judge rows. PLANT: point a model-only lane at the subject dial, and the lane-tag test goes red.
  LIFT ~1,200 lines. — read: sovereign/bench/README.md, quality/instruments.toml, quality_check_cmd/exec.rs, grounding/judge.rs, chat_cmd/bootstrap.rs — check: CLEAN, LINT, TEST(sovereign-eval), TEST(sovereign-cli-llm), LIFT(bench), PLANT, LAYER, BOUNDARY
- [ ] pc-contracts — depends [] — OUTCOME: a program that takes the shared layer takes only shared vocabulary. The §12 3a ladder is re-applied to sovereign-contracts itself (42,441 lines, 58 modules, named by 51 manifests).
  - Single-program modules MOVE to their owners (re-exported at their historical paths until the last consumer repoints):
    - svrn: skills, intent_policy, data_roots, tool_result_cache, guest_pages, observer, memory_config, mcp_config, lessons; types' grounding_journal, stage_attribution and grounding_verdict; daemon_wire's svrn-only half.
    - serve/cmnwlth: fim, worker_pod, local_inference, build_stamp.
  - Traits whose implementer and consumer are both svrn (~14: TaskStore, MemoryStore, DocumentStore, BudgetStore, InsightStore, the oracles…) move into svrn.
  - (mobile_host.rs and `Launch::Server` were already deleted by phase-b's pb-delete-dead.)
  - `egress` (reqwest; 125 crates of closure) moves to its owner.
  - Fix the stale "24,619 lines" leaf comment (ARCH_LAYERS ~:899-903).
  - PROOF: bench's closure crate count, before and after (measured by REVIEW-pb-census). LIFT(bench) and LIFT(serve) pass. PLANT: re-add a moved module to contracts' lib.rs, and the single-owner census test fails. Add that census as a test: a contracts module named by one program only is red.
  LIFT ~1,500 lines, mostly moves; split by owner if the census exceeds it. — read: sovereign-contracts/src/lib.rs, quality/ARCH_LAYERS.toml contracts leaf row, FIVE_PROGRAMS §12 3a — check: CLEAN, LINT, TEST(sovereign-contracts), TESTALL, LIFT(bench), PLANT, LAYER, BOUNDARY
- [ ] pc-provider-split — depends [pc-contracts] — OUTCOME: a client of a model server names only the dial surface.
  - `InferenceProvider` (traits.rs:298, 29 methods, 114 impl lines in 75 files) splits by who needs what:
    - the 10 dial methods (the `complete` family, embed, rerank) go to the wire side, beside oicp-client;
    - the 10 metadata methods stay with them if a dialer reads them, otherwise they go to serve;
    - the 7 slot-administration methods (warmup, load/unload_extra_slot, compute_children, …) go to serve;
    - the 2 mesh methods (peer_manifests, lender_manifest) go to serve's router.
  - One dimension per commit: introduce the narrow trait, repoint consumers, then shrink.
  - PROOF: sovereign-cli-llm, sovereign-eval and sovereign-core name only the dial trait (grep in the body). PLANT: call a slot-admin method from sovereign-core, and it fails to compile.
  LIFT ~1,500 lines. — read: sovereign-contracts/src/traits.rs:290-700, the 75 impl files' census — check: CLEAN, LINT, TESTALL, PLANT, LAYER, BOUNDARY
- [ ] pc-config-split — depends [pc-contracts] — OUTCOME: each program reads ONLY its own config file (FIVE_PROGRAMS §1, §4 rule 7). Today `SetupConfig` (setup_config.rs:32) is one 13-section schema across programs, loaded in 13 crates.
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
- [ ] pc-solo-durable — depends [] — OUTCOME: on a solo cw-rails, a KV write it acknowledged survives a kill (operator 2026-09-25, five-programs-66: a gap with an owner, not "by design").
  - Measured in fp-solo-clients' e2e (30667d2d7): a write cw-rails had acknowledged was gone after a kill milliseconds later. It became durable only on the pump's next tick (commonwealth-rails kv.rs:52 `PUMP_INTERVAL` = 2 s). A meshed node may have a peer copy; a solo node has only this journal.
  - The shape is the row's to choose: append before the ack in solo mode, or ack only what the pump has appended. The cost goes in the body as write latency measured against today's.
  - PROOF: a commonwealth-rails test writes through the door, kills cw-rails with SIGKILL at once, restarts it, and reads the row back. PLANT: restore today's ack-before-append in solo mode → red.
  LIFT ~300 lines. — read: commonwealth-rails/src/kv.rs, commonwealth-rails/src/kv/tests.rs, sovereign-daemon/tests/solo_rails_e2e.rs (its wait-for-append), ralph/decisions/five-programs-66.md — check: CLEAN, LINT, TEST(commonwealth-rails), PLANT, LAYER, BOUNDARY
