// SPDX-License-Identifier: AGPL-3.0-or-later
//! Persistent configuration written by `sovereign setup` and read by
//! `sovereign daemon run`. Lives at `~/.sovereign/config.toml` —
//! co-located with the rest of the user-scoped sovereign state (corpora,
//! indexes, notes db, mesh.json). Distinct from the project-level
//! `.sovereign/sovereign.toml` which configures per-project watchers.
//!
//! The split is: user-scoped state (this file + everything else under
//! `~/.sovereign/`) versus project-scoped state (test/lint runners,
//! workspace roots — in the repo's `.sovereign/sovereign.toml`).
//!
//! ## Legacy location
//!
//! Earlier versions wrote this file to `dirs::config_dir()/sovereign/
//! config.toml` (i.e. `~/.config/sovereign/...` on Linux,
//! `~/Library/Application Support/sovereign/...` on macOS). `load()` and
//! `exists()` transparently migrate the file from that path on first
//! access; no operator action is required.
//!
//! This module used to live in `sovereign-cli`. It moved here so the
//! desktop app (which depends on `sovereign-core` but *not* on the CLI
//! binary crate) can read the same config and attach to a CLI-started
//! daemon without redefining the schema.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Top-level structure of `~/.sovereign/config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupConfig {
    /// `[models]` — GGUF slot paths. The only required section.
    pub models: ModelsSection,
    /// `[daemon]` — listener ports and service options.
    #[serde(default)]
    pub daemon: DaemonSection,
    /// `[data]` — where mutable state lives on disk.
    #[serde(default)]
    pub data: DataSection,
    /// Operator-tunable defaults for the watched-folder reconciliation
    /// scheduler. Per-corpus values stored in
    /// `WatchedFolderConfig` always win — this section just supplies
    /// the defaults that `corpus watch` uses when a CLI flag is
    /// omitted, plus a global `paused_at_boot` override for batch
    /// hosts that don't want sweeps running unattended.
    #[serde(default)]
    pub watched_folders: WatchedFoldersSection,
    /// Rolling-summary memory compaction (see
    /// `crate::memory_compaction`). Default values are inner-work-
    /// tuned (threshold=6, batch=3, async); operators on other
    /// surfaces can `mode = "disabled"` to opt out until their own
    /// benches exist.
    #[serde(default)]
    pub memory: MemorySection,
    /// Dial-by-key mesh access over iroh. Off by default; see below.
    #[serde(default)]
    pub iroh: IrohSection,
    /// Opt-in participation in a mesh-hosted shared primary model.
    /// Off by default; see [`SharedModelSection`].
    #[serde(default)]
    pub shared_model: SharedModelSection,
    /// How this node finds + joins mesh peers (mDNS vs. static seeds).
    /// Defaults reproduce the zero-config LAN behaviour; enterprise/VPC
    /// fleets that block multicast turn mDNS off and list seed addresses
    /// here. See [`DiscoverySection`].
    #[serde(default)]
    pub discovery: DiscoverySection,
    /// External MCP servers whose tools are loaded into the agent's tool
    /// registry at startup (the `[[mcp_servers]]` array). Read by every chat
    /// surface — `sovereign chat`, the desktop, and `sovereign serve` — via
    /// the shared loader `sovereign_tools::mcp::load_from_setup_config`, so a
    /// server added in one place is available in all of them. Declared last so
    /// it serializes as a trailing array-of-tables (valid TOML after the
    /// scalar sections above). Empty by default; back-compat is automatic
    /// (`#[serde(default)]`), and being a typed field it survives a
    /// `save()`/`load()` round-trip instead of being dropped as an unknown key.
    #[serde(default)]
    pub mcp_servers: Vec<crate::mcp_config::McpServerConfig>,
}

/// Dial-by-key mesh access over iroh (Track W of
/// `sovereign/docs/specs/TRANSPORT_MIGRATION.md`). When `enabled`, the
/// daemon binds an iroh endpoint from its `<data_dir>/node_key`
/// identity — the SAME Ed25519 key it already gossips as
/// `MemberRecord.node_pubkey`, so "known member" and "dialable by key"
/// are one fact — and forwards accepted bi-streams to the local
/// internal and client routers, chosen by negotiated ALPN
/// (`cwth/http/0` → internal, `cwth/client/0` → client). A peer or
/// phone can then reach this daemon with no VPN.
///
/// Off by default and **purely additive**: the tailnet/LAN
/// (`IpTransport`) path is unaffected whether this is on or off — this
/// only makes the daemon *also* reachable by key. Spec name for this
/// block is `[mesh.iroh]`; in `~/.sovereign/config.toml` (the unified
/// SetupConfig) it is the top-level `[iroh]` section, matching
/// `sovereign-server`'s `[iroh]`.
///
/// ```toml
/// [iroh]
/// enabled = true
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IrohSection {
    /// Tri-state on purpose. `None` (absent — the common case) means
    /// AUTO: the daemon turns iroh on iff this node participates in a
    /// mesh (the `client-exposed` marker written by every explicit
    /// create/join surface) — consent-by-mesh-participation, so a
    /// meshless daemon never contacts relay infrastructure.
    /// `Some(true)` forces on (headless/explicit); `Some(false)` is
    /// the kill-switch (still overridden by a mesh-wide
    /// `require_encryption`, which cannot run without iroh).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Per-traffic-class transport routing (Track W3). Only consulted
    /// when `enabled`. Since the iroh-first flip (2026-07), iroh
    /// enabled means EVERY class routes iroh-first with automatic
    /// per-dial IP fallback, and this section is an opt-OUT: name a
    /// class `"ip"` to pin it to the IP path. A legacy `"iroh"` entry
    /// names the default (logged no-op). Nested here (not a top-level
    /// `[transport]`) because routing a class to iroh is meaningless
    /// without the endpoint this section turns on, and nesting means
    /// existing `SetupConfig` literals (which build `iroh` via
    /// `Default`) need no change.
    ///
    /// ```toml
    /// [iroh]
    /// enabled = true
    /// [iroh.transport]
    /// inference = "ip"   # opt one class out; everything else rides iroh-first
    /// ```
    #[serde(default)]
    pub transport: TransportSection,
    /// Self-hosted iroh relays (W4). Empty (the default) = n0's public
    /// relays, the bootstrap posture. Non-empty overrides the relay set
    /// with these URLs (address-lookup discovery is unchanged), so an
    /// enterprise fleet can point every node at its own `iroh-relay` on
    /// an allowlisted domain:443 — the answer for a corporate firewall
    /// that category-blocks n0's relay domains. Per-node, gossiped via
    /// `MemberRecord.relay_url`, so a mixed fleet interops with no
    /// flag-day. Consumed by `build_relayed_endpoint`.
    ///
    /// ```toml
    /// [iroh]
    /// enabled = true
    /// relay_urls = ["https://relay.corp.example:443"]
    /// ```
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relay_urls: Vec<String>,
    /// Which discovery/relay infrastructure to use (H1 sovereignty
    /// knob). `"n0"` or absent (the default) = n0's public relays AND
    /// n0's DNS/pkarr address-lookup. `"none"` / `"self"` / `"local"`
    /// = sever ALL n0 contact: reach peers only via gossiped direct
    /// addresses (a flat LAN/VPC) and/or `relay_urls` above (a
    /// self-hosted relay). Setting `relay_urls` ALONE does not stop the
    /// n0 DNS lookup — set `discovery = "none"` for a true no-third-party
    /// deployment. Consumed by `build_relayed_endpoint` via `RelayConfig`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery: Option<String>,
}

