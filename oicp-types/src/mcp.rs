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

/// The request methods an MCP server mount answers. The one place a method
/// name is matched as a string: every mount parses into this and matches on
/// the variant, so a method cannot be spelled two ways (principle 9).
/// Notifications are not here; a request without an id is one, whatever its
/// method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpMethod {
    /// `initialize`: version negotiation, capabilities, server info.
    Initialize,
    /// `ping`: liveness, answered with an empty result.
    Ping,
    /// `tools/list`: the tools the mount exposes.
    ToolsList,
    /// `tools/call`: run one tool.
    ToolsCall,
}

impl McpMethod {
    /// Every variant, in declaration order.
    pub const ALL: [McpMethod; 4] = [
        McpMethod::Initialize,
        McpMethod::Ping,
        McpMethod::ToolsList,
        McpMethod::ToolsCall,
    ];

    /// The method's wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            McpMethod::Initialize => "initialize",
            McpMethod::Ping => "ping",
            McpMethod::ToolsList => "tools/list",
            McpMethod::ToolsCall => "tools/call",
        }
    }

    /// Parse a wire method name; `None` is "method not found" (-32601).
    pub fn parse(method: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == method)
    }
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

    /// `ALL` holds every variant once (the match has no wildcard, so a new
    /// variant fails to compile here until it is placed), and each wire
    /// spelling parses back to its variant.
    #[test]
    fn mcp_method_all_is_exhaustive_and_round_trips() {
        let position = |m: McpMethod| match m {
            McpMethod::Initialize => 0,
            McpMethod::Ping => 1,
            McpMethod::ToolsList => 2,
            McpMethod::ToolsCall => 3,
        };
        for (i, m) in McpMethod::ALL.into_iter().enumerate() {
            assert_eq!(position(m), i, "{m:?} is out of place in ALL");
            assert_eq!(McpMethod::parse(m.as_str()), Some(m));
        }
        assert_eq!(McpMethod::parse("notifications/initialized"), None);
    }
}
