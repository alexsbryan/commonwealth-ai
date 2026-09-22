// SPDX-License-Identifier: AGPL-3.0-or-later
//! The address a phone opens, and the QR that carries it.
//!
//! Split out of `mesh_guest` when `--wall` landed. The LINK COMPOSER moved on
//! again, to `sovereign_mesh::deep_link::wall_page_base` / `wall_https_link`,
//! when the daemon needed the same rule to return a grant's link (2026-09-22 —
//! one composer for the CLI, the daemon, and the desktop that reads the
//! daemon's link). What stays here is the QR RENDERER and the BASE the link is
//! composed with: the door names the PORT, this machine names the ADDRESS — so
//! nobody types an address, and a wildcard or loopback bind is never
//! advertised.

use sovereign_contracts::guest_pages::DEFAULT_GUEST_PORT;

/// The port `[daemon] guest_bind` names, when it names one.
pub(crate) fn port_of_bind(bind: &str) -> Option<u16> {
    bind.trim()
        .rsplit_once(':')
        .and_then(|(_, p)| p.trim().trim_end_matches('/').parse().ok())
}

/// The base a guest link should carry: **the address derived, the port from
/// the door**.
///
/// `bind` is `[daemon] guest_bind` (a LISTEN address) and `best` is this
/// machine's best advertised ip (`relay_candidates`: tailscale, then LAN, then
/// IPv6 — the ranking the invite already uses). A concrete bind wins, because
/// the operator declared a reachable address. A wildcard or loopback bind is a
/// listen address and must never be advertised; it falls back to `best` on the
/// bind's port. With no bind at all there is nothing to derive — `None`, never
/// an invented address.
///
/// `None` for `best` when the wildcard fallback is needed is also `None`: "no
/// network detected" is absence, and a link carrying `0.0.0.0` or `127.0.0.1`
/// would be a plausible-looking, dead result.
pub(crate) fn advertised_base(bind: Option<&str>, best: Option<&str>) -> Option<String> {
    let b = bind.map(str::trim).filter(|b| !b.is_empty())?;
    let (host, port) = b.rsplit_once(':')?;
    let host = host.trim().trim_matches(['[', ']']);
    let port = port.trim();
    if port.is_empty() {
        return None;
    }
    let listen_only = host.is_empty()
        || host == "0.0.0.0"
        || host == "::"
        || host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false);
    let advertised = if listen_only { best?.trim() } else { host };
    Some(format!("http://{advertised}:{port}"))
}

/// `[daemon] guest_bind` as the base a guest link should carry — written by
/// `svrn ring serve --bind`, and DERIVED from this machine's addresses when the
/// bind is wildcard (see [`advertised_base`]). This is why nobody types an
/// address: the door names the port, the machine names the address. `--url`
/// remains the override for a page served from somewhere else (the static
/// origin).
pub(crate) fn guest_bind_url() -> Option<String> {
    let cfg = sovereign_core::setup_config::SetupConfig::load().ok()?;
    let bind = cfg.daemon.guest_bind.as_deref();
    let port = bind.and_then(port_of_bind).unwrap_or(DEFAULT_GUEST_PORT);
    let best = sovereign_mesh::mesh_discovery::relay_candidates(port)
        .into_iter()
        .find(|c| c.recommended)
        .or_else(|| {
            sovereign_mesh::mesh_discovery::relay_candidates(port)
                .into_iter()
                .next()
        })
        .map(|c| c.ip);
    advertised_base(bind, best.as_deref())
}

/// The QR module margin, in modules. Four is the quiet zone the QR standard
/// requires; a scanner cannot find the finder patterns without it.
const QR_QUIET_ZONE: usize = 4;

