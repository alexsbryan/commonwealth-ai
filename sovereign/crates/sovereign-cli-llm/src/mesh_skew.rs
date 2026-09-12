// SPDX-License-Identifier: AGPL-3.0-or-later
//! Why a mesh verb's route was not found on a running, healthy daemon.
//!
//! The CLI and the daemon are two binaries rebuilt independently, and an
//! `HTTP 404` says nothing about which code answered it. Before this, a verb
//! whose route the daemon predated printed an EMPTY LINE and exited 1 — the
//! route's own refusals carry `{"error": …}`, an unmounted route carries
//! nothing, and the error path rendered both the same way.
//!
//! The decision itself is [`sovereign_contracts::daemon_wire::BuildStamp`]'s
//! and is tested there against the incident that minted it. This file is the
//! transport half: which failures are worth asking about, and where the far
//! side's stamp comes from.

use sovereign_contracts::daemon_wire::{explain_route_missing, BuildStamp};

/// The stamp the daemon on `port` publishes, or `None` when it answered
/// without one.
///
/// `Err` is reserved for "the question could not be put" — the daemon was
/// unreachable or `/status` did not parse — so a caller can tell that apart
/// from a daemon that answered and carries no stamp, which is itself the
/// answer (ARCH §6).
async fn daemon_stamp(client: &reqwest::Client, port: u16) -> Result<Option<BuildStamp>, String> {
    #[derive(serde::Deserialize)]
    struct StatusHead {
        #[serde(default)]
        process: Option<ProcessHead>,
    }
    #[derive(serde::Deserialize)]
    struct ProcessHead {
        #[serde(default)]
        build: Option<BuildStamp>,
    }
    // `/status` rather than `/v1/mesh/status`: it is the OLDER surface, and
    // the daemon we are interrogating is by hypothesis an old one. Asking the
    // newer route to explain why a newer route is missing is a trap.
    let url = format!("http://127.0.0.1:{port}/status");
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("{url} did not answer ({e})"))?;
    if !resp.status().is_success() {
        return Err(format!("{url} answered {}", resp.status()));
    }
    let text = resp.text().await.map_err(|e| format!("{url}: {e}"))?;
    let head: StatusHead =
        serde_json::from_str(&text).map_err(|e| format!("{url} did not parse ({e})"))?;
    let stamp = head.process.and_then(|p| p.build);
    // The one decision in this file the operator cannot read off the message
    // it produces: the rendered text says "no stamp", never which URL was
    // asked or what it answered (ARCH principle 1).
    tracing::debug!(
        target: "mesh",
        url = %url,
        carries_stamp = stamp.is_some(),
        daemon_build = stamp.as_ref().map(|s| s.one_line()).unwrap_or_default(),
        "mesh_skew:daemon_stamp_read"
    );
    Ok(stamp)
}

/// Render a mesh route's failure for a person, asking the daemon which build
/// it is only when the failure could be skew.
///
/// A `404` carrying `{"error": …}` is the ROUTE answering — an unknown peer,
/// say — and is rendered as-is. A `404` with no error body is an unmounted
/// route, which is the case worth explaining; everything else is the daemon's
/// own refusal and is passed through unchanged.
///
/// The branches below carry no tracing of their own, deliberately: each one's
/// outcome is the STRING it returns, which lands on the operator's terminal in
/// words. A tracing event would restate, at a level nobody has enabled, what
/// the operator is already reading. The one decision that is not visible that
/// way — what `/status` actually said — is traced in [`daemon_stamp`].
pub(crate) async fn render_failure(
    client: &reqwest::Client,
    port: u16,
    route: &str,
    status: reqwest::StatusCode,
    body: String,
) -> String {
    let daemon_said = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string));
    if let Some(msg) = daemon_said {
        return msg;
    }
    if status != reqwest::StatusCode::NOT_FOUND {
        return if body.trim().is_empty() {
            format!("{route} → {status} (no detail from the daemon)")
        } else {
            body
        };
    }
    let local = sovereign_core::run_identity::stamp(env!("CARGO_PKG_VERSION"));
    match daemon_stamp(client, port).await {
        Ok(remote) => explain_route_missing(route, &local, remote.as_ref()),
        Err(why) => format!(
            "the daemon has no route {route}, and its build could not be read ({why}).\n  \
             this CLI: {}\nWhether this is version skew is unknown — not ruled out. \
             `svrn daemon restart` and retry before looking further.\n",
            local.one_line()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The route's OWN refusal must survive untouched — a 404 that carries
    /// `{"error": …}` is an answer, not a missing route, and rewriting it as
    /// version skew would send the reader after the wrong thing.
    #[tokio::test]
    async fn a_routes_own_error_body_is_passed_through() {
        let client = reqwest::Client::new();
        let out = render_failure(
            &client,
            1, // never dialed: the error body short-circuits before any request
            "GET /v1/mesh/media",
            reqwest::StatusCode::NOT_FOUND,
            r#"{"error":"no member named dave"}"#.to_string(),
        )
        .await;
        assert_eq!(out, "no member named dave");
    }

    /// A non-404 with an empty body still says something. It used to print a
    /// blank line, which is the failure this module exists for.
    #[tokio::test]
    async fn an_empty_non_404_body_still_names_the_status() {
        let client = reqwest::Client::new();
        let out = render_failure(
            &client,
            1,
            "POST /v1/mesh/media/fanout",
            reqwest::StatusCode::CONFLICT,
            String::new(),
        )
        .await;
        assert!(out.contains("409"), "{out}");
        assert!(out.contains("POST /v1/mesh/media/fanout"), "{out}");
    }

    /// Port 1 has no daemon, so the stamp cannot be read. The verdict is
    /// "unknown", stated as unknown — never as "not skew".
    #[tokio::test]
    async fn an_unreachable_daemon_reports_unknown_not_absence_of_skew() {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(500))
            .build()
            .unwrap();
        let out = render_failure(
            &client,
            1,
            "POST /v1/mesh/media/fanout",
            reqwest::StatusCode::NOT_FOUND,
            String::new(),
        )
        .await;
        assert!(out.contains("unknown — not ruled out"), "{out}");
        assert!(out.contains("this CLI:"), "{out}");
    }
}
