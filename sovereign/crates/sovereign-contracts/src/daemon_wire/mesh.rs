// SPDX-License-Identifier: AGPL-3.0-or-later
//! Mesh wire shapes — `/v1/mesh/*` (`sovereign_mesh::mesh_http`), the
//! UI-facing mesh view (`sovereign_mesh::types`), this node's own
//! reachability record (`sovereign_mesh::{daemon, iroh_watchdog}`), and the
//! relay-candidate row (`sovereign_mesh::mesh_discovery`). Moved here at
//! sv-surface svt-3 (2026-09-11) once `OriginKind` had a layer-0 home
//! (`oicp_types::origin`); `sovereign-mesh` re-exports every item at its
//! historical path, so the routes, their tests and the CLI are unchanged.
//!
//! [`MeshStatusSummary`] is the one item here that is NOT a relocation — see
//! its doc.

use serde::{Deserialize, Serialize};

pub use oicp_types::OriginKind;

/// Serde default for [`MeshMember::active`] / [`MemberDto::active`]: a payload
/// from a daemon that predates the field describes members it considers
/// present, so absent reads as active. Defaulting to `false` would tombstone a
/// whole mesh on upgrade.
fn default_true() -> bool {
    true
}

// ─── `sovereign_mesh::types` — the UI-facing mesh view ──────────

/// Mesh status as shown in the UI sidebar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshStatus {
    /// The mesh's display name.
    pub name: String,
    /// Members the daemon currently judges online (or busy).
    pub members_online: usize,
    /// Every roster row, tombstones excluded.
    pub members_total: usize,
    /// The shared model the mesh is serving, when a plan is active.
    pub model_name: Option<String>,
    /// Corpora shared across the mesh, by id.
    pub knowledge_corpora: Vec<String>,
    /// This node is a live member (never `false` on a served answer; kept for the UI).
    pub is_connected: bool,
    /// `sovereign://join/...` invite for the active mesh. `None` when
    /// the daemon resumed a mesh from before the cached-plaintext
    /// feature shipped — the UI hides the share card and offers
    /// "Rotate" to recover an inviteable link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub join_link: Option<String>,
    /// Bare `cwth-XXXX-XXXX-XXXX` — the link's payload, exposed so
    /// users can paste into chat clients that mangle deep-link URLs.
    /// Same `None` semantics as `join_link`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub join_key: Option<String>,
    /// Track W: this founder's own iroh reachability (relay-homed?,
    /// discoverable?, plus the self-heal watchdog's recovery history). `None`
    /// when iroh isn't running. Populated from `/v1/mesh/status`; drives the
    /// desktop's "Reachable / Reconnecting" indicator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_reachability: Option<SelfReachability>,
}

