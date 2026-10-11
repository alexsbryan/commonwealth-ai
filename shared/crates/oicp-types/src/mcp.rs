// SPDX-License-Identifier: AGPL-3.0-or-later
//! MCP wire vocabulary: protocol-version negotiation and the method set every
//! MCP server mount speaks. It sits beside [`crate::jsonrpc`], the envelope it
//! rides on (FIVE_PROGRAMS §12 3a rung 2; phase-b pb-mcp, decision phase-b-13).
//! [`ToolOutcome`] is what one `tools/call` produced and its `CallToolResult`
//! wire shape (phase-b pb-code-server): every MCP host, and the registry half
//! both the daemon and the code server run, speak it.

use serde_json::{json, Value};

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

/// The HTTP header a connection names its repo's code corpus with
/// (`x-svrn-corpus: svrngs`): the code tools answer about that corpus alone.
/// Absent, they answer about every corpus.
pub const MCP_CORPUS_HEADER: &str = "x-svrn-corpus";

/// The HTTP header that caps what a connection may reach by effect. Its one
/// value is [`MCP_EFFECTS_READ`]; any other is refused, never widened.
pub const MCP_EFFECTS_HEADER: &str = "x-svrn-effects";

/// [`MCP_EFFECTS_HEADER`]'s one value: the connection lists and calls only
/// tools whose effect is [`crate::Effect::Read`].
pub const MCP_EFFECTS_READ: &str = "read";

/// What one request carries besides its JSON-RPC body: the transport's view
/// of who asked. Stdio has none of it, so there it is the default: every
/// corpus, every effect.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpRequestContext {
    /// The `X-Agent-Session` header the agent sent, if it sent one.
    pub agent_session: Option<String>,
    /// [`MCP_CORPUS_HEADER`]: the one corpus this connection's code
    /// questions are about. `None` is every corpus.
    pub corpus: Option<String>,
    /// [`MCP_EFFECTS_HEADER`] was [`MCP_EFFECTS_READ`]: no tool whose effect
    /// is not `Read` is listed or called.
    pub read_only: bool,
    /// Who asked, as the host's auth layer resolved it: the non-secret label
    /// of its principal (`asserted:claude-code` for a named client). `None`
    /// when the transport resolved no one (stdio, a host with no auth layer).
    /// Never read from a header the client types.
    pub caller: Option<String>,
}

/// What the call log is told about a call that executed a tool. It never goes
/// on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallAudit {
    /// What the tool may change, as its descriptor declares it.
    pub effect: crate::Effect,
    /// The tool succeeded with a null or empty JSON result.
    pub empty_result: bool,
}

/// What one tool run produced. A tool that ran and failed is an outcome with
/// `is_error` set, which the agent reads and can recover from; it is never a
/// JSON-RPC error, which a client treats as a broken transport.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolOutcome {
    /// The text the agent reads.
    pub text: String,
    /// The tool said no.
    pub is_error: bool,
    /// Optional machine-readable result (`structuredContent`).
    pub structured: Option<Value>,
    /// `None` when no tool executed: the host refused the call before one ran.
    pub audit: Option<CallAudit>,
}

impl ToolOutcome {
    /// A tool answered.
    pub fn answer(text: impl Into<String>, structured: Option<Value>) -> Self {
        Self {
            text: text.into(),
            structured,
            ..Self::default()
        }
    }

    /// A tool said no.
    pub fn refusal(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
            ..Self::default()
        }
    }

    /// The MCP `CallToolResult` this outcome is on the wire.
    pub fn into_call_result(self) -> Value {
        let mut result = json!({
            "content": [ { "type": "text", "text": self.text } ],
            "isError": self.is_error,
        });
        if let Some(structured) = self.structured {
            result["structuredContent"] = structured;
        }
        result
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
