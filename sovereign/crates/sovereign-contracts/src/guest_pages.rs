// SPDX-License-Identifier: AGPL-3.0-or-later
//! `[daemon.guest_pages]` — the apps a wall's owner has DECLARED admit guests.
//!
//! # The resource declares, the credential identifies
//!
//! This system already has the model: a recipe declares `mesh_sharing` per
//! corpus at registration, and membership then reaches what was declared
//! shared — nobody lists corpora on a membership. Guests get the same shape.
//! An app registered here has declared it admits guests; a wall grant
//! (`svrn mesh grant --wall`) reaches every namespace so declared at this door
//! and nothing else on the rail. `--rail <ns>` stays as the narrowing knob for
//! an owner who wants one link to reach exactly one app.
//!
//! ```toml
//! [daemon.guest_pages]
//! ring-doc = "/srv/ring-doc"
//! house-expenses = { dir = "/srv/house", guests = "read" }
//! ```
//!
//! # What this module is NOT
//!
//! It is the declaration's *shape*, not its authority. Which namespaces a
//! daemon owns and may therefore never declare guest-open is
//! `sovereign_mesh::ring_roster::is_daemon_owned`, and the one reader that
//! turns these entries into the door's page surface is
//! `sovereign_daemon::guest_door::GuestPages::from_config`. Keeping the type
//! here and both decisions there is what stops a second answer growing next to
//! either (ARCH §10.6).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Where a guest door serves the ring page: the path every guest link's base
/// carries. `http://<guest_bind>/ring/#token=…` for a wall holding one app,
/// `…/ring/<namespace>/#token=…` for one of several — and the same path a
/// static origin serves the browser runtime from (svg the guest link composes,
/// `sovereign_mesh::deep_link::wall_page_base`).
///
/// It lives here, with the page contract, because THREE crates need to agree
/// on it and the only one they all depend on is this leaf: the daemon serves
/// it, the CLI composes links with it, and the mesh crate composes the same
/// links for the daemon's grant responses. It was `sovereign_daemon`'s, which
/// the mesh crate may not depend on (`[[forbid]] sovereign-mesh ->
/// sovereign-daemon`); the move is 2026-09-22 and the daemon re-exports it, so
/// every existing name still resolves.
pub const PAGE_PREFIX: &str = "/ring/";

/// Where the door listens when nobody names a port. NOT `9743`: the rail
/// listener owns `client_port + 2`, so a default-config daemon already holds
/// it (measured 2026-09-22 — `ring host` would have collided with its own
/// rail). This is the slot after that: `9741` client, `9742` internal, `9743`
/// rail, `9744` guest door. A door bound with no port named binds
/// `0.0.0.0:<this>`; the address a guest link carries is still DERIVED per
/// host (never the wildcard) — see `sovereign_mesh::deep_link::advertised_base`.
pub const DEFAULT_GUEST_PORT: u16 = 9744;

/// What guests may do on a registered page's rail namespace.
///
/// A closed set, and that IS the property (ARCH §9): there is nothing between
/// "read" and "read and write", so a narrowing an operator can spell is one
/// the route can decide on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GuestAccess {
    /// Guests read the journal and append to it — what registering a page has
    /// always meant, and the default when the entry is a bare path.
    #[default]
    Write,
    /// Guests read the journal; their append is refused by name. The page is
    /// still served: an app the room may only look at is still an app.
    Read,
}

impl GuestAccess {
    /// The word an operator wrote, so a refusal can quote the config back.
    pub fn as_str(self) -> &'static str {
        match self {
            GuestAccess::Write => "write",
            GuestAccess::Read => "read",
        }
    }
}

/// One `[daemon.guest_pages]` entry: where the bundle is, and what guests may
/// do there.
///
/// Two spellings of one entry, because the common case should cost one line —
/// `ring-doc = "/srv/ring-doc"` is the whole declaration for an app guests
/// read and write, and the table form exists only for the narrowing. Untagged,
/// so TOML decides by shape rather than by a discriminant nobody would want to
/// type; a table carrying an unspellable `guests` value matches neither arm
/// and is refused, which is the right direction for a config that governs
/// reach (ARCH §18.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GuestPage {
    /// `ring-doc = "/srv/ring-doc"` — guests read and write.
    Open(PathBuf),
    /// `ring-doc = { dir = "/srv/ring-doc", guests = "read" }`.
    Narrowed {
        /// The bundle directory.
        dir: PathBuf,
        /// What guests may do. Absent is [`GuestAccess::Write`], which is what
        /// the bare-path form means — the two spellings agree on the default
        /// rather than each having one.
        #[serde(default)]
        guests: GuestAccess,
    },
    /// `my-doc = { proxy = "127.0.0.1:4318" }` — an app **already running** on
    /// this machine, of any kind, reached through the door.
    ///
    /// This is the answer to "anything I can develop here, I can share": the
    /// door reverse-proxies to the port, so the app keeps being the app — hot
    /// reload, its own framework, its own dev server — and the room reaches it
    /// by scanning the same one QR. No bundle, no rail, and no knowledge of
    /// what the app is (`sovereign/apps/ring-runtime`'s opposite: that one is
    /// a page the door ships; this one is a page you are editing).
    ///
    /// A rail-backed app served this way still gets guest attribution: the
    /// door answers the dev shim's path with the GUEST shim, so the page's
    /// rail calls come back to the door and carry the guest's session rather
    /// than the app's own member token.
    Proxied {
        /// `host:port` on this machine — loopback or the machine's own
        /// address; the door dials it, so it is never a public target.
        proxy: String,
        /// What guests may do on the rail, as above. For an app with no rail
        /// this is inert.
        #[serde(default)]
        guests: GuestAccess,
    },
}

