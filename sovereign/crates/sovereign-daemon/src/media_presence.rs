// SPDX-License-Identifier: AGPL-3.0-or-later
//! The holder's media-presence poll — "is somebody in this house watching the
//! library right now?", read from the mesh's serving process and reported to
//! this node's own capabilities.
//!
//! The decision lives in cw-rails (fp-46): it holds the origin and the house
//! credential, it runs the poll, and `GET /v1/mesh/media/presence` serves its
//! last reading. This loop mirrors that answer onto the node's own activity
//! report — a second CALLER, never a second decider (ARCH 8): the house
//! credential does not live here, and no origin is asked from this process.
//!
//! The served `null` and a serving process that does not answer both publish
//! "no presence" — a viewer must not start a stream on a missing answer —
//! but they are not the same event: a failed dial is a named absence in the
//! log (principle 6), while the served `null` is the poll's own honest
//! reading that nobody was watching.

use std::time::Duration;

/// How often the serving process is asked. Matched to the gossip round (10 s):
/// a faster poll cannot reach a peer sooner, and a slower one would let the
/// wall show "free" after the holder pressed play.
pub const POLL_INTERVAL: Duration = Duration::from_secs(10);

/// How long the activity route is given to take a report. Short on purpose —
/// the report is loopback, and a hung receiver must not stall the poll.
const REPORT_TIMEOUT: Duration = Duration::from_secs(3);

/// Read the serving process's last presence reading.
///
/// `Some(v)` is a real reading; `None` is "no presence to publish" — either
/// the served `null` (the poll over there could not ask) or a dial that did
/// not answer, which is logged with the base URL rather than defaulted
/// silently. There is no error case on purpose: every failure here is a
/// `None` the caller publishes, and collapsing it into an `Err` the caller
/// would then have to re-widen is how a refusal becomes a default.
async fn read_presence(base: &str) -> Option<f32> {
    match crate::rails_client::media_presence(base).await {
        Ok(reading) => reading,
        Err(e) => {
            tracing::warn!(
                %base,
                error = %e,
                "media presence: the mesh's serving process did not answer — publishing no presence"
            );
            None
        }
    }
}

/// Report a CHANGE to this node's own `/internal/node/activity`, the one
/// endpoint that owns what this node can serve. `null` is sent explicitly so
/// "nobody answered" clears the last reading instead of outliving it.
async fn report(client: &reqwest::Client, internal_url: &str, media_available: Option<f32>) {
    let body = serde_json::json!({
        "reason": "media-presence",
        "media_available": media_available,
    });
    match client
        .post(format!("{internal_url}/internal/node/activity"))
        .json(&body)
        .timeout(REPORT_TIMEOUT)
        .send()
        .await
    {
        Ok(resp) if resp.status().as_u16() == 204 => {
            tracing::info!(
                ?media_available,
                "media presence: reported — the next gossip round carries it"
            );
        }
        Ok(resp) => tracing::warn!(
            status = resp.status().as_u16(),
            "media presence: the activity route did not accept the report"
        ),
        Err(e) => tracing::warn!(error = %e, "media presence: could not reach the activity route"),
    }
}

/// Run the poll until the process ends.
///
/// Reports only on a CHANGE, so the transition is one line in the log rather
/// than one every ten seconds — the same discipline
/// `recompute_local_availability` keeps for the inference half. A serving
/// process with no reading publishes `None` on the first tick, reports it
/// once, and then costs one dial per interval.
pub async fn run(rails_base: String, internal_url: String) {
    let client = match reqwest::Client::builder().build() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "media presence: no HTTP client — the poll will not run");
            return;
        }
    };
    tracing::info!(
        interval_s = POLL_INTERVAL.as_secs(),
        %rails_base,
        %internal_url,
        "media presence: poll started"
    );
    // `None` is the published default, so the first tick reports only when it
    // finds a real reading — a node whose serving process has no reading yet
    // stays silent.
    let mut last: Option<Option<f32>> = Some(None);
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        let now = read_presence(&rails_base).await;
        if last != Some(now) {
            report(&client, &internal_url, now).await;
            last = Some(now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for the mesh's serving process: the presence route serving
    /// a reading this daemon had no part in deciding.
    async fn rails_serving(media_available: Option<f32>) -> std::net::SocketAddr {
        let app = axum::Router::new().route(
            "/v1/mesh/media/presence",
            axum::routing::get(move || async move {
                axum::Json(serde_json::json!({ "media_available": media_available }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        addr
    }

    fn base(addr: std::net::SocketAddr) -> String {
        format!("http://{addr}")
    }

    /// Positive: the served reading is what this poll reads — the decision
    /// is the serving process's; this loop only mirrors it.
    #[tokio::test]
    async fn the_served_reading_is_what_the_poll_reads() {
        let addr = rails_serving(Some(0.0)).await;
        assert_eq!(
            read_presence(&base(addr)).await,
            Some(0.0),
            "the holder is watching; the served reading must reach this poll verbatim"
        );
    }

    /// The served `null` is a VALUE — the poll over there could not ask —
    /// and it publishes no presence, never a default.
    #[tokio::test]
    async fn the_served_null_publishes_no_presence() {
        let addr = rails_serving(None).await;
        assert_eq!(
            read_presence(&base(addr)).await,
            None,
            "a served \"nobody answered\" must read as no presence"
        );
    }

    /// Negative: no serving process is a named absence (principle 6) that
    /// publishes no presence — a viewer must not start a stream on a missing
    /// answer.
    #[tokio::test]
    async fn an_absent_serving_process_publishes_no_presence() {
        // Port 1 on loopback: nothing listens there, so the dial is refused
        // at connect time rather than timing out.
        assert_eq!(
            read_presence("http://127.0.0.1:1").await,
            None,
            "nothing to ask must read as no presence, with the absence logged"
        );
    }
}
