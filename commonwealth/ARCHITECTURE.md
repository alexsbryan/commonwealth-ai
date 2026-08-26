
# Commonwealth: Technical Design v2

_A coordination daemon for community-owned distributed inference and knowledge._

---

> **Historical record (kept for design rationale, not as truth-on-disk).**
> This document was written early in the project and has drifted from the
> code as the system evolved (the contribution ledger was redesigned, the
> crate layout grew from 6 to 9, the FairnessPolicy enum was abandoned in
> favour of per-peer affinity preferences, and the §4 scheduler generation
> described below — model portfolio, adaptive scheduler, shard plan_builder,
> usage prediction — was never wired into the runtime and was deleted
> 2026-06-10; see `sovereign/docs/specs/OICP_RATIONALIZATION.md` for what
> replaced it). Read it for the *why*
> behind the original design, the design-philosophy section that still
> governs the project, and the threat-model framing — those parts hold.
>
> For the **current** shape of the running system — file paths, route
> tables, type signatures, CLI subcommands — read
> [`../sovereign/SYSTEM_OVERVIEW.md`](../sovereign/SYSTEM_OVERVIEW.md) §5
> instead. That doc is kept current with the code on the commit it
> appears in (per ARCH_PRINCIPLES §1.1).

---

## Design Philosophy

Three principles govern this project. They are constitutional constraints. A contribution that violates them is rejected regardless of its technical merit.

**This is a commons, not a product.** No token. No blockchain. No telemetry. No venture-funded company behind it. No "early access" tier. Apache 2.0 license. The project is designed so that forking it is trivial and there is no moat to build around it. If someone forks Commonwealth and adds a token, they have exercised their right under the license. They have not captured Commonwealth, because there is nothing to capture — no central registry, no privileged node, no state that doesn't exist on every participant's machine.

**Social trust, not cryptographic verification.** Blockchains solve coordination among adversaries. Commonwealth solves coordination among neighbors. You join a mesh because someone you know invited you. The threat model is "my neighbor's kid started a game and their node slowed down," not "a malicious actor is feeding me poisoned weights." This is a fundamental design choice that simplifies the architecture dramatically. If your threat model requires Byzantine fault tolerance, Commonwealth is not your project.

**The daemon coordinates. It does not infer or index.** Commonwealth does not contain an inference engine or a search index. It orchestrates llama.cpp instances for inference and SQLite-based indexes for knowledge. When llama.cpp improves (and it improves monthly), Commonwealth benefits automatically. The daemon is the nervous system. The muscles are someone else's well-maintained project.

---

## What Commonwealth Does

A concrete scenario. Five people in a neighborhood each have a desktop machine:

|Person|Machine|GPU|VRAM|Storage|
|---|---|---|---|---|
|Alice|Framework Desktop|Strix Halo|32 GB shared|1 TB|
|Bob|Custom build|RTX 4090|24 GB|512 GB|
|Carol|Mac Studio|M3 Ultra|192 GB unified|2 TB|
|Dave|Custom build|2× RTX 3090|48 GB total|1 TB|
|Eve|MacBook Air|Integrated|16 GB shared|256 GB|

Without Commonwealth, each person runs models limited by their individual hardware, and each must download and index their own knowledge bases.