/// A member of the mesh, as shown in the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshMember {
    /// The member's display name.
    pub name: String,
    /// The member's mesh node id, display form.
    pub node_id: String,
    /// This row is the node that answered.
    pub is_self: bool,
    /// Liveness as the daemon judges it.
    pub status: MemberStatus,
    /// Advertised total GPU VRAM (GB) summed across the member's GPUs — the
    /// planning input for `svrn mesh plan --from-mesh`.
    #[serde(default)]
    pub vram_gb: u32,
    /// Advertises itself as a shared-model anchor (an eligible tensor-split worker).
    #[serde(default)]
    pub can_anchor: bool,
    /// 0-5 bar-chart level, from the contribution ledger.
    pub contribution_level: u8, // 0-5 bar chart
    /// Human label beside the bar ("Top contributor", "Mostly uses, that's ok!").
    pub contribution_label: String, // "Top contributor", "Mostly uses, that's ok!"
    /// Tailnet (or other reachable) addresses for this member, as
    /// known to the local daemon's gossip view. Surfaced for
    /// operator use — see `sovereign mesh status` for the
    /// human-readable rendering and `--addr` for scripting. Empty
    /// when the member hasn't advertised a routable address yet
    /// (most often during a fresh join, before the first gossip
    /// round, or when the member crashed without graceful shutdown).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<String>,
    /// The local origins this member serves to the mesh
    /// (`NodeCapabilities::origins`) — `media` when its `[iroh] media_origin`
    /// is live. Empty for a member whose daemon predates the field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub origins: Vec<OriginKind>,
    /// Stable hash of this member's advertised hardware
    /// (`sovereign_core::mesh_measurements::hardware_fingerprint`).
    ///
    /// Part of the measurement cache key: a measured throughput number is only
    /// valid on the hardware it was measured on, so a machine change has to
    /// break the key rather than quietly serve the old number. `None` for a
    /// peer running a daemon that predates this field, which `mesh plan` treats
    /// as "not measured" rather than substituting a placeholder — one shared
    /// default would collide every unidentified host into a single key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hw_fingerprint: Option<u64>,
    /// The endpoint key this member is dialed on, lowercase hex.
    ///
    /// EXPOSED SO THE COLLISION IS VISIBLE. `node_pubkey` is what peers dial
    /// and what the iroh acceptor admits on, so two ACTIVE members carrying
    /// one key is a defect (`commonwealth_core::mesh::aliased_endpoint_keys`)
    /// — but until 2026-08-28 no read surface carried the field, so an
    /// operator staring at `svrn mesh status` could not see it and the live
    /// invariant check could not test for it. `None` for a peer running a
    /// pre-identity build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_pubkey: Option<String>,
    /// `removed_at.is_none()` — this row is not a tombstone.
    ///
    /// Distinct from [`MeshMember::status`], which is a liveness judgement: a
    /// departed member is `Offline` AND inactive, while a crashed one is
    /// `Offline` and still active. The alias rule is scoped on THIS, because a
    /// tombstoned row sharing a key with a rejoined node is a legitimate
    /// rejoin rather than a collision.
    #[serde(default = "default_true")]
    pub active: bool,
    /// GPU compute backend as advertised (`cuda` | `rocm` | `metal` | `vulkan`).
    ///
    /// Displayed beside a measurement so the reader knows which stack produced
    /// it. The same silicon driven through a different backend runs at a
    /// materially different rate, so this is also folded into
    /// [`MeshMember::hw_fingerprint`] — it annotates *and* discriminates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
}

/// A member's liveness, as the UI renders it. `rename_all = "lowercase"` IS
/// the string set `MemberDto::status` carries — one decider for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemberStatus {
    /// Present and answering.
    Online,
    /// Present and serving a request.
    Busy,
    /// Present, idle past the away threshold.
    Away,
    /// Not heard from within the liveness window.
    Offline,
}

/// User-friendly contribution summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContributionSummary {
    /// Compute this node served to the mesh, in hours.
    pub compute_hours_contributed: f64,
    /// Compute this node consumed from the mesh, in hours.
    pub compute_hours_used: f64,
    /// Corpus bytes this node hosts for others, in GB.
    pub storage_hosted_gb: f64,
    /// Bytes this node served, in GB.
    pub bandwidth_served_gb: f64,
    /// `compute_hours_contributed >= compute_hours_used`.
    pub is_net_contributor: bool,
    /// One sentence for the UI ("You're a net contributor. Thank you!").
    pub summary_text: String, // "You're a net contributor. Thank you!"
}

/// Corpus available to add to the mesh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshCorpus {
    /// Corpus id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// One-line description.
    pub description: String,
    /// Human-formatted count ("6.8M articles").
    pub article_count: String, // "6.8M articles"
    /// Human-formatted size ("22 GB").
    pub download_size: String, // "22 GB"
    /// Where the corpus stands for this node.
    pub status: CorpusStatus,
}

/// Where a [`MeshCorpus`] stands for this node.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorpusStatus {
    /// Offered by the mesh, not installed here.
    Available,
    /// Being installed — `percent` done, on `node`.
    Installing {
        /// Percent complete.
        percent: f32,
        /// The node doing the install.
        node: String,
    },
    /// Installed locally.
    Installed,
    /// Served by `peer_name`, not held locally.
    SharedByPeer {
        /// The peer serving it.
        peer_name: String,
    },
}

