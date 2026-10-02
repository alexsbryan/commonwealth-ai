# Commonwealth: Implementation Plan

_Phased build plan for the Commonwealth coordination daemon. Each phase declares its dependencies explicitly. Phase 7 (Integration & Functional Test Harness) is a cross-cutting priority that grows alongside every other phase._

---

> **Historical record (kept for the rationale behind the phased build, not
> as truth-on-disk).** The original plan promised six crates; the codebase
> has nine. `commonwealth-scheduler` and `commonwealth-orchestrator` were
> merged into `commonwealth-inference`; `commonwealth-app`,
> `commonwealth-knowledge`, `commonwealth-state`, and
> `commonwealth-test-harness` shipped as new crates after this plan was
> written. The phase numbering and deliverable specs are therefore stale.
>
> For the **current** shape of the workspace — crate layout, what's
> implemented, what's deferred — read
> [`../sovereign/SYSTEM_OVERVIEW.md`](../sovereign/SYSTEM_OVERVIEW.md) §5
> and §10 (Architecture Roadmap). This file is preserved so the original
> phasing argument and dependency analysis remain accessible to anyone
> tracing why the project landed where it did.

---

## Phase 0: Project Scaffolding
**Depends on:** nothing

- Initialize Cargo workspace with six crates per the architecture:
  - `commonwealth-core` — shared types, mesh state, shard plans, ledger
  - `commonwealth-discovery` — mDNS, gossip, latency probing
  - `commonwealth-scheduler` — model selection, layer + knowledge assignment
  - `commonwealth-orchestrator` — process management, index management
  - `commonwealth-api` — Axum HTTP server, OICP endpoints
  - `commonwealth-daemon` — CLI + daemon entry point
- Set up `Cargo.toml` workspace with shared dependency versions
- Add `.gitignore`, `rustfmt.toml`, `clippy.toml`
- Add CI configuration (cargo check, clippy, test, fmt)
- Establish error handling pattern (thiserror for library crates, anyhow for daemon)

**Deliverable:** `cargo build` succeeds with empty crates. CI green.

---

## Phase 1: Core Types & Data Structures
**Depends on:** Phase 0

**Crate:** `commonwealth-core`

- Fundamental IDs: `MeshId`, `NodeId`, `ModelId`, `ProcessId` (newtypes)
- Mesh state: `Mesh`, `MemberRecord`, `NodeStatus`
- Node capabilities: `NodeCapabilities`, `HardwareProfile`, `GpuInfo`, `AvailableResources`, `ComputeType`
- Model registry: `ModelInfo`, `ModelAvailability`, `ModelArchitecture`
- Inference plans: `InferencePlan`, `ShardPlan`, `ShardAssignment`, `LayerRange`
- Knowledge plans: `KnowledgeShardPlan`, `KnowledgeShardAssignment`, `ChunkRange`, `CorpusShardInfo`
- Ledger: `LedgerEntry`, `LedgerEntryKind`, `ContributionUnit`
- Fairness: `FairnessPolicy`
- Mesh peering: `MeshPeering`, `PeerTrustLevel`
- Latency: `LatencyMatrix`, `LatencyRecord`
- OICP types: `CapabilityProfile`, `InferenceRequirements` (from `docs/oicp.md`)
- Configuration structs: `DaemonConfig`, `MeshConfig`
- Serialization: serde Serialize/Deserialize on all types; TOML for config, JSON for API
- Unit tests for serialization round-trips, type invariants

**Deliverable:** All shared types compile. Serde round-trip tests pass.

---

## Phase 2: Membership & Discovery
**Depends on:** Phase 1

**Crate:** `commonwealth-discovery`

### 2a: mDNS Discovery
- Advertise `_commonwealth._tcp.local` via mDNS (use `mdns-sd` crate)
- Discover other nodes on LAN automatically
- Parse service records into `SocketAddr` + `NodeId`

### 2b: Join & Leave Mechanics
- `commonwealth init` — generate mesh, produce join key (`cwth-XXXX-XXXX-XXXX`)
- `commonwealth join <key>` — verify join key against existing member, exchange mTLS certs
- Join key: BLAKE3 hash stored, raw key never persisted
- Membership revocation: proposal + majority-of-online confirmation via gossip