/// Per-traffic-class transport selection (Track W3 of
/// TRANSPORT_MIGRATION.md). Each class is `"iroh"` (default when
/// `[iroh] enabled` — dial-by-key QUIC, iroh-first with per-dial IP
/// fallback) or `"ip"` (pin to the tailnet/LAN overlay). Unset =
/// the default. `inference = "ip"` is the escape hatch if streaming
/// latency regresses on a flipped mesh. The interpretation (string →
/// `TrafficClass`) lives in `sovereign-mesh`, which owns both this
/// config and the transport types; this struct is intentionally just
/// data.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportSection {
    /// Transport for mesh gossip; `None` = default (see type doc).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gossip: Option<String>,
    /// Transport for control-plane RPC; `None` = default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_plane: Option<String>,
    /// Transport for knowledge-search fan-out; `None` = default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge_search: Option<String>,
    /// Transport for model transfers; `None` = default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_transfer: Option<String>,
    /// Transport for inference traffic — the latency-sensitive class and the documented escape hatch (`"ip"`); `None` = default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inference: Option<String>,
    /// Transport for status probes; `None` = default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_probe: Option<String>,
}

/// `[shared_model]` — opt-in participation in a mesh-hosted shared
/// primary model (e.g. a desktop fleet collectively running one big
/// model across an RPC layer-split, like GLM-5.2). Off by default: the
/// zero value is `role = "consumer"` with no `model_id`, which is
/// inert. The node's `[models] primary` stays its LOCAL model and the
/// always-available fallback; this section is an overlay that, when a
/// shared model is actually available on the mesh, routes the primary
/// turn into it instead (see the desktop-fleet plan).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SharedModelSection {
    /// This node's role in the shared-model cluster.
    #[serde(default)]
    pub role: SharedModelRole,
    /// The shared model's id, as advertised in peers' `loaded_models`.
    /// `None` ⇒ not participating (the section is inert).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// The designated host — the node that owns the loaded distributed
    /// instance. `None` ⇒ discover from the mesh; set explicitly to
    /// pin the always-on anchor as host (the v1 recommendation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_node_id: Option<String>,
    /// Minimum eligible anchors before the host attempts the distributed
    /// load and advertises the model available — the quorum gate. Below
    /// it the cluster reports "forming (k/N)".
    #[serde(default = "default_quorum_anchors")]
    pub quorum_anchors: u32,
    /// Optional explicit floor (GB) on pooled anchor memory. The host
    /// always gates on the computed `sum(anchor_vram) >= model_size × 1.2`;
    /// set this to require headroom beyond that. `None` ⇒ computed check
    /// only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_pooled_gb: Option<f64>,
    /// How anchors fetch their shard of the model — the host emits this as
    /// `SOVEREIGN_RPC_SHARD_FETCH`. Defaults to [`ShardFetch::Ranges`] (each
    /// node pulls only its slice), which is required whenever no single node
    /// can hold the whole GGUF — the desktop-fleet case (e.g. a 440 GB model on
    /// 64 GB nodes).
    #[serde(default)]
    pub shard_fetch: ShardFetch,
}

/// How a shared-model anchor obtains its shard of the GGUF.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShardFetch {
    /// Range-GET only this node's tensors (`O(model/N)` disk). Required when no
    /// node can hold the whole GGUF; the desktop-fleet default. NOTE: this is
    /// the least-validated distributed path — built + unit-tested, not yet
    /// proven cross-machine at scale.
    #[default]
    Ranges,
    /// Fetch the whole GGUF to disk, then warm this node's shard from it. Needs
    /// full-model disk per anchor; simpler, and the path closest to what's been
    /// validated cross-machine.
    Whole,
}

impl ShardFetch {
    /// The `SOVEREIGN_RPC_SHARD_FETCH` value the host advertises to the RPC
    /// warm/load path.
    pub fn as_env(self) -> &'static str {
        match self {
            ShardFetch::Ranges => "ranges",
            ShardFetch::Whole => "whole",
        }
    }
}

/// A node's role in a shared-model cluster (`[shared_model] role`). The
/// fleet stratifies into a small memory-holding anchor core and a larger
/// query-only consumer ring (see the desktop-fleet plan).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharedModelRole {
    /// Query the shared model; contribute no memory to holding it. The
    /// default — a node opts into anchoring explicitly.
    #[default]
    Consumer,
    /// Lend memory to the RPC layer-split that holds the model.
    Anchor,
    /// Own the loaded distributed instance and serve it to the fleet.
    /// Implies `Anchor` (the host also holds a shard).
    Host,
}

/// Default quorum: a single anchor (the assumed always-on 128 GB+ box)
/// is enough to attempt a load; raise for larger cores.
fn default_quorum_anchors() -> u32 {
    1
}

/// `[memory]` top-level section. Currently only nests
/// `[memory.compaction]`; future memory-layer knobs (retention,
/// decay tuning, scope-wall overrides) belong here too.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemorySection {
    /// `[memory.compaction]` — rolling-summary compaction knobs.
    #[serde(default)]
    pub compaction: crate::memory_config::CompactionConfig,
}