With Commonwealth, their pooled VRAM is ~312 GB and their pooled storage is ~4.3 TB. The mesh can run a 70B model sharded across multiple nodes, serve concurrent requests by routing to different subsets of hardware, and host a shared knowledge index that even Eve's MacBook Air can query without storing anything locally. Eve's 256 GB machine stores 4 GB of local files (Sovereign's Fast model, Embed model, and personal data) and gets access to the mesh's intelligence and knowledge — a 70B reasoning model and hundreds of gigabytes of indexed knowledge.

From any client's perspective (Sovereign, OmO, Open WebUI, a curl command), the mesh looks like a single API endpoint on localhost. The client sends a request. It gets tokens back. Or it sends a knowledge query. It gets search results back. The distributed orchestration is invisible.

---

## Architecture Overview

```
                    ┌─────────────────────────────────────┐
                    │          Client (Sovereign,          │
                    │          OmO, Open WebUI, curl)      │
                    └───────────────┬─────────────────────┘
                                    │ HTTP :9741
                                    ▼
┌──────────────────────────────────────────────────────────────────┐
│                     Commonwealth Daemon                          │
│                     (runs on every node)                         │
│                                                                  │
│  ┌──────────────┐  ┌──────────────┐  ┌────────────────────────┐  │
│  │   API Layer  │  │  Membership  │  │     Scheduler          │  │
│  │  (Axum)      │  │  & Discovery │  │                        │  │
│  │              │  │              │  │  Inference planning     │  │
│  │  OpenAI-     │  │  mDNS +     │  │  Knowledge sharding    │  │
│  │  compatible  │  │  Gossip +   │  │  Model portfolio mgmt  │  │
│  │  + Knowledge │  │  Trust Ring │  │  Usage prediction       │  │
│  │  + OICP      │  │  + Peering  │  │                        │  │
│  └──────┬───────┘  └──────┬──────┘  └───────────┬────────────┘  │
│         │                 │                      │               │
│  ┌──────▼─────────────────▼──────────────────────▼────────────┐  │
│  │                   Orchestrator                             │  │
│  │                                                            │  │
│  │  Manages llama.cpp processes (inference)                   │  │
│  │  Manages corpus index shards (knowledge)                   │  │
│  │  Handles model downloads + index transfers                 │  │
│  └──────┬────────────────────────────┬────────────────────────┘  │
│         │                            │                           │
└─────────┼────────────────────────────┼───────────────────────────┘
          │ spawn/manage               │ manage
          ▼                            ▼
   ┌──────────────┐            ┌──────────────┐
   │ llama-server │            │ Corpus Index │
   │ + rpc-server │            │ (SQLite +    │
   │ (inference)  │            │  sqlite-vec) │
   └──────────────┘            └──────────────┘
```

The daemon is a single binary that runs on every participating machine. There is no master node. Every node can accept client requests, and every node participates in scheduling decisions. The mesh is symmetric — removing any single node degrades capacity but does not break the system.

---

## 1. Membership and Discovery

### The Trust Ring

A Commonwealth mesh is not open to the internet. It is a closed group of people who know each other.

```rust
pub struct Mesh {
    pub id: MeshId,
    pub name: String,  // "Sunset District Co-op", "Lab Cluster 3"
    pub join_key_hash: [u8; 32],  // BLAKE3 hash; raw key never persisted
    pub members: HashMap<NodeId, MemberRecord>,
    pub peers: Vec<MeshPeering>,  // Trusted peer meshes
}

pub struct MemberRecord {
    pub node_id: NodeId,
    pub name: String,               // "Alice's Desktop"
    pub invited_by: NodeId,
    pub joined_at: u64,
    pub last_seen: u64,
    pub status: NodeStatus,
    pub capabilities: NodeCapabilities,
    pub addresses: Vec<SocketAddr>,
}

pub enum NodeStatus {
    Online,
    Busy,       // Under heavy local load
    Away,       // Not responding but not formally departed
    Offline,    // Gracefully disconnected
}
```

### Joining a Mesh

```
$ commonwealth init --name "Sunset District Co-op"

Mesh created: Sunset District Co-op
Join key: cwth-7f3a-9b2e-4d1c

Share this key with people you want in the mesh.
They run: sovereign mesh join cwth-7f3a-9b2e-4d1c
```

(Mesh lifecycle UX — create, join, status across nodes — lives in the
`sovereign` CLI; the `commonwealth` binary manages the standalone daemon.)

The join key is shared out-of-band — spoken aloud, sent via Signal, printed on a card. The join process requires a human social interaction. You cannot join a Commonwealth mesh by scanning the network.

When a new node joins, an existing member verifies the join key, adds the new node to its member list, and the record propagates via gossip. The join key is verified once and discarded.

> **Implementation note — drifted from this design doc (see `sovereign/SYSTEM_OVERVIEW.md` for the authoritative current state).** Ongoing inter-node auth was *designed* as per-session mutual TLS with pinned certs; **the shipped system does not implement that** — the TLS scaffolding was removed rather than left as a security façade. In practice: the client port (`:9741`) gates non-loopback callers with a bearer token; the internal port (`:9742`) is perimeter-trusted (operators run a WireGuard/Tailscale underlay) plus the join-key/`join_key_hash` check in `Mesh::merge_from` that rejects foreign-mesh gossip; iroh's QUIC handshake verifies node keys when enabled (default off).

The join key is mandatory. This is an architectural constraint that prevents anonymous open meshes, which would require Byzantine fault tolerance and introduce the coordination complexity the project explicitly avoids.

### Revoking Membership

Any member can revoke another — this handles "Bob moved away" and "Dave's machine got compromised."

> **Implementation note.** The shipped mechanism (mesh-hardening pass) is a gossiped **tombstone**: `revoke_member` stamps a `removed_at` timestamp on the member rather than deleting it, and the tombstone propagates via gossip. Through the event-time LWW in `Mesh::merge_from` (the merge compares `max(last_seen, removed_at)`), the tombstone out-competes any stale *live* copy a lagging peer still holds — so a revoked node can't be re-added on the next round (the prior "immortal ghost" bug), while a genuine rejoin whose activity post-dates the removal still wins. The *majority-vote confirmation* described here (one member proposes, a simple majority of online members confirms) is scaffolding (`RevocationProposal`) and is **not yet wired** to a gossip-vote protocol — today revocation is single-actor (any join-key holder). Graceful self-`leave` clears the departing node's local state; gossiping a self-tombstone on leave is the remaining operational step.

### Mesh Peering

Meshes can establish trust relationships with other meshes for resource sharing.

```rust
pub struct MeshPeering {
    pub peer_mesh_id: MeshId,
    pub peer_mesh_name: String,
    pub trust_level: PeerTrustLevel,
    pub established_at: u64,
    pub contact_nodes: Vec<SocketAddr>,
}

pub enum PeerTrustLevel {
    /// Share model files and corpus indexes only.
    ModelAndKnowledgeSharing,
    /// Share everything plus allow overflow inference routing.
    Full,
}
```

Peering is established by exchanging mesh-level keys out-of-band, same as node membership. `ModelAndKnowledgeSharing` enables a new mesh to bootstrap its model files and corpus indexes from a nearby established mesh in minutes instead of downloading from the internet. `Full` peering enables cross-mesh inference overflow but is opt-in and rare.

### Network Discovery

**mDNS** for local networks — every daemon advertises `_commonwealth._tcp.local`. Zero configuration.

**Gossip over VPN** for city-scale — nodes on WireGuard or Tailscale overlays discover each other transitively. Commonwealth does not manage the VPN.

### Gossip Protocol

Epidemic gossip, 10-second intervals, 2-3 random peers per round. Member state, capability updates, shard plans, and ledger entries all propagate via gossip. Timestamp-based conflict resolution. A 100-node mesh converges in under a minute.

Gossip payloads are small. Capability updates propagate only when they cross significance thresholds (>10% change in free VRAM, GPU utilization crossing 0.5 or 0.9). This keeps background traffic negligible.

---

## 2. Node Capabilities

Every node continuously monitors its own hardware and reports capabilities.

```rust
pub struct NodeCapabilities {
    pub hardware: HardwareProfile,
    pub available: AvailableResources,
    pub active_processes: Vec<ProcessInfo>,
    pub hosted_corpora: Vec<CorpusShardInfo>,  // Which knowledge shards this node holds
    pub reported_at: u64,
}

pub struct HardwareProfile {
    pub gpus: Vec<GpuInfo>,
    pub system_ram_gb: u32,
    pub cpu_cores: u32,
    pub total_storage_gb: u32,
    pub free_storage_gb: u32,
    pub network_bandwidth_mbps: Option<u32>,
}

pub struct GpuInfo {
    pub name: String,
    pub vram_gb: u32,
    pub compute_type: ComputeType,  // Cuda, Rocm, Metal, Vulkan
    pub estimated_tflops: f32,
}

pub struct AvailableResources {
    pub free_vram_gb: f32,
    pub free_ram_gb: f32,
    pub free_storage_gb: f32,
    pub gpu_utilization: f32,
    pub cpu_utilization: f32,
    pub available_for_mesh: bool,  // User can pause contribution
}

pub struct CorpusShardInfo {
    pub corpus_id: String,
    pub chunk_range: Option<ChunkRange>,
    pub is_replica: bool,
    pub last_updated: u64,
}
```

The daemon reads GPU state every 5 seconds (via `nvidia-smi`, `rocm-smi`, or Metal APIs) and storage state every 30 seconds. Updates propagate via gossip only when they cross significance thresholds.

### Network Latency Measurement

Periodic RTT probes (small UDP packets, every 30 seconds) build a pairwise latency matrix. Nodes share measurements via gossip, so every node has the full mesh topology for scheduling decisions.

```rust
pub struct LatencyMatrix {
    entries: HashMap<(NodeId, NodeId), LatencyRecord>,
}

pub struct LatencyRecord {
    pub rtt_ms: f32,        // Exponentially weighted moving average
    pub jitter_ms: f32,
    pub bandwidth_estimate_mbps: f32,
    pub last_measured: u64,
}
```

---

## 3. Model Registry

The mesh maintains a shared understanding of available models.

```rust
pub struct ModelInfo {
    pub id: ModelId,
    pub name: String,
    pub repo: String,
    pub file: String,
    pub size_bytes: u64,
    pub total_layers: u32,
    pub architecture: ModelArchitecture,
    pub available_on: HashMap<NodeId, ModelAvailability>,
    pub oicp_capabilities: CapabilityProfile,  // OICP ratings for this model
    pub quantization: String,                   // "Q4_K_M", "Q8_0", etc.
}
```

### OICP Capability Profiles

Each model in the registry carries an OICP capability profile — ratings on the nine capability dimensions (General, Code, Analysis, Math, Creative, Instruction, Multilingual, Vision, LongContext) at the five-point proficiency scale. These ratings come from the community profile registry (a public git repo of per-model-per-quantization TOML files) with optional local overrides.

```toml
# In mesh-wide config
[[mesh.models.available]]
repo = "Qwen/Qwen3-Coder-30B-GGUF"
quant = "Q4_K_M"
oicp_profile = "qwen/qwen3-coder-30b-Q4_K_M"  # From community registry

[[mesh.models.available]]
repo = "Qwen/Qwen3-30B-GGUF"
quant = "Q4_K_M"
oicp_profile = "qwen/qwen3-30b-Q4_K_M"
```

### Model Acquisition

Downloads prioritize local peer transfer — if another mesh node (or a peered mesh) has the model, it serves the file directly over HTTP rather than downloading from Hugging Face. The mesh-wide preferred model list propagates via gossip; members with sufficient storage are prompted to download.

---

## 4. Scheduler

The Scheduler is the core intelligence of Commonwealth. It manages two types of resource allocation: **inference** (which nodes host which model layers) and **knowledge** (which nodes host which corpus index shards).

### 4.1 Inference Scheduling

#### Model Portfolio Management

The Scheduler maintains a portfolio of loaded models, not just a single model. On meshes with sufficient aggregate VRAM, multiple models are loaded simultaneously — a coding model and a general-purpose model, for instance — serving different request types concurrently.

```rust
pub struct InferencePlan {
    pub model_plans: Vec<ShardPlan>,  // One per loaded model
}

pub struct ShardPlan {
    pub model: ModelId,
    pub entry_node: NodeId,
    pub assignments: Vec<ShardAssignment>,
    pub estimated_tokens_per_sec: f32,
    pub estimated_ttft_ms: u32,
}

pub struct ShardAssignment {
    pub node_id: NodeId,
    pub layers: LayerRange,
    pub gpu_index: u32,
    pub rpc_address: SocketAddr,
}
```

#### OICP-Aware Model Selection

When a request arrives with OICP capability requirements, the Scheduler scores each loaded model against the requirements:

```rust
impl Scheduler {
    pub async fn select_model_for_request(
        &self,
        oicp: &InferenceRequirements,
    ) -> Result<&ShardPlan> {
        let plans = self.current_inference_plan.read().await;

        // Check if any loaded model satisfies the requirements.
        let mut best_plan: Option<(&ShardPlan, f32)> = None;

        for plan in &plans.model_plans {
            let model = self.mesh.models.get(&plan.model)?;
            if !satisfies_required(&model.oicp_capabilities, &oicp.required) {
                continue;
            }
            let score = score_preferred(&model.oicp_capabilities, &oicp.preferred);
            if best_plan.map_or(true, |(_, best)| score > best) {
                best_plan = Some((plan, score));
            }
        }

        if let Some((plan, _)) = best_plan {
            return Ok(plan);
        }

        // No loaded model satisfies. Consider swapping.
        self.consider_model_swap(oicp).await
    }
}
```

The cached resolution from OICP requirements to a specific model plan is a hash map lookup — sub-millisecond. The mapping is recomputed only when the portfolio changes.

#### Model Swap Decisions

The Scheduler only swaps models when the capability mismatch is significant:

```rust
async fn consider_model_swap(&self, oicp: &InferenceRequirements) -> Result<&ShardPlan> {
    let current_best = self.best_loaded_score(oicp);
    let potential_best = self.best_available_score(oicp);

    // Only swap if the improvement justifies the cost (30-60 second load time).
    if potential_best - current_best < SWAP_THRESHOLD {
        // Serve on the current best, even if it's not ideal.
        return self.current_best_plan(oicp);
    }

    // Swap: load the better model, keep old model serving during transition.
    self.transition_model(oicp).await
}
```

#### Graceful Model Transitions

When the Scheduler decides to swap models, it does not hard-cut. The old model continues serving while the new model loads. Only after the new model is ready does the Scheduler drain the old model and unload it.

```rust
pub struct ModelTransition {
    pub outgoing: ShardPlan,
    pub incoming: ShardPlan,
    pub state: TransitionState,
}

pub enum TransitionState {
    Loading,     // New model loading. Old model still serving.
    Ready,       // New model loaded. Draining old model.
    Complete,    // Old model unloaded.
}
```

This eliminates 503 gaps during model swaps. Requires enough aggregate memory to hold both models during the transition — if the mesh can't do this, it falls back to hard-cut.

#### Usage Pattern Prediction

The Scheduler reads the contribution ledger's historical data to predict demand patterns:

```rust
pub struct UsagePredictor {
    patterns: HashMap<(Weekday, Hour), CapabilityDistribution>,
}

pub struct CapabilityDistribution {
    pub code_fraction: f32,
    pub analysis_fraction: f32,
    pub general_fraction: f32,
}
```

If the histogram shows coding requests dominate weekday mornings and research requests dominate evenings, the Scheduler preemptively loads the appropriate model during idle transition periods — before the first mismatched request arrives.

#### Layer Assignment

Given a model, the Scheduler assigns layers across available nodes:

- **Proportional allocation.** More VRAM → more layers.
- **Contiguous assignment.** Each node gets a contiguous range to minimize cross-node communication.
- **Topology-aware ordering.** Adjacent ranges go to low-latency node pairs.
- **Privacy-aware entry node selection.** Prefer making the requester's own node the entry point (layer 0 host) when feasible, because the entry node sees the full prompt.

#### Scheduling Modes

**Persistent plans.** The common case. Models stay loaded. Requests served immediately.

**On-demand scheduling.** For models not currently loaded. 5-30 seconds for model load.

**Opportunistic rebalancing.** Background process checks if the current plan is still optimal. Rebalances only during idle periods.

#### Concurrency

When two nodes independently decide to schedule a new model, leader election per scheduling decision resolves conflicts. Lower NodeId wins. Simple, deterministic, rare.

### 4.2 Knowledge Scheduling

#### Corpus Index Sharding

Knowledge bases are sharded across nodes based on available disk space, the same way model layers are sharded across nodes based on available VRAM.

```rust
pub struct KnowledgeShardPlan {
    pub assignments: Vec<KnowledgeShardAssignment>,
    pub redundancy_achieved: HashMap<String, usize>,  // corpus_id → replica count
}

pub struct KnowledgeShardAssignment {
    pub node_id: NodeId,
    pub corpus_id: String,
    pub chunk_range: Option<ChunkRange>,  // None = entire corpus on this node
    pub is_replica: bool,
}

pub struct ChunkRange {
    pub start_id: u64,
    pub end_id: u64,  // exclusive
}
```

The natural first partition is by corpus — Wikipedia on nodes A and B, OpenAlex on nodes C and D, SEP on node E. When a single corpus is too large for any one node, it is split by chunk ID range within the corpus.

#### Knowledge Assignment

```rust
pub struct KnowledgeAssigner;

impl KnowledgeAssigner {
    pub fn assign(
        &self,
        corpora: &[CorpusInfo],
        nodes: &[NodeWithCapacity],
        redundancy_target: usize,  // Minimum copies per shard
    ) -> Result<KnowledgeShardPlan> {
        // For each corpus:
        //   1. If it fits on one node, assign it whole.
        //   2. If it doesn't, split by chunk ID range proportional
        //      to nodes' free disk space.
        //   3. Assign replicas to different nodes than the primary.
        //   4. Co-locate corpora often queried together (wikipedia + sep)
        //      on the same node when possible.
        //   5. Respect mesh_sharing flags — corpora with restricted
        //      licenses are not replicated, only stored on nodes
        //      that ingested from source.
    }
}
```

#### Knowledge Query Path

A knowledge query from a client fans out to all nodes holding relevant shards in parallel:

```
Query: "Ostrom design principles commons governance"
Corpora needed: wikipedia, openalex, sep

        ┌──────────────────────────────┐
        │  Requesting node (Eve)       │
        │  1. Receives query + embedding│
        │  2. Fan out to shard hosts   │
        └──┬──────────┬───────────┬────┘
           │          │           │
           ▼          ▼           ▼
       Node A      Node C      Node E
       wikipedia   openalex    sep
       (search)    (search)    (search)
           │          │           │
           └──────────┴───────────┘
                      │
                      ▼
              Merge + rerank at Eve
```

Each shard node runs sqlite-vec nearest-neighbor search and FTS5 keyword search over its local chunks, returns top-K results. The requesting node merges across shards, deduplicates, reranks by score, and returns the global top-K.

Latency: ~50-100ms over LAN for a fan-out to 3-5 shard nodes. Well within the pipeline's time budget.

#### Redundancy

Each shard is hosted on at least 2 nodes (configurable). When a node goes offline, queries route to the replica. When a new node joins with free disk space, the Scheduler may assign it as a new replica for under-replicated shards.

---

## 5. Orchestrator

The Orchestrator translates scheduling decisions into running processes and managed indexes.

```rust
pub struct Orchestrator {
    inference_processes: HashMap<ProcessId, ManagedProcess>,
    knowledge_shards: HashMap<String, ManagedShard>,  // corpus_id → shard state
    config: OrchestratorConfig,
}
```

### Inference Process Management

When the Scheduler produces a `ShardPlan`, the Orchestrator on each assigned node starts `rpc-server` processes bound to assigned GPUs and layer ranges. The entry node starts `llama-server` configured with RPC backends pointing at all assigned nodes.

For multi-model portfolios, the Orchestrator manages multiple `llama-server` processes on the entry node — one per loaded model, each on a different port. The API layer routes incoming requests to the appropriate `llama-server` based on OICP capability matching.

### Knowledge Shard Management

When the Scheduler assigns a knowledge shard to a node, the Orchestrator:

1. Checks if the corpus index data is already present locally (from a prior ingestion or peer transfer).
2. If not, initiates a transfer from a mesh peer that has it — or, for corpora with `mesh_sharing = false`, prompts the user to run the corpus recipe locally.
3. Opens the shard's SQLite database for search queries.
4. Reports the shard as available to the mesh via gossip.

### Process Health Monitoring

Liveness checks (PID alive), responsiveness checks (HTTP health endpoint for llama-server, TCP connect for rpc-server), and performance tracking (request latency).

If a process dies or becomes unresponsive:

1. Orchestrator marks it failed, reports via gossip.
2. Scheduler recomputes the affected plan (inference or knowledge).
3. All nodes apply the new plan. Recovery: 5-15 seconds.

### Graceful Departure

**Design intent, not yet implemented** — there is no `pause` command
today. The intended behavior: announce impending departure, give the
Scheduler a 30-second window to preemptively rebalance model layers and
knowledge shards onto surviving nodes, then go quiet with no 503s. Until
that ships, stopping the daemon is an abrupt departure and recovery
follows the node-failure path above (5-15 seconds).

---

## 6. API Layer

### Client API — Inference (OpenAI-Compatible)

```
POST /v1/chat/completions
     Standard OpenAI chat completions. Streamed or non-streamed.
     The daemon routes to the appropriate llama-server based on
     OICP requirements in the request body.

     Extended field (non-standard):
     "oicp": {
       "capabilities": { "required": {...}, "preferred": {...} },
       "performance": { "latency": "interactive" },
       "privacy": { "sharding": "mesh_allowed" }
     }

GET  /v1/models
     Lists models currently available on the mesh.
     Includes OICP capability profiles and performance estimates.
```

Every node in the mesh can accept client requests. The daemon routes internally — the client always talks to localhost:9741.

#### Privacy Enforcement

Requests with `privacy: local_only` are NOT forwarded to the mesh for inference. The API layer returns 400 if it receives such a request, because `local_only` requests should be handled by the client's local inference engine and never reach Commonwealth. If a `local_only` request does arrive (client misconfiguration), the error message says so clearly.

Requests with `privacy: mesh_allowed` are served normally across the mesh.

#### OICP Request Routing

The API layer inspects the `oicp` field, looks up the cached OICP-to-model resolution, and forwards the request to the correct `llama-server` process. If no `oicp` field is present, the default model (the first in the portfolio) is used.

Multiple concurrent requests with different OICP requirements route to different models simultaneously. This is how a mesh serves a coding agent and a research assistant at the same time.

### Client API — Knowledge

```
POST /v1/knowledge/search
     Search the mesh's knowledge index.
     Body: {
       "query_embedding": [0.123, -0.456, ...],
       "query_text": "Ostrom design principles commons governance",
       "corpora": ["wikipedia", "openalex", "sep"],
       "limit": 20
     }
     → {
       "results": [
         {
           "content": "Elinor Ostrom identified eight...",
           "title": "Elinor Ostrom",
           "corpus_id": "wikipedia",
           "score": 0.89,
           "url": "https://en.wikipedia.org/wiki/Elinor_Ostrom",
           "metadata": { ... }
         },
         ...
       ]
     }
```

The query embedding is computed by the client (Sovereign's local Embed slot). The API layer fans out the query to all nodes holding relevant shards, merges results, and returns the global top-K.

Privacy: knowledge queries carry the query text and embedding. These reveal what the user is researching but not their personal content. Clients that need complete query privacy should search only their local corpus and not call this endpoint.

### Client API — OICP Capabilities

```
GET  /oicp/v1/capabilities
     Provider manifest per the OICP spec.
     {
       "oicp_version": "0.2.0",
       "provider": {
         "name": "Sunset District Co-op",
         "type": "mesh"
       },
       "models": [
         {
           "id": "qwen3-coder-30b-q4km",
           "quantization": "Q4_K_M",
           "capabilities": { "code": 4, "instruction": 3, "general": 2 },
           "context_tokens": 32768,
           "status": {
             "available": true,
             "loaded": true,
             "estimated_tokens_per_sec": 45.0,
             "estimated_ttft_ms": 1100
           }
         }
       ],
       "knowledge": {
         "corpora": [
           {
             "id": "wikipedia",
             "total_chunks": 6800000,
             "shards": 3,
             "replicas": 2,
             "fully_available": true,
             "last_updated": "2026-03-15"
           }
         ],
         "search_endpoint": "/v1/knowledge/search"
       },
       "federation": {
         "peers": [
           {
             "name": "Mission District Co-op",
             "capabilities_url": "http://10.0.1.50:9741/oicp/v1/capabilities",
             "trust_level": "model_and_knowledge_sharing"
           }
         ]
       }
     }
```

Clients poll this every 30 seconds to maintain a current view of mesh capabilities — both inference models and knowledge corpora. The `federation.peers` field lets clients optionally discover capabilities across peered meshes.

### Status API

```
GET  /status
     {
       "node_id": "...",
       "mesh": {
         "name": "Sunset District Co-op",
         "members_online": 4,
         "members_total": 5,
         "pooled_vram_gb": 98.5,
         "pooled_storage_gb": 3400
       },
       "inference": {
         "loaded_models": [
           { "model": "qwen3-coder-30b-q4km", "nodes": 3, "tps": 45.0 },
           { "model": "qwen3-30b-q4km", "nodes": 2, "tps": 38.0 }
         ]
       },
       "knowledge": {
         "hosted_corpora": ["wikipedia", "openalex", "sep"],
         "total_chunks_searchable": 21500000
       },
       "contribution": {
         "compute_hours": 3.2,
         "storage_gb": 170,
         "bandwidth_gb": 48.2,
         "balance": "+52.8h equivalent"
       }
     }
```

### Mesh-Internal API

Nodes communicate on a separate port (default 9742), authenticated by mutual TLS.

```
POST /internal/gossip            — Member state exchange
POST /internal/scheduling/intent — Scheduling lock acquisition
POST /internal/scheduling/plan   — Shard plan broadcast
POST /internal/model/transfer    — Peer-to-peer model file transfer
POST /internal/index/transfer    — Peer-to-peer corpus index transfer
POST /internal/knowledge/search  — Inter-node shard query (fan-out)
GET  /internal/latency/probe     — RTT measurement
```

---

## 7. Capacity Fairness

### The Ledger

Every completed inference request, knowledge query, and resource contribution produces ledger entries.

```rust
pub struct LedgerEntry {
    pub timestamp: u64,
    pub node_id: NodeId,
    pub kind: LedgerEntryKind,
    pub amount: f64,
    pub unit: ContributionUnit,
}

pub enum LedgerEntryKind {
    Contributed { served_request_from: NodeId },
    Consumed { served_by: Vec<NodeId> },
}

pub enum ContributionUnit {
    GpuSeconds,      // Inference compute
    StorageGbDays,   // Corpus index hosting
    BandwidthGb,     // Index transfers, model transfers, query serving
}
```

The ledger is append-only, replicated to all nodes via gossip, human-readable.

```
$ commonwealth balance

Sunset District Co-op — Contribution Balance (last 30 days)
──────────────────────────────────────────────────────────────
Node              Compute    Storage    Bandwidth    Balance
──────────────────────────────────────────────────────────────
Alice's Desktop    12.3h      0 GB       2.1 GB      +14.4
Bob's Build        18.7h      0 GB       1.8 GB      +20.5
Carol's Mac         8.6h     170 GB      48.2 GB     +52.8
Dave's Rig         22.1h      55 GB      12.4 GB     +34.5
Eve's MacBook Air   0.2h      0 GB       0.1 GB      -18.3
──────────────────────────────────────────────────────────────
```

Carol's Mac contributes most via storage and bandwidth (hosting the full corpus index). Dave contributes most via compute. Eve contributes almost nothing but consumes mesh resources. The ledger makes this visible.

### Fairness Policy

```rust
pub enum FairnessPolicy {
    /// Default. Everyone sees the ledger. Social pressure does the work.
    Transparent,
    /// Below threshold: lower scheduling priority.
    SoftThrottle { threshold_hours: f64, priority_reduction: f32 },
    /// Below threshold: requests denied.
    HardCap { threshold_hours: f64 },
}
```

The group decides. Commonwealth doesn't have an opinion about what's fair.

---

## 8. Fault Tolerance

### Node Departure During Inference

1. Entry node detects RPC connection failure. Active requests get HTTP 503 with `Retry-After: 10`.
2. Node-failure event broadcasts to mesh.
3. Scheduler recomputes inference and knowledge plans excluding the failed node.
4. Surviving nodes apply new plans. Recovery: 5-15 seconds.
5. If the failed node hosted knowledge shards with replicas, queries route to replicas seamlessly.

### Node Departure During Knowledge Query

If a knowledge shard node fails mid-query, the requesting node receives an error for that shard only. Other shards return results normally. The merged result set may be incomplete — the OICP capabilities endpoint reports `fully_available: false` for affected corpora. Clients can factor this into coverage assessment.

### Graceful Departure

`commonwealth pause` announces impending departure with a 30-second countdown. The Scheduler preemptively rebalances both inference and knowledge plans. In-flight requests complete on the old plan. New requests use the new plan. No 503s.

### Node Return

Returned nodes are noted but don't trigger immediate rebalancing. The next scheduled rebalance or the next capacity shortage incorporates them.

---

## 9. Security Model

### Threat Model

The authoritative, surface-by-surface threat model lives in
[`docs/THREAT_MODEL.md`](../docs/THREAT_MODEL.md) at the repository root;
this table is a summary and defers to it wherever they disagree.

|Threat|Mitigation|
|---|---|
|Unauthorized join|Join key required (BLAKE3 hash, constant-time compare), shared out-of-band, plus an Ed25519 proof-of-possession — a bad proof is rejected with 401.|
|Unintended client access|The client API (`:9741`) requires a bearer token from any non-loopback caller (loopback is exempt) and fails closed when no token is configured.|
|Eavesdropping|**Plaintext mesh is the default.** An encrypted mesh (founder-set policy) moves inter-node traffic onto iroh QUIC/TLS and forces the client API to loopback. The worker-pod path is TLS with an owner-pinned certificate. Unused per-session-TLS scaffolding was deliberately removed (2026-06-15) rather than left as a security façade — never claim blanket inter-node encryption.|
|Node impersonation|Ed25519 identity proof at join; on an encrypted mesh, iroh dial-by-key authenticates the peer's key on every connection.|
|Tensor-split RPC|**Not encrypted.** Multi-host tensor sharding (`SOVEREIGN_RPC_SERVE`) is raw TCP — the sole residual plaintext even on an encrypted mesh. Never claim end-to-end encryption while it is in use.|
|Compromised node serving bad inference|**Not defended.** Social trust model.|
|External attacks|The internal API (`:9742`) is expected to sit behind a VPN/LAN/tailnet perimeter, never the public internet.|
|Prompt privacy|Entry node sees full prompt. Scheduler prefers requester as entry node. Other nodes see only opaque activations.|
|Query privacy|Knowledge queries reveal research topics, not personal content. Clients control what they query.|

### Activation Privacy

When inference is sharded, intermediate activations traverse the network. These are floating-point tensors, not human-readable text. However, research has demonstrated activation inversion attacks that can recover portions of input text. The Scheduler mitigates this by preferring the requester's own node as the entry point (which processes layer 0 and sees the full prompt), reducing the number of nodes that handle early-layer activations. Clients that need stronger guarantees should use `privacy: local_only` in their OICP requirements.

---

## 10. Configuration

### Daemon Configuration

```toml
# ~/.commonwealth/config.toml

[node]
name = "Alice's Desktop"
data_dir = "~/.commonwealth"
api_port = 9741
internal_port = 9742

[contribution]
schedule = "always"       # "always" | "idle" | "manual" | cron
reserve_vram_gb = 4
reserve_ram_gb = 8
reserve_storage_gb = 50   # Keep at least 50GB free

[inference]
llama_server = "/usr/local/bin/llama-server"
rpc_server = "/usr/local/bin/rpc-server"

[knowledge]
index_dir = "~/.commonwealth/indexes"  # Where corpus shards are stored

[fairness]
policy = "transparent"

[network]
vpn_interface = "wg0"     # Optional: for cross-LAN discovery
```

### Mesh-Wide Configuration

Propagated via gossip. Any member proposes; majority confirms.

```toml
[mesh.policy]
fairness = "transparent"
max_concurrent_requests = 10
redundancy_target = 2          # Minimum copies per shard

[mesh.models]
preferred = [
    { repo = "Qwen/Qwen3-Coder-30B-GGUF", quant = "Q4_K_M",
      oicp_profile = "qwen/qwen3-coder-30b-Q4_K_M" },
    { repo = "Qwen/Qwen3-30B-GGUF", quant = "Q4_K_M",
      oicp_profile = "qwen/qwen3-30b-Q4_K_M" },
]

[mesh.corpora]
preferred = ["wikipedia", "openalex", "sep", "stackexchange"]
```

---

## 11. CLI

Every command below does real work — the aspirational placeholder
commands that printed `(In production, this would …)` were removed
2026-07-01 rather than left as a façade. Mesh lifecycle across nodes
(create / join / status / rotate) lives in the `sovereign` CLI; the
HTTP API on `:9741`/`:9742` is the daemon's actual control plane.

```
commonwealth init --name "Mesh Name"     Create a mesh, output join key
commonwealth status                      Node + mesh state from the running daemon (GET /status)
commonwealth balance                     Contribution ledger (local store)
commonwealth models                      Models advertised by the daemon (GET /v1/models)
commonwealth corpus status               Ingestion/shard status for tracked corpora
commonwealth corpus collaborate <id>     Recruit peers to share a mid-flight ingestion
commonwealth daemon start                Run the daemon
commonwealth recipe test <path>          Run the community-recipe test harness
commonwealth recipe validate <path>      Validate recipe fields without downloading
commonwealth peer-preference set|list|clear   Per-peer affinity (Ostrom sanctions, local-only)
```

Deliberately absent: member revocation (`Mesh::merge_from` is grow-only
with no tombstone, so a revoke cannot propagate — shipping the command
before the tombstone would report false success on a security action),
graceful pause/resume/leave (see §8 Graceful Departure), and daemon log
files (the daemon logs to stderr; use your service manager's journal).

The daemon runs as a system service (`commonwealth daemon start/stop/status`).

---

## 12. Build and Distribution

### Single Binary

Commonwealth compiles to a single static binary. No runtime dependencies except llama.cpp binaries.

```
commonwealth/
├── crates/
│   ├── commonwealth-core/          # Types, mesh state, shard plans, ledger
│   ├── commonwealth-discovery/     # mDNS, gossip, latency probing
│   ├── commonwealth-scheduler/     # Model selection, layer + knowledge assignment
│   ├── commonwealth-orchestrator/  # Process management, index management
│   └── commonwealth-api/           # Axum HTTP server, OICP endpoint
└── Cargo.toml
    └── install.sh
```

### Installation

```bash
curl -sSf https://commonwealth.dev/install.sh | sh
```

### Platform Support

|Platform|GPU Support|Notes|
|---|---|---|
|Linux (x86_64)|CUDA, ROCm, Vulkan|Primary platform|
|macOS (ARM)|Metal|Via llama.cpp Metal backend|
|Windows (x86_64)|CUDA, Vulkan|Via WSL2 or native|

The daemon is pure Rust with no GPU dependencies — it manages processes that use GPUs, not GPU APIs directly.

---

## 13. Integration Contracts

### With Sovereign

Two touchpoints:

1. **Inference:** `RemoteApiProvider` at `http://localhost:9741/v1`. OpenAI-compatible with OICP extensions. Sovereign sends completion requests with per-step OICP requirements (capabilities, latency, privacy). Commonwealth routes to the appropriate model.
    
2. **Knowledge:** `RemoteKnowledgeProvider` at `http://localhost:9741/v1/knowledge/search`. Sovereign sends query embeddings and text, receives ranked corpus chunks. Used by resource-constrained nodes that don't store corpora locally.
    
3. **Capabilities:** `GET /oicp/v1/capabilities` polled every 30 seconds. Sovereign's `HybridProvider` refreshes its view of available models and knowledge.
    

Sovereign doesn't know about nodes, shard plans, or gossip. Commonwealth doesn't know about skills, Router intent, or task DAGs.

### With Any OpenAI-Compatible Client

Commonwealth serves any client that speaks the OpenAI chat completions protocol. OmO, Open WebUI, LiteLLM, curl. OICP requirements are optional — clients that don't send them get the default model.

---

## 14. What This Doesn't Do (Intentionally)

**No training or fine-tuning.** Inference and knowledge serving only.

**No model hosting.** Models download from Hugging Face or transfer from mesh peers.

**No Byzantine fault tolerance.** Social trust model.

**No NAT traversal.** Use WireGuard or Tailscale.

**No incentive token.** Constitutional constraint.

**No centralized coordination.** No master node, no registry, no cloud service. If the internet goes down, LAN nodes continue working.

**No corpus ingestion.** Commonwealth hosts and serves pre-built indexes. The actual ingestion (parsing, chunking, embedding) is done by Sovereign's corpus recipe engine. Commonwealth receives the resulting index and serves it. This separation keeps the daemon focused on coordination and serving, not on content processing.

---

## 15. Timeline

**Month 1:** Core types, mDNS discovery, gossip protocol with member state propagation. Two nodes discovering each other and exchanging capabilities.

**Month 2:** Inference scheduler (single model), Orchestrator (llama.cpp process management). Two nodes running a model sharded across them.

**Month 3:** API layer (OpenAI-compatible + OICP), fault tolerance, graceful pause/resume. Real inference requests served. Integration test: Sovereign connected via `RemoteApiProvider`.

**Month 4:** Multi-model portfolio management, OICP-aware model selection, usage prediction. Knowledge shard plan and knowledge query endpoint. Contribution ledger with compute + storage + bandwidth tracking.

**Month 5:** Mesh peering, peer-to-peer model and index transfer. Graceful model transitions. Packaging, systemd/launchd, documentation.

**Month 6:** Beta: 5-10 people in a real community running the mesh for daily use — inference and knowledge serving. Measure failure modes, scheduling quality, and social dynamics.

---

## 16. Success Metrics

**Primary:** Five people with heterogeneous hardware, on the same LAN, run a model larger than any individual machine can host and query a knowledge index larger than any individual machine can store — and a non-technical participant can do both from Sovereign without knowing the mesh exists. Setup to first query: under 15 minutes per node.

**Secondary:** A node departs mid-inference (hard power-off). The mesh recovers inference and knowledge availability within 30 seconds, with no manual intervention.

**Tertiary:** A resource-constrained node (MacBook Air, 256GB storage, no discrete GPU) joins a mesh and immediately has access to: a 70B model for synthesis, Wikipedia + OpenAlex + SEP for knowledge search, consuming only 4GB of local storage. The user cannot distinguish their experience from that of a well-resourced node except by checking the provenance metadata in Sovereign's responses.