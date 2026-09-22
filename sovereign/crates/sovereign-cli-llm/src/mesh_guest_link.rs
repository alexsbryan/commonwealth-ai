// SPDX-License-Identifier: AGPL-3.0-or-later
//! The address a phone opens, and the QR that carries it.
//!
//! Split out of `mesh_guest` when `--wall` landed. The LINK COMPOSER moved on
//! again, to `sovereign_mesh::deep_link::wall_page_base` / `wall_https_link`,
//! when the daemon needed the same rule to return a grant's link (2026-09-22 —
//! one composer for the CLI, the daemon, and the desktop that reads the
//! daemon's link). What stays here is the QR RENDERER: the CLI writes the SVG;
//! the desktop draws its own from the link.

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