/// Absolute paths to the loaded GGUF models. Two slots are
/// required (`primary` + `embed`); `fast` is optional with a clean
/// subsume story (when unset, the primary model serves the fast role
/// — Speed::Fast requests land on the primary slot). A fourth slot
/// (`code`) is the optional PR-E2 specialization, loaded lazily when
/// a code-hinted request arrives.
///
/// Why `fast` is optional but `embed` is required: chat slots all
/// run the same `llama_decode` family, so primary can stand in for
/// fast (mmap weight-sharing makes the cost ~per-slot-KV-cache, not
/// per-weight-copy). Embedding is a different model class entirely
/// (Qwen3-Embedding vs Darwin/Qwen-instruct); the primary can't
/// substitute, so embed-less configs raise an invariant exception
/// at the first embed call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsSection {
    /// The "primary" model — what the UX calls the Main responder.
    /// Internally this is the `thoughtful` profile slot.
    pub primary: PathBuf,
    /// Optional fast/speed slot. When `None`, the primary slot
    /// subsumes the fast role — see [`Self::fast_path`] /
    /// [`Self::has_explicit_fast`]. The field stays private-ish in
    /// the API surface: callers should go through those methods
    /// instead of reading the Option directly, so the "subsume to
    /// primary" decision lives in one place. Right setting for
    /// VRAM-tight peers (e.g. L40S, 45 GB) where the primary alone
    /// (~30 GB Q6) leaves no room for a second model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast: Option<PathBuf>,
    /// Embedding model — required; a chat slot cannot substitute (different model class, see type doc).
    pub embed: PathBuf,
    /// Optional code-specialist model. When present, `code`-hinted
    /// inference requests route here instead of the primary. Lazy-
    /// loaded on first use and unloaded after the same idle window
    /// as the primary. `None` means the node relies on the primary
    /// model for code work (the common case; a well-rounded general
    /// model still handles code adequately per v0.3 §4.4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<PathBuf>,

    /// Per-slot llama.cpp context size (n_ctx). `None` falls back to
    /// `default_context_size()` (16384), the conservative default that
    /// fits a 30B+ primary on a 64 GB Mac without OOMing the KV cache.
    /// Bump this on a Strix Halo (128 GB unified) or any box where the
    /// primary's KV cache + weights + fast slot still fit comfortably:
    /// 32768 roughly doubles output budget for atlas Phase 1, where
    /// long structured outputs were truncating against the 16384 cap.
    /// Applies to all loaded slots (fast / primary / embed / code /
    /// extras) — per-slot override would need a richer schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_size: Option<u32>,

    /// Additional named chat slots loaded eagerly at daemon startup
    /// alongside primary/fast/embed/code. Keys are operator-chosen
    /// slot names; values are absolute paths to the GGUF files.
    /// Slots stay resident for the daemon's lifetime — they don't
    /// participate in the primary slot's idle-unload lifecycle.
    ///
    /// Routing: an inbound `/v1/chat/completions` request whose
    /// `model` field matches an extra slot's loaded model id (the
    /// gguf file stem) lands on that slot. Untagged requests still
    /// route via the existing Speed-based selection across
    /// fast/primary/code.
    ///
    /// Operators wire per-phase routing by combining this map with
    /// `EnrichConfig.chat_models` (corpus-side phase → model_id
    /// map). See `project_antifragile_pipeline_phase1.md` for the
    /// end-to-end picture.
    ///
    /// TOML shape:
    ///
    /// ```toml
    /// [models.extra]
    /// reasoning = "/path/to/Qwopus-27B-v3-Q6_K.gguf"
    /// bulk = "/path/to/Qwen3.5-9B.Q8_0.gguf"
    /// ```
    ///
    /// Empty map (the default) preserves the historical 3-slot
    /// behaviour for operators who haven't opted in.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, PathBuf>,

    /// Total memory budget for the eagerly-loaded extras lineup,
    /// in gigabytes. When set, an attempt to load a new extras slot
    /// that would push (existing extras' on-disk gguf size) +
    /// (new slot's gguf size) above this budget triggers LRU
    /// eviction: cold extras slots are dropped (in least-recently-
    /// used order, skipping any slot serving an in-flight request)
    /// until the new slot fits.
    ///
    /// `None` (the default) disables eviction entirely — the
    /// pre-LRU behaviour. Operators on tight VRAM (e.g. a 64 GB Mac
    /// running Qwopus-27B Q6 ≈ 21 GB primary + Qwen3.5-9B Q8 ≈ 9 GB
    /// extras + embed ≈ 1 GB + OS + KV cache) set this to a value
    /// like 12.0 to keep the cumulative extras footprint bounded
    /// without dropping the primary.
    ///
    /// Note: only the gguf size on disk is counted, NOT the KV
    /// cache or activation buffers. Real GPU memory use is roughly
    /// `gguf_size + n_ctx * model_size_factor`. Pad the budget
    /// accordingly — `max_extras_memory_gb` is a conservative
    /// upper bound on weights, not a strict OS-level limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_extras_memory_gb: Option<f32>,

    /// Multi-primary slot pool. When set, the daemon loads `copies`
    /// independent instances of `path` as separate primary-class slots
    /// at startup, in addition to the singleton `primary` field.
    /// Inbound chat-completion requests targeting Speed::Slow /
    /// Speed::Normal are dispatched round-robin across the pool, so a
    /// host with sufficient VRAM (e.g. MI300X 192 GB) can serve N
    /// concurrent Phase 1 extracts without queueing.
    ///
    /// `None` (the default) preserves single-primary behaviour. Use
    /// case is bulk ingest workers that want horizontal parallelism on
    /// one box; a 6-copy pool of Darwin-36B-Q6 (28 GB each) fits in
    /// 168 GB and yields ~6× throughput vs. a single slot.
    ///
    /// TOML shape:
    /// ```toml
    /// [models.primary_pool]
    /// copies = 6
    /// path = "/workspace/models/Darwin-36B-Opus-Q6_K.gguf"
    /// ```
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_pool: Option<PrimaryPoolSection>,
}

/// Multi-primary slot pool config. See `ModelsSection::primary_pool`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrimaryPoolSection {
    /// Number of additional primary-class slot copies to spawn at
    /// daemon startup. The singleton `primary` (always loaded) counts
    /// as one slot; `copies` of those plus the singleton total
    /// `1 + copies` primary slots. `0` is treated as no pool.
    pub copies: u32,
    /// GGUF path for each pool member. Today every copy points at the
    /// same file; future variants could carry per-slot model paths.
    pub path: PathBuf,
}

fn default_context_size() -> u32 {
    16384
}

impl ModelsSection {
    /// Effective n_ctx — the configured value, or the safe default.
    /// Use this anywhere you'd otherwise hardcode a context size, so
    /// cold-start and reload paths can't drift.
    pub fn effective_context_size(&self) -> u32 {
        self.context_size.unwrap_or_else(default_context_size)
    }

    /// Path the fast-slot loader should use, with primary as the
    /// fallback when no distinct fast model is configured. Callers
    /// that need to know whether a fast model is configured *as
    /// opposed to* subsumed by primary should use
    /// [`Self::has_explicit_fast`] alongside this. Encapsulates the
    /// subsume rule in one place so the rest of the codebase can
    /// treat fast as if it always existed.
    pub fn fast_path(&self) -> &Path {
        self.fast.as_deref().unwrap_or(&self.primary)
    }

    /// `true` when an explicit fast GGUF is configured (the
    /// `[models].fast` key is set in `config.toml`). `false` when
    /// the primary subsumes the fast role.
    ///
    /// Use when the distinction matters — e.g. VRAM accounting
    /// (don't double-count primary's weights), mesh advertising
    /// (don't list a duplicate `fast` slot to peers when it's just
    /// an alias), reload-diff (a change from None to Some/primary-
    /// equal is operator-visible). For path lookups, prefer
    /// [`Self::fast_path`].
    pub fn has_explicit_fast(&self) -> bool {
        self.fast.is_some()
    }

    /// Memory budget in raw bytes for the extras lineup.
    /// `None` when unset — caller treats that as "no eviction".
    /// Returned in bytes so the eviction policy can compare directly
    /// against per-slot `size_bytes`.
    pub fn max_extras_memory_bytes(&self) -> Option<u64> {
        self.max_extras_memory_gb.map(|gb| {
            // 1 GiB = 2^30 bytes. Saturate to u64::MAX on absurd
            // inputs rather than overflow.
            let bytes = (gb as f64) * (1u64 << 30) as f64;
            if bytes <= 0.0 {
                0
            } else if bytes >= u64::MAX as f64 {
                u64::MAX
            } else {
                bytes as u64
            }
        })
    }
}

