// SPDX-License-Identifier: AGPL-3.0-or-later
//! The protocols a mesh endpoint forwards to a program's registered origin
//! that are not an origin kind of their own: mesh-internal HTTP, the client
//! API and the ggml RPC byte stream. Moved from commonwealth-transport iroh.rs
//! (pb-serve-distributes-standalone; the client API's in pb-serve-ranks) so
//! serve, which cannot link the transport, registers under the same names the
//! endpoint accepts.

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
