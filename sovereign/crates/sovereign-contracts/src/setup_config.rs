// SPDX-License-Identifier: AGPL-3.0-or-later
//! Persistent configuration written by `sovereign setup` and read by
//! `sovereign daemon run`. Lives at `~/.svrnmesh/config.toml` —
//! co-located with the rest of the user-scoped sovereign state (corpora,
//! indexes, notes db, mesh.json). Distinct from the project-level
//! `.sovereign/sovereign.toml` which configures per-project watchers.
//!
//! The split is: user-scoped state (this file + everything else under
//! `~/.svrnmesh/`) versus project-scoped state (test/lint runners,
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

/// Top-level structure of `~/.svrnmesh/config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupConfig {
    /// `[models]` — GGUF slot paths, or `None` on a node that holds no
    /// weights at all.
    ///
    /// `None` is the `terminal` class (see [`SetupConfig::node_class`]): a full
    /// mesh member — it holds the mesh key, gossips, shares knowledge and the
    /// ledger — that runs no local inference and routes every turn to a bound
    /// `[node] entry`. An IoT device or a laptop beside a heavy box.
    ///
    /// Absent rather than empty, deliberately. This was a required
    /// `ModelsSection` whose "no models" state was `ModelsSection::default()` —
    /// three empty `PathBuf`s, indistinguishable from a config that lost its
    /// paths. `Option` makes the two different values, so a site that needs a
    /// model refuses by name ([`SetupConfig::models`]) instead of opening `""`
    /// and reporting whatever llama.cpp says about it (§18.3).
    ///
    /// `#[serde(default)]` means a `config.toml` with no `[models]` parses. It
    /// does not mean it LOADS: `load_from` refuses a config that declares
    /// neither models nor an entry node, so "my models went missing" cannot
    /// quietly become "I am a terminal".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<ModelsSection>,
    /// `[node]` — what kind of participant this node is on the mesh.
    #[serde(default)]
    pub node: NodeSection,
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
    /// `[compute]` — supervised compute-child process boundary
    /// (DISTRIBUTED_PILOT_READINESS.md P1). Off by default; see
    /// [`ComputeSection`]. When enabled, the daemon spawns each declared
    /// pool's replicas as child processes and routes matching requests to
    /// them, so a ggml SIGABRT kills only a child, not the daemon.
    #[serde(default)]
    pub compute: ComputeSection,
    /// `[engine]` — WHICH inference engine serves this node. Absent
    /// (the overwhelmingly common case) means [`EngineKind::Llama`], the
    /// in-process llama.cpp engine this repo has always run, so an
    /// existing `config.toml` is unaffected. See [`EngineSection`].
    #[serde(default)]
    pub engine: EngineSection,
    /// `[search]` — the operator's web-search provider and its key. The
    /// ONE search config surface: the deep-research loop, the desktop's
    /// chat tools and the conversation tool builder all read this, so a
    /// key set once works everywhere. Before this existed the desktop
    /// kept its own `[search_backend]` in `desktop.toml` while the loop
    /// read `SVRNMESH_TAVILY_API_KEY`, and neither could see the other —
    /// which is why a desktop-configured provider never reached a run.
    #[serde(default)]
    pub search: SearchSection,
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

pub use crate::engine_config::{EngineKind, EngineSection};

/// `[search]` — the web-search provider the operator configured.
///
/// DuckDuckGo is always available and needs no key; it is the
/// zero-config fallback and is registered whatever this section says.
/// `provider` names the one to PREFER when it is keyed — the closed set
/// is what `sovereign_tools_base::web::search` implements: `duckduckgo`,
/// `tavily`, `brave`.
///
/// The key may also arrive as `SVRNMESH_TAVILY_API_KEY` (the older path,
/// and still the right shape for CI and one-off shells). This section
/// wins when both are present; the env var keeps existing setups working.
///
/// ```toml
/// [search]
/// provider = "tavily"
/// api_key = "tvly-..."
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchSection {
    /// The preferred backend id. Empty ⇒ no preference: DuckDuckGo.
    #[serde(default)]
    pub provider: String,
    /// The provider's key. `None` ⇒ the provider is not keyed and the
    /// preference is inert — DuckDuckGo serves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
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
/// block is `[mesh.iroh]`; in `~/.svrnmesh/config.toml` (the unified
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
    /// Transport for the ggml tensor-split RPC stream (distributed
    /// inference activation traffic); `None` = default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpc_tensor: Option<String>,
}

/// `[node]` — this node's participant class on the mesh.
///
/// How a terminal names the node it routes through.
///
/// A closed set of two, so an enum rather than "whichever of these fields is
/// populated" (ARCH §2.1). The two are not ranked and one config carries
/// exactly one of them — [`SetupConfig::validate_class`] refuses a file that
/// sets both, because that file has two answers to "where does a turn go".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryBinding {
    /// The entry node's mesh IDENTITY, resolved through the mesh on every
    /// turn. What `svrn setup --terminal <join-link>` writes, and the form
    /// ARCH §7.5 asks for: a node that moves networks, changes DHCP lease, or
    /// is only reachable over an encrypted mesh's iroh path is still the same
    /// node, and still found.
    Node(String),
    /// A literal `…/v1` address. For an entry node that is not a mesh member —
    /// a daemon on this same machine, or one on a trusted LAN — where there is
    /// no identity to resolve and the address genuinely is the whole truth.
    ///
    /// Carries the §7.5 exposure knowingly: nothing detects the day another
    /// machine answers at that address. `svrn doctor`'s `entry_node_identity`
    /// check exists for exactly this form.
    Address(String),
}

impl EntryBinding {
    /// How to name this binding to a person — an identity, or an address.
    pub fn describe(&self) -> String {
        match self {
            Self::Node(id) => format!("entry node {id}"),
            Self::Address(url) => url.clone(),
        }
    }
}

