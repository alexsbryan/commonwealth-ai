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

/// Why a fan-out naming an ORIGIN KIND was refused — the skew a new CLI meets
/// against a daemon that predates the kind.
///
/// # Why this is not [`render_failure`]
///
/// `render_failure` explains a MISSING ROUTE. This is a route that exists and
/// answered, refusing the request — so its body is not empty and the 404
/// branch never fires. An old daemon's `FanoutRequest` has
/// `kind: Option<OriginKind>` over a closed set of two, and serde's `default`
/// applies to an ABSENT field and not to an unparseable one, so
/// `{"kind":"offer"}` fails the whole struct. axum answers 422 with a
/// sentence about a struct field, and the operator — who has a rebuilt CLI
/// and a daemon they forgot to restart — reads it as a bug in the verb.
///
/// # How the verdict is DETERMINED rather than guessed
///
/// By a probe, not by grepping serde's English. The same route is asked the
/// same question naming a kind every build has known (`media`) with an EMPTY
/// peer list — zero targets, so nothing is dialed and nothing is asked of any
/// neighbour. If that answers 200, this daemon's fan-out route works and what
/// it refused was the KIND. If it fails too, the cause is something else and
/// this says so rather than blaming skew (ARCH §18.3: could-not-judge is not
/// a verdict of no-skew).
///
/// A daemon carrying `AskedKind` (2026-09-13 and later) never reaches here
/// for this reason: it refuses an unknown kind ITSELF, by name, with a
/// `{"error": …}` body that `render_failure` passes through untouched. This
/// function is for the builds that shipped before that, and it is the last
/// thing that will need to be.
pub(crate) async fn render_kind_refusal(
    client: &reqwest::Client,
    port: u16,
    route: &str,
    kind: &str,
    status: reqwest::StatusCode,
    body: String,
) -> String {
    // The daemon's own named refusal wins outright — a build that can say
    // what it does not know has already said it better than this can.
    if let Some(msg) = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
    {
        return format!("{msg}\n");
    }
    let route_works = probe_known_kind(client, port).await;
    let local = sovereign_core::run_identity::stamp(env!("CARGO_PKG_VERSION"));
    let remote = daemon_stamp(client, port).await.ok().flatten();
    let builds = format!(
        "  this CLI:    {}\n  that daemon: {}\n",
        local.one_line(),
        remote
            .as_ref()
            .map(|s| s.one_line())
            .unwrap_or_else(|| "carries no build stamp (older than the stamp itself)".to_string())
    );
    tracing::debug!(
        target: "mesh",
        route,
        kind,
        status = status.as_u16(),
        route_works,
        "mesh_skew:kind_refusal"
    );
    match route_works {
        Some(true) => format!(
            "the daemon on :{port} does not know the origin kind `{kind}`.\n\
             It answered {status} to `{route}` naming `{kind}`, and 200 to the same route \
             naming `media`, so the route is fine and the KIND is what it could not read.\n\
             That daemon is older than this CLI.\n{builds}\
             Repair:  svrn daemon stop && svrn daemon start\n\
             An empty catalogue would have been the wrong answer here, and this is why \
             you are not looking at one.\n"
        ),
        Some(false) => format!(
            "`{route}` answered {status} for kind `{kind}`, and answered a failure to the \
             same route naming `media` too — so this is NOT the origin kind.\n{builds}\
             What the daemon said:\n{}\n",
            if body.trim().is_empty() {
                "(nothing)".to_string()
            } else {
                body
            }
        ),
        None => format!(
            "`{route}` answered {status} for kind `{kind}`, and the control request could \
             not be put at all, so whether this is version skew is UNKNOWN — not ruled \
             out.\n{builds}\
             `svrn daemon stop && svrn daemon start` and retry before looking further.\n\
             What the daemon said:\n{}\n",
            if body.trim().is_empty() {
                "(nothing)".to_string()
            } else {
                body
            }
        ),
    }
}

/// Ask the fan-out route a question EVERY build has understood, targeting
/// nobody.
///
/// `peers: []` selects zero members, so this dials no peer and asks no
/// origin — it costs one loopback round trip and has no effect any neighbour
/// could observe. `Some(true)` means the route answered; `Some(false)` that
/// it refused this too; `None` that the question could not be put.
async fn probe_known_kind(client: &reqwest::Client, port: u16) -> Option<bool> {
    let url = format!("http://127.0.0.1:{port}/v1/mesh/fanout");
    let body = serde_json::json!({ "path": "/", "kind": "media", "peers": [] });
    let resp = client.post(&url).json(&body).send().await.ok()?;
    Some(resp.status().is_success())
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

    /// A daemon that CAN name what it does not know has already said it
    /// better than the skew renderer can — its sentence is passed through,
    /// not replaced by a probe-and-guess. This is what every build from
    /// 2026-09-13 on does, which is why the branch below is for older ones.
    #[tokio::test]
    async fn a_daemon_that_names_the_unknown_kind_is_quoted_not_second_guessed() {
        let client = reqwest::Client::new();
        let out = render_kind_refusal(
            &client,
            1, // never dialed: the error body short-circuits before any probe
            "POST /v1/mesh/fanout",
            "offer",
            reqwest::StatusCode::BAD_REQUEST,
            // BUILT rather than typed. The hand-written raw string here was
            // `…origin kind "offer""}`, which is not JSON — so this test
            // passed through the probe branch instead of the one it names,
            // and said nothing. Caught by the gate on 2026-09-13; a fixture
            // that has to be valid JSON is built by the JSON library.
            serde_json::json!({
                "error": "bad request: this node does not know the origin kind \"offer\" \
                          — it serves media, app, offer."
            })
            .to_string(),
        )
        .await;
        assert!(out.contains("does not know the origin kind"), "{out}");
        assert!(
            !out.contains("this CLI:"),
            "a quoted refusal adds no skew preamble: {out}"
        );
    }

    /// The control request could not be put, so the verdict is UNKNOWN and is
    /// stated as unknown. The failing input is a renderer that reads "the
    /// probe did not answer" as "not skew" — a could-not-judge reported as a
    /// verdict (ARCH §18.3).
    #[tokio::test]
    async fn an_unprobeable_daemon_reports_unknown_rather_than_blaming_skew() {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(500))
            .build()
            .unwrap();
        let out = render_kind_refusal(
            &client,
            1,
            "POST /v1/mesh/fanout",
            "offer",
            reqwest::StatusCode::UNPROCESSABLE_ENTITY,
            "Failed to deserialize the JSON body".to_string(),
        )
        .await;
        assert!(out.contains("UNKNOWN — not ruled out"), "{out}");
        assert!(out.contains("this CLI:"), "{out}");
        // And it does not swallow what the daemon actually said.
        assert!(out.contains("Failed to deserialize"), "{out}");
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
