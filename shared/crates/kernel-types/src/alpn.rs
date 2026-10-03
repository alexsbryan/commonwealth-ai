// SPDX-License-Identifier: AGPL-3.0-or-later
//! Wire identity: the ALPN strings a dial carries to say which protocol it
//! speaks and, with it, which trust posture it is asking for.
//!
//! In `kernel-types` because all three product domains spell these bytes
//! alike across a network boundary — the daemon's transport, the mobile
//! bridge, and the wasm guest runtime — and "both ends of an exchange must
//! spell alike" is exactly the fact this crate exists to own (ARCH §12: a
//! constant with no shared owner is a constant with a future bug). The
//! routing and the refusals live in `commonwealth-transport`/`iroh_access`;
//! what lives here is the spelling and the trust each spelling carries.

/// ALPN for GUEST client traffic — someone holding a `sovereign://guest/…`
/// bearer who is NOT a mesh member.
///
/// The trust split is the point (canon): `RPC_ALPN` is members only with no
/// downgrade, and **this one allows any dialer** — a guest is by definition
/// not a member, and their whole credential is the bearer checked behind the
/// door. Distinct from `CLIENT_ALPN` for the same reason in reverse: client
/// connections forward to a listener that admits loopback before it reads a
/// bearer, which is correct for members and exactly wrong for guests.
///
/// The second site (`cmnwlth/apps/ring-runtime`, the wasm guest runtime)
/// imports this — the import is the bar, so the two ends of the guest dial
/// cannot drift (ROOT_CAUSE_FIXES B3).
pub const GUEST_ALPN: &[u8] = b"cwth/guest/0";
