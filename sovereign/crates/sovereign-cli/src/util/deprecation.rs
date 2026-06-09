// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deprecation banner for renamed CLI surfaces. Implementation moved
//! to `sovereign-cli-shared::deprecation` so sibling binaries can
//! emit identical banners. This shim preserves the in-crate
//! `crate::util::deprecation::*` import path.

pub use sovereign_cli_shared::deprecation::announce;
