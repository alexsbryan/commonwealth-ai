// SPDX-License-Identifier: AGPL-3.0-or-later
//! The published candidate record, the port that supplies it, and the
//! slot-alias policy both sides derive from.
//!
//! Moved from `sovereign-scheduler` (fp-1, §12 decision 3 — wire vocabulary
//! into the two owners the rules already name). The ranker's impls and its
//! fs-touching decision trace stay in the scheduler, which re-exports
//! everything here at its historical paths.

use async_trait::async_trait;
use kernel_types::NodeId;
use oicp_types::BenchmarkResult;

/// Where `serve`, the model server, listens on loopback when nobody names a
/// port — and so where the svrn daemon dials it (pb-svrn-dials-serve). ONE
/// number both programs derive from through [`serve_port`]
/// (`sovereign_serve::ServeArgs::parse`,
/// `sovereign_daemon::serve_client::default_serve_base`), as the guest door's
/// is `guest_pages::DEFAULT_GUEST_PORT`: the slot after cw-rails' 9747, outside
/// the daemon's 9741 client, 9742 internal, 9743 rail and 9744 guest door.
pub const DEFAULT_SERVE_PORT: u16 = 9748;

/// The one variable that moves serve's loopback port off
/// [`DEFAULT_SERVE_PORT`]: for a host whose own serve already holds that port,
/// such as a lift sandbox beside the deployed daemon's serve.
pub const SERVE_PORT_ENV: &str = "SOVEREIGN_SERVE_PORT";

/// Where serve listens on loopback when nobody names a port, and so where the
/// daemon dials and stops it: [`SERVE_PORT_ENV`] when set, else
/// [`DEFAULT_SERVE_PORT`]. THE one reader, which both programs call, so they
/// cannot disagree on the number (principle 8).
pub fn serve_port() -> u16 {
    resolve_serve_port(std::env::var(SERVE_PORT_ENV).ok().as_deref())
}

/// [`serve_port`] over a given value. A value that is not a nonzero port is
/// named at warn and the default used; both programs resolve it alike.
pub fn resolve_serve_port(raw: Option<&str>) -> u16 {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return DEFAULT_SERVE_PORT;
    };
    match raw.parse::<u16>() {
        Ok(port) if port != 0 => {
            tracing::debug!(target: "serving_path", port, "serve port from {SERVE_PORT_ENV}");
            port
        }
        _ => {
            tracing::warn!(
                target: "serving_path",
                value = raw,
                default = DEFAULT_SERVE_PORT,
                "{SERVE_PORT_ENV} is not a nonzero port; serve's default port is used"
            );
            DEFAULT_SERVE_PORT
        }
    }
}

/// A candidate the scheduler may rank.
///
/// Renamed from `PeerInferenceEndpoint` (registry `[[noun]]`, decided
/// 2026-09-14): the record covers the local slot and a remote lender alike, so
/// the noun is the venue, not the peer.
#[derive(Debug, Clone)]
pub struct InferenceVenue {
    pub node_id: NodeId,
    pub name: String,
    /// Candidate base URLs in try-order. Each is a
    /// `http://<ip>:9741/v1` prefix ready to hand to `RemoteApiProvider::new`.
    /// Multiple when the peer is dual-homed (WiFi + Tailscale); the wrapper
    /// tries them in order until one succeeds.
    pub base_urls: Vec<String>,
    /// Peer's gossiped `system_ram_gb`, a crude-but-correct-direction signal
    /// in the v1 routing heuristic.
    pub system_ram_gb: u32,
    /// Peer's gossiped baseline-model benchmark; `None` for older peers or one
    /// that has not completed its startup probe.
    pub benchmark: Option<BenchmarkResult>,
    /// Peer's gossiped self-reported concurrent inference count. Authoritative
    /// over the founder-local view.
    pub current_in_flight: Option<u32>,
    /// Peer's gossiped `inference_availability` (0.0–1.0; 1.0 = fully idle).
    pub inference_availability: Option<f32>,
    /// `MemberRecord::last_seen` for the gossip record the two load signals
    /// above were read from (unix seconds; `0` = unknown). The staleness half
    /// of the pair F1 measures.
    pub gossip_last_seen_unix: u64,
    /// Whether this venue is a pinned worker pod. The scheduler normalises a
    /// pinned pod's claim affinity because it has no users of its own. The
    /// transport handle itself is NOT here: the host resolves it by `node_id`.
    pub pinned_transport: bool,
}

