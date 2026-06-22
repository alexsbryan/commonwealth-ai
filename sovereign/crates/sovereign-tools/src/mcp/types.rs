// SPDX-License-Identifier: AGPL-3.0-or-later
/// Information about a tool discovered from an MCP server.
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    /// The tool's declared output schema (MCP `outputSchema`), when the server
    /// provides one. Passed through to `ToolDescriptor.output_schema` so the
    /// planner can compose `{N.key}` references; `None` for servers that don't
    /// declare it (downstream steps then pipe the full text via `{N.output}`).
    pub output_schema: Option<serde_json::Value>,
}
