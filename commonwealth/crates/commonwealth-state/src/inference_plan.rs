// SPDX-License-Identifier: AGPL-3.0-or-later
//! The distributed-inference plan. The records live in
//! `oicp_types::inference_plan` since pb-mesh-exit-core; this path re-exports
//! them so every existing `commonwealth_state::inference_plan::*` site
//! resolves unchanged.
pub use oicp_types::inference_plan::*;