/// The one port the roster crosses into Serving through.
///
/// The registry `[[noun]]` decided the name 2026-09-14. The two Fabric leaks
/// that rode the legacy trait — `local_node_id` and `ledger_emission_for` —
/// are gone; see `sovereign-scheduler`'s `venue` module for the narrative.
#[async_trait]
pub trait VenueSource: Send + Sync {
    /// Everything routable right now. No filtering, ranking or ordering
    /// guarantee: the scheduler does all three.
    async fn candidates(&self) -> Vec<InferenceVenue>;
}

/// Alias policy for one canonical slot role.
pub struct SlotAliasPolicy {
    /// The canonical role name (`primary`, `fast`, `embed`, `code`).
    pub role: &'static str,
    /// Extra synonyms resolvable on inbound requests, beyond the
    /// bare role (e.g. operators say "coder" where OICP says "code").
    pub synonyms: &'static [&'static str],
    /// Whether `build_self_manifest` advertises this role's aliases
    /// as mesh-routable `ProviderModel` rows. `false` is a deliberate
    /// policy decision and must carry a rationale comment on the row.
    pub mesh_advertised: bool,
}

/// The canonical table. Every alias either site knows about derives
/// from here. `sovereign-scheduler::slot_aliases` holds the
/// advertisement view and the parity tests over both.
pub const SLOT_ALIAS_POLICY: &[SlotAliasPolicy] = &[
    SlotAliasPolicy {
        role: "primary",
        synonyms: &[],
        mesh_advertised: true,
    },
    SlotAliasPolicy {
        role: "fast",
        synonyms: &[],
        mesh_advertised: true,
    },
    SlotAliasPolicy {
        role: "embed",
        // Deliberately not advertised: the embed slot is never a
        // chat-completion candidate and peer selection never consults
        // it (see `build_self_manifest`'s module doc). Local
        // resolution still wants the alias so `/v1/embeddings`-side
        // callers can address the slot by role.
        synonyms: &[],
        mesh_advertised: false,
    },
    SlotAliasPolicy {
        role: "code",
        // Deliberately not advertised AS AN ALIAS today: the code
        // slot is advertised under its concrete GGUF id (with a
        // `code` capability hint) but shares the lazy chat mutex
        // with the primary — first request pays a 5–30s hot-swap.
        // Advertising a stable `coder` alias would invite latency-
        // sensitive mesh traffic onto a cold slot. Revisit when the
        // code slot gets its own residency. NOTE: this means a peer
        // requesting literal "coder" 503s by policy — if that bites,
        // flip this to `true` and wire the advertisement block (the
        // parity test will walk you through it).
        synonyms: &["coder"],
        mesh_advertised: false,
    },
];

/// Alias keys the daemon must RESOLVE for a registered slot role:
/// the bare role + `commonwealth/<role>`, ditto for each synonym.
/// Returns empty for non-canonical roles (`primary_<i>` pool members,
/// `extras:<name>`) — those are routed by their literal key.
pub fn resolution_alias_keys(role: &str) -> Vec<String> {
    let Some(policy) = SLOT_ALIAS_POLICY.iter().find(|p| p.role == role) else {
        return Vec::new();
    };
    let mut keys = Vec::new();
    for name in std::iter::once(policy.role).chain(policy.synonyms.iter().copied()) {
        keys.push(name.to_string());
        keys.push(format!("commonwealth/{name}"));
    }
    keys
}

/// Alias ids `build_self_manifest` must ADVERTISE for a role —
/// namespaced form first (canonical), bare form second (the
/// OpenAI-client shortcut). Empty when the role isn't mesh-advertised.
pub fn advertised_alias_ids(role: &str) -> Vec<String> {
    let Some(policy) = SLOT_ALIAS_POLICY.iter().find(|p| p.role == role) else {
        return Vec::new();
    };
    if !policy.mesh_advertised {
        return Vec::new();
    }
    vec![
        format!("commonwealth/{}", policy.role),
        policy.role.to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unset or blank is the default; a port moves it; anything that is not a
    /// nonzero port is the default, never a listener on an ephemeral port.
    #[test]
    fn serve_port_resolves_the_override_or_the_default() {
        assert_eq!(resolve_serve_port(None), DEFAULT_SERVE_PORT);
        assert_eq!(resolve_serve_port(Some("  ")), DEFAULT_SERVE_PORT);
        assert_eq!(resolve_serve_port(Some("18748")), 18748);
        assert_eq!(resolve_serve_port(Some("0")), DEFAULT_SERVE_PORT);
        assert_eq!(resolve_serve_port(Some("serve")), DEFAULT_SERVE_PORT);
    }
}