/// Join confirmation info shown when a user taps a deep link. Answer of
/// `POST /v1/mesh/join/preview`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinConfirmation {
    /// The mesh the invite is for (`"Unknown Mesh"` when the link carries no name).
    pub mesh_name: String,
    /// Who sent the invite, when the link says.
    pub invited_by: Option<String>,
    /// The bare `cwth-…` key.
    pub join_key: String,
    /// `?relay=` host:port from the link, for a joiner mDNS will not reach.
    pub relay_hint: Option<String>,
    /// Founder's iroh dial string — present when the invite carries a
    /// no-VPN connect path (either mesh kind). `encrypted` below says
    /// which join posture it implies; `None` ⇒ legacy IP/mDNS join.
    #[serde(default)]
    pub iroh_dial: Option<String>,
    /// True iff the invite is for an ENCRYPTED mesh (`iroh=` param —
    /// fail-closed key-dialed join). False with `iroh_dial` present
    /// means a plaintext mesh reachable over iroh (`dial=` param —
    /// prefer-iroh join, IP/mDNS fallback).
    #[serde(default)]
    pub encrypted: bool,
    /// Unix-seconds TTL after which the invite is rejected (display).
    #[serde(default)]
    pub expires_at: Option<u64>,
}

/// Body of `POST /v1/mesh/join/preview` — the invite as the user pasted
/// it. The HOST parses it (`parse_join_argument`: bare key, https URL or
/// `sovereign://` link) and answers a [`JoinConfirmation`], so the desktop
/// previews exactly what `POST /v1/mesh/join` would accept and nothing
/// else — one parser, not a second one linked into the client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinPreviewRequest {
    /// The invite as pasted: bare key, https URL, or `sovereign://` link.
    pub link: String,
}

// ─── `sovereign_mesh::{daemon, iroh_watchdog}` — own reachability ──

/// The founder's OWN iroh reachability (Track W hardening), for
/// `/v1/mesh/status.self_reachability` and the desktop "Reachable /
/// Reconnecting" indicator. Flattens the reachability watchdog's live health
/// snapshot so the wire object is one flat record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelfReachability {
    /// This node's current dial-by-key string (all relays + direct addrs), or
    /// `None` before any reachable address is known.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub dial: Option<String>,
    /// iroh endpoint id (hex).
    pub endpoint_id: String,
    /// Live watchdog health: relay-homed, discovery probe, recovery history.
    #[serde(flatten)]
    pub health: ReachabilityStatus,
}

/// Live reachability snapshot the watchdog writes each cycle and the status API
/// reads (`/v1/mesh/status.self_reachability`). Survives endpoint rebuilds —
/// the watchdog owns the shared `Arc`, so counts/last-recovery persist across a
/// rebuilt endpoint.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ReachabilityStatus {
    /// At least one home relay is connected (dialable via relay).
    pub relay_homed: bool,
    /// Currently-connected home relay URL(s), for display.
    pub relay_urls: Vec<String>,
    /// Last self-discovery probe: `Some(true)` = own record resolved,
    /// `Some(false)` = missing/stale, `None` = not run / discovery off.
    pub discovery_ok: Option<bool>,
    /// Most recent relay error (`RelayStatus::last_error`), if disconnected.
    pub last_error: Option<String>,
    /// Last self-heal action taken.
    pub last_recovery: Option<RecoveryEvent>,
    /// Total endpoint rebuilds this watchdog has performed.
    pub rebuilds: u32,
    /// Peers the peer-path term looked at on its last poll.
    #[serde(default)]
    pub peer_paths_total: usize,
    /// How many of those carry an ACTIVE path (direct / relayed / mixed).
    /// `0 of N` is not by itself a fault — see `peer_paths_wedged`.
    #[serde(default)]
    pub peer_paths_active: usize,
    /// The third health term: this endpoint HELD a live path and now holds
    /// none, sustained past `peer_path_bad_streak` polls. The signal both
    /// inbound terms are blind to.
    #[serde(default)]
    pub peer_paths_wedged: bool,
    /// True while unhealthy / mid-recovery (drives the UI "Reconnecting" state).
    pub degraded: bool,
}

