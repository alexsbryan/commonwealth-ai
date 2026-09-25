// SPDX-License-Identifier: AGPL-3.0-or-later
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

/// The daemon's client-facing base URL, honouring the established
/// `SOVEREIGN_DAEMON_URL` knob (declared in `quality/env-flags.toml`;
/// the boot bridge also maps legacy `SOVEREIGN_*` to `SVRNMESH_*`, so
/// read both, SOVEREIGN first for parity with every other reader).
///
/// The sandbox lane (`cli-journey-sandbox.sh`) points this at its
/// isolated daemon (e.g. `http://127.0.0.1:19741`); without the knob
/// the canonical port is the answer. One accessor per path (§10.6):
/// daemon-first MCP calls resolve their target here.
pub fn daemon_base_url() -> String {
    // Delegates rather than deciding (§10.6). This used to read the env here
    // and fall back to the COMPILED port above, which made it blind to an
    // operator's `[daemon] client_port` — every other reader followed the
    // config and this one silently went to 9741. It also accepted a
    // set-but-blank `SOVEREIGN_DAEMON_URL=` as an empty base URL and left
    // trailing slashes on, so `http://h:9841//v1/models` reached a strict
    // router as a different route. `client_daemon_base` answers all three.
    sovereign_contracts::setup_config::client_daemon_base()
}

/// `http://localhost:<port>/v1` — the OpenAI-compatible API root.
/// Use `v1_models_url` for the readiness probe.
///
/// `http://localhost:<port>/v1` — the OpenAI-compatible API root.
pub fn v1_url(port: u16) -> String {
    format!("http://localhost:{port}/v1")
}

/// `http://localhost:<port>/v1/models` — used for readiness probes
/// and the model enumeration flow in opencode config generation.
pub fn v1_models_url(port: u16) -> String {
    format!("http://localhost:{port}/v1/models")
}

/// The daemon's OpenAI-compatible `/v1` base, honouring the same knob
/// [`daemon_base_url`] does.
///
/// [`daemon_base_url`] returns a ROOT (`http://localhost:9741`) because the
/// override knob is a root and its own doc says "every caller appends
/// `/v1/…`". That is a real trap: a caller who forgets gets a 404 from a live,
/// healthy daemon, which reads as "the daemon is broken" rather than "you
/// built the wrong URL". It cost `snapshot restore --archive` its embedding
/// probe on 2026-09-08 — `POST http://localhost:9741/embeddings: 404 Not
/// Found`, an unjudgeable restore, correctly refused for the wrong reason.
///
/// One accessor per path (§10.6): append endpoint segments to THIS, not to
/// `daemon_base_url`. A knob already ending in `/v1` is honoured as-is rather
/// than doubled.
pub fn daemon_v1_base() -> String {
    let base = daemon_base_url();
    let base = base.trim_end_matches('/');
    if base.ends_with("/v1") {
        base.to_string()
    } else {
        format!("{base}/v1")
    }
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

    /// The exact input that 404'd on 2026-09-08: a daemon ROOT with an
    /// endpoint appended straight onto it. `daemon_v1_base` is the answer, and
    /// these are the three shapes the knob can arrive in.
    #[test]
    fn daemon_v1_base_adds_the_version_segment_exactly_once() {
        // SAFETY: single-threaded test setting a process env knob it also
        // clears; the helper reads it through `daemon_url_override`.
        for (knob, want) in [
            ("http://localhost:9741", "http://localhost:9741/v1"),
            ("http://localhost:9741/", "http://localhost:9741/v1"),
            ("http://127.0.0.1:19741/v1", "http://127.0.0.1:19741/v1"),
        ] {
            unsafe { std::env::set_var("SOVEREIGN_DAEMON_URL", knob) };
            assert_eq!(
                daemon_v1_base(),
                want,
                "a daemon base of `{knob}` must yield `{want}` — appending an endpoint to the \
                 bare root is the 404 that cost `snapshot restore` its probe"
            );
        }
        unsafe { std::env::remove_var("SOVEREIGN_DAEMON_URL") };
    }

    #[test]
    fn url_helpers_accept_nondefault_ports() {
        // Makes sure we're not hardcoding 9741 anywhere.
        assert_eq!(v1_models_url(12345), "http://localhost:12345/v1/models");
    }

    // (Was gated on `feature = "dev-tools"`, which this crate never
    // declared — the test silently never ran. P0.2 cfg audit, 2026-07-12.)
    #[test]
    fn v1_url_strips_to_api_root() {
        assert_eq!(v1_url(9741), "http://localhost:9741/v1");
        assert_eq!(v1_url(12345), "http://localhost:12345/v1");
    }
}
