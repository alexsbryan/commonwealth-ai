// SPDX-License-Identifier: AGPL-3.0-or-later
//! The address a phone opens, and the QR that carries it.
//!
//! Split out of `mesh_guest` when `--wall` landed: the link composer and its
//! QR renderer are one concern — "what does a scan reach" — and the two tests
//! that pin them (a decode of the actual SVG among them) are most of what the
//! parent file was carrying for them. Nothing here changed in the move.

use sovereign_mesh::deep_link::build_https_guest_link;

/// The address a phone opens: the door `--url` names, plus the page path of
/// whatever this grant reaches — the app `--rail` names, or the door's own
/// index under `--wall`, which lists every app the owner declared. Either way
/// the namespace is typed once rather than twice. A base already spelling a
/// `/ring/` page is returned as typed, never rewritten.
pub(crate) fn wall_page_base(base: &str, rail: Option<&str>, wall: bool) -> String {
    let prefix = sovereign_daemon::guest_door::PAGE_PREFIX;
    if base.contains(prefix) {
        return base.to_string();
    }
    let root = base.trim_end_matches('/');
    match rail {
        Some(ns) => format!("{root}{prefix}{ns}/"),
        None if wall => format!("{root}{prefix}"),
        None => base.to_string(),
    }
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

    /// The QR carries the door's page path for whatever this grant reaches:
    /// the app `--rail` names, or the door's index under `--wall`. A base the
    /// operator already spelled a page path into is never rewritten.
    #[test]
    fn the_wall_link_composes_the_page_path_from_the_rail() {
        let (b, pinned) = ("http://h:9", "http://h:9/ring/");
        assert_eq!(
            wall_page_base(b, Some("wall"), false),
            "http://h:9/ring/wall/"
        );
        assert_eq!(wall_page_base(pinned, Some("w"), false), pinned);
        assert_eq!(wall_page_base(b, None, false), b);
        // `--wall`: the index, which lists every declared app. One scan reaches
        // the whole wall rather than one page of it.
        assert_eq!(wall_page_base(b, None, true), "http://h:9/ring/");
        assert_eq!(wall_page_base(pinned, None, true), pinned);
    }

    /// Rasterise the SVG `wall_qr_svg` writes — its `viewBox` and one
    /// `M{x},{y}h1v1h-1` per dark module — onto a DARK surround, and decode
    /// it. The surround is the point: on an all-white canvas rqrr decodes a
    /// code with no quiet zone at all (measured), so only a page that is not
    /// white shows whether the SVG carries its own.
    #[test]
    fn the_wall_qr_svg_decodes_back_to_the_exact_link() {
        let link = build_https_guest_link(
            "0123456789abcdef0123456789abcdef",
            "http://192.168.1.10:9750/app/ring-doc/",
            1_787_900_000,
            Some("big, rail:wall"),
        );
        let svg = wall_qr_svg(&link).expect("encodes");
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
        assert_eq!(decoded, link);
    }
}