/// One self-heal action, for the status surface / operator timeline.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryEvent {
    /// `"relay_nudge"` | `"relay_bounce"` | `"endpoint_rebuild"`.
    pub action: String,
    /// Unix seconds when the action ran.
    pub at_unix: u64,
    /// The action succeeded.
    pub ok: bool,
}

// ─── `sovereign_mesh::mesh_http` — `/v1/mesh/status` rows ───────

/// One member row on `/v1/mesh/status`.
#[derive(Debug, Serialize, Deserialize)]
pub struct MemberDto {
    /// The member's mesh node id, display form.
    pub node_id: String,
    /// The member's display name.
    pub name: String,
    /// This row is the node that answered.
    pub is_self: bool,
    /// `"online"` | `"busy"` | `"away"` | `"offline"` — [`MemberStatus`]'s
    /// serde repr, which is how a client parses it back.
    pub status: String,
    /// Advertised total GPU VRAM (GB) summed across this member's GPUs — the
    /// live input for `svrn mesh plan --from-mesh`. `0` if the member advertises
    /// no GPU (or gossip from an older daemon that didn't carry it).
    #[serde(default)]
    pub vram_gb: u32,
    /// This member advertises itself as a shared-model anchor (an eligible
    /// tensor-split worker). `svrn mesh plan --from-mesh` places the model
    /// across the anchors + self.
    #[serde(default)]
    pub can_anchor: bool,
    /// Routable addresses (typically tailnet `host:port`) advertised
    /// by this member. Empty until the first gossip round populates
    /// them. Consumed by `sovereign mesh status` to render the per-
    /// member address row and to power `--self --addr-only` for
    /// scripting the SOVEREIGN_FOUNDER_ADDR capture pattern in
    /// pod-deployment workflows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<String>,
    /// The local origins this member serves over the mesh — `["media"]` when
    /// its `[iroh] media_origin` is live. What `svrn mesh media` (no peer)
    /// lists; empty for a daemon that predates the field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub origins: Vec<OriginKind>,
    /// The endpoint key this member is dialed on, lowercase hex. See
    /// [`MeshMember::node_pubkey`] — two ACTIVE members sharing one is the
    /// roster's identity collision, and this is the read surface that makes
    /// it visible to an operator and to the live invariant check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_pubkey: Option<String>,
    /// `removed_at.is_none()` — not a tombstone. The alias rule is scoped on
    /// this, not on `status`.
    #[serde(default = "default_true")]
    pub active: bool,
    /// Stable hash of this member's advertised hardware. Part of the
    /// measurement cache key `svrn mesh plan` builds — a throughput number is
    /// only valid on the hardware it was measured on. `None` for peers on a
    /// daemon that predates the field; `mesh plan` then reports "not measured"
    /// rather than guessing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hw_fingerprint: Option<u64>,
    /// GPU compute backend as advertised (`cuda` | `rocm` | `metal` | `vulkan`),
    /// shown beside a measurement so the reader knows which stack produced it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
}

/// One membership in the known-mesh list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownMeshDto {
    /// Hex `MeshId` — the stable handle `POST /v1/mesh/switch` takes.
    pub mesh_id: String,
    /// Display name.
    pub name: String,
    /// Roster size.
    pub members_total: usize,
    /// Exactly one entry is `true` whenever the daemon is in a mesh.
    pub is_active: bool,
    /// Newest `last_seen` across the roster — "when was this mesh last live
    /// for us", which is what a parked row wants to show.
    pub last_seen_unix: u64,
}

