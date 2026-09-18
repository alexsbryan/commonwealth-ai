// SPDX-License-Identifier: AGPL-3.0-or-later
//! Commonwealth HTTP API — the `:9741` front door.
//!
//! The host cluster left this crate for `sovereign-daemon` at domains
//! `dm-daemon-api-edge` (2026-09-18): the edge, `state` with its six parts,
//! `server`, the routes and the crate's own integration tests. What remains
//! here is the set of shims for the modules that left earlier, kept so any
//! path this crate still exports resolves until
//! `REVIEW-build-sovereign-api-retire` deletes the crate. Every consumer was
//! repointed to the owner in the same commit.
//!
//! Panic-ratchet (tech-debt PR4): production code in this crate is held
//! to `clippy::unwrap_used` / `clippy::expect_used`. A panic in a request
//! handler surfaces to the caller as an opaque 500, so prefer `?` / typed
//! errors / graceful fallbacks (`unwrap_or_default`, poison-recovery via
//! `into_inner`, `let … else`) over `unwrap()` / `expect()`. Genuine
//! infallible-by-construction sites carry a local `#[allow(...)]` with a
//! rationale. Test code is exempt via the workspace `clippy.toml`
//! (`allow-{unwrap,expect}-in-tests`).
//!
//! Soft `warn`, not `deny` / CI gate — surfaces new production
//! `unwrap()`/`expect()` in `cargo clippy` output without blocking the
//! build. Clippy-only; does NOT affect `cargo check`/`cargo build`. The
//! production surface was clean (0 sites) as of the PR4 sweep.
#![warn(clippy::unwrap_used, clippy::expect_used)]

pub use sovereign_grants::auto_recover; // shim: moved by domains dm-auto-recover-move
                                        // The fan-out core is `commonwealth_transport::fanout` since the rails carve
                                        // (a package-only member fans out too, with none of this crate). Re-exported
                                        // so `sovereign_api::fanout::…` paths keep resolving.
pub use code_next_edit::next_edit; // shim: moved by domains dm-next-edit-move
pub use code_next_edit::next_edit_journal; // shim: moved by domains dm-next-edit-move
pub use code_next_edit::next_edit_model; // shim: moved by domains dm-next-edit-move
pub use code_next_edit::next_edit_symbols; // shim: moved by domains dm-next-edit-move
pub use code_next_edit::next_edit_syntax; // shim: moved by domains dm-next-edit-move
pub use commonwealth_transport::fanout;
pub use oicp_types::openai_types; // shim: moved by domains dm-wire-openai-types
pub use oicp_types::responses_types; // shim: moved by domains dm-wire-responses-types
pub use sovereign_core::answering::turn_fidelity; // shim: moved by domains REVIEW-build-answering-inversion

pub use commonwealth_core::{Error, Result};
