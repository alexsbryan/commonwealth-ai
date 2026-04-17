//! Canonical port constants + URL builders used by every subcommand
//! that needs to reach the sovereign daemon.
//!
//! Keeping these centralised means a port migration (as happened when
//! we consolidated `:8080` → `:9741` in the port-unification pass)
//! lands in one file instead of silently missing a doctor probe or a
//! generated opencode config.

/// Default port for the client-facing router — serves both
/// `/v1/chat/completions` (OpenAI-compatible) and `/mcp` (tool server).
pub const DEFAULT_CLIENT_PORT: u16 = 9741;

/// Default port for the internal mesh router — gossip, join handshake,
/// scheduling. Not meant to be reached from user code.
pub const DEFAULT_INTERNAL_PORT: u16 = 9742;

/// `http://localhost:<port>/mcp` — the MCP JSON-RPC entry point.
pub fn mcp_url(port: u16) -> String {
    format!("http://localhost:{port}/mcp")
}

/// `http://localhost:<port>/v1` — the OpenAI-compatible API root.
/// Use `v1_models_url` / `v1_chat_completions_url` for specific
/// endpoints.
pub fn v1_url(port: u16) -> String {
    format!("http://localhost:{port}/v1")
}

/// `http://localhost:<port>/v1/models` — used for readiness probes
/// and the model enumeration flow in opencode config generation.
pub fn v1_models_url(port: u16) -> String {
    format!("http://localhost:{port}/v1/models")
}

/// `http://localhost:<port>/oicp/v1/capabilities` — the OICP manifest
/// endpoint. `project init` probes this to enumerate model IDs for
/// the generated opencode provider block.
pub fn oicp_capabilities_url(port: u16) -> String {
    format!("http://localhost:{port}/oicp/v1/capabilities")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_ports_match_spec() {
        assert_eq!(DEFAULT_CLIENT_PORT, 9741);
        assert_eq!(DEFAULT_INTERNAL_PORT, 9742);
    }

    #[test]
    fn url_helpers_use_supplied_port() {
        assert_eq!(mcp_url(9741), "http://localhost:9741/mcp");
        assert_eq!(v1_url(9741), "http://localhost:9741/v1");
        assert_eq!(v1_models_url(9741), "http://localhost:9741/v1/models");
        assert_eq!(
            oicp_capabilities_url(9741),
            "http://localhost:9741/oicp/v1/capabilities"
        );
    }

    #[test]
    fn url_helpers_accept_nondefault_ports() {
        // Makes sure we're not hardcoding 9741 anywhere.
        assert_eq!(mcp_url(12345), "http://localhost:12345/mcp");
    }
}