/// Render `link` as an SVG QR code, square modules, with the standard quiet
/// zone.
pub(crate) fn wall_qr_svg(link: &str) -> Result<String, String> {
    use fast_qr::convert::{svg::SvgBuilder, Builder};
    let qr = fast_qr::QRBuilder::new(link)
        .build()
        .map_err(|e| format!("cannot encode the link as a QR code: {e}"))?;
    Ok(SvgBuilder::default().margin(QR_QUIET_ZONE).to_str(&qr))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_mesh::deep_link::build_https_guest_link;

    /// The rule this module exists for: a concrete bind is advertised as
    /// written; a wildcard or loopback bind (a LISTEN address) is replaced by
    /// this machine's best candidate on the door's port; and with nothing to
    /// derive — no bind, or no candidate while one is needed — the answer is
    /// absence, never `0.0.0.0`/`127.0.0.1` in a link.
    #[test]
    fn the_advertised_base_is_derived_and_never_a_listen_address() {
        let concrete = Some("192.168.1.20:19947");
        let best = Some("100.64.0.9");
        assert_eq!(
            advertised_base(concrete, best).as_deref(),
            Some("http://192.168.1.20:19947")
        );
        for listen_only in ["0.0.0.0:19947", "127.0.0.1:19947", "[::1]:19947"] {
            assert_eq!(
                advertised_base(Some(listen_only), best).as_deref(),
                Some("http://100.64.0.9:19947"),
                "{listen_only} must not be advertised"
            );
        }
        // A wildcard bind with no candidate is absence, not a dead address.
        assert_eq!(advertised_base(Some("0.0.0.0:19947"), None), None);
        // Nothing declared, and nothing to derive from.
        assert_eq!(advertised_base(None, best), None);
        assert_eq!(advertised_base(Some(""), best), None);
        assert_eq!(advertised_base(Some("no-port"), best), None);
    }

    #[test]
    fn the_door_port_comes_from_the_bind_or_the_default() {
        assert_eq!(port_of_bind("10.0.0.5:19947"), Some(19947));
        assert_eq!(port_of_bind("0.0.0.0:9743"), Some(9743));
        assert_eq!(port_of_bind("nonsense"), None);
        assert_eq!(DEFAULT_GUEST_PORT, 9743);
    }

    /// The QR carries the door's page path for whatever this grant reaches:
    /// the app `--rail` names, or the door's index under `--wall`. A base the
    /// operator already spelled a page path into is never rewritten.
    /// Rasterise the SVG `wall_qr_svg` writes — its `viewBox` and one
    /// `M{x},{y}h1v1h-1` per dark module — onto a DARK surround, and decode
    /// it. The surround is the point: on an all-white canvas rqrr decodes a
    /// code with no quiet zone at all (measured), so only a page that is not
    /// white shows whether the SVG carries its own.
    fn decode_qr_svg(svg: &str) -> String {
        let n: usize = svg
            .split_once(r#"viewBox="0 0 "#)
            .and_then(|(_, rest)| rest.split(' ').next())
            .and_then(|s| s.parse().ok())
            .expect("a viewBox");
        let mut dark = vec![false; n * n];
        for cmd in svg.split('M').skip(1) {
            let Some((xy, _)) = cmd.split_once('h') else {
                continue;
            };
            let Some((x, y)) = xy.split_once(',') else {
                continue;
            };
            let (x, y): (usize, usize) = (x.parse().unwrap(), y.parse().unwrap());
            dark[y * n + x] = true;
        }
        const PX: usize = 4;
        const SURROUND: usize = 4;
        let side = (n + 2 * SURROUND) * PX;
        let mut img = rqrr::PreparedImage::prepare_from_greyscale(side, side, |x, y| {
            let (mx, my) = (x / PX, y / PX);
            let inside =
                (SURROUND..SURROUND + n).contains(&mx) && (SURROUND..SURROUND + n).contains(&my);
            if !inside || dark[(my - SURROUND) * n + (mx - SURROUND)] {
                0
            } else {
                255
            }
        });
        let grids = img.detect_grids();
        assert_eq!(grids.len(), 1, "exactly one QR code found in the SVG");
        let (_, decoded) = grids[0].decode().expect("decodes");
        decoded
    }

    #[test]
    fn the_wall_qr_svg_decodes_back_to_the_exact_link() {
        let link = build_https_guest_link(
            "0123456789abcdef0123456789abcdef",
            "http://192.168.1.10:9750/app/ring-doc/",
            1_787_900_000,
            Some("big, rail:wall"),
            None,
            None,
        );
        assert_eq!(decode_qr_svg(&wall_qr_svg(&link).expect("encodes")), link);
    }

    /// The QR carries the marks too: a 3-actor `at=` link (64-hex actor ids,
    /// realistic size) plus an `iroh=` dial string still encodes — measured
    /// at QR v17 on fast_qr 0.14.0 defaults — and decodes back to the exact
    /// link.
    #[test]
    fn the_wall_qr_svg_encodes_a_three_actor_at_link() {
        let at = format!(
            "{}:1,{}:2,{}:3",
            "a".repeat(64),
            "b".repeat(64),
            "c".repeat(64)
        );
        let link = build_https_guest_link(
            "0123456789abcdef0123456789abcdef",
            "http://192.168.1.10:9750/app/ring-doc/",
            1_787_900_000,
            Some("big, rail:wall"),
            Some(&at),
            Some("3b1f0a@https://relay.example:443/,192.168.1.10:41234"),
        );
        assert_eq!(decode_qr_svg(&wall_qr_svg(&link).expect("encodes")), link);
    }
}
