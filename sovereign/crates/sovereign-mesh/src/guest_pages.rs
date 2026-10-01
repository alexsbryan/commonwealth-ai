// SPDX-License-Identifier: AGPL-3.0-or-later
//! The guest page's browser side, shared by the door and the dev servers —
//! where the page lives on the door and what `window.ring` speaks. The
//! guard every page bundle is served behind is `host_kit::shell::serve_under`.
//!
//! The shim (`ring_shim`, `RING_SHIM`) moved on to
//! `sovereign_contracts::guest_pages` (pb-mesh-exit-mesh), so the door, `svrn
//! ring dev` and the meshapp dev server reach one implementation without this
//! crate; every name stays reachable here until pb-mesh-dissolve.

/// Where the door serves the ring page. Defined once in
/// `sovereign_contracts::guest_pages` — the daemon, the CLI and this crate
/// must agree on it — and re-exported here so the page surface names it
/// beside the functions that serve under it.
pub use sovereign_contracts::guest_pages::PAGE_PREFIX;

/// Moved to `sovereign_contracts::guest_pages` (pb-mesh-exit-mesh);
/// re-exported at their historical path.
pub use sovereign_contracts::guest_pages::{ring_shim, RING_SHIM};