/// One stored fact: where to send work this node cannot do itself. The class
/// itself is DERIVED from it plus `[models]` ([`SetupConfig::node_class`])
/// rather than stored, so there is no third field to disagree with the other
/// two (§7.5). Writing `class = "terminal"` beside a populated `[models]` would
/// be exactly that disagreement, and the config would have to pick a winner.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeSection {
    /// The peer this node routes inference through when it holds no models of
    /// its own, as an **HTTP base URL including `/v1`** — e.g.
    /// `http://halo:9741/v1`. Written by `svrn setup --terminal`, which
    /// normalises whatever the operator typed, and handed straight to
    /// `SplitInferenceProvider::new`, whose first parameter is `endpoint_v1`.
    ///
    /// This doc used to say "a mesh node name or id, as `svrn mesh status`
    /// prints it". It never was: a reader who believed it would write a name,
    /// `validate_class` would accept the file, and the failure would surface at
    /// the first turn, far from the cause.
    ///
    /// **Known tension, deliberately carried (2026-08-30).** ARCH §7.5 says the
    /// address is a mutable attribute of a thing and never its name, and this
    /// mesh has the scar to prove it — an iroh bridge's loopback port used as
    /// peer identity produced 14 rebuilds in 21 minutes for a peer that had not
    /// moved. Keyed on a URL, a terminal does not follow its entry node across
    /// networks, gets none of `PeerTransport`'s ranked multi-homed candidates,
    /// and — when a DHCP lease moves — forwards to whatever machine now answers
    /// there without erroring.
    ///
    /// The identity-keyed design was priced (order `tn-1-terminal-honesty`, D5)
    /// and deferred: resolving through `PeerEndpointSource` per turn needs a
    /// provider that wraps `SplitInferenceProvider`, and `InferenceProvider`
    /// has 27 methods of which 24 carry defaults — including `embed_batch`,
    /// whose default is the per-item loop `SplitInferenceProvider` overrides
    /// precisely because it made corpus ingest embed-bound. A wrapper that
    /// forgets one method silently inherits that, with nothing red. See
    /// `sovereign/DEFAULTS_LEDGER.md` for the re-open condition.
    ///
    /// Required when `[models]` is absent and meaningless when it is present.
    /// Named deliberately rather than discovered: a node with no weights and no
    /// entry has nowhere to send a turn, and picking "whoever gossiped last"
    /// would make its behaviour depend on mesh timing (N4 §4.5 — a consumer
    /// should bind to a home node, not run a scheduler over a view that is
    /// stalest exactly when it wakes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    /// The entry node's mesh node id, used as the BINDING — resolved through
    /// the mesh on every turn rather than stored as an address.
    ///
    /// This is what ARCH §7.5 asks for and what [`entry`](Self::entry) could
    /// not give: the identity is what the operator chose, the address is a
    /// mutable attribute of it, and this mesh has the scar to prove the
    /// difference — an iroh bridge's loopback port used as peer identity
    /// produced 14 rebuilds in 21 minutes for a peer that had not moved.
    ///
    /// Written by `svrn setup --terminal <join-link>`, which joins the mesh
    /// first and so has a real node id to bind. Resolution goes through the
    /// same `PeerTransport` seam every other peer-bound traffic class uses, so
    /// a terminal inherits multi-homed candidate ranking and — on an encrypted
    /// mesh — the iroh path, which is the only ingress such a mesh has.
    ///
    /// NOT to be confused with [`entry_node_id`](Self::entry_node_id), which
    /// records what an ADDRESS binding pointed at so drift can be detected.
    /// This one IS the binding, and an identity cannot drift.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_node: Option<String>,
    /// The entry node's mesh node id, recorded at setup time.
    ///
    /// NOT the binding — [`entry`](Self::entry) is, and it is a URL. This is
    /// the identity that URL pointed at when the operator ran `svrn setup
    /// --terminal`, kept so the mismatch can be DETECTED even though it cannot
    /// currently be resolved around.
    ///
    /// That distinction is the whole point. ARCH §7.5 says a stable thing keyed
    /// on a volatile address eventually answers confidently and wrongly, and a
    /// terminal is exposed to exactly that: when a DHCP lease moves and another
    /// machine takes the address, the terminal forwards there without erroring.
    /// Resolving by identity was priced and deferred
    /// (`sovereign/DEFAULTS_LEDGER.md`), so the interim posture is to make the
    /// drift visible rather than silent — `svrn doctor`'s `entry_node_identity`
    /// check probes the address and compares what answers against this.
    ///
    /// `None` on a config written before this field existed, or when the entry
    /// node did not report an id; the check then reports could-not-judge rather
    /// than inventing a verdict (§18.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_node_id: Option<String>,
    /// The entry node's embed model id, recorded at setup time.
    ///
    /// A terminal embeds over HTTP, so the vector space its corpora land in is
    /// the ENTRY node's, not its own — and the memory-embedding staleness guard
    /// compares against whatever `embed_model_id()` reports. Reporting a
    /// placeholder would make that guard agree with itself and with nothing
    /// else, so the real id is captured once, when the mesh is up and can be
    /// asked, rather than guessed at every boot (§18.3).
    ///
    /// `None` until `svrn setup --terminal` records it; the forwarder then has
    /// no id to vouch with and the guard treats the space as unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_embed_model: Option<String>,
}

impl NodeSection {
    /// Where this node sends work it cannot do itself, or `None` when it was
    /// never told.
    ///
    /// THE one place that judgement is made, so provider construction,
    /// `node_class`, `svrn doctor` and the status surface cannot reach
    /// different answers (§10.6). A file carrying both forms is refused at
    /// load, so the order of these two arms is not a precedence rule — it is
    /// only what the compiler needs.
    pub fn binding(&self) -> Option<EntryBinding> {
        if let Some(node) = self.entry_node.as_deref().filter(|s| !s.trim().is_empty()) {
            return Some(EntryBinding::Node(node.to_string()));
        }
        self.entry
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(|url| EntryBinding::Address(url.to_string()))
    }
}

/// What kind of participant a node is — derived, never stored.
///
/// NOT to be confused with [`SharedModelRole`], which is a different axis
/// entirely: that one says whether a node lends GPU memory to an RPC
/// layer-split, and its `Consumer` default describes almost every node in a
/// fleet, weights or no weights. A node can be a `Holder` here and a `Consumer`
/// there at the same time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeClass {
    /// Holds model weights and serves them — the ordinary node.
    Holder,
    /// Holds no weights. A full mesh member that routes every turn to
    /// `[node] entry`.
    Terminal,
    /// Neither models nor an entry: `svrn setup` has not run here, or it ran
    /// and left a placeholder `[models]` behind.
    ///
    /// A third state, not a flavour of `Terminal`. "Holds nothing and knows
    /// where to send work" and "holds nothing and does not" fail in different
    /// ways and want different messages, so they must not collapse into one
    /// (§18.2).
    Unconfigured,
}

impl NodeClass {
    /// The stable wire/display spelling. ONE place the class becomes a string,
    /// so `/v1/mesh/status`, `svrn mesh status` and `svrn doctor` cannot drift
    /// into three vocabularies for one closed set (ARCH §2.1).
    pub fn id(&self) -> &'static str {
        match self {
            Self::Holder => "holder",
            Self::Terminal => "terminal",
            Self::Unconfigured => "unconfigured",
        }
    }
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
    /// Memory headroom FACTOR the host gates and places against: it requires
    /// `pooled >= model_size × headroom`. Default 1.2 (20% for KV + compute
    /// buffers). `svrn mesh plan` reads this SAME value as its default headroom,
    /// so the plan you preview uses the headroom the load executes with. Lower
    /// packs tighter (more aggressive); higher is safer. Env `SOVEREIGN_RPC_HEADROOM`
    /// wins if pre-set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headroom: Option<f64>,
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    /// The PRIMARY slot's window, and the default for every other slot
    /// that does not name its own (see `fast_context_size` and the
    /// `[edit]` section's `context_size`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_size: Option<u32>,

    /// The FAST slot's window. `None` falls back to `context_size`, so
    /// omitting it is byte-identical to the behaviour before this key
    /// existed.
    ///
    /// # Why this key exists
    ///
    /// KV cache scales LINEARLY in `n_ctx`, and until 2026-08-25
    /// `context_size` was one global applied to every slot — so a 4B fast
    /// model carried the same 64k cache as a 27B primary, whether or not
    /// anything ever filled it. Measured on this host, the four live
    /// contexts held ~17.7 GB of KV + compute between them.
    ///
    /// The `[edit]` section already had exactly this key for exactly this
    /// reason ("inline completion never needs the chat slot's 16k window,
    /// and a small ctx keeps KV cost tiny"). The mechanism was not missing;
    /// it had simply never been extended to the slot that costs the most.
    ///
    /// # Sizing it is a measurement, not a guess
    ///
    /// The fast slot is the OVERFLOW path: `pick_slot` routes any prompt
    /// too large for FastShort's per-sequence budget here, so its window
    /// must cover the largest prompt that lands on it, not the typical one.
    /// Read the `kv budget: slot context built` trace lines (one per
    /// context, emitted at load) before choosing a value — that is what
    /// they are for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_context_size: Option<u32>,

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

    /// Opt-in **dedicated code-editing model** — the slot serving
    /// next-edit suggestions (`sovereign/docs/NEXT_EDIT.md`) and, when
    /// the model's vocab carries FIM markers, inline completion too
    /// (`sovereign/docs/INLINE_COMPLETION.md`).
    ///
    /// Declaring the section opts into a *specialised* editing model.
    /// It is NOT the opt-in for editing assistance as such: with the
    /// section absent the daemon falls back to the resident chat model
    /// for next-edit (`install_fallback_next_edit_slot`), so a user who
    /// configures nothing still gets suggestions. What the section buys
    /// is a model chosen for the job — typically much faster, and the
    /// only way to get `/v1/completions` at all.
    ///
    /// When `path` equals the fast slot's resolved GGUF
    /// (`Self::fast_path`), the daemon serves from the always-resident
    /// fast slot instead of loading a duplicate ("lean mode", plan
    /// decision D8); otherwise it loads a dedicated, pinned extras slot
    /// under the reserved name `"edit"`.
    ///
    /// TOML shape:
    /// ```toml
    /// [models.edit]
    /// path = "/models/Qwen2.5-Coder-1.5B-Q8_0.gguf"
    /// ```
    ///
    /// `[models.fim]` is accepted as a deprecated alias so configs
    /// written before the rename keep working unchanged — the key was
    /// renamed because next-edit, not FIM, is the lane most users
    /// reach for, and a section named `fim` made the common case look
    /// like the exotic one.
    #[serde(default, alias = "fim", skip_serializing_if = "Option::is_none")]
    pub edit: Option<EditSection>,
}

