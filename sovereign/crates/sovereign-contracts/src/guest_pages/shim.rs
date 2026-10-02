// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ring page's browser side: `window.ring` for a namespace. Moved from
//! `sovereign_mesh::guest_pages` (pb-mesh-exit-mesh) so the door, `svrn ring
//! dev` and the meshapp dev server name it without the mesh crate; the shim
//! source is data (`ring_shim.js`), and sovereign-mesh re-exports both items
//! at their historical path until pb-mesh-dissolve.

/// `window.ring` for `namespace`, over one of two transports.
///
/// `rail_base: None` is `svrn ring dev`: every op is POSTed to the dev
/// server's `/__ring/<op>`, which holds the grant, so the browser never sees
/// one. `Some(base)` is the guest door: the page calls the rail routes at
/// `base` itself, presenting the bearer it reads from the URL FRAGMENT —
/// which a browser never sends to a server. One source, both callers.
pub fn ring_shim(namespace: &str, rail_base: Option<&str>) -> String {
    let base = rail_base.map_or_else(
        || "null".to_string(),
        |b| serde_json::Value::from(b).to_string(),
    );
    RING_SHIM
        .replace("{{NAMESPACE}}", namespace)
        .replace("{{RAIL_BASE}}", &base)
}

/// `window.ring` — the whole client surface, and it is small on purpose.
///
/// **It ships the fold, not just the transport.** `log()` and `record()` are
/// the two routes; `fold()` is the third thing, and it is the reason this is
/// an SDK rather than a fetch wrapper. The rail computes the order and the
/// void set server-side, and `fold` is what makes an app author consume that
/// rather than re-derive it: they write a reducer over one act at a time and
/// never touch `log.ops` directly. Hand somebody a raw log and hope, and the
/// first thing they write is `ops.filter(...).sort(...)` — and their house
/// disagrees with itself about who owes what.
///
/// `live` is the fourth thing and it is a different kind: it writes nothing
/// down, so it is namespaced apart rather than sitting beside `record`.
///
/// Public (the source, before the per-namespace placeholders are filled) so
/// the door's pin tests read the exact text a page is served.
pub const RING_SHIM: &str = include_str!("ring_shim.js");

#[cfg(test)]
mod tests {
    /// The dev transport's op URL resolves against the PAGE, never the
    /// origin root: a member reaches `ring show` through the app bridge under
    /// `/<app>/`, and a root-absolute `/__ring/` would post to the bridge
    /// root instead of the app (81b0558f1).
    #[test]
    fn the_dev_shim_posts_ops_relative_to_the_page() {
        assert!(super::RING_SHIM.contains("fetch('__ring/' + op"));
        assert!(!super::RING_SHIM.contains("fetch('/__ring/"));
    }
}