/// Network listener configuration. Defaults match the spec:
/// :9741 serves /v1 + /mcp, :9742 carries internal mesh gossip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonSection {
    /// Port serving `/v1` + `/mcp` for clients (default 9741).
    #[serde(default = "default_client_port")]
    pub client_port: u16,
    /// Port carrying internal mesh gossip (default 9742).
    #[serde(default = "default_internal_port")]
    pub internal_port: u16,
    /// When true, `sovereign setup` registers a launchd/systemd service
    /// so the daemon survives logout/restart.
    #[serde(default = "default_autostart")]
    pub autostart: bool,
    /// Idle seconds before the lazy primary slot is unloaded to reclaim
    /// VRAM. Default 300s (5 min) — long enough that mid-conversation
    /// pauses (read the answer, think, formulate follow-up) don't
    /// cause an eviction-then-reload that re-pays the 10–90s
    /// model-load wait on the next turn, short enough that an
    /// abandoned session frees memory within a single coffee break.
    /// For batch workloads (the atlas enrich pipeline runs many
    /// short LLM calls back-to-back) bump to 1800 (30 min). Set to
    /// a very high number to effectively pin the slot for the
    /// daemon's lifetime; combined with the desktop's window-focus
    /// prewarm, that gives "always-hot" semantics at the cost of
    /// pinning ~28 GB for a 35B Q6.
    #[serde(default = "default_primary_idle_secs")]
    pub primary_idle_secs: u64,

    /// Idle seconds before an unused **extras** chat slot is unloaded
    /// to reclaim VRAM. Defaults to `0` — eviction-driven only, no
    /// idle drop. Set to a positive value (e.g. `1800` for 30 min) to
    /// have a background task drop extras slots that haven't served a
    /// request in that long, even when no other load is competing for
    /// memory. Useful on shared hardware where another user might
    /// need the GPU back.
    ///
    /// Slots currently serving a request (Arc strong_count > 1) are
    /// skipped. The check runs every 10s; the actual idle window is
    /// `max(extras_idle_secs, 10)`.
    #[serde(default = "default_extras_idle_secs")]
    pub extras_idle_secs: u64,

    /// Cooperative yield window for background corpus ingestion. When
    /// the daemon serves a foreground inference request on
    /// `/v1/chat/completions`, it stamps a "last-active" timestamp;
    /// the ingest pipeline polls this before each embed batch and
    /// enrichment phase and pauses while the timestamp is within
    /// `yield_to_foreground_secs` seconds of now.
    ///
    /// Why: an embed batch is an atomic `llama_decode` that can hold
    /// the GPU for ~7s. While it runs the primary chat slot can't
    /// interleave tokens, so chat latency collapses (1 tok/s instead
    /// of 7+ on Q1_0 8B class models). Yielding between batches
    /// frees the GPU for the user without cancelling ongoing
    /// ingest work.
    ///
    /// `0` (or unset) disables the feature entirely — appropriate
    /// for batch hosts that never serve interactive chat. Default
    /// `60` covers the common case where a user might issue a chat
    /// query and follow it up within a minute. Bump to 120-180 for
    /// "stay paused while I'm working" behavior, drop to 30 for
    /// "resume quickly between exchanges".
    #[serde(default = "default_yield_to_foreground_secs")]
    pub yield_to_foreground_secs: u64,

    /// Maximum concurrent peer (mesh) inference requests this node admits
    /// before returning `503 + Retry-After`. A headless contributor MUST be
    /// bounded — an unbounded peer fan-out is what OOM-killed the daemon (see
    /// `commonwealth-api::admission`). The desktop sets this from the
    /// "share GPU" consent instead; a CLI daemon relies on this default.
    /// `0` opts out of contributing entirely; raise it on capable hardware.
    /// Default `1`: serve peers, never stampede.
    #[serde(default = "default_max_peer_inflight")]
    pub max_peer_inflight: usize,

    /// Enable background freshness watchers (currently:
    /// `wikipedia-newsworthy`'s daily portal-ingest + article-refresh
    /// loop; future entries will share this gate). When true, the
    /// daemon spawns each watcher at startup; when false, none spawn
    /// and the corresponding `/internal/<watcher>/*` routes report
    /// `disabled` instead of firing ticks.
    ///
    /// Default `true` preserves the standing behavior. Flip to
    /// `false` for measurement runs where a freshness tick's
    /// atlas-rebuild would contend with the enrichment LLM and stall
    /// foreground ingest (the foreground yield hook only fires on
    /// `/v1/chat/completions`, not on background enrichment, so it
    /// can't gate this on its own). Restore to `true` once the
    /// baseline lands.
    #[serde(default = "default_freshness_watchers_enabled")]
    pub freshness_watchers_enabled: bool,

    /// When true, the inference adapter treats every tools-using
    /// request with `tool_choice: "auto"` (or unset) as if the caller
    /// had sent `tool_choice: "required"`, so the JSON-Schema
    /// tool-envelope grammar engages. Identical to setting the env var
    /// `SOVEREIGN_FORCE_TOOL_CALLS=1` — env wins when both are set,
    /// otherwise this config flag drives the same code path. Empirically
    /// (2026-05-08 measurement) the grammar pass eliminates Qwopus and
    /// FINAL-Bench native-markup parse failures (`<tool_call>{...}` with
    /// arguments-as-string-of-json) at zero per-call cost — the
    /// `JsonConstraint` path is already optimised for these short
    /// envelopes.
    ///
    /// Defaults to `false` so the disabled-default keeps existing
    /// non-tools-using callers unaffected. Set to `true` in
    /// `setup_config.toml` for daemons that primarily host opencode /
    /// Aider / autonomous-loop traffic.
    #[serde(default = "default_force_tool_calls")]
    pub force_tool_calls: bool,
    /// Engage the llguidance alternation grammar for tools-using
    /// requests (`start: text | tool_envelope`). When set, the daemon
    /// installs a JSON-Schema-driven tool envelope via llguidance's
    /// canonical `TopLevelGrammar::from_json_schema` path; the
    /// schema-driven mask is the structural fix for empty-args /
    /// content-as-envelope failures observed under the prior
    /// JsonConstraint mask. Propagated to process env at daemon boot
    /// (mirrors the `force_tool_calls` propagation) — the inference
    /// adapter reads `SOVEREIGN_ALTERNATION_GRAMMAR` per request.
    ///
    /// Defaults to `false` because the canonical path is still
    /// landing; flip to `true` per-host once the bench shows clean
    /// tool emissions on the agent-bench's from-scratch tier.
    #[serde(default = "default_alternation_grammar")]
    pub alternation_grammar: bool,

    /// Address the client API (`:9741`) binds to. Defaults to
    /// `127.0.0.1` — **secure by default**: the OpenAI-compatible
    /// surface (inference, knowledge, apps, Ollama shim) is reachable
    /// only from this machine, so the single-user desktop/CLI/attach
    /// case needs no authentication.
    ///
    /// Set to `0.0.0.0` (or a specific routable interface) to serve
    /// remote callers — required for multi-machine **mesh federation**
    /// (peers POST `/v1/chat/completions` here) or remote clients. When
    /// bound non-loopback, the daemon REQUIRES a bearer token of every
    /// non-loopback caller (auto-generated to `<data.dir>/client-token`
    /// unless `client_token` is set) — see `client_token` and
    /// `commonwealth_api::client_auth`. The internal mesh port
    /// (`:9742`, mTLS) always binds `0.0.0.0` independently of this.
    #[serde(default = "default_client_bind")]
    pub client_bind: String,

    /// Explicit bearer token for the client API when bound non-loopback.
    /// `None` (default) ⇒ the daemon auto-generates and persists one to
    /// `<data.dir>/client-token` (0600) on first non-loopback boot and
    /// prints it once. Set this to pin a known token (e.g. to
    /// distribute the same secret to mesh peers reproducibly). Ignored
    /// when `client_bind` is loopback (no token needed). Env override:
    /// `SOVEREIGN_CLIENT_TOKEN`.
    #[serde(default)]
    pub client_token: Option<String>,

    /// Interface the internal mesh API (`:9742`) binds to. Defaults to
    /// `0.0.0.0` (every interface) — the historical behaviour, and the
    /// right choice when a cloud firewall / security group already scopes
    /// who can reach the port. Pin it to a specific private address (e.g.
    /// the VPC NIC `10.0.1.4`) to keep the **unauthenticated** internal API
    /// off any other interface — defense-in-depth on a multi-homed host.
    /// Ignored under `require_encryption`, which forces the internal router
    /// loopback-only (the iroh acceptor is then the sole network ingress).
    #[serde(default = "default_internal_bind")]
    pub internal_bind: String,
}

