// SPDX-License-Identifier: AGPL-3.0-or-later
//! The protocols a mesh endpoint forwards to a program's registered origin
//! that are not an origin kind of their own: mesh-internal HTTP, the client
//! API and the ggml RPC byte stream. Moved from commonwealth-transport iroh.rs
//! (pb-serve-distributes-standalone; the client API's in pb-serve-ranks) so
//! serve, which cannot link the transport, registers under the same names the
//! endpoint accepts. The offer origin's ALPN joined them for the same reason:
//! svrn registers it and cannot link the transport (pb-mesh-exit-transport).

/// ALPN for mesh-internal HTTP-over-iroh tunnels. Version-suffixed so
/// a future class-aware protocol can coexist during migration.
pub const ALPN: &[u8] = b"cwth/http/0";

/// ALPN for the ggml tensor-split RPC byte stream (task 6): a worker's
/// acceptor forwards this to its local rpc-server (`127.0.0.1:50052`);
/// the host reaches it through a bridge-local endpoint minted for
/// [`crate::TrafficClass::RpcTensor`]. Raw bytes, not HTTP — the pump is
/// byte-generic. Version-suffixed like its siblings.
pub const RPC_ALPN: &[u8] = b"cwth/rpc/0";

/// ALPN for client-API traffic (Track M: phone → `sovereign-server`).
/// Distinct from [`ALPN`] so one daemon can later accept both and
/// route by protocol instead of by port.
pub const CLIENT_ALPN: &[u8] = b"cwth/client/0";

/// ALPN for GUEST client traffic — someone holding a `sovereign://guest/…`
/// bearer who is NOT a mesh member.
///
/// Distinct from [`CLIENT_ALPN`] because the two want opposite trust. A
/// member on `CLIENT_ALPN` reaches a member client that admits it by the
/// key its handshake proved (peer federated inference carries no
/// `Authorization` header at all); a guest's whole credential is its bearer.
/// So a guest gets its own protocol, forwarded to a bind of the client router
/// whose auth layer does not trust loopback
/// (`sovereign_daemon::client_auth::ClientAuthPolicy`). Moved here from the
/// iroh-gated `guest` module (pb-mesh-exit-transport) so a program that
/// registers the guest door names it without linking iroh; `guest` re-exports
/// it at its historical path.
///
/// The BYTES live in `kernel-types` (`alpn`) — the wasm guest runtime dials
/// the same spelling from a standalone closure and imports them there
/// (ROOT_CAUSE_FIXES B3: one definition, the second site imports).
pub use kernel_types::alpn::GUEST_ALPN;

/// A MEMBER reaching the HTTP origin that lists what this node's operator has
/// to SELL or LEND (`[iroh] offer_origin`) — a drill going spare, six eggs, a
/// room for a week.
///
/// The bridge parses nothing, exactly as `MEDIA_ALPN`'s does not: what an
/// offer IS stays the origin's, so a house can point this at a static JSON
/// file, a spreadsheet exporter, or a real shop. The catalogue a member sees
/// is COMPUTED by asking every publisher at once
/// (`commonwealth_media::fanout`) rather than stored, so there is no listing
/// to be excluded from and nobody positioned to rank
/// (`docs/internal/rings/reference/RING_APPLICATIONS.md` §Commerce).
pub const OFFER_ALPN: &[u8] = b"cwth/offer/0";
