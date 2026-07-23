// SPDX-License-Identifier: AGPL-3.0-or-later
//! Canonical port constants + URL builders used by every subcommand
//! that needs to reach the svrn daemon.
//!
//! Keeping these centralised means a port migration (as happened when
//! we consolidated `:8080` → `:9741` in the port-unification pass)
//! lands in one file instead of silently missing a doctor probe or a
//! generated opencode config.

/// Default port for the client-facing router — serves both
/// `/v1/chat/completions` (OpenAI-compatible) and `/mcp` (tool server).
pub const DEFAULT_CLIENT_PORT: u16 = 9741;

/// `http://localhost:<port>/v1` — the OpenAI-compatible API root.
/// Use `v1_models_url` for the readiness probe.
///
/// Consumed by `session_cmd` (default build) and `awareness_cmd`
/// (`dev-tools`), so it is no longer feature-gated.
pub fn v1_url(port: u16) -> String {
    format!("http://localhost:{port}/v1")
}

/// `http://localhost:<port>/v1/models` — used for readiness probes
/// and the model enumeration flow in opencode config generation.
pub fn v1_models_url(port: u16) -> String {
    format!("http://localhost:{port}/v1/models")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_ports_match_spec() {
        assert_eq!(DEFAULT_CLIENT_PORT, 9741);
    }

    #[test]
    fn url_helpers_use_supplied_port() {
        assert_eq!(v1_models_url(9741), "http://localhost:9741/v1/models");
    }

    #[test]
    fn url_helpers_accept_nondefault_ports() {
        // Makes sure we're not hardcoding 9741 anywhere.
        assert_eq!(v1_models_url(12345), "http://localhost:12345/v1/models");
    }

    #[test]
    fn v1_url_strips_to_api_root() {
        assert_eq!(v1_url(9741), "http://localhost:9741/v1");
        assert_eq!(v1_url(12345), "http://localhost:12345/v1");
    }
}