/// Filesystem paths for mutable state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataSection {
    /// Root of data directory. Models, indexes, notes, and mesh.json
    /// all live underneath. Default: `~/.sovereign`.
    #[serde(default = "default_data_dir")]
    pub dir: PathBuf,
}

impl Default for DaemonSection {
    fn default() -> Self {
        Self {
            client_port: default_client_port(),
            internal_port: default_internal_port(),
            autostart: default_autostart(),
            primary_idle_secs: default_primary_idle_secs(),
            extras_idle_secs: default_extras_idle_secs(),
            yield_to_foreground_secs: default_yield_to_foreground_secs(),
            max_peer_inflight: default_max_peer_inflight(),
            freshness_watchers_enabled: default_freshness_watchers_enabled(),
            force_tool_calls: default_force_tool_calls(),
            alternation_grammar: default_alternation_grammar(),
            client_bind: default_client_bind(),
            client_token: None,
            internal_bind: default_internal_bind(),
        }
    }
}

impl Default for DataSection {
    fn default() -> Self {
        Self {
            dir: default_data_dir(),
        }
    }
}

/// How this node discovers and joins mesh peers. Defaults reproduce the
/// historical zero-config LAN behaviour (mDNS on, no static seeds). An
/// enterprise/VPC fleet that blocks multicast sets `mdns = false` and
/// lists the founder/seed `host:port` addresses in `seed_addrs`; the
/// daemon then forms the mesh entirely from those static seeds and never
/// touches the multicast socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoverySection {
    /// Advertise + browse `_commonwealth._tcp` over mDNS for zero-config
    /// LAN peer discovery. Default `true`. Set `false` on hosts where
    /// multicast is unavailable or undesirable (cloud VPCs, hardened
    /// network namespaces): the daemon skips the multicast socket entirely
    /// (its bind is otherwise fatal at boot) and relies on `seed_addrs` /
    /// `?relay=` hints. Force-off env override: `SOVEREIGN_DISABLE_MDNS=1`.
    #[serde(default = "default_mdns_enabled")]
    pub mdns: bool,
    /// Static internal-API `host:port` addresses (e.g. `"10.0.1.4:9742"`)
    /// to join at boot when mDNS is off or finds nothing. Each is tried
    /// as a direct `/internal/join` target — the same path a `?relay=`
    /// hint uses — until one accepts. Empty by default: the founder needs
    /// none, a joiner lists at least one reachable seed.
    #[serde(default)]
    pub seed_addrs: Vec<String>,
    /// Shared mesh join key (`cwth-XXXX-XXXX-XXXX`) presented to each
    /// `seed_addrs` peer at boot. Set on a fleet JOINER; leave unset on the
    /// founder / a standalone node (which then forms its own solo mesh).
    /// Same trust model as a join link shared out-of-band — keep this file
    /// readable only by the daemon user. A joiner with a `join_key` but no
    /// reachable `seed_addrs` fails to boot rather than split-braining into
    /// its own mesh.
    #[serde(default)]
    pub join_key: Option<String>,
}

impl Default for DiscoverySection {
    fn default() -> Self {
        Self {
            mdns: default_mdns_enabled(),
            seed_addrs: Vec::new(),
            join_key: None,
        }
    }
}

fn default_mdns_enabled() -> bool {
    true
}

/// Defaults for watched-folder corpora (`sovereign corpus watch`).
/// Per-corpus values from `WatchedFolderConfig` override these — the
/// only setting here that's *not* overridable per-corpus is
/// `paused_at_boot`, which is an operator/host-level decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchedFoldersSection {
    /// Default seconds between reconciliation sweeps (fallback when `corpus watch` omits the flag).
    #[serde(default = "default_wf_sweep_interval_secs")]
    pub default_sweep_interval_secs: u64,
    /// Default grace period before a vanished file's chunks are hard-deleted, seconds (default 7 days).
    #[serde(default = "default_wf_grace_secs")]
    pub default_soft_delete_grace_secs: u64,
    /// Deletion-guard default: a sweep removing at least this many files pauses the corpus for confirmation.
    #[serde(default = "default_wf_absolute_threshold")]
    pub default_absolute_threshold: usize,
    /// Deletion-guard default: a sweep removing at least this fraction of live files pauses the corpus (ORed with the absolute threshold).
    #[serde(default = "default_wf_fractional_threshold")]
    pub default_fractional_threshold: f32,
    /// When `true`, all watched-folder corpora start in
    /// `PausedManual` on daemon boot. Use on batch hosts where the
    /// operator wants to inspect status before the scheduler starts
    /// sweeping unattended.
    #[serde(default)]
    pub paused_at_boot: bool,
    /// Cap on the number of watched-folder sweeps that may run
    /// concurrently. Default 2 — bounded so a user with several
    /// large folders doesn't fan out unboundedly.
    #[serde(default = "default_wf_max_concurrent")]
    pub max_concurrent_sweeps: usize,
}

impl Default for WatchedFoldersSection {
    fn default() -> Self {
        Self {
            default_sweep_interval_secs: default_wf_sweep_interval_secs(),
            default_soft_delete_grace_secs: default_wf_grace_secs(),
            default_absolute_threshold: default_wf_absolute_threshold(),
            default_fractional_threshold: default_wf_fractional_threshold(),
            paused_at_boot: false,
            max_concurrent_sweeps: default_wf_max_concurrent(),
        }
    }
}

fn default_wf_sweep_interval_secs() -> u64 {
    120
}
fn default_wf_grace_secs() -> u64 {
    7 * 86_400
}
fn default_wf_absolute_threshold() -> usize {
    100
}
fn default_wf_fractional_threshold() -> f32 {
    0.25
}
fn default_wf_max_concurrent() -> usize {
    2
}

