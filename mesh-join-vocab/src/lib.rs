//! Mesh join-key + deep-link FORMAT vocabulary. Layer 0; no fs, no store.
//!
//! `join_key` is the key format decider (`hash_join_key`,
//! `validate_join_key_format`); `deep_link` is the `sovereign://` / https
//! join-and-guest link grammar (`DeepLink`, the parse/build family). Both were
//! extracted verbatim from `commonwealth-discovery` (fp-9/fp-24), which
//! re-exports them at their historical paths — one implementation, two spellings
//! at most, never a twin (ARCH §10.6).
//!
//! The one behaviour input is the `SOVEREIGN_JOIN_HOST` env override read by
//! `deep_link`'s https builders; it rides with the format that consumes it and
//! is declared in `quality/env-flags.toml`.

pub mod deep_link;
pub mod join_key;
