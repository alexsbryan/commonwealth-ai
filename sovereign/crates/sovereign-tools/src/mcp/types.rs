// SPDX-License-Identifier: AGPL-3.0-or-later
/// Information about a tool discovered from an MCP server.
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}