fn default_client_port() -> u16 {
    9741
}
fn default_client_bind() -> String {
    // Secure by default: loopback-only. Operators serving a mesh /
    // remote clients set "0.0.0.0" and accept the bearer-token
    // requirement that engages for non-loopback callers.
    "127.0.0.1".to_string()
}
fn default_internal_port() -> u16 {
    9742
}
fn default_internal_bind() -> String {
    // Historical behaviour: the internal mesh API binds every interface.
    // Operators on multi-homed hosts can pin it to a private NIC.
    "0.0.0.0".to_string()
}
fn default_autostart() -> bool {
    true
}
fn default_primary_idle_secs() -> u64 {
    300
}
/// Default `0` keeps existing operators on the historical "extras
/// stay loaded forever" behaviour — they explicitly opt in by
/// setting a positive value.
fn default_extras_idle_secs() -> u64 {
    0
}
/// Default `60` enables foreground-yield with a one-minute window:
/// background ingest pauses for a minute after each chat request, then
/// resumes. Set to `0` in `config.toml` to disable on batch hosts
/// where ingest throughput trumps interactive latency.
fn default_yield_to_foreground_secs() -> u64 {
    60
}
/// Headless contributor default: serve peers, but one at a time. Bounded by
/// construction so a CLI daemon is never an unbounded peer fan-out target.
fn default_max_peer_inflight() -> usize {
    1
}
fn default_freshness_watchers_enabled() -> bool {
    true
}

fn default_force_tool_calls() -> bool {
    false
}

fn default_alternation_grammar() -> bool {
    false
}

/// `~/.sovereign/`. Previously lived in `sovereign-cli::util::dirs`;
/// inlined here so `sovereign-core` has no dependency on the CLI crate.
/// Falls back to `.` if the home directory can't be resolved — matches
/// the prior behaviour.
fn default_data_dir() -> PathBuf {
    // Prefer `~/.svrnmesh`, falling back to a populated legacy `~/.sovereign`
    // (and to `.` if home can't be resolved). The rename back-compat layer
    // lives in `crate::rebrand`.
    crate::rebrand::svrnmesh_root()
}

impl SetupConfig {
    /// The canonical config path: `~/.sovereign/config.toml`. Co-located
    /// with `~/.sovereign/`'s other user-scoped state (corpora, indexes,
    /// notes db, mesh.json) so operators only have one user directory
    /// to remember. Falls back to `./.sovereign/config.toml` if the home
    /// directory can't be resolved — matches `default_data_dir()`.
    pub fn default_path() -> PathBuf {
        default_data_dir().join("config.toml")
    }

    /// The pre-consolidation location: `dirs::config_dir()/sovereign/
    /// config.toml` (`~/.config/sovereign/...` on Linux,
    /// `~/Library/Application Support/sovereign/...` on macOS). Returned
    /// for migration only; new writes always go to `default_path()`.
    pub fn legacy_default_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| {
                dirs::home_dir()
                    .map(|h| h.join(".config"))
                    .unwrap_or_else(|| PathBuf::from("."))
            })
            .join("sovereign")
            .join("config.toml")
    }

    /// If the canonical path doesn't exist but the legacy path does,
    /// move the file. Idempotent — after the first call the legacy path
    /// is gone, so subsequent calls are no-ops. Errors are logged via
    /// `eprintln!` and swallowed so a migration hiccup never blocks
    /// daemon startup; the caller will hit a normal "config not found"
    /// error path if the legacy file genuinely can't be migrated.
    fn migrate_legacy_if_needed() {
        migrate_config_between(&Self::legacy_default_path(), &Self::default_path());
    }

    /// Whether the config file exists on disk. Used by `sovereign setup`
    /// to short-circuit when the user has already configured, and by the
    /// desktop app's bootstrap probe to decide whether to skip the
    /// model-selection screens in the setup wizard.
    pub fn exists() -> bool {
        Self::migrate_legacy_if_needed();
        Self::default_path().exists()
    }

    /// Load from the canonical path.
    pub fn load() -> Result<Self, String> {
        Self::migrate_legacy_if_needed();
        let path = Self::default_path();
        Self::load_from(&path)
    }

    /// Load and parse a specific config file, applying `~`-expansion to the model/data paths.
    pub fn load_from(path: &Path) -> Result<Self, String> {
        let content =
            std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let mut cfg: SetupConfig =
            toml::from_str(&content).map_err(|e| format!("parse {}: {e}", path.display()))?;
        cfg.expand_paths();
        Ok(cfg)
    }

    /// Write to the canonical path, creating parent directories as needed.
    /// Serializes with `toml::to_string_pretty` for human readability.
    pub fn save(&self) -> Result<PathBuf, String> {
        let path = Self::default_path();
        self.save_to(&path)?;
        Ok(path)
    }

    /// Write to an explicit path, creating parent directories (pretty TOML).
    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        let toml = toml::to_string_pretty(self).map_err(|e| format!("serialize config: {e}"))?;
        std::fs::write(path, toml).map_err(|e| format!("write {}: {e}", path.display()))?;
        Ok(())
    }

    /// Remove the config file. Used by `sovereign setup --reset`.
    pub fn remove() -> Result<(), String> {
        let path = Self::default_path();
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| format!("remove {}: {e}", path.display()))?;
        }
        Ok(())
    }

    /// Expand leading `~` in all path fields to the user's home dir.
    /// TOML stores `~/.sovereign/...` literally; we resolve at load time.
    fn expand_paths(&mut self) {
        self.models.primary = expand_home(&self.models.primary);
        if let Some(fast) = &self.models.fast {
            self.models.fast = Some(expand_home(fast));
        }
        self.models.embed = expand_home(&self.models.embed);
        if let Some(p) = self.models.code.as_mut() {
            *p = expand_home(p);
        }
        for path in self.models.extra.values_mut() {
            *path = expand_home(path);
        }
        self.data.dir = expand_home(&self.data.dir);
    }
}

/// Move `legacy → new_path` iff legacy exists and new_path doesn't.
/// Pure on its inputs so tests can drive it without overriding `$HOME`.
/// Logs to stderr on success or failure; never panics.
fn migrate_config_between(legacy: &Path, new_path: &Path) {
    if new_path.exists() || !legacy.exists() || new_path == legacy {
        return;
    }
    if let Some(parent) = new_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!(
                "sovereign: migrate config: create {}: {e}",
                parent.display()
            );
            return;
        }
    }
    match std::fs::rename(legacy, new_path) {
        Ok(()) => eprintln!(
            "sovereign: migrated config {} → {}",
            legacy.display(),
            new_path.display()
        ),
        Err(e) => eprintln!(
            "sovereign: migrate config {} → {} failed: {e}",
            legacy.display(),
            new_path.display()
        ),
    }
}

