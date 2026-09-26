// SPDX-License-Identifier: AGPL-3.0-or-later
//! MCP wire vocabulary: protocol-version negotiation and the method set every
//! MCP server mount speaks. It sits beside [`crate::jsonrpc`], the envelope it
//! rides on (FIVE_PROGRAMS §12 3a rung 2; phase-b pb-mcp, decision phase-b-13).

/// MCP protocol revisions both server mounts can speak, newest first.
///
/// What each revision demands of a server beyond 2024-11-05, and why we
/// can claim it: 2025-03-26 adds the Streamable-HTTP transport (both
/// mounts serve `POST` + SSE on one endpoint) and requires accepting
/// JSON-RPC batch bodies (both mounts do); 2025-06-18 removes batching
/// again and everything else it adds (structured output, elicitation,
/// OAuth for non-local servers) is optional — our servers are
/// loopback-only.
pub const MCP_SUPPORTED_PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// Spec-conformant `initialize` version negotiation: echo the client's
/// requested revision when we support it; otherwise answer with our
/// newest and let the client decide whether to proceed or disconnect.
///
/// Takes the raw `initialize` params so all call sites stay one-liners.
pub fn negotiate_mcp_protocol_version(params: Option<&serde_json::Value>) -> &'static str {
    params
        .and_then(|p| p.get("protocolVersion"))
        .and_then(|v| v.as_str())
        .and_then(|requested| {
            MCP_SUPPORTED_PROTOCOL_VERSIONS
                .iter()
                .find(|v| **v == requested)
                .copied()
        })
        .unwrap_or(MCP_SUPPORTED_PROTOCOL_VERSIONS[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Version negotiation echoes a supported requested revision,
    /// counters an unknown one with our newest, and defaults to the
    /// newest when the client sends no version at all.
    #[test]
    fn protocol_version_negotiation() {
        let req = |v: &str| serde_json::json!({ "protocolVersion": v });
        assert_eq!(
            negotiate_mcp_protocol_version(Some(&req("2024-11-05"))),
            "2024-11-05"
        );
        assert_eq!(
            negotiate_mcp_protocol_version(Some(&req("2025-03-26"))),
            "2025-03-26"
        );
        assert_eq!(
            negotiate_mcp_protocol_version(Some(&req("2025-06-18"))),
            "2025-06-18"
        );
        // Unknown revision → counter-offer our newest.
        assert_eq!(
            negotiate_mcp_protocol_version(Some(&req("2099-01-01"))),
            "2025-06-18"
        );
        // No params / no protocolVersion → newest.
        assert_eq!(negotiate_mcp_protocol_version(None), "2025-06-18");
        assert_eq!(
            negotiate_mcp_protocol_version(Some(&serde_json::json!({}))),
            "2025-06-18"
        );
    }
}
