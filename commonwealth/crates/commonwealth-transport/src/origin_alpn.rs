// SPDX-License-Identifier: AGPL-3.0-or-later
//! The three ORIGIN protocols, re-exported at their historical path.
//!
//! The constants and their pinning tests moved to
//! `sovereign_contracts::transport` (fp-40, §12 decision 3) — they are wire
//! vocabulary. This shim exists so every call site keeps spelling these
//! `commonwealth_transport::origin_alpn::MEDIA_ALPN` and, via `iroh.rs`'s
//! re-export, `commonwealth_transport::iroh::MEDIA_ALPN`.
//!
//! The map from `OriginKind` to these remains deliberately NOT here — the
//! chain is `commonwealth_media::class_of` (kind → `TrafficClass`) then
//! `iroh::IrohTransport`'s `alpn_for_class` (class → ALPN). A third map from
//! kind straight to ALPN would be a second answer to a question already
//! answered (ARCH §8).

pub use sovereign_contracts::transport::{APP_ALPN, MEDIA_ALPN, OFFER_ALPN};
