// SPDX-License-Identifier: AGPL-3.0-or-later
//! MCP server configuration schema.
//!
//! The config DTOs moved to [`sovereign_core::mcp_config`] so that
//! `SetupConfig` can carry a typed `mcp_servers` field without a crate cycle
//! (`sovereign-tools` depends on `sovereign-core`, never the reverse). This
//! module re-exports them so existing `sovereign_tools::mcp::config::*` and
//! `super::config::*` paths keep resolving unchanged. The serde round-trip
//! tests live with the definitions in `sovereign-core`.

pub use sovereign_core::mcp_config::{McpAuthConfig, McpServerConfig, McpTransportConfig};
