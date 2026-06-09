// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tracing subscriber bootstrap. Implementation moved to
//! `sovereign-cli-shared::tracing_init`. This shim preserves the
//! in-crate `crate::util::tracing_init::init_tracing(...)` call site.

pub use sovereign_cli_shared::tracing_init::init_tracing;