/// `[models.edit]` — dedicated code-editing model declaration
/// (deprecated alias: `[models.fim]`). See `ModelsSection::edit`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditSection {
    /// GGUF path for the editing model (required — presence of the
    /// section is the opt-in).
    ///
    /// Any chat-capable model serves next-edit. FIM
    /// (`/v1/completions`) additionally requires FIM marker tokens in
    /// the tokenizer (Mellum2, Qwen2.5-Coder are known-good); the
    /// daemon probes the vocab at install and, finding none, serves
    /// next-edit only rather than refusing the slot.
    pub path: PathBuf,
    /// Context size for the dedicated editing slot. `None` falls back
    /// to 4096 — inline completion never needs the chat slot's 16k
    /// window, and a small ctx keeps KV cost tiny.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_size: Option<u32>,
    /// Generation cap per completion. Default 48.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<usize>,
    /// Sampling temperature. Default 0.2 (near-greedy; FIM wants
    /// the highest-probability continuation, not variety).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Server keeps the TAIL of the client-supplied prefix beyond
    /// this many chars. Default 8000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_prefix_chars: Option<usize>,
    /// Server keeps the HEAD of the client-supplied suffix beyond
    /// this many chars. Default 2000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_suffix_chars: Option<usize>,
    /// Prompt/parse contract the next-edit model lane uses with this
    /// slot's model (`"region_instruct"` default, `"zeta2"`,
    /// `"sweep"`). Explicit because the contract is a property of the
    /// fine-tune, not the tokenizer family — it cannot be probed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_edit_format: Option<crate::types::NextEditFormat>,
}

/// Built-in `[models.edit]` FIM-lane sampling defaults.
///
/// Public consts rather than inline `unwrap_or` literals because there
/// is a second reader: the automatic next-edit fallback
/// (`EmbeddedLlamaCpp::install_fallback_next_edit_slot`) builds a slot
/// when there is no `[models.edit]` section at all, so it has no
/// `EditSection` to ask. Two copies of these numbers would be two
/// deciders for one policy (ARCH §10.6).
pub mod fim_defaults {
    /// Per-completion generation cap.
    pub const MAX_TOKENS: usize = 48;
    /// Sampling temperature (near-greedy: FIM wants the most likely
    /// continuation, not variety).
    pub const TEMPERATURE: f32 = 0.2;
    /// Prefix clamp — the server keeps this many chars of TAIL.
    pub const MAX_PREFIX_CHARS: usize = 8000;
    /// Suffix clamp — the server keeps this many chars of HEAD.
    pub const MAX_SUFFIX_CHARS: usize = 2000;
}

impl EditSection {
    /// Effective per-completion generation cap.
    pub fn effective_max_tokens(&self) -> usize {
        self.max_tokens.unwrap_or(fim_defaults::MAX_TOKENS)
    }
    /// Effective sampling temperature.
    pub fn effective_temperature(&self) -> f32 {
        self.temperature.unwrap_or(fim_defaults::TEMPERATURE)
    }
    /// Effective prefix clamp (tail kept).
    pub fn effective_max_prefix_chars(&self) -> usize {
        self.max_prefix_chars
            .unwrap_or(fim_defaults::MAX_PREFIX_CHARS)
    }
    /// Effective suffix clamp (head kept).
    pub fn effective_max_suffix_chars(&self) -> usize {
        self.max_suffix_chars
            .unwrap_or(fim_defaults::MAX_SUFFIX_CHARS)
    }
    /// Effective next-edit prompt/parse contract.
    pub fn effective_next_edit_format(&self) -> crate::types::NextEditFormat {
        self.next_edit_format.unwrap_or_default()
    }
}

/// `[compute]` — the supervised compute-child process boundary
/// (DISTRIBUTED_PILOT_READINESS.md P1). Off by default: an absent or
/// `enabled = false` section spawns no children and changes nothing. When
/// enabled, the daemon spawns each `[[compute.slot]]` as ONE child process
/// (`current_exe() --compute-child …`) and the provider facade routes
/// matching requests to it. There is no N-replica pool — a live embed run
/// showed process replicas lose to in-process batching for a fits-on-one-box
/// model; the boundary is kept for crash isolation + the distributed case.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ComputeSection {
    /// Master switch. `false` (default) → no children, zero behaviour change.
    #[serde(default)]
    pub enabled: bool,
    /// The compute slots to run — each is exactly one child process.
    #[serde(default)]
    pub slot: Vec<ComputeSlotConfig>,
    /// Run the mesh's DISTRIBUTED primary in a compute child instead of
    /// in-process. `false` (default) keeps the legacy in-daemon path exactly as
    /// it was.
    ///
    /// This is the containment for ggml's uncatchable RPC aborts. Distributing
    /// a primary across mesh workers puts ggml's RPC client in the daemon, and
    /// that client has no error path: a worker that dies mid-decode
    /// (`ggml-rpc.cpp:491`) — or one already gone when the prune reload frees
    /// its buffers (`:386`, which killed the daemon live on 2026-07-27) —
    /// SIGABRTs the whole process, taking gossip, `/status`, and the client API
    /// with it. In a child, that abort kills the child; the daemon observes the
    /// exit and respawns against the surviving workers.
    ///
    /// Requires `enabled = true`. Needs no `[[compute.slot]]` entries: the
    /// primary's model comes from `[models]`, and its worker set from mesh
    /// discovery at runtime.
    #[serde(default)]
    pub distributed_primary: bool,
}

/// One `[[compute.slot]]` — a single model in a single supervised child.
/// Requests whose `model_id` equals `name` route to the child; an `embed`
/// slot with `capture_embed = true` additionally captures all `/v1/embeddings`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputeSlotConfig {
    /// Addressable id: a request `model_id` equal to this routes to the
    /// child. Also used as the child `--name`.
    pub name: String,
    /// `"generate"` (EmbeddedLlamaCpp) or `"embed"` (EmbedOnlyProvider).
    pub role: String,
    /// GGUF path the child loads.
    pub model: PathBuf,
    /// Context size (generate role). Default 4096 (child-side).
    #[serde(default)]
    pub context_size: Option<u32>,
    /// GPU offload layers. `None` (default) = auto; `Some(0)` = CPU-only.
    #[serde(default)]
    pub n_gpu_layers: Option<u32>,
    /// Spawn this child at daemon boot. Default `true`.
    #[serde(default = "default_true")]
    pub warm: bool,
    /// Embed slots only: when `true`, ALL `/v1/embeddings` traffic routes to
    /// this child (embeddings carry no model id to route on). Default false.
    #[serde(default)]
    pub capture_embed: bool,
}

