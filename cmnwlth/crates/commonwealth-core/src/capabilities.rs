// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a node advertises. The records live in `oicp_types::capabilities`
//! since pb-mesh-exit-core; this path re-exports them so every existing
//! `commonwealth_core::capabilities::*` site resolves unchanged.
pub use oicp_types::capabilities::*;