/// The client's read of `GET /v1/mesh/status` — the fields a client that
/// does not link the daemon reads off `sovereign_mesh::mesh_http::
/// StatusResponse`, deserialised from the SAME bytes (serde ignores the
/// rest).
///
/// NOT a relocation, and named so rather than buried: `StatusResponse`
/// closes over two runtime records that have no home at this layer —
/// `rpc_workers: Vec<worker_eligibility::WorkerStatusView>` (the
/// eligibility state machine's own view) and `iroh_transport:
/// Vec<daemon::IrohPeerPath>` (over `commonwealth_media::PeerTransportPath`,
/// cross-family). The route keeps the whole type; this is the subset a
/// client is owed. The two cannot drift silently: `sovereign-mesh`'s
/// `wire_view_drift` test serialises the real `StatusResponse` and parses
/// this from it, field by field (ARCH principle 5 — a pin with a failing
/// input you can name). A field a client needs that is not here is added
/// HERE and pinned there, never read off a second parse.
#[derive(Debug, Serialize, Deserialize)]
pub struct MeshStatusSummary {
    /// The daemon is in a mesh and serving it.
    pub running: bool,
    /// Every mesh this node is a member of — the active one and the parked
    /// ones. Empty on a daemon with persistence disabled.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub meshes: Vec<KnownMeshDto>,
    /// The active mesh's name; `None` when solo.
    pub mesh_name: Option<String>,
    /// Members the daemon judges online.
    pub members_online: usize,
    /// Every roster row.
    pub members_total: usize,
    /// The roster.
    pub members: Vec<MemberDto>,
    /// Current shareable invite. `None` when the daemon is solo, or when the
    /// persisted mesh predates the join_key.secret cache (a rotate recovers
    /// the link). The frontend hides the share card when these are absent.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub join_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    /// `sovereign://join/…` invite for the active mesh, same `None` cases as `join_key`.
    pub join_link: Option<String>,
    /// Client-API bearer token for remote callers. `Some` once the daemon is
    /// exposed (shared mesh); `None` for a loopback-only solo daemon.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub client_token: Option<String>,
    /// This NODE's OWN iroh reachability. `None` when iroh isn't running.
    /// `alias` for the one release in which a desktop and a daemon of
    /// different versions may meet (the route renamed it from
    /// `founder_reachability`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "founder_reachability"
    )]
    pub self_reachability: Option<SelfReachability>,
}

// ─── `GET /status` — the serving host's identity ─────────────────

/// The one field a client reads off `GET /status` (`commonwealth_api::
/// routes_status::StatusResponse`): the serving host's own node id, in
/// `NodeId`'s `Display` form. A client that needs the daemon's identity
/// asks the daemon; it does not read the daemon's `<data_dir>/node_id`
/// file (ARCH principle 12). Deserialised from the full answer, so the
/// rest is ignored; the field's presence and type are pinned from the
/// route's side in `sovereign-mesh`'s `wire_view_drift` test.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonIdentity {
    /// The serving host's node id (`kernel_types::NodeId`'s `Display` form).
    pub node_id: String,
}

// ─── `sovereign_mesh::mesh_discovery` — invite relay picker ─────

/// One reachable address the founder can paste into the `?relay=…`
/// query param of a sovereign:// invite when mDNS won't traverse the
/// network between them and the joiner. Answer rows of
/// `GET /v1/mesh/relay-candidates`; the desktop UI uses `kind` to
/// recommend the best one (Tailscale > LAN > IPv6).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RelayCandidate {
    /// Bare IP literal (no brackets for IPv6 — frontend formats it).
    pub ip: String,
    /// What kind of network this address is on. Drives the
    /// recommendation ordering and the human-readable label.
    /// One of: "tailscale", "lan", "ipv6", "other".
    pub kind: String,
    /// Pre-formatted `host:port` (or `[host]:port` for IPv6) ready
    /// to drop into `?relay=<value>`. Saves the UI from having to
    /// re-implement IPv6 bracket rules.
    pub url_fragment: String,
    /// True for the single best candidate the daemon would pick if
    /// asked to autoselect. Today: Tailscale > LAN > IPv6, first
    /// of its tier wins. The frontend pre-selects this in the
    /// invite-card relay picker.
    pub recommended: bool,
}
