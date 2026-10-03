// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`ClientSurface`] — who reaches a bind of the client router.
//!
//! Split out of `server.rs` because it is a different job from building a
//! router: this is the trust-posture decider, and `server.rs` is the assembly
//! that reads it. Together they put `server.rs` over the 1200-line bar
//! (ARCH §3.1), and a security-load-bearing closed set was buried in the
//! middle of a mounting function.
//!
//! The type is one enum and five total `match`es over it. That is the point:
//! adding a principal class breaks every one of them, so a new surface cannot
//! inherit a posture by silence.

use crate::client_auth::ClientAuthPolicy;

/// Who reaches a bind of the client router — the closed set of principal
/// classes, and the ONE thing that decides both the auth posture and the
/// route set (§2.1, §10.6).
///
/// The daemon binds this router three times, and the binds differ in two
/// ways that used to be tracked separately (and so drifted apart):
///
/// | Surface | Reached by | Trusts a loopback peer | Serves `/v1/*` | Serves `/internal/*` |
/// |---|---|---|---|---|
/// | `Operator` | a real local caller on `:9741` | yes | yes | yes |
/// | `Guest` | `GUEST_ALPN`, a non-member on `CLIENT_ALPN`, and the guest door | no | yes | no |
/// | `Rail` | a deployed ring app, on its own loopback bind | no | **no** | no |
///
/// cw-rails forwards to the `Guest` bind over loopback, so every tunnelled
/// request wears a loopback address it did not earn: `Guest` refuses to
/// trust the address. A MEMBER dialling `CLIENT_ALPN` reaches serve's member
/// client, never this router (pb-mesh-exit-transport), so no bind here serves
/// a member the general client routes.
///
/// The principal class picks the listener, and the listener does not SERVE
/// what that principal must never reach. See `routes_internal::guest_grant`
/// module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientSurface {
    /// The daemon's own `:9741` listener.
    Operator,
    /// The loopback bind svrn registers with cw-rails on `GUEST_ALPN`, where
    /// cw-rails also sends a non-member dialling `CLIENT_ALPN`.
    Guest,
    /// The loopback bind a deployed ring app talks to. Serves the rail
    /// routes and NOTHING else.
    ///
    /// A ring app runs on this machine, so its loopback address is real —
    /// and worth exactly nothing, because the operator's own desktop shares
    /// it. It is a separate principal that presents a grant, and the routes
    /// it must never reach are absent from this listener rather than gated
    /// on it.
    Rail,
}

impl ClientSurface {
    /// Whether a loopback peer address is evidence of a local caller.
    /// True only where the caller reached us by actually being on this
    /// machine, or by proving a member key at the QUIC handshake.
    pub fn auth_policy(self) -> ClientAuthPolicy {
        match self {
            Self::Operator => ClientAuthPolicy::default(),
            Self::Guest | Self::Rail => ClientAuthPolicy::UNTRUSTED_LOOPBACK,
        }
    }

    /// Whether the operator-only `/internal/*` routes are mounted at all.
    /// Only the surface an operator's own tools talk to.
    ///
    /// Exhaustive rather than `matches!` on purpose: a new variant must
    /// break here and be decided, not inherit `false` silently. This method
    /// guards an 18.5 GB disk load and a credential-forging lever, and a
    /// silent default is exactly the mistake nobody would notice.
    pub fn serves_operator_routes(self) -> bool {
        match self {
            Self::Operator => true,
            Self::Guest | Self::Rail => false,
        }
    }

    /// Whether the general client surface — `/v1/*`, `/api/*`, `/status`,
    /// `/oicp/*`, `/app/*` — is mounted.
    ///
    /// False for `Rail`, and that is the whole security property: a ring
    /// app cannot reach inference, knowledge, or app management because
    /// those routes are **not mounted on the listener it can reach**. Not a
    /// predicate that has to be right — a mount that has to exist (§7.1).
    pub fn serves_general_client_routes(self) -> bool {
        match self {
            Self::Operator | Self::Guest => true,
            Self::Rail => false,
        }
    }

    /// Whether the ring-app rail routes are mounted.
    ///
    /// `Operator` serves them too — a local caller can already reach every
    /// route on this daemon, so withholding them there would buy nothing
    /// and would leave `svrn ring` unable to read its own ledger. `Guest`
    /// serves them because the guest door (`crate::guest_door`) is this
    /// surface, and a guest holding a wall grant writes to the ring: the
    /// grant's `Scope::Rails` names the one namespace, and a guest with no
    /// rail scope is refused by the auth layer before routing.
    pub fn serves_rail_routes(self) -> bool {
        match self {
            Self::Operator | Self::Rail | Self::Guest => true,
        }
    }

    /// Whether the guest door's own answer route (`POST /v1/guest/ask`) is
    /// mounted.
    ///
    /// `Guest` because that is the door, and the route exists for the person
    /// standing in the room. `Operator` because a local caller already
    /// reaches every route on this daemon, and because `Scope::Rails` names
    /// this path — `every_scope_paths_are_mounted_and_never_privileged`
    /// drives the operator router and a grant naming an unmounted path would
    /// be born broken.
    ///
    /// **`Rail` is false and that is the whole point.** The route drives
    /// inference on this box; a deployed ring app reaching it would turn a
    /// rail listener into a compute surface. Same discipline as
    /// [`Self::serves_general_client_routes`]: not a predicate that has to be
    /// right, a mount that is absent.
    ///
    /// Mounting it is necessary and not sufficient: the handler refuses any
    /// caller the auth layer did not attach a `Guest` grant to, so the
    /// operator mount answers "not a guest" rather than serving.
    pub fn serves_guest_ask_route(self) -> bool {
        match self {
            Self::Operator | Self::Guest => true,
            Self::Rail => false,
        }
    }
}