### 2c: Gossip Protocol
- Epidemic gossip: 10-second interval, 2-3 random peers per round
- Payload types: member state, capability updates, shard plans, ledger entries
- Timestamp-based conflict resolution (last-write-wins)
- Significance thresholds for capability updates (>10% VRAM change, GPU util crossing 0.5/0.9)
- Gossip transport: internal API endpoint (`POST /internal/gossip`)

### 2d: Mutual TLS
- Per-session TLS certificates generated during join handshake
- Certificate pinning for ongoing authentication
- Internal API (port 9742) requires mTLS

**Deliverable:** Two nodes on the same LAN discover each other via mDNS, complete join handshake, and exchange member state via gossip. Unit tests for gossip convergence. Integration test with two in-process nodes.

---

## Phase 3: Node Capabilities & Monitoring
**Depends on:** Phase 2

**Crate:** `commonwealth-discovery` (extends)

- Hardware detection:
  - GPU enumeration via `nvidia-smi` (CUDA), `rocm-smi` (ROCm), Metal system APIs
  - System RAM, CPU cores, storage (cross-platform)
- Resource polling loop:
  - GPU state every 5 seconds
  - Storage state every 30 seconds
  - CPU/RAM utilization
- Latency probing:
  - UDP RTT probes every 30 seconds to all known peers
  - Build pairwise `LatencyMatrix`
  - Share measurements via gossip (every node has full topology)
- Capability propagation:
  - Only gossip when significance thresholds crossed
  - `available_for_mesh` toggle (user can pause contribution)

**Deliverable:** Nodes report real hardware capabilities. Latency matrix populated across mesh. Unit tests for resource parsing, threshold detection.

---

## Phase 4: Inference Scheduler (Single Model)
**Depends on:** Phase 3

**Crate:** `commonwealth-scheduler`

### 4a: Layer Assignment Algorithm
- Proportional allocation: more VRAM gets more layers
- Contiguous layer ranges per node (minimize cross-node communication)
- Topology-aware ordering: adjacent ranges on low-latency node pairs
- Privacy-aware entry node: prefer requester's node for layer 0

### 4b: Shard Plan Generation
- Input: model info (total layers, size), node capabilities, latency matrix
- Output: `ShardPlan` with assignments, estimated TPS, estimated TTFT
- Scheduling modes:
  - Persistent plans (common case, models stay loaded)
  - On-demand scheduling (5-30s model load)
  - Opportunistic rebalancing (idle periods only)

### 4c: Concurrency / Leader Election
- Per-decision leader election: lowest `NodeId` wins
- Deterministic, simple, handles rare concurrent scheduling conflicts

**Deliverable:** Given a model and a set of node capabilities, the scheduler produces an optimal shard plan. Comprehensive unit tests for assignment correctness (proportionality, contiguity, topology awareness). Property-based tests for edge cases (single node, all equal nodes, wildly asymmetric VRAM).

---

## Phase 5: Orchestrator — Inference Process Management
**Depends on:** Phase 4

**Crate:** `commonwealth-orchestrator`

- Translate `ShardPlan` into running processes:
  - Start `rpc-server` on each assigned node (bound to specific GPU + layer range)
  - Start `llama-server` on entry node (configured with RPC backends)
- Process lifecycle:
  - Spawn, monitor PID, health-check (HTTP for llama-server, TCP for rpc-server)
  - Performance tracking (request latency)
  - Restart on crash
- Failure handling:
  - Detect process death / unresponsiveness
  - Report failure via gossip
  - Trigger scheduler recomputation
- Graceful departure:
  - `commonwealth pause`: 30s countdown, scheduler rebalances, in-flight requests complete
  - `commonwealth resume`: node re-enters mesh

**Deliverable:** Orchestrator starts llama-server/rpc-server processes from a shard plan, monitors health, and handles failures. Integration tests with mock llama-server processes.

---

## Phase 6: API Layer (Core)
**Depends on:** Phase 5

**Crate:** `commonwealth-api`

### 6a: Client API — Inference
- Axum HTTP server on port 9741
- `POST /v1/chat/completions` — OpenAI-compatible, streamed and non-streamed
- `GET /v1/models` — list loaded models with capabilities and performance estimates
- Route requests to appropriate `llama-server` process
- Privacy enforcement: reject `privacy: local_only` with 400 + clear error message