fn default_true() -> bool {
    true
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

    /// Effective n_ctx for the FAST slot — its own value when set,
    /// otherwise the primary's. Defaulting to the primary is what keeps
    /// adding this key a no-op for every existing config on disk.
    pub fn effective_fast_context_size(&self) -> u32 {
        self.fast_context_size
            .unwrap_or_else(|| self.effective_context_size())
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

    /// The primary GGUF's filename stem — the id the slot manager resolves by
    /// and the name a caller passes as `model` over HTTP.
    ///
    /// Lives here rather than on [`SetupConfig`] because every sibling slot
    /// accessor does (`fast_path`, `has_explicit_fast`, `effective_context_size`,
    /// `max_extras_memory_bytes`); a reader looking for "what can this slot
    /// table tell me" should find them in one place.
    pub fn primary_stem(&self) -> Option<String> {
        self.primary
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string)
    }

    /// The embed GGUF's filename stem. See [`Self::primary_stem`].
    pub fn embed_stem(&self) -> Option<String> {
        self.embed
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string)
    }

    /// `true` when this section actually names a model — i.e. it is a slot
    /// table and not a placeholder.
    ///
    /// A `[models]` section can EXIST and hold nothing: the desktop wizard
    /// writes `ModelsSection::default()` mid-flight so a config file is present
    /// before the user has chosen slots, and a truncated or badly-merged file
    /// lands in the same shape. Both are "setup has not finished", not "this
    /// node holds models" — so presence of the section is the wrong question
    /// and this is the right one.
    ///
    /// `primary` is the discriminator because it is the one slot with no
    /// fallback: `fast_path()` subsumes to it, `embed` without a primary cannot
    /// serve a turn, and every path that refuses for lack of models refuses for
    /// lack of THIS. Used by [`SetupConfig::node_class`] and
    /// [`SetupConfig::models`] so "does this node hold models" has one answer
    /// (§7.5).
    pub fn is_populated(&self) -> bool {
        !self.primary.as_os_str().is_empty()
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
    /// all live underneath. Default: `~/.svrnmesh`.
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

/// THE default client port. Public so callers that must not honour the
/// env knob (see [`client_daemon_base`]) can fall back to the same number
/// the config's own serde default uses, instead of re-compiling a `9741`
/// of their own beside it (§10.6).
pub fn default_client_port() -> u16 {
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

/// Base URL of the daemon's INTERNAL listener (`http://127.0.0.1:<internal_port>`),
/// honouring `[daemon] internal_port` in `~/.svrnmesh/config.toml`.
///
/// WHY THIS EXISTS IN THE CONTRACTS CRATE. Four CLI call sites independently
/// hardcoded `http://127.0.0.1:9742`, so `corpus install`, `alignment` progress,
/// `pipeline pause` and one `doctor` probe could only ever reach a daemon on the
/// default port. Any operator who moved `internal_port` — and every sandboxed or
/// side-by-side daemon — got "Is `svrn daemon` running?" from a daemon running
/// perfectly well. The CLI journey harness surfaced the first of the four in
/// 2026-07-28 when a fixture corpus made the install step actually execute.
///
/// The desktop always resolved this properly
/// (`sovereign-desktop/src-tauri/src/bootstrap.rs`); the CLI never did. It lives
/// HERE rather than in `sovereign-cli-shared` because the resolution needs
/// [`SetupConfig`], and that crate deliberately carries no heavy dependencies.
///
/// Falls back to the compiled default when the config is absent or unreadable,
/// which is the same posture the `#[serde(default)]` on the field itself takes:
/// a missing config means defaults, not an error.
pub fn internal_daemon_base() -> String {
    let port = SetupConfig::load()
        .map(|cfg| cfg.daemon.internal_port)
        .unwrap_or_else(|_| default_internal_port());
    internal_daemon_base_for(port)
}

/// Pure builder behind [`internal_daemon_base`], for callers that already hold a
/// resolved port (tests, and the daemon itself, which knows what it bound).
pub fn internal_daemon_base_for(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

/// Base URL of the daemon this CLI should TALK to — the ONE decider for
/// "where do I send `/v1/chat/completions`, `/mcp/message`, an embed request?".
///
/// Precedence, highest first:
///   1. `SOVEREIGN_DAEMON_URL` — explicit per-invocation override
///   2. `SVRNMESH_DAEMON_URL`  — the post-rename spelling of the same knob
///   3. `[daemon] client_port` in `~/.svrnmesh/config.toml`
///   4. the compiled default (see [`default_client_port`])
///
/// The twin of [`internal_daemon_base`], and it exists for the same reason one
/// level over: `sovereign-cli-shared::urls` builds these URLs from a port the
/// CALLER supplies, and callers that had no port to hand passed
/// `DEFAULT_CLIENT_PORT` — so the v2 enrichment pipeline dispatched every
/// `/v1/chat/completions` at `localhost:9741` whatever the config said.
///
/// The failure mode is worse than "cannot reach the daemon". A sandboxed run on
/// a normal host does not fail at all — 9741 answers, because that is the
/// OPERATOR's daemon, so an isolated enrichment silently drives the production
/// process and its models. It only surfaced as an error because the CLI journey
/// sandbox runs in a private netns where nothing answers on 9741
/// (2026-07-29, `enrich-atlas` step 3, found once the journey's unfalsifiable
/// steps were made falsifiable).
///
/// WHY THE ENV LEG LANDED HERE (2026-08-25). That 2026-07-29 fix closed the
/// config half and left the env half open, so the knob and the config were TWO
/// deciders for one question and neither read the other's input:
/// `sovereign_cli_shared::urls::daemon_base_url` honoured the env and ignored a
/// moved `client_port`; this function honoured `client_port` and ignored the
/// env. `SOVEREIGN_DAEMON_URL` therefore reached ONE daemon-talking call site
/// (`mcp_client`) out of the ~30 in the tree, and "point this session at a
/// second daemon" was a per-verb flag hunt whose misses were SILENT — `svrn
/// enrich` and `svrn backlog score` kept driving the operator's local daemon
/// whatever the knob said, and answered successfully while doing it. §10.6.
///
/// NOT the accessor for MANAGING the daemon process on this host. `daemon
/// stop/restart/reload`, its readiness probe, and `corpus watch register`
/// deliberately do NOT honour the env: a URL cannot be SIGTERM'd, and a remote
/// daemon cannot watch this host's filesystem. Pointing the knob at a rented
/// pod and running `daemon restart` would otherwise kill the local daemon and
/// then report READY off the pod's answer — a success-shaped wrong result of
/// exactly the kind §18.3 forbids. Those callers hold the configured port and
/// call [`client_daemon_base_for`], naming the choice at the call site.
///
/// `localhost` rather than `127.0.0.1` to match `urls::v1_url`, which every
/// existing client-side caller already uses.
pub fn client_daemon_base() -> String {
    match daemon_url_override() {
        Some(url) => url,
        None => {
            let port = SetupConfig::load()
                .map(|cfg| cfg.daemon.client_port)
                .unwrap_or_else(|_| default_client_port());
            client_daemon_base_for(port)
        }
    }
}

/// The env leg of [`client_daemon_base`], isolated so the precedence is
/// testable without a config file and so there is ONE answer to "which
/// spelling of the knob counts, and what counts as set".
///
/// `SOVEREIGN_` first, then `SVRNMESH_`, for parity with every other reader of
/// the pair (the boot bridge maps the legacy prefix forward, so both arrive).
///
/// A set-but-blank value is treated as UNSET rather than as an empty base URL:
/// `SOVEREIGN_DAEMON_URL= svrn enrich` should fall through to the config, not
/// dispatch at a bare `/v1/chat/completions`. Trailing slashes are trimmed
/// because every caller appends `/v1/…`, and `http://h:9841//v1/models` is a
/// different route to a strict router than `http://h:9841/v1/models`.
pub fn daemon_url_override() -> Option<String> {
    ["SOVEREIGN_DAEMON_URL", "SVRNMESH_DAEMON_URL"]
        .iter()
        .find_map(|key| {
            let raw = std::env::var(key).ok()?;
            let trimmed = raw.trim().trim_end_matches('/');
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        })
}

/// Pure builder behind [`client_daemon_base`], and the accessor for callers
/// that manage the LOCAL daemon process rather than talk to a daemon as a
/// client — see the env-blindness note on [`client_daemon_base`].
pub fn client_daemon_base_for(port: u16) -> String {
    format!("http://localhost:{port}")
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

/// `~/.svrnmesh/`. Previously lived in `sovereign-cli::util::dirs`;
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
    /// What kind of participant this node is. Derived from `[models]` and
    /// `[node] entry` — the ONE place that judgement is made (§7.5), so
    /// preflight, provider construction, `svrn doctor` and the status surface
    /// cannot reach different answers.
    ///
    /// Judged on CONTENT, not on whether the `[models]` table exists. A
    /// section that is present but names no primary is a placeholder — the
    /// desktop wizard writes one mid-flight — and a node holding a placeholder
    /// holds no models, whatever the file looks like. Keying this on presence
    /// made that node a `Holder` with nothing in it, which is the same
    /// three-empty-`PathBuf`s ambiguity `Option` was introduced to remove.
    ///
    /// Deliberately a different question from [`validate_class`], which asks
    /// whether the FILE is coherent enough to load and stays presence-based so
    /// the wizard's mid-flight config keeps loading. One implementation each;
    /// they are not two answers to one question (§10.6).
    pub fn node_class(&self) -> NodeClass {
        let holds_models = self
            .models
            .as_ref()
            .is_some_and(ModelsSection::is_populated);
        match (holds_models, self.node.binding().is_some()) {
            (true, _) => NodeClass::Holder,
            (false, true) => NodeClass::Terminal,
            (false, false) => NodeClass::Unconfigured,
        }
    }

    /// The model slots this node holds, or a refusal that says why there are
    /// none.
    ///
    /// The one way to read `[models]` on a path that genuinely needs a GGUF.
    /// The two absences are reported apart because they are fixed differently:
    /// a terminal is working as configured and the caller should route instead,
    /// while an unconfigured node needs `svrn setup` (§18.2, §18.3).
    ///
    /// An unpopulated section refuses exactly like a missing one: a caller on
    /// this path needs a GGUF, and handing it `ModelsSection::default()`'s
    /// empty `PathBuf`s would have it open `""` and report whatever llama.cpp
    /// says about that — the substitution this accessor exists to prevent
    /// (§18.3).
    pub fn models(&self) -> Result<&ModelsSection, String> {
        let populated = self.models.as_ref().filter(|m| m.is_populated());
        populated.ok_or_else(|| match self.node_class() {
            NodeClass::Terminal => format!(
                "this node is a terminal — it holds no models and routes \
                 inference to its entry node ({}). Nothing here can be served \
                 from a local slot.",
                self.node
                    .binding()
                    .map(|b| b.describe())
                    .unwrap_or_else(|| "unset".to_string()),
            ),
            _ => format!(
                "no models are configured in {} — run `svrn setup` (or \
                 `svrn setup --terminal <join-link>` for a node that routes to \
                 a peer instead of holding its own).",
                Self::default_path().display(),
            ),
        })
    }

    /// [`models`](Self::models), for the paths that WRITE a slot.
    ///
    /// Same refusal, same wording, one implementation — `svrn model set` was
    /// reaching for `is_none()` plus an `expect`, and borrowing the message
    /// from `models()` to stay in step with it.
    pub fn models_mut(&mut self) -> Result<&mut ModelsSection, String> {
        // Judged before the mutable borrow so the error can still read `self`.
        if let Err(e) = self.models() {
            return Err(e);
        }
        Ok(self
            .models
            .as_mut()
            .expect("models() returned Ok, so the section is present"))
    }

    /// The chat context window this node budgets against.
    ///
    /// Delegates to `[models] context_size` on a holder. On a terminal there is
    /// no local slot to read, so this is the documented default — and it is an
    /// APPROXIMATION of the entry node's real window, not a reading of it. The
    /// value drives client-side prompt budgeting only; the entry node enforces
    /// its own limit and refuses an over-long prompt on its own terms, so a
    /// mismatch costs a rejected turn rather than a silently truncated one.
    pub fn effective_context_size(&self) -> u32 {
        self.models
            .as_ref()
            .map(|m| m.effective_context_size())
            .unwrap_or_else(default_context_size)
    }

    /// The primary model's GGUF file stem — the id the slot manager resolves
    /// by, and the name a caller passes as `model` over HTTP.
    ///
    /// `None` on a terminal (no slots) and on a primary path with no stem.
    /// Collapses a chain that was copy-pasted at six call sites
    /// (`audit_extract`, `code_cmd`, `chat_cmd::bootstrap`,
    /// `recipe_agent_live_trial`, `deep_research::launch`, `mesh_bench`), each
    /// of which had to be updated in lockstep to stay right (§10.6).
    pub fn primary_model_stem(&self) -> Option<String> {
        self.models.as_ref()?.primary_stem()
    }

    /// The embed model's GGUF file stem. `None` on a terminal.
    ///
    /// Callers that merely LABEL may fall back to a default name; callers that
    /// actually embed must not, because this name decides which vector space
    /// the result lands in (`sovereign-cli-shared::models`'s doc states that
    /// split, and `build_daemon_embed_fn` is the side that refuses).
    pub fn embed_model_stem(&self) -> Option<String> {
        self.models.as_ref()?.embed_stem()
    }

    /// The embed model this node's own embedding calls land in — the local
    /// GGUF's stem on a holder, the ENTRY NODE's recorded id on a terminal.
    ///
    /// A terminal embeds over HTTP, so the vector space its text lands in is
    /// the entry node's. Anything keyed on "which space is this" — the corpus
    /// engine's cache key, the provider's `embed_model_id()`, a label — wants
    /// this one.
    ///
    /// **Not** what this node advertises to peers; that is
    /// [`advertised_embed_model_id`], and the two differ on purpose. Answering
    /// both from one accessor is what made a terminal offer its entry node's
    /// model as its own (§10.6): the chain `stem → entry` is right for the
    /// first question and a capability lie for the second.
    ///
    /// `None` means this node cannot name its embedding space at all — an
    /// unconfigured node, or a terminal whose entry node declares no embed
    /// slot. Callers that persist or compare under this name must map `None`
    /// to the trait's `"unknown"` sentinel rather than to an empty string:
    /// `""` is not the sentinel, so it reads downstream as a real model named
    /// empty-string and matches other rows stored the same way (§18.3, and
    /// `sovereign-core::memory`'s `model_known` check is the reader).
    pub fn local_embed_model_id(&self) -> Option<String> {
        self.embed_model_stem()
            .or_else(|| self.node.entry_embed_model.clone())
    }

    /// The embed model this node offers MESH PEERS — `None` on a terminal.
    ///
    /// Deliberately local-only, and deliberately its own name rather than a
    /// call to [`embed_model_stem`]: this is the one question whose answer must
    /// never fall back to the entry node. A terminal can embed, but only by
    /// forwarding, so advertising an embed model here would have the
    /// collaborative-ingestion planner partition work onto a node that can only
    /// proxy every chunk straight back to the machine the planner was trying to
    /// spread load off (`sovereign-mesh::capabilities`, whose own doc names
    /// `None` as "don't include me in distribution").
    pub fn advertised_embed_model_id(&self) -> Option<String> {
        self.embed_model_stem()
    }

    /// Reject a config whose class cannot be honoured.
    ///
    /// The guard that lets `models` be optional without letting absence be
    /// defaulted: a `[models]` section that went missing — a bad merge, a
    /// half-written file, a hand edit — must not read as a deliberate terminal,
    /// because a terminal without an entry node has nowhere to send a turn and
    /// would fail later, at the first request, far from the cause.
    ///
    /// Asks about PRESENCE, not content, and so does not go through
    /// [`node_class`](Self::node_class) — the two are different questions and
    /// each keeps its own implementation. Loadability has to stay permissive:
    /// the desktop wizard writes a config with an empty `[models]` table before
    /// the user has chosen slots, and refusing to load that would break setup
    /// mid-flight. What must be refused is the file that declares NEITHER a
    /// slot table nor an entry node, because nothing later in the boot can tell
    /// that apart from a deliberate terminal.
    fn validate_class(&self, path: &Path) -> Result<(), String> {
        // Two bindings is not a preference to resolve, it is a file that says
        // two different things about where a turn goes. Refuse it here rather
        // than let `binding()` pick a winner — a silent pick is how the wrong
        // one ends up load-bearing and nobody finds out until a turn lands on
        // the wrong machine (§18.3).
        if self.node.entry_node.is_some() && self.node.entry.is_some() {
            return Err(format!(
                "{} sets BOTH `[node] entry_node` (a mesh identity) and \
                 `[node] entry` (an address), which are two answers to where a \
                 turn goes. Keep the identity and delete the address, or \
                 re-run `svrn setup --reset --terminal <join-link>`.",
                path.display(),
            ));
        }
        match self.models.is_some() || self.node.binding().is_some() {
            true => Ok(()),
            false => Err(format!(
                "{} declares neither `[models]` nor a `[node]` entry binding, \
                 so this node can neither serve a turn nor route one. Run \
                 `svrn setup` to hold models locally, or `svrn setup \
                 --terminal <join-link>` to route to a peer that does.",
                path.display(),
            )),
        }
    }

    /// A config for a process that has **no `config.toml`** — no model slots
    /// configured, every other section at its documented default (`:9741` /
    /// `:9742`, loopback client bind, `0.0.0.0` internal bind, no bearer
    /// token). This is the real state of `svrn mesh create` on a machine that
    /// has not run `svrn setup`, and of any test that does not care about
    /// models.
    ///
    /// Deliberately **not** an `impl Default`: `load().unwrap_or_default()`
    /// would silently substitute this for a `config.toml` that exists but
    /// would not parse, which is the substitution ARCH §18.3 forbids. A caller
    /// has to type the name, and a caller reaching for it after a failed load
    /// must report the failure first.
    pub fn unconfigured() -> Self {
        Self {
            models: None,
            node: NodeSection::default(),
            daemon: DaemonSection::default(),
            data: DataSection::default(),
            watched_folders: WatchedFoldersSection::default(),
            memory: MemorySection::default(),
            iroh: IrohSection::default(),
            shared_model: SharedModelSection::default(),
            discovery: DiscoverySection::default(),
            compute: ComputeSection::default(),
            engine: EngineSection::default(),
            // Added on main while this branch was out. `unconfigured()`'s
            // contract is "every other section at its documented default",
            // so the default is the answer here, not a judgement call.
            search: SearchSection::default(),
            mcp_servers: Vec::new(),
        }
    }

    /// The canonical config path: `~/.svrnmesh/config.toml`. Co-located
    /// with `~/.svrnmesh/`'s other user-scoped state (corpora, indexes,
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
    // Deliberately NOT routed through `rebrand::mesh_config_dir()`. That
    // accessor prefers the *rebranded* dir, which is the opposite of what a
    // migration source needs: this function's whole job is to name the OLD
    // location so the file can be moved off it. Using the branded accessor
    // here would make the migration look for the destination and silently
    // find nothing to migrate.
    #[allow(clippy::disallowed_methods)] // migration SOURCE: names the pre-rebrand path literally
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
        cfg.validate_class(path)?;
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
    /// TOML stores `~/.svrnmesh/...` literally; we resolve at load time.
    fn expand_paths(&mut self) {
        // A terminal has no `[models]`, so there is nothing to expand — and
        // nothing to invent. `data.dir` is expanded either way: every node has
        // one, weights or no weights.
        if let Some(models) = self.models.as_mut() {
            models.primary = expand_home(&models.primary);
            if let Some(fast) = &models.fast {
                models.fast = Some(expand_home(fast));
            }
            models.embed = expand_home(&models.embed);
            if let Some(p) = models.code.as_mut() {
                *p = expand_home(p);
            }
            for path in models.extra.values_mut() {
                *path = expand_home(path);
            }
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
#[allow(clippy::disallowed_methods)] // tilde-expansion of USER-SUPPLIED input — the one legitimate raw home_dir use
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

    /// The whole point of the helper is that it does NOT bake in 9742. A
    /// regression here restores the four-way-duplicated bug it replaced:
    /// every internal-API caller silently pinned to the default port.
    #[test]
    fn internal_base_url_honours_a_moved_port() {
        assert_eq!(internal_daemon_base_for(9742), "http://127.0.0.1:9742");
        assert_eq!(internal_daemon_base_for(19742), "http://127.0.0.1:19742");
    }

    /// The loading path falls back to the compiled default rather than
    /// erroring, matching the `#[serde(default)]` posture on the field: a
    /// missing config means defaults. Asserted against the constant so moving
    /// `default_internal_port` cannot leave the fallback behind.
    #[test]
    fn internal_base_url_falls_back_to_the_declared_default() {
        assert_eq!(
            internal_daemon_base_for(default_internal_port()),
            "http://127.0.0.1:9742"
        );
    }

    /// `ComputeSection`'s fields carry `#[serde(default)]` with no
    /// `skip_serializing_if`, so `save_to` materializes `distributed_primary =
    /// false` literally into every config it writes.
    ///
    /// This is stated as a test because it KILLS an attractive design. It is
    /// tempting to make the compute-child containment auto-arm — "default it on
    /// when the node is a shared-model host, unless the operator explicitly said
    /// false" — but with the flag written out explicitly there is no way to
    /// distinguish "unset" from "deliberately false". An `Option<bool>`
    /// migration would not fire on any existing config, including the one that
    /// motivated it, and an auto-arm that ignores an explicit `false` is not a
    /// default, it is an override of a stated choice. Containment is therefore
    /// enforced by a boot guard that REFUSES and names the fix, not by silently
    /// changing what the operator asked for.
    #[test]
    fn compute_section_is_serialized_explicitly_so_unset_is_not_recoverable() {
        let toml = toml::to_string_pretty(&ComputeSection::default()).expect("serialize");
        assert!(
            toml.contains("distributed_primary = false"),
            "expected an explicit `distributed_primary = false` in:\n{toml}"
        );
        assert!(
            toml.contains("enabled = false"),
            "expected an explicit `enabled = false` in:\n{toml}"
        );
    }

    /// THE NO-REGRESSION BAR for per-slot windows (2026-08-25).
    ///
    /// Adding `fast_context_size` must be invisible to every config.toml
    /// already on disk. An omitted key resolves to the primary's window, which
    /// is exactly what the single global scalar did — so a host that does not
    /// set it builds byte-identical contexts to the ones it built before the
    /// key existed. If this ever fails, the change has stopped being additive
    /// and every existing install's fast slot has silently been resized.
    #[test]
    fn an_unset_fast_window_is_the_primary_window() {
        let mut m = models("/p.gguf", None, "/e.gguf");
        m.fast_context_size = None;

        m.context_size = None; // and the default path too
        assert_eq!(m.effective_fast_context_size(), m.effective_context_size());

        for ctx in [4096, 16_384, 65_536] {
            m.context_size = Some(ctx);
            assert_eq!(m.effective_fast_context_size(), ctx);
            assert_eq!(m.effective_fast_context_size(), m.effective_context_size());
        }
    }

    /// The lever itself: when set, the fast slot's window is its own and the
    /// primary's is untouched. Named failing input for the inverse defect —
    /// wiring `fast_context_size` to BOTH slots would shrink the primary's
    /// window on any host that set it, which is the more damaging mistake.
    #[test]
    fn a_set_fast_window_moves_only_the_fast_slot() {
        let mut m = models("/p.gguf", None, "/e.gguf");
        m.context_size = Some(65_536);
        m.fast_context_size = Some(8_192);

        assert_eq!(m.effective_fast_context_size(), 8_192);
        assert_eq!(
            m.effective_context_size(),
            65_536,
            "the primary keeps its window — this key sizes the fast slot only"
        );
    }

    fn models(primary: &str, fast: Option<&str>, embed: &str) -> ModelsSection {
        ModelsSection {
            primary: PathBuf::from(primary),
            fast: fast.map(PathBuf::from),
            embed: PathBuf::from(embed),
            code: None,
            context_size: None,
            fast_context_size: None,
            extra: BTreeMap::new(),
            max_extras_memory_gb: None,
            primary_pool: None,
            edit: None,
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
        assert_eq!(
            cfg.models().unwrap().primary,
            PathBuf::from("/models/primary.gguf")
        );
        assert!(cfg.models().unwrap().fast.is_none());
        assert_eq!(
            cfg.models().unwrap().fast_path(),
            Path::new("/models/primary.gguf")
        );
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

    /// The three participant states are three values, and none of them is a
    /// flavour of another (§18.2).
    #[test]
    fn node_class_is_derived_from_models_and_entry() {
        let mut cfg = SetupConfig::unconfigured();
        assert_eq!(cfg.node_class(), NodeClass::Unconfigured);

        cfg.node.entry = Some("http://halo:9741/v1".into());
        assert_eq!(cfg.node_class(), NodeClass::Terminal);

        cfg.models = Some(models("/m/p.gguf", None, "/m/e.gguf"));
        assert_eq!(
            cfg.node_class(),
            NodeClass::Holder,
            "holding weights makes a node a holder even with an entry configured — \
             the entry is then simply unused"
        );
    }

    /// A mesh identity is a binding in its own right — the one `svrn setup
    /// --terminal <join-link>` writes, and the one ARCH §7.5 asks for.
    #[test]
    fn an_identity_alone_makes_a_terminal() {
        let mut cfg = SetupConfig::unconfigured();
        cfg.node.entry_node = Some("44ae76142b0c3c723051ff98f043104a".into());
        assert_eq!(cfg.node_class(), NodeClass::Terminal);
        assert_eq!(
            cfg.node.binding(),
            Some(EntryBinding::Node(
                "44ae76142b0c3c723051ff98f043104a".into()
            ))
        );
    }

    /// An address alone still binds — an entry node that is not a mesh member
    /// has no identity to resolve, and that case stays supported.
    #[test]
    fn an_address_alone_still_binds() {
        let mut cfg = SetupConfig::unconfigured();
        cfg.node.entry = Some("http://halo:9741/v1".into());
        assert_eq!(
            cfg.node.binding(),
            Some(EntryBinding::Address("http://halo:9741/v1".into()))
        );
    }

    /// An empty string is not a binding. Serde `default` plus a hand-edited
    /// file can produce one, and treating it as present would send every turn
    /// to `"" `and report the node as a working terminal (§18.3).
    #[test]
    fn a_blank_binding_is_no_binding() {
        let mut cfg = SetupConfig::unconfigured();
        cfg.node.entry = Some("   ".into());
        cfg.node.entry_node = Some(String::new());
        assert_eq!(cfg.node.binding(), None);
        assert_eq!(cfg.node_class(), NodeClass::Unconfigured);
    }

    /// Two bindings is a file that says two different things about where a
    /// turn goes. Refused at LOAD, so it cannot be resolved by precedence
    /// somewhere further in and land turns on the wrong machine.
    #[test]
    fn a_config_carrying_both_bindings_is_refused_at_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[node]\n\
             entry = \"http://halo:9741/v1\"\n\
             entry_node = \"44ae76142b0c3c723051ff98f043104a\"\n\
             \n\
             [data]\n\
             dir = \"/tmp/x\"\n",
        )
        .expect("write");
        let err = SetupConfig::load_from(&path).expect_err("both bindings must be refused");
        assert!(err.contains("entry_node"), "got: {err}");
        assert!(err.contains("entry"), "got: {err}");
    }

    /// One binding loads fine — the guard above must not have made every
    /// terminal config unloadable.
    #[test]
    fn a_config_with_one_binding_loads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[node]\n\
             entry_node = \"44ae76142b0c3c723051ff98f043104a\"\n\
             \n\
             [data]\n\
             dir = \"/tmp/x\"\n",
        )
        .expect("write");
        let cfg = SetupConfig::load_from(&path).expect("one binding is a valid terminal");
        assert_eq!(cfg.node_class(), NodeClass::Terminal);
    }

    /// A `[models]` table that EXISTS but names nothing is not a holder.
    ///
    /// The desktop wizard writes exactly this shape mid-flight
    /// (`commands/config_setup.rs` — `Some(ModelsSection::default())` so a file
    /// exists before the user has picked slots), and a truncated or
    /// badly-merged file lands in it too. Judged on presence, such a node was a
    /// `Holder` holding nothing and `models()` handed callers three empty
    /// `PathBuf`s — the same ambiguity `Option<ModelsSection>` was introduced to
    /// remove, surviving on the other side of the boundary.
    #[test]
    fn a_placeholder_models_table_is_unconfigured_not_a_holder() {
        let mut cfg = SetupConfig::unconfigured();
        cfg.models = Some(ModelsSection::default());

        assert_eq!(
            cfg.node_class(),
            NodeClass::Unconfigured,
            "a `[models]` section naming no primary holds no models, whatever \
             shape the file is in"
        );
        let err = cfg
            .models()
            .expect_err("a placeholder section must refuse, not hand back empty paths");
        assert!(
            err.contains("svrn setup"),
            "the refusal must name the fix, got: {err}"
        );
    }

    /// ...and it must still LOAD, or the wizard breaks mid-flight.
    ///
    /// Loadability and class are deliberately different questions with
    /// different implementations: `validate_class` asks whether the file is
    /// coherent enough to open (presence), `node_class` asks what the node is
    /// (content). Collapsing them would either break the desktop's mid-setup
    /// write or resurrect the placeholder-as-holder bug above.
    #[test]
    fn a_placeholder_models_table_still_loads() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            "[models]\nprimary = \"\"\nembed = \"\"\n\n[daemon]\n[data]\n",
        )
        .unwrap();

        let cfg = SetupConfig::load_from(&path)
            .expect("the desktop wizard's mid-flight config must keep loading");
        assert_eq!(cfg.node_class(), NodeClass::Unconfigured);
    }

    /// "Which embed model" is TWO questions, and a terminal answers them
    /// differently.
    ///
    /// `local_embed_model_id` is the space this node's own text lands in — the
    /// entry node's, because a terminal embeds over HTTP.
    /// `advertised_embed_model_id` is what it offers PEERS, and must stay
    /// `None`: the collaborative-ingestion planner filters candidates by exact
    /// match on it, so advertising here partitions work onto a node that can
    /// only proxy every chunk back to the machine the planner was spreading
    /// load off. One accessor answering both is how the terminal came to
    /// advertise its entry node's model as its own (§10.6).
    #[test]
    fn a_terminal_embeds_under_its_entry_node_but_advertises_nothing() {
        let mut cfg = SetupConfig::unconfigured();
        cfg.node.entry = Some("http://halo:9741/v1".into());
        cfg.node.entry_embed_model = Some("qwen3-embedding-0.6b".into());

        assert_eq!(
            cfg.local_embed_model_id().as_deref(),
            Some("qwen3-embedding-0.6b"),
            "a terminal's vectors land in its ENTRY node's space, and that is \
             the honest name for them"
        );
        assert_eq!(
            cfg.advertised_embed_model_id(),
            None,
            "a terminal holds no embed slot, so it offers peers none"
        );
    }

    /// A holder answers both questions with its own slot.
    #[test]
    fn a_holder_advertises_the_slot_it_embeds_with() {
        let mut cfg = SetupConfig::unconfigured();
        cfg.models = Some(models("/m/p.gguf", None, "/m/qwen3-embedding-0.6b.gguf"));

        assert_eq!(
            cfg.local_embed_model_id().as_deref(),
            Some("qwen3-embedding-0.6b")
        );
        assert_eq!(
            cfg.advertised_embed_model_id().as_deref(),
            Some("qwen3-embedding-0.6b"),
            "the two answers coincide on a holder — which is why keying both on \
             one accessor stayed invisible until a terminal existed"
        );
    }

    /// An entry node with no embed slot leaves the space UNNAMEABLE, and that
    /// must not become a name.
    ///
    /// `svrn setup --terminal` supports this state explicitly (it prints "no
    /// embed slot" and records the absence). The value must stay `None` here so
    /// the provider maps it to the trait's `"unknown"` sentinel; an empty
    /// string would pass `memory.rs`'s `model_known` gate as a real model named
    /// empty-string and get embeddings persisted under it (§18.3).
    #[test]
    fn a_terminal_whose_entry_has_no_embed_slot_names_no_model() {
        let mut cfg = SetupConfig::unconfigured();
        cfg.node.entry = Some("http://halo:9741/v1".into());

        assert_eq!(cfg.local_embed_model_id(), None);
        assert_eq!(cfg.advertised_embed_model_id(), None);
    }

    /// `[models]` going missing must not read as "I meant to be a terminal".
    ///
    /// This is the guard that lets the section be optional at all: without it,
    /// a bad merge or a half-written file silently reclassifies the node, and
    /// the failure surfaces later as a turn with nowhere to go (§18.3).
    #[test]
    fn a_config_with_neither_models_nor_an_entry_is_refused_at_load() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "[daemon]\n[data]\n").unwrap();

        let err = SetupConfig::load_from(&path)
            .expect_err("a config that can neither serve nor route must not load");
        assert!(
            err.contains("neither") && err.contains("svrn setup"),
            "the refusal must name both the cause and the fix, got: {err}"
        );
    }

    /// The terminal config is a real, loadable shape — not merely one the
    /// validator tolerates.
    #[test]
    fn a_terminal_config_loads_and_reports_its_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            "[node]\nentry = \"http://halo:9741/v1\"\n\n[daemon]\n[data]\n",
        )
        .unwrap();

        let cfg = SetupConfig::load_from(&path).expect("a terminal config must load");
        assert_eq!(cfg.node_class(), NodeClass::Terminal);
        assert_eq!(cfg.node.entry.as_deref(), Some("http://halo:9741/v1"));
        assert!(cfg.models.is_none());
    }

    /// A terminal's refusal has to say WHICH absence this is, because the two
    /// are fixed differently: route instead, versus run setup.
    #[test]
    fn the_two_absences_of_models_are_reported_apart() {
        let mut terminal = SetupConfig::unconfigured();
        terminal.node.entry = Some("http://halo:9741/v1".into());
        let terminal_err = terminal.models().unwrap_err();
        assert!(
            terminal_err.contains("terminal") && terminal_err.contains("http://halo:9741/v1"),
            "a terminal's refusal must name the class and the entry node, got: {terminal_err}"
        );

        let unconfigured_err = SetupConfig::unconfigured().models().unwrap_err();
        assert!(
            unconfigured_err.contains("svrn setup") && !unconfigured_err.contains("terminal —"),
            "an unconfigured node's refusal must point at setup, got: {unconfigured_err}"
        );
    }

    /// A terminal budgets against the documented default rather than reading a
    /// slot it does not have — and says so instead of panicking.
    #[test]
    fn a_terminal_reports_the_default_context_window() {
        let mut cfg = SetupConfig::unconfigured();
        cfg.node.entry = Some("http://halo:9741/v1".into());
        assert_eq!(cfg.effective_context_size(), default_context_size());
        assert_eq!(cfg.primary_model_stem(), None);
        assert_eq!(cfg.embed_model_stem(), None);
    }

    #[test]
    fn roundtrip_minimal_config() {
        let cfg = SetupConfig {
            engine: Default::default(),
            compute: Default::default(),
            search: Default::default(),
            models: Some(ModelsSection {
                primary: PathBuf::from("/models/primary.gguf"),
                fast: Some(PathBuf::from("/models/fast.gguf")),
                embed: PathBuf::from("/models/embed.gguf"),
                code: None,
                context_size: None,
                fast_context_size: None,
                extra: BTreeMap::new(),
                max_extras_memory_gb: None,
                primary_pool: None,
                edit: None,
            }),
            node: NodeSection::default(),
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
        assert_eq!(
            loaded.models().unwrap().primary,
            cfg.models().unwrap().primary
        );
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
            engine: Default::default(),
            compute: Default::default(),
            search: Default::default(),
            models: Some(ModelsSection {
                primary: PathBuf::from("/m/p.gguf"),
                fast: None,
                embed: PathBuf::from("/m/e.gguf"),
                code: None,
                context_size: None,
                fast_context_size: None,
                extra: BTreeMap::new(),
                max_extras_memory_gb: None,
                primary_pool: None,
                edit: None,
            }),
            node: NodeSection::default(),
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
    #[allow(clippy::disallowed_methods)] // test asserts tilde-expansion against the REAL home
    fn expand_home_resolves_tilde() {
        // Reads the process-global HOME — must serialize against the tests
        // that swap it (see `crate::test_support::home_env_lock`).
        let _home_guard = crate::test_support::home_env_lock();
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
        // Reads the process-global HOME (`default_path()` ->
        // `default_data_dir()` -> `svrnmesh_root()`) — must serialize against
        // the tests that swap it (see `crate::test_support::home_env_lock`).
        // It passed under a swapped HOME only because the swapping test
        // populates BOTH brand dirs in its tempdir and the assertion accepts
        // either spelling; that is luck, not coverage.
        let _home_guard = crate::test_support::home_env_lock();
        // Config lives directly under home in a hidden, brand-named dir:
        // `~/.svrnmesh/config.toml` (preferred) or the legacy
        // `~/.svrnmesh/config.toml`. Post-rename, `default_path()` ->
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
        assert!(cfg.models().unwrap().extra.is_empty());
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
        assert_eq!(cfg.models().unwrap().extra.len(), 2);
        assert_eq!(
            cfg.models().unwrap().extra.get("reasoning"),
            Some(&PathBuf::from("/m/big.gguf"))
        );
        assert_eq!(
            cfg.models().unwrap().extra.get("bulk"),
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
        assert!(cfg.models().unwrap().max_extras_memory_bytes().is_none());
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
            cfg.models().unwrap().max_extras_memory_bytes(),
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
        assert_eq!(cfg.models().unwrap().max_extras_memory_bytes(), Some(0));
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // test asserts tilde-expansion against the REAL home
    fn extra_slots_expand_home_at_load() {
        // `~/...` paths inside `[models.extra]` resolve like the
        // primary/fast/embed paths do — load-time expansion via
        // `expand_paths`.
        //
        // Reads the process-global HOME — must serialize against the tests
        // that swap it (see `crate::test_support::home_env_lock`).
        let _home_guard = crate::test_support::home_env_lock();
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
            cfg.models().unwrap().extra.get("reasoning"),
            Some(&home.join("dev/big.gguf"))
        );
    }

    // ---- the daemon-endpoint decider (§10.6) -------------------------------
    //
    // These mutate PROCESS-global env. nextest (the default engine) is
    // process-per-test and would not need serialising; `--engine cargo` runs
    // every test in this binary in one process, and a gate that passes only
    // under one executor is not a gate (§18.1). Hence the lock.
    static DAEMON_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    const DAEMON_URL_KEYS: [&str; 2] = ["SOVEREIGN_DAEMON_URL", "SVRNMESH_DAEMON_URL"];

    /// Clears BOTH spellings, applies `pairs`, and restores the prior values on
    /// drop — so a developer running the suite with the knob exported in their
    /// shell gets the same verdict as CI.
    struct DaemonEnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        prior: Vec<(&'static str, Option<String>)>,
    }

    impl DaemonEnvGuard {
        fn set(pairs: &[(&'static str, &str)]) -> Self {
            let lock = DAEMON_ENV_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let prior = DAEMON_URL_KEYS
                .iter()
                .map(|k| (*k, std::env::var(k).ok()))
                .collect();
            for k in DAEMON_URL_KEYS {
                std::env::remove_var(k);
            }
            for (k, v) in pairs {
                std::env::set_var(k, v);
            }
            Self { _lock: lock, prior }
        }
    }

    impl Drop for DaemonEnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.prior {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    /// RED on the tree before 2026-08-25: `client_daemon_base` read the config
    /// and nothing else, so the knob reached one call site out of ~30 and
    /// `svrn enrich` silently drove the operator's daemon instead of the one
    /// the operator had just pointed it at.
    #[test]
    fn client_daemon_base_honours_the_env_knob() {
        let _g = DaemonEnvGuard::set(&[("SOVEREIGN_DAEMON_URL", "http://127.0.0.1:19741")]);
        assert_eq!(client_daemon_base(), "http://127.0.0.1:19741");
    }

    #[test]
    fn client_daemon_base_accepts_the_svrnmesh_spelling() {
        let _g = DaemonEnvGuard::set(&[("SVRNMESH_DAEMON_URL", "http://127.0.0.1:19742")]);
        assert_eq!(client_daemon_base(), "http://127.0.0.1:19742");
    }

    /// Both set is not a coin flip: SOVEREIGN_ wins, matching every other
    /// reader of the pair.
    #[test]
    fn client_daemon_base_prefers_sovereign_over_svrnmesh() {
        let _g = DaemonEnvGuard::set(&[
            ("SOVEREIGN_DAEMON_URL", "http://127.0.0.1:19741"),
            ("SVRNMESH_DAEMON_URL", "http://127.0.0.1:19742"),
        ]);
        assert_eq!(client_daemon_base(), "http://127.0.0.1:19741");
    }

    /// A blank knob is UNSET, not an empty base URL — otherwise
    /// `SOVEREIGN_DAEMON_URL= svrn enrich` would POST to `/v1/chat/completions`
    /// with no host and fail somewhere unrecognisable.
    #[test]
    fn client_daemon_base_treats_blank_env_as_unset() {
        let _g = DaemonEnvGuard::set(&[("SOVEREIGN_DAEMON_URL", "   ")]);
        assert!(
            client_daemon_base().starts_with("http://localhost:"),
            "blank knob must fall through to the configured port, got {}",
            client_daemon_base()
        );
    }

    /// Callers append `/v1/…`; a trailing slash would build a double-slash
    /// path that a strict router treats as a different route.
    #[test]
    fn client_daemon_base_trims_trailing_slash() {
        let _g = DaemonEnvGuard::set(&[("SOVEREIGN_DAEMON_URL", "http://127.0.0.1:19741/")]);
        assert_eq!(client_daemon_base(), "http://127.0.0.1:19741");
        assert_eq!(
            format!("{}/v1/models", client_daemon_base()),
            "http://127.0.0.1:19741/v1/models"
        );
    }

    /// The unchanged leg: with no knob the configured port still decides, and
    /// the host spelling stays `localhost` for parity with `urls::v1_url`.
    #[test]
    fn client_daemon_base_without_env_is_the_configured_port() {
        let _g = DaemonEnvGuard::set(&[]);
        let base = client_daemon_base();
        assert!(
            base.starts_with("http://localhost:"),
            "expected the configured-port form, got {base}"
        );
    }

    /// The pure builder is the accessor for daemon-process MANAGEMENT, and it
    /// must stay env-blind — `daemon restart` pointing at a rented pod would
    /// otherwise kill the local daemon and report READY off the pod (§18.3).
    #[test]
    fn client_daemon_base_for_ignores_the_env_knob() {
        let _g = DaemonEnvGuard::set(&[("SOVEREIGN_DAEMON_URL", "http://a-rented-pod:9841")]);
        assert_eq!(client_daemon_base_for(9741), "http://localhost:9741");
    }
}
