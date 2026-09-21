// SPDX-License-Identifier: AGPL-3.0-or-later
//! Opening the route a guest link names. Split from the link FORMAT, which
//! is mesh-free and lives in `sovereign_cli_shared::guest_link`.

use sovereign_cli_shared::guest_link::GuestLink;

/// The base URL every request under `link` must be sent to — opening a mesh
/// tunnel first when the link names an iroh endpoint.
///
/// **The one decider.** Nothing else turns a [`GuestLink`] into an address:
/// a second reader that took `link.url` when a `dial` was present would send
/// a bearer in plaintext to a mesh that closed its plaintext ingress on
/// purpose.
///
/// The tunnel is parked in a process-lifetime slot rather than returned,
/// because dropping it shuts the local port and every caller here holds the
/// URL for as long as the process runs. Repeat calls reuse the one tunnel —
/// a second dial would bind a second endpoint for the same lender.
pub async fn open_route(link: &GuestLink) -> Result<String, String> {
    let Some(dial) = link.dial.as_deref() else {
        return Ok(link.url.clone());
    };
    if let Some(open) = TUNNEL.get() {
        return Ok(open.base_url().to_string());
    }
    // The guest's OWN iroh posture, not the lender's: a node that severed n0
    // discovery must not be put back on it by accepting a lend.
    let (relay_urls, discovery) = sovereign_contracts::setup_config::SetupConfig::load()
        .map(|c| (c.iroh.relay_urls.clone(), c.iroh.discovery.clone()))
        .unwrap_or_default();
    let tunnel =
        sovereign_mesh::guest_tunnel::GuestTunnel::open(dial, relay_urls, discovery.as_deref())
            .await
            .map_err(|e| {
                format!(
                    "could not reach {} over the mesh tunnel: {e}\n\
             The link names an iroh endpoint, which means the lending node's \
             plaintext API is closed (an encrypted mesh). There is no plaintext \
             fallback — ask for a fresh link, or ask them to check `svrn mesh status`.",
                    link.url
                )
            })?;
    Ok(TUNNEL.get_or_init(|| tunnel).base_url().to_string())
}

/// The one live tunnel this process holds. See [`open_route`].
static TUNNEL: std::sync::OnceLock<sovereign_mesh::guest_tunnel::GuestTunnel> =
    std::sync::OnceLock::new();

#[cfg(test)]
mod tests {
    use sovereign_cli_shared::guest_link::GuestLink;

    fn link(expires_at: u64) -> GuestLink {
        GuestLink {
            token: "t".into(),
            url: "http://box:9741".into(),
            dial: None,
            expires_at,
            summary: None,
        }
    }

    /// No dial routes to the link's own url — the plaintext arm, no network.
    #[tokio::test]
    async fn a_link_without_a_dial_routes_straight_to_its_url() {
        assert_eq!(
            super::open_route(&link(9_000)).await.unwrap(),
            "http://box:9741"
        );
    }
}