/// Resolve a `~/...` path to the user's home directory. Returns the
/// path unchanged if it doesn't start with `~` or home can't be found.
fn expand_home(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(stripped) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(stripped);
        }
    }
    p.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn models(primary: &str, fast: Option<&str>, embed: &str) -> ModelsSection {
        ModelsSection {
            primary: PathBuf::from(primary),
            fast: fast.map(PathBuf::from),
            embed: PathBuf::from(embed),
            code: None,
            context_size: None,
            extra: BTreeMap::new(),
            max_extras_memory_gb: None,
            primary_pool: None,
        }
    }

    #[test]
    fn fast_path_returns_primary_when_fast_unset() {
        let m = models("/models/primary.gguf", None, "/models/embed.gguf");
        assert_eq!(m.fast_path(), Path::new("/models/primary.gguf"));
        assert!(!m.has_explicit_fast());
    }

    #[test]
    fn fast_path_returns_explicit_fast_when_set() {
        let m = models(
            "/models/primary.gguf",
            Some("/models/fast.gguf"),
            "/models/embed.gguf",
        );
        assert_eq!(m.fast_path(), Path::new("/models/fast.gguf"));
        assert!(m.has_explicit_fast());
    }

    #[test]
    fn parse_config_without_fast_field_succeeds() {
        // The pod entrypoint writes a `[models]` table with only
        // primary + embed when SINGLE_MODEL=primary is set. Before
        // this commit, deserializing that TOML failed with
        // "missing field `fast`" and killed every Vast.ai pod at
        // the daemon-launch stage. Lock the now-Optional behaviour
        // in so a future refactor can't silently reintroduce the
        // hard requirement.
        let toml = r#"
[models]
primary = "/models/primary.gguf"
embed = "/models/embed.gguf"

[daemon]
[data]
"#;
        let cfg: SetupConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.models.primary, PathBuf::from("/models/primary.gguf"));
        assert!(cfg.models.fast.is_none());
        assert_eq!(cfg.models.fast_path(), Path::new("/models/primary.gguf"));
    }

    #[test]
    fn iroh_enabled_tristate_parses_all_legacy_forms() {
        // `enabled` went bool → Option<bool> (auto-enable on mesh
        // participation, 2026-07). Pre-existing configs wrote
        // `enabled = true` / `enabled = false`; most wrote nothing.
        // All three must parse, and absent must be None (= auto).
        let base = r#"
[models]
primary = "/m/p.gguf"
embed = "/m/e.gguf"
"#;
        let cfg: SetupConfig = toml::from_str(base).unwrap();
        assert_eq!(cfg.iroh.enabled, None);

        let on: SetupConfig = toml::from_str(&format!("{base}\n[iroh]\nenabled = true\n")).unwrap();
        assert_eq!(on.iroh.enabled, Some(true));

        let off: SetupConfig =
            toml::from_str(&format!("{base}\n[iroh]\nenabled = false\n")).unwrap();
        assert_eq!(off.iroh.enabled, Some(false));

        // And None round-trips as None (absent, not `enabled = false`).
        let out = toml::to_string_pretty(&cfg).unwrap();
        let reparsed: SetupConfig = toml::from_str(&out).unwrap();
        assert_eq!(reparsed.iroh.enabled, None);
    }

    #[test]
    fn iroh_relay_urls_default_empty_and_parse() {
        let base = r#"
[models]
primary = "/m/p.gguf"
embed = "/m/e.gguf"
"#;
        // Absent = empty (n0 default), and omitted on re-serialize.
        let cfg: SetupConfig = toml::from_str(base).unwrap();
        assert!(cfg.iroh.relay_urls.is_empty());
        let out = toml::to_string_pretty(&cfg).unwrap();
        assert!(
            !out.contains("relay_urls"),
            "empty relay_urls must serialize as absent: {out}"
        );

        // A configured self-hosted relay fleet round-trips, and the
        // sovereignty `discovery` knob parses.
        let with_relays: SetupConfig = toml::from_str(&format!(
            "{base}\n[iroh]\nenabled = true\nrelay_urls = [\"https://relay.corp.example:443\"]\ndiscovery = \"none\"\n"
        ))
        .unwrap();
        assert_eq!(
            with_relays.iroh.relay_urls,
            vec!["https://relay.corp.example:443".to_string()]
        );
        assert_eq!(with_relays.iroh.discovery.as_deref(), Some("none"));
        // Absent discovery = None (n0 default).
        assert_eq!(cfg.iroh.discovery, None);
    }

    #[test]
    fn roundtrip_minimal_config() {
        let cfg = SetupConfig {
            models: ModelsSection {
                primary: PathBuf::from("/models/primary.gguf"),
                fast: Some(PathBuf::from("/models/fast.gguf")),
                embed: PathBuf::from("/models/embed.gguf"),
                code: None,
                context_size: None,
                extra: BTreeMap::new(),
                max_extras_memory_gb: None,
                primary_pool: None,
            },
            daemon: DaemonSection::default(),
            data: DataSection::default(),
            watched_folders: WatchedFoldersSection::default(),
            memory: Default::default(),
            iroh: Default::default(),
            shared_model: Default::default(),
            discovery: Default::default(),
            mcp_servers: Vec::new(),
        };
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        cfg.save_to(&path).unwrap();
        let loaded = SetupConfig::load_from(&path).unwrap();
        assert_eq!(loaded.models.primary, cfg.models.primary);
        assert_eq!(loaded.daemon.client_port, 9741);
        assert_eq!(loaded.daemon.internal_port, 9742);
        assert!(loaded.daemon.autostart);
    }

    #[test]
    fn roundtrip_preserves_mcp_servers() {
        // Guards the clobber fix: the typed `mcp_servers` field must survive a
        // save()/load() round-trip. An untyped sibling `[[mcp_servers]]` array
        // would be dropped by `toml::to_string_pretty`, silently losing the
        // user's servers on the next desktop config save.
        use crate::mcp_config::{McpAuthConfig, McpServerConfig, McpTransportConfig};
        let cfg = SetupConfig {
            models: ModelsSection {
                primary: PathBuf::from("/m/p.gguf"),
                fast: None,
                embed: PathBuf::from("/m/e.gguf"),
                code: None,
                context_size: None,
                extra: BTreeMap::new(),
                max_extras_memory_gb: None,
                primary_pool: None,
            },
            daemon: DaemonSection::default(),
            data: DataSection::default(),
            watched_folders: WatchedFoldersSection::default(),
            memory: Default::default(),
            iroh: Default::default(),
            shared_model: Default::default(),
            discovery: Default::default(),
            mcp_servers: vec![McpServerConfig {
                name: "vision".into(),
                description: Some("Describe images".into()),
                enabled: true,
                transport: McpTransportConfig::Http {
                    url: "https://vision.example/mcp".into(),
                    auth: McpAuthConfig::None,
                },
                global: true,
            }],
        };
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        cfg.save_to(&path).unwrap();
        let loaded = SetupConfig::load_from(&path).unwrap();
        assert_eq!(
            loaded.mcp_servers.len(),
            1,
            "mcp_servers must survive save/load"
        );
        assert_eq!(loaded.mcp_servers[0].name, "vision");
        assert!(matches!(
            &loaded.mcp_servers[0].transport,
            McpTransportConfig::Http { url, .. } if url == "https://vision.example/mcp"
        ));

        // An older config with no [[mcp_servers]] loads as empty (serde default).
        let old = "[models]\nprimary = \"/m/p.gguf\"\nembed = \"/m/e.gguf\"\n";
        let parsed: SetupConfig = toml::from_str(old).unwrap();
        assert!(parsed.mcp_servers.is_empty());
    }

    #[test]
    fn expand_home_resolves_tilde() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(expand_home(Path::new("~/foo/bar")), home.join("foo/bar"));
        assert_eq!(
            expand_home(Path::new("/abs/path")),
            PathBuf::from("/abs/path")
        );
    }

    #[test]
    fn defaults_match_spec() {
        let toml_str = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"
"#;
        let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.daemon.client_port, 9741);
        assert_eq!(cfg.daemon.internal_port, 9742);
        assert!(cfg.daemon.autostart);
    }

    #[test]
    fn yield_to_foreground_secs_defaults_to_60() {
        // A config that omits the field must come back with the
        // 60-second default so existing operators get yield enabled
        // without editing config.toml. Lock the default here so a
        // future bump (or zero-out) is intentional and reviewed.
        let toml_str = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"
"#;
        let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.daemon.yield_to_foreground_secs, 60);
    }

    #[test]
    fn max_peer_inflight_defaults_to_1() {
        // A config that omits the field must come back bounded (1), NOT
        // unbounded — a headless contributor with no ceiling is the
        // resource-exhaustion hole this default closes. Lock it so a future
        // change is intentional and reviewed.
        let toml_str = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"
"#;
        let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.daemon.max_peer_inflight, 1);
    }

    #[test]
    fn freshness_watchers_enabled_defaults_to_true() {
        let toml_str = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"
"#;
        let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
        assert!(cfg.daemon.freshness_watchers_enabled);
    }

    #[test]
    fn freshness_watchers_enabled_explicit_false() {
        let toml_str = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"

[daemon]
freshness_watchers_enabled = false
"#;
        let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
        assert!(!cfg.daemon.freshness_watchers_enabled);
    }

    #[test]
    fn yield_to_foreground_secs_explicit_override() {
        // Operators can set 0 to disable, or a higher value for a
        // longer pause window.
        let toml_str = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"

[daemon]
yield_to_foreground_secs = 0
"#;
        let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.daemon.yield_to_foreground_secs, 0);
    }

    #[test]
    fn default_path_is_hidden_brand_dir_with_config_toml() {
        // Config lives directly under home in a hidden, brand-named dir:
        // `~/.svrnmesh/config.toml` (preferred) or the legacy
        // `~/.sovereign/config.toml`. Post-rename, `default_path()` ->
        // `svrnmesh_root()` -> `rebrand::resolve_branded_dir` resolves to
        // whichever the machine actually has: a populated `~/.svrnmesh` wins,
        // else a populated legacy `~/.sovereign`, else `~/.svrnmesh` on a
        // fresh install. The brand component is therefore environment-
        // dependent, so the assertion must accept either spelling.
        //
        // The leading dot is load-bearing: `Path::ends_with` matches whole
        // components, so the dotted `.svrnmesh`/`.sovereign` distinguishes the
        // canonical hidden-dir layout from the legacy
        // `~/.config/sovereign/config.toml` (which ends with the *undotted*
        // `sovereign/config.toml`). Keep the dot literal so a regression back
        // to that legacy layout still fails this test.
        let p = SetupConfig::default_path();
        assert!(
            p.ends_with(".svrnmesh/config.toml") || p.ends_with(".sovereign/config.toml"),
            "unexpected path: {}",
            p.display()
        );
    }

    #[test]
    fn migrate_moves_legacy_when_new_path_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("legacy/config.toml");
        let new_path = tmp.path().join("new/config.toml");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, "primary=\"/m/p.gguf\"\n").unwrap();

        migrate_config_between(&legacy, &new_path);

        assert!(new_path.exists(), "new path should exist after migration");
        assert!(
            !legacy.exists(),
            "legacy path should be gone after migration"
        );
        assert_eq!(
            std::fs::read_to_string(&new_path).unwrap(),
            "primary=\"/m/p.gguf\"\n"
        );
    }

    #[test]
    fn migrate_is_noop_when_new_path_already_exists() {
        // Operator has already migrated (or is on a fresh install): the
        // legacy file may still exist as a stale leftover, but we must
        // not clobber the canonical file.
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("legacy/config.toml");
        let new_path = tmp.path().join("new/config.toml");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::create_dir_all(new_path.parent().unwrap()).unwrap();
        std::fs::write(&legacy, "stale\n").unwrap();
        std::fs::write(&new_path, "canonical\n").unwrap();

        migrate_config_between(&legacy, &new_path);

        assert_eq!(std::fs::read_to_string(&new_path).unwrap(), "canonical\n");
        assert!(legacy.exists(), "legacy untouched when new path present");
    }

    #[test]
    fn migrate_is_noop_when_legacy_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("legacy/config.toml");
        let new_path = tmp.path().join("new/config.toml");
        // Neither file exists.
        migrate_config_between(&legacy, &new_path);
        assert!(!new_path.exists());
    }

    #[test]
    fn extra_slots_default_empty_when_absent() {
        // Operators upgrading the binary keep their existing
        // config.toml. The `serde(default)` on `extra` means a config
        // without the `[models.extra]` table loads with an empty
        // map — preserving the legacy 3-slot lineup.
        let toml_str = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"
"#;
        let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
        assert!(cfg.models.extra.is_empty());
    }

    #[test]
    fn extra_slots_parse_from_toml_table() {
        // `[models.extra]` table → BTreeMap<String, PathBuf>.
        // BTreeMap iteration order is sorted, which makes startup
        // logging deterministic across reboots.
        let toml_str = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"

[models.extra]
reasoning = "/m/big.gguf"
bulk = "/m/small.gguf"
"#;
        let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.models.extra.len(), 2);
        assert_eq!(
            cfg.models.extra.get("reasoning"),
            Some(&PathBuf::from("/m/big.gguf"))
        );
        assert_eq!(
            cfg.models.extra.get("bulk"),
            Some(&PathBuf::from("/m/small.gguf"))
        );
    }

    #[test]
    fn max_extras_memory_bytes_unset_returns_none() {
        let toml_str = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"
"#;
        let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
        assert!(cfg.models.max_extras_memory_bytes().is_none());
    }

    #[test]
    fn max_extras_memory_bytes_converts_gigabytes() {
        let toml_str = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"
max_extras_memory_gb = 12.0
"#;
        let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
        // 12 GiB = 12 * 2^30 bytes.
        assert_eq!(
            cfg.models.max_extras_memory_bytes(),
            Some(12 * (1u64 << 30))
        );
    }

    #[test]
    fn max_extras_memory_bytes_saturates_on_negative_input() {
        // Defensive: a negative or zero budget effectively forbids
        // any extras — return Some(0) rather than panicking or
        // overflowing.
        let toml_str = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"
max_extras_memory_gb = 0.0
"#;
        let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.models.max_extras_memory_bytes(), Some(0));
    }

    #[test]
    fn extra_slots_expand_home_at_load() {
        // `~/...` paths inside `[models.extra]` resolve like the
        // primary/fast/embed paths do — load-time expansion via
        // `expand_paths`.
        let home = dirs::home_dir().unwrap();
        let toml_str = r#"
[models]
primary = "~/dev/primary.gguf"
fast = "/abs/fast.gguf"
embed = "~/dev/embed.gguf"

[models.extra]
reasoning = "~/dev/big.gguf"
"#;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, toml_str).unwrap();
        let cfg = SetupConfig::load_from(&path).unwrap();
        assert_eq!(
            cfg.models.extra.get("reasoning"),
            Some(&home.join("dev/big.gguf"))
        );
    }
}
