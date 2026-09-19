//! The live media route — see [`MediaRoute`].

use std::net::SocketAddr;

/// `[iroh] media_origin` and `[iroh] media_allow`, held LIVE: the daemon owns
/// one (as it owns its [`commonwealth_media::PublishedApps`]), boot seeds it,
/// `svrn daemon reload` replaces it, and the acceptor, the ALPN set and the
/// gossip stamp read it per use. Read once at acceptor build, a changed offer
/// cost a daemon restart, and the restart left peers dialing the holder's
/// endpoint into a 120 s timeout (ring-room 99ca7e4cb leg 3).
#[derive(Clone, Default)]
pub struct MediaRoute {
    state: std::sync::Arc<std::sync::RwLock<(Option<SocketAddr>, std::sync::Arc<Vec<String>>)>>,
    /// Headers this node adds to requests reaching its OWN media origin, from
    /// `<data_dir>/secrets/media/` (0600 files, `commonwealth_media::declared`)
    /// — how a holder authenticates to its own Jellyfin without any viewer
    /// holding its key. Re-read at acceptor build and by a reload that moves
    /// the origin, so `declare` then `offer` needs no restart.
    declared: std::sync::Arc<std::sync::RwLock<std::sync::Arc<Vec<(String, String)>>>>,
    /// `[iroh] media_viewer_user` — the origin's own id for the read-only
    /// account every member reaches this library as. Held here beside the
    /// origin and the credential because the presence poll needs all three
    /// per tick and a reload moves them together. `None` until an offer has
    /// created one, which the poll reads as "cannot tell the holder from the
    /// house" and publishes as no presence at all.
    viewer_user: std::sync::Arc<std::sync::RwLock<Option<String>>>,
    hook: std::sync::Arc<std::sync::Mutex<Option<std::sync::Arc<dyn Fn() + Send + Sync>>>>,
}

impl std::fmt::Debug for MediaRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaRoute")
            .field("origin", &self.origin())
            .field("allow", &self.allow())
            .finish()
    }
}

impl MediaRoute {
    /// A route holding `origin` and `allow` — the daemon's seed, and a test's.
    pub fn fixed(origin: Option<SocketAddr>, allow: Vec<String>) -> Self {
        let route = Self::default();
        route.set(origin, allow);
        route
    }

    /// The ONE parse of the two config keys, shared by boot and reload. An
    /// origin that is not a host:port is refused by name, never dropped: a
    /// library silently not served is the §18.3 substitution.
    pub fn parse(
        origin: Option<&str>,
        allow: &[String],
    ) -> Result<(Option<SocketAddr>, Vec<String>), String> {
        let origin = match origin {
            None => None,
            Some(raw) => Some(raw.parse().map_err(|e| {
                format!(
                    "[iroh] media_origin = \"{raw}\" is not a host:port ({e}) — a node that \
                     cannot parse what it would serve must not pretend to serve it"
                )
            })?),
        };
        Ok((origin, allow.to_vec()))
    }

    /// Replace both keys at once, then tell the acceptor so it re-decides
    /// whether `cwth/media/0` is advertised.
    pub fn set(&self, origin: Option<SocketAddr>, allow: Vec<String>) {
        tracing::info!(
            target: "transport",
            media_origin = ?origin,
            media_allow = ?allow,
            "iroh(mesh): media route set — the next dial and the next gossip stamp read it"
        );
        *self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            (origin, std::sync::Arc::new(allow));
        let hook = self
            .hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(hook) = hook {
            hook();
        }
    }

    /// The origin a member's media dial is forwarded to right now.
    pub fn origin(&self) -> Option<SocketAddr> {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0
    }

    /// Who may reach it right now, by name or id prefix. Empty = every member.
    pub fn allow(&self) -> std::sync::Arc<Vec<String>> {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .1
            .clone()
    }

    /// Replace the declared headers (never logged: they are credentials).
    pub fn set_declared(&self, declared: Vec<(String, String)>) {
        tracing::info!(
            target: "transport",
            declared = declared.len(),
            "iroh(mesh): media declarations read — the next media dial carries them"
        );
        *self
            .declared
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = std::sync::Arc::new(declared);
    }

    /// The declared headers the next media dial carries.
    pub fn declared(&self) -> std::sync::Arc<Vec<(String, String)>> {
        self.declared
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Replace the viewer account the presence poll reads sessions against.
    /// The id is not a credential and is logged.
    pub fn set_viewer_user(&self, viewer_user: Option<String>) {
        tracing::info!(
            target: "transport",
            ?viewer_user,
            "iroh(mesh): media viewer account set — the next presence poll reads it"
        );
        *self
            .viewer_user
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = viewer_user;
    }

    /// The viewer account the next presence poll reads sessions against.
    pub fn viewer_user(&self) -> Option<String> {
        self.viewer_user
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn on_change(&self, hook: std::sync::Arc<dyn Fn() + Send + Sync>) {
        *self
            .hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(hook);
    }
}
