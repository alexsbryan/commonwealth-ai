// SPDX-License-Identifier: AGPL-3.0-or-later
//! `[iroh]` and `[iroh.transport]` — dial-by-key mesh access, the three
//! origins this node serves to members, and which traffic class rides which
//! transport.
//!
//! Extracted from `setup_config.rs` on 2026-09-13, adding `offer_origin`:
//! that file was 47 lines past ARCH §3.1's ceiling before this order touched
//! it, and a fourth origin key was where its next line would have gone. Pure
//! data either way — every reader still spells these
//! `sovereign_core::setup_config::IrohSection`, because `setup_config`
//! re-exports them, so the move changed no import and no behaviour.
//!
//! Why THIS section and not a different 240 lines: `[iroh]` is the one block
//! that grows with each origin kind (a key and an allow-list per kind), so it
//! is the block with a reason to move. The rest of the file is stable.

use serde::{Deserialize, Serialize};

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
    /// `[iroh] media_origin` — a local HTTP media server that MEMBERS of this
    /// mesh may reach over iroh, as `host:port`.
    ///
    /// Absent (the default) means this node serves no media and does not
    /// advertise the protocol at all, so a dial is closed rather than hanging.
    /// Present means the daemon's acceptor forwards `MEDIA_ALPN` to it for a
    /// dialer the roster carries — no VPN, no port-forward, no public exposure:
    /// the origin stays bound to loopback and the only way in is a mesh key.
    ///
    /// It is the OPERATOR's declaration, like `[compute.work_offer] image`: this
    /// repository ships no media server and must not guess at one. A value that
    /// does not parse as a socket address refuses the boot rather than being
    /// dropped (§18.3).
    ///
    /// ```toml
    /// [iroh]
    /// enabled = true
    /// media_origin = "127.0.0.1:8096"   # Jellyfin's default
    /// ```
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_origin: Option<String>,
    /// `[iroh] media_allow` — which MEMBERS may reach `media_origin`, each by
    /// member name or a node-id prefix of at least four characters, as `svrn
    /// mesh status` shows them. Empty (the default) admits every member, as
    /// before; a non-member is refused regardless of this list. Checked in the
    /// acceptor where the dialer's key was verified, so it is a list of
    /// identities the mesh has gossiped — never of addresses, and never a
    /// header a client could have typed. The origin itself is handed the
    /// admitted member's name and node id on every request (`X-Mesh-Member`,
    /// `X-Mesh-Node`), so a server that authenticates nothing can still map a
    /// member to one of its own users.
    ///
    /// ```toml
    /// [iroh]
    /// media_origin = "127.0.0.1:8096"
    /// media_allow = ["LittleMac", "node-44ae7614"]
    /// ```
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media_allow: Vec<String>,
    /// `[iroh.apps]` — the HTTP apps this node publishes to members BY NAME,
    /// each `name = "host:port"` on loopback. Served on one ALPN
    /// (`cwth/app/0`) and demultiplexed by the request's first path segment,
    /// so `GET /chores/tasks` reaches `chores` as `GET /tasks`.
    ///
    /// This is the OPEN half of the origin design: the kind is a closed enum
    /// with one variant, and which apps exist is data that changes without a
    /// code change (ARCH §9). A name is `[A-Za-z0-9_-]+`; anything else is
    /// refused at the acceptor rather than sanitized, so the name can never
    /// express traversal.
    ///
    /// ```toml
    /// [iroh.apps]
    /// chores = "127.0.0.1:5000"
    /// printer = "127.0.0.1:8080"
    /// ```
    ///
    /// Config is the DURABLE tier and is deliberately not the only one: a
    /// declaration you have to maintain is worth it for something that is
    /// always up, and is exactly wrong for a thing somebody ran at 1am. A
    /// config entry nobody deletes becomes a `failed: connection refused`
    /// row months later, which is the rot the closure-loop rule names — the
    /// ephemeral registration tiers (`svrn run --as`, a TTL'd POST) are the
    /// default and land beside this, not instead of it.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub apps: std::collections::BTreeMap<String, String>,
    /// `[iroh] app_allow` — which MEMBERS may reach `[iroh.apps]`, by the same
    /// name-or-id-prefix rule as `media_allow`. Empty (the default) admits
    /// every member.
    ///
    /// A SEPARATE list from `media_allow`, which is the practical reason
    /// `App` is its own origin kind: admitting a housemate to your chore app
    /// is not the same decision as admitting them to your film library. One
    /// shared list would make the narrower grant inexpressible, and a house
    /// that cannot say "everyone sees the print queue, two people see my
    /// films" says yes to everything instead.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub app_allow: Vec<String>,
    /// `[iroh] offer_origin` — a local HTTP server listing what this
    /// operator has to SELL or LEND, as `host:port`, which MEMBERS of this
    /// mesh may reach over iroh.
    ///
    /// Absent (the default) means this node publishes no offers and does not
    /// advertise the protocol at all, so a dial is closed rather than left
    /// hanging. Present means the acceptor forwards `OFFER_ALPN` to it for a
    /// dialer the roster carries — and `svrn mesh offers` on any member's
    /// machine then shows what is here, with nobody holding a credential of
    /// this node's.
    ///
    /// **What it serves is entirely yours.** This repository ships no
    /// catalogue server, defines no item schema, and merges nothing: the
    /// mesh's catalogue is COMPUTED by asking every publisher at once and
    /// returning a row each, so there is no stored listing to be excluded
    /// from and nobody positioned to rank
    /// (`docs/internal/RING_APPLICATIONS.md` §Commerce). A static JSON file
    /// behind `python3 -m http.server` is a legitimate offer origin.
    ///
    /// Its own key rather than an `[iroh.apps]` entry named `offers`,
    /// because it is its own TRUST class — see `offer_allow`.
    ///
    /// A value that does not parse as a socket address refuses the boot
    /// rather than being dropped, exactly as `media_origin` does: a node that
    /// cannot parse what it would serve must not boot pretending to serve it.
    ///
    /// ```toml
    /// [iroh]
    /// enabled = true
    /// offer_origin = "127.0.0.1:8710"
    /// ```
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offer_origin: Option<String>,
    /// `[iroh] offer_allow` — which MEMBERS may reach `offer_origin`, by the
    /// same name-or-id-prefix rule as `media_allow`. Empty (the default)
    /// admits every member; a non-member is refused regardless.
    ///
    /// A THIRD list, and that is the whole reason `Offer` is its own origin
    /// kind. "Everyone in the house may see what I have going spare" and
    /// "two people may reach my chore app" are different grants; one list for
    /// both makes the narrower one inexpressible, and a house that cannot say
    /// so says yes to everything instead.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub offer_allow: Vec<String>,
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