impl GuestPage {
    /// The bundle directory the door serves for this namespace. Only
    /// meaningful for a bundle page; a [`GuestPage::Proxied`] one has none
    /// (ask [`Self::proxy`]).
    pub fn dir(&self) -> &Path {
        match self {
            GuestPage::Open(dir) => dir,
            GuestPage::Narrowed { dir, .. } => dir,
            GuestPage::Proxied { .. } => Path::new(""),
        }
    }

    /// The `host:port` the door proxies to, for a [`GuestPage::Proxied`] page.
    pub fn proxy(&self) -> Option<&str> {
        match self {
            GuestPage::Proxied { proxy, .. } => Some(proxy),
            _ => None,
        }
    }

    /// What guests may do here.
    pub fn guests(&self) -> GuestAccess {
        match self {
            GuestPage::Open(_) => GuestAccess::default(),
            GuestPage::Narrowed { guests, .. } => *guests,
            GuestPage::Proxied { guests, .. } => *guests,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize)]
    struct Wrap {
        pages: std::collections::BTreeMap<String, GuestPage>,
    }

    /// The one-line form is the default posture, spelled as a bare path.
    #[test]
    fn a_bare_path_means_guests_read_and_write() {
        let w: Wrap = toml::from_str(
            r#"[pages]
ring-doc = "/srv/ring-doc""#,
        )
        .expect("bare path parses");
        let e = &w.pages["ring-doc"];
        assert_eq!(e.dir(), Path::new("/srv/ring-doc"));
        assert_eq!(e.guests(), GuestAccess::Write);
    }

    /// The table form narrows, and a table with no `guests` key means the same
    /// thing the bare path does — one default, not two.
    #[test]
    fn the_table_form_narrows_and_defaults_to_the_same_posture() {
        let w: Wrap = toml::from_str(
            r#"[pages.a]
dir = "/srv/a"
guests = "read"

[pages.b]
dir = "/srv/b""#,
        )
        .expect("table form parses");
        assert_eq!(w.pages["a"].guests(), GuestAccess::Read);
        assert_eq!(w.pages["a"].dir(), Path::new("/srv/a"));
        assert_eq!(w.pages["b"].guests(), GuestAccess::Write);
    }

    /// A `guests` value nothing can spell is REFUSED, never read as the
    /// permissive default — an operator who asked for read-only and silently
    /// got read-write would never find out (ARCH §18.3).
    #[test]
    fn an_unspellable_guests_value_is_refused() {
        let parsed = toml::from_str::<Wrap>(
            r#"[pages.a]
dir = "/srv/a"
guests = "readonly""#,
        );
        assert!(
            parsed.is_err(),
            "a misspelled mode parsed as {:?} instead of being refused",
            parsed.map(|w| w.pages["a"].guests())
        );
    }

    /// A page can be an app already RUNNING on this machine, not only a bundle
    /// the door ships. The table form keeps it unambiguous against the path
    /// forms: a bare string is still a directory, and a `proxy` key is not a
    /// `dir` key.
    #[test]
    fn a_proxied_page_names_a_port_and_is_not_mistaken_for_a_bundle() {
        let w: Wrap = toml::from_str(
            r#"[pages]
bundle = "/srv/bundle"
my-doc = { proxy = "127.0.0.1:4318" }
read-only-app = { proxy = "127.0.0.1:5173", guests = "read" }"#,
        )
        .expect("proxy entries parse");
        assert_eq!(w.pages["bundle"].proxy(), None);
        assert_eq!(w.pages["my-doc"].proxy(), Some("127.0.0.1:4318"));
        assert_eq!(w.pages["my-doc"].guests(), GuestAccess::Write);
        assert_eq!(w.pages["read-only-app"].guests(), GuestAccess::Read);
        // And a proxy entry has no directory to serve.
        assert_eq!(w.pages["my-doc"].dir(), Path::new(""));
    }
}