### 6b: Status API
- `GET /status` — node, mesh, inference, knowledge, contribution summary

### 6c: Internal API
- Separate listener on port 9742 with mTLS
- `POST /internal/gossip`
- `POST /internal/scheduling/intent` and `/plan`
- `POST /internal/model/transfer`
- `GET /internal/latency/probe`
- `POST /internal/knowledge/search` (fan-out target)

**Deliverable:** A client can `curl localhost:9741/v1/chat/completions` and get streamed tokens from a mesh-distributed model. `/v1/models` and `/status` return accurate mesh state. Integration test: full request path from HTTP to llama-server and back.

---

## Phase 7: Integration & Functional Test Harness (PRIORITY)
**Depends on:** Phase 1 (scaffolding starts), Phase 6 (full E2E)

This phase is **cross-cutting** — its scaffolding begins at Phase 1 and grows with each subsequent phase. It is called out as a dedicated phase because it represents a distinct body of work that must be planned, structured, and maintained as a first-class concern.

### 7a: Test Infrastructure (starts at Phase 1)
- `tests/` directory at workspace root for integration tests
- `commonwealth-test-harness` internal crate (not published):
  - `SimulatedNode` — in-process node with configurable hardware profile
  - `SimulatedMesh` — spin up N nodes with deterministic IDs, controlled network topology
  - Mock `llama-server` / `rpc-server` that respond with canned tokens
  - Configurable latency injection between simulated nodes
  - Configurable failure injection (node crash, process death, network partition)
  - Time control for gossip convergence tests (don't wait real 10s intervals)
- Deterministic seeding for reproducible test runs

### 7b: Unit Test Standards (each phase)
- Every public function has unit tests in its crate
- Property-based tests (proptest) for algorithms: layer assignment, shard planning, gossip convergence, knowledge assignment
- Serde round-trip tests for all types

### 7c: Integration Test Suite (grows per phase)
Each scenario below becomes testable as its dependent phase lands:

| Test Scenario | Phases Required | Description |
|---|---|---|
| Mesh formation | 2 | Init mesh, 2 nodes join, verify member state converged |
| Gossip convergence | 2, 3 | 5 nodes, verify capability state converges within bounded rounds |
| Shard plan correctness | 4 | Given capabilities, verify plan satisfies constraints |
| Inference E2E | 6 | HTTP request → router → llama-server mock → streamed response |
| Node failure recovery | 8 | Kill node mid-inference, verify 503 + re-plan within 15s |
| Graceful pause/resume | 8 | Pause node, verify no 503, resume, verify rebalance |
| OICP routing | 9 | Two models loaded, requests with different OICP route correctly |
| Multi-model portfolio | 10 | Concurrent requests to different models, verify isolation |
| Model swap | 10 | Trigger swap, verify no 503 gap during transition |
| Knowledge query E2E | 11 | Fan-out query across 3 shard nodes, verify merged results |
| Knowledge + failure | 11, 8 | Shard node fails, verify partial results + replica fallback |
| Ledger accuracy | 12 | Serve requests, verify ledger entries match actual resource usage |
| Fairness throttling | 12 | Node exceeds balance, verify priority reduction |
| Mesh peering | 13 | Two meshes, verify model transfer and overflow routing |

### 7d: Stress / Chaos Tests
- Concurrent request storm (100 simultaneous requests)
- Rapid node join/leave cycles
- All-nodes-restart (mesh reformation from cold state)
- Long-running stability (1000 requests over simulated time)

**Deliverable:** A test harness that makes it trivial to spin up multi-node meshes in-process with controlled timing and failure injection. A growing suite of integration tests that exercise end-to-end flows and give high confidence the system works as specified.

---

## Phase 8: Fault Tolerance & Graceful Operations
**Depends on:** Phase 6

- Node departure detection during inference:
  - Entry node detects RPC connection failure
  - Active requests get HTTP 503 with `Retry-After: 10`
  - Node-failure event broadcasts to mesh
  - Scheduler recomputes plans excluding failed node
  - Recovery target: 5-15 seconds
- Node departure during knowledge query:
  - Partial results from surviving shard nodes
  - OICP capabilities endpoint reports `fully_available: false`
- Graceful departure (`commonwealth pause`):
  - 30-second countdown announcement
  - Scheduler preemptively rebalances
  - In-flight requests complete on old plan
  - New requests use new plan
  - Zero 503s during graceful departure
- Node return:
  - Returned nodes noted, not immediately rebalanced
  - Next scheduled rebalance incorporates them

**Deliverable:** Hard node failure recovers within 15s. Graceful pause produces zero 503s. Integration tests confirm both scenarios.

---

## Phase 9: OICP Support
**Depends on:** Phase 6

- OICP capability profiles attached to model registry entries
- Community profile registry format (TOML files per model/quant)
- `GET /oicp/v1/capabilities` — provider manifest per spec
- OICP-aware model selection in scheduler:
  - `satisfies_required()` — hard filter
  - `score_preferred()` — soft ranking
  - Cached OICP-to-model resolution (sub-millisecond lookup, recomputed on portfolio change)
- Request routing: inspect `oicp` field in `/v1/chat/completions`, route to matching model
- Default model when no `oicp` field present
- Privacy enforcement: `local_only` → 400; `mesh_allowed` → normal routing

**Deliverable:** Requests with OICP requirements route to the correct model. `/oicp/v1/capabilities` returns accurate manifest. Integration tests for routing correctness.

---

## Phase 10: Multi-Model Portfolio & Usage Prediction
**Depends on:** Phase 9

- Multiple models loaded simultaneously (one `llama-server` per model on entry node, different ports)
- Model swap decisions:
  - Threshold-based: only swap when capability improvement justifies 30-60s load cost
  - `SWAP_THRESHOLD` constant, configurable
- Graceful model transitions:
  - `ModelTransition` state machine: Loading → Ready → Complete
  - Old model continues serving during new model load
  - Drain old model after new is ready
  - Fallback to hard-cut if not enough aggregate memory for both
- Usage pattern prediction:
  - `UsagePredictor` reads ledger history
  - Per-(weekday, hour) capability distribution
  - Preemptive model loading during idle transitions

**Deliverable:** Mesh serves concurrent requests to different models. Model swaps have zero 503 gap when memory allows. Integration tests for concurrent routing and swap transitions.

---

## Phase 11: Knowledge Subsystem
**Depends on:** Phase 6

**Crates:** `commonwealth-scheduler` (knowledge assignment), `commonwealth-orchestrator` (shard management), `commonwealth-api` (query endpoint)

- Knowledge shard planning:
  - Assign corpora to nodes by available disk space
  - Split large corpora by chunk ID range
  - Co-locate frequently co-queried corpora
  - Respect `mesh_sharing` flags (restricted license handling)
  - Redundancy target (default 2 copies per shard)
- Shard management in orchestrator:
  - Check local presence, initiate peer transfer if missing
  - Open SQLite + sqlite-vec databases for search
  - Report shard availability via gossip
- `POST /v1/knowledge/search` endpoint:
  - Fan-out to all nodes holding relevant shards
  - Each shard node: sqlite-vec nearest-neighbor + FTS5 keyword search
  - Requesting node: merge, deduplicate, rerank, return global top-K
- `POST /internal/knowledge/search` — inter-node shard query
- Index transfer: `POST /internal/index/transfer`

**Deliverable:** Knowledge queries fan out across shard nodes and return merged results. Redundancy ensures availability when a shard node is offline. Integration tests for query correctness, fan-out merge, and replica failover.

---

## Phase 12: Contribution Ledger & Fairness
**Depends on:** Phase 6

- Ledger entries for every completed inference request, knowledge query, resource contribution
- `ContributionUnit`: `GpuSeconds`, `StorageGbDays`, `BandwidthGb`
- Append-only, replicated to all nodes via gossip
- Balance computation: net contribution per node over rolling window (default 30 days)
- `commonwealth balance` CLI display
- Fairness policies:
  - `Transparent` (default): everyone sees the ledger
  - `SoftThrottle`: below threshold → lower scheduling priority
  - `HardCap`: below threshold → requests denied
- Mesh-wide fairness policy propagated via gossip config

**Deliverable:** Ledger accurately tracks resource usage. Balance command shows per-node contribution. Fairness policies enforce throttling. Integration tests for ledger accuracy and policy enforcement.

---

## Phase 13: Mesh Peering
**Depends on:** Phase 8, Phase 11

- Peer mesh trust establishment (out-of-band key exchange, same as node join)
- `MeshPeering` with `ModelAndKnowledgeSharing` and `Full` trust levels
- Model file transfer across meshes (`POST /internal/model/transfer`)
- Corpus index transfer across meshes (`POST /internal/index/transfer`)
- `Full` peering: overflow inference routing to peer mesh when local mesh is at capacity
- `federation.peers` in `/oicp/v1/capabilities` response

**Deliverable:** Two meshes share models and indexes. Full peering enables overflow routing. Integration tests with simulated dual-mesh topology.

---

## Phase 14: CLI & Daemon Lifecycle
**Depends on:** Phase 10, Phase 11, Phase 12

**Crate:** `commonwealth-daemon`

- Full CLI command set (clap):
  - `init`, `join`, `status`, `balance`, `pause`, `resume`, `leave`
  - `models`, `corpora`, `logs`
  - `mesh set`, `mesh members`, `mesh revoke`, `mesh peer`
  - `daemon start/stop/status`
- Daemon runs as system service:
  - systemd unit file (`contrib/systemd/`)
  - launchd plist (`contrib/launchd/`)
- Configuration file loading (`~/.commonwealth/config.toml`)
- Graceful signal handling (SIGTERM → graceful departure sequence)
- Log output with structured logging (tracing crate)

**Deliverable:** All CLI commands functional. Daemon starts as system service and handles signals gracefully. Manual test: full lifecycle from init through inference to pause/resume.

---

## Phase 15: Packaging & Distribution
**Depends on:** Phase 14

- Single static binary compilation (musl on Linux, native on macOS)
- `install.sh` script
- Platform matrix:
  - Linux x86_64 (CUDA, ROCm, Vulkan)
  - macOS ARM (Metal)
  - Windows x86_64 (CUDA, Vulkan via WSL2 or native)
- Release automation (GitHub Actions: build, test, package, publish)
- `contrib/` directory with service files

**Deliverable:** `curl -sSf .../install.sh | sh` installs Commonwealth on supported platforms. CI produces release artifacts.

---

## Phase Dependency Graph

```
Phase 0: Scaffolding
  └─▶ Phase 1: Core Types
       └─▶ Phase 2: Membership & Discovery
            └─▶ Phase 3: Capabilities & Monitoring
                 └─▶ Phase 4: Inference Scheduler
                      └─▶ Phase 5: Orchestrator
                           └─▶ Phase 6: API Layer ◀── CRITICAL PATH CONVERGES HERE
                                │
                                ├─▶ Phase 8:  Fault Tolerance ──────┐
                                ├─▶ Phase 9:  OICP Support          │
                                │    └─▶ Phase 10: Multi-Model      │
                                ├─▶ Phase 11: Knowledge ────────────┼─▶ Phase 13: Mesh Peering
                                ├─▶ Phase 12: Ledger & Fairness     │
                                │                                    │
                                └─▶ Phase 7:  Integration Tests (scaffolding from Phase 1)
                                                     ▲
                                                     │ grows with every phase

Phase 10 + 11 + 12 ──▶ Phase 14: CLI & Daemon ──▶ Phase 15: Packaging
```

### Parallelization After Phase 6

Once Phase 6 lands, four independent work streams proceed in parallel:

| Stream | Phases | Focus |
|---|---|---|
| A | 8 → 13 | Fault tolerance → Mesh peering |
| B | 9 → 10 | OICP → Multi-model portfolio |
| C | 11 (→ 13) | Knowledge subsystem |
| D | 12 | Ledger & fairness |

Phase 7 (Integration Tests) runs alongside all streams.

---

## Testing Philosophy

Unit tests are written inline with each phase. Integration tests in Phase 7 validate that the composed system behaves correctly under realistic conditions including failure modes. The test harness is the single most important investment for long-term confidence — it is not an afterthought but a co-equal deliverable alongside the production code.

Key principles:
- **Deterministic by default.** Simulated nodes with controlled time, no real network I/O in tests.
- **Failure injection is first-class.** Every integration test scenario has a corresponding failure variant.
- **Property-based tests for algorithms.** Scheduler and assignment algorithms are pure functions — ideal for proptest.
- **Mock external processes.** llama-server and rpc-server are mocked in tests; real process management tested separately with actual binaries in a dedicated CI job.
