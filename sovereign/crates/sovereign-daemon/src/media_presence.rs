// SPDX-License-Identifier: AGPL-3.0-or-later
//! The holder's media-presence poll — "is somebody in this house watching the
//! library right now?", asked of the origin and reported to this node's own
//! capabilities.
//!
//! It is the media twin of `sovereign-server`'s `ActivityReporter` and rides
//! the same route for the same reason: one internal endpoint owns "what can
//! this node serve right now", and gossip reads what that endpoint wrote. The
//! decision itself is `commonwealth_media::presence`, which has no HTTP in it
//! and is tested without a server.
//!
//! Three things make a report possible, all of them live on [`MediaRoute`]:
//! an origin (there is a library), the declared credential (we may ask it),
//! and the viewer account id (we can tell the holder from the house). Missing
//! any of the last two, the poll publishes `None` — nobody answered — rather
//! than guessing at `FREE`, because a viewer starts a stream on `FREE`.

use std::time::Duration;

use sovereign_mesh::iroh_access::MediaRoute;

/// How often the origin is asked. Matched to the gossip round (10 s): a
/// faster poll cannot reach a peer sooner, and a slower one would let the
/// wall show "free" after the holder pressed play.
pub const POLL_INTERVAL: Duration = Duration::from_secs(10);

/// How long the origin is given to answer. Short on purpose — it is on
/// loopback, and a hung origin must read as "did not answer" this round
/// rather than stall the next one.
const ASK_TIMEOUT: Duration = Duration::from_secs(3);

/// Ask the origin once and turn its answer into what this node publishes.
///
/// `Ok(Some(v))` is a real reading; `Ok(None)` is an honest "could not ask".
/// There is no error case on purpose: every failure here is a `None` the
/// caller publishes, and collapsing it into an `Err` the caller would then
/// have to re-widen is how a refusal becomes a default.
async fn read_presence(client: &reqwest::Client, route: &MediaRoute) -> Option<f32> {
    let origin = route.origin()?;
    let Some(viewer) = route.viewer_user() else {
        tracing::debug!(
            %origin,
            "media presence: no viewer account declared for this origin — publishing no presence"
        );
        return None;
    };
    let mut req = client
        .get(format!("http://{origin}/Sessions"))
        .timeout(ASK_TIMEOUT);
    for (name, value) in route.declared().iter() {
        req = req.header(name, value);
    }
    let body = match req.send().await {
        Ok(resp) if resp.status().is_success() => match resp.text().await {
            Ok(body) => body,
            Err(e) => {
                tracing::warn!(
                    %origin,
                    error = %e,
                    "media presence: the origin's answer could not be read — publishing no presence"
                );
                return None;
            }
        },
        Ok(resp) => {
            tracing::warn!(
                %origin,
                status = resp.status().as_u16(),
                "media presence: the origin refused the sessions read — publishing no presence"
            );
            return None;
        }
        Err(e) => {
            tracing::warn!(
                %origin,
                error = %e,
                "media presence: the origin could not be asked — publishing no presence"
            );
            return None;
        }
    };
    match commonwealth_media::media_available_from_sessions(&body, &viewer) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(
                %origin,
                error = %e,
                "media presence: the origin answered something that is not sessions"
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
        .timeout(ASK_TIMEOUT)
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
/// `recompute_local_availability` keeps for the inference half. A node with no
/// `[iroh] media_origin` reads `None` on the first tick, reports it once, and
/// then costs one `Option` read per interval.
pub async fn run(route: MediaRoute, internal_url: String) {
    let client = match reqwest::Client::builder().build() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "media presence: no HTTP client — the poll will not run");
            return;
        }
    };
    tracing::info!(
        interval_s = POLL_INTERVAL.as_secs(),
        %internal_url,
        "media presence: poll started"
    );
    // `None` is the published default, so the first tick reports only when it
    // finds a real reading — a node with no library stays silent.
    let mut last: Option<Option<f32>> = Some(None);
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        let now = read_presence(&client, &route).await;
        if last != Some(now) {
            report(&client, &internal_url, now).await;
            last = Some(now);
        }
    }
}
