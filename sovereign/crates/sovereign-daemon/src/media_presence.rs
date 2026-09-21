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
//! an origin (there is a library), the HOUSE credential (we may ask it), and
//! the viewer account id (we can tell the holder from the house). Missing any
//! of the last two, the poll publishes `None` — nobody answered — rather than
//! guessing at `FREE`, because a viewer starts a stream on `FREE`.
//!
//! Two credentials, two purposes. The read-only viewer token is what `offer`
//! DECLARES and what every member's dial carries; the house token is the
//! install-stage credential that same verb spent, kept on the holder's machine
//! alone (`commonwealth_media::house_dir_under`) and read by nothing but this
//! poll. They are not interchangeable in either direction: the viewer's cannot
//! see the holder's sessions, and the house's must never leave the house.

use std::time::Duration;

use sovereign_mesh::iroh_access::MediaRoute;

/// How often the origin is asked. Matched to the gossip round (10 s): a
/// faster poll cannot reach a peer sooner, and a slower one would let the
/// wall show "free" after the holder pressed play.
pub const POLL_INTERVAL: Duration = Duration::from_secs(10);

/// How long the origin is given to answer. Short on purpose — the origin is
/// whatever `[iroh] media_origin` names (`MediaRoute::parse` requires a
/// host:port and nothing more), normally a server on this machine, and a hung
/// origin must read as "did not answer" this round rather than stall the next.
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
    // The HOUSE credential, never the declared one. Jellyfin 12's
    // `GET /Sessions` scopes a non-administrator to the sessions it may
    // remote-control — `controllableByUserId` is the only user filter the
    // endpoint takes and there is no parameter that widens a caller
    // (`target/ralph/jf-inventory/openapi.json`, Jellyfin 12.0.0: `/Sessions`
    // parameters `controllableByUserId`/`deviceId`/`activeWithinSeconds`, and
    // `UserPolicy.EnableRemoteControlOfOtherUsers`, which the viewer account
    // does not have). Asked with the declared token the holder's own playback
    // is simply not in the answer, which reads as FREE — the room run's ~25
    // polls of `holder_playing=false` against a playing session
    // (`room-sessions.json`).
    let house = route.house();
    if house.is_empty() {
        tracing::debug!(
            %origin,
            "media presence: no house credential stored for this origin — publishing no presence"
        );
        return None;
    }
    let mut req = client
        .get(format!("http://{origin}/Sessions"))
        .timeout(ASK_TIMEOUT);
    for (name, value) in house.iter() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use commonwealth_media::{FREE, IN_USE};

    const HOUSE: &str = r#"MediaBrowser Token="house-key""#;
    const VIEWER_TOKEN: &str = r#"MediaBrowser Token="viewer-key""#;
    const VIEWER_ID: &str = "1111111111111111111111111111aaaa";
    const HOLDER_ID: &str = "2222222222222222222222222222bbbb";

    /// A stand-in for the origin that answers `GET /Sessions` the way
    /// Jellyfin 12 does: an administrator sees every session, and the
    /// read-only viewer is scoped to the sessions it may remote-control —
    /// its own. `UserPolicy.EnableRemoteControlOfOtherUsers` is what the
    /// viewer does not have, and `controllableByUserId` is the only user
    /// filter `/Sessions` takes, so there is no way for the viewer's token to
    /// see the row below (`target/ralph/jf-inventory/openapi.json`, 12.0.0).
    async fn origin_that_scopes_by_credential() -> std::net::SocketAddr {
        let app = axum::Router::new().route(
            "/Sessions",
            axum::routing::get(|headers: axum::http::HeaderMap| async move {
                let who = headers
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                let body = if who == HOUSE {
                    format!(
                        r#"[{{"UserId":"{HOLDER_ID}","NowPlayingItem":{{"Name":"A film"}}}},
                            {{"UserId":"{VIEWER_ID}","NowPlayingItem":null}}]"#
                    )
                } else {
                    format!(r#"[{{"UserId":"{VIEWER_ID}","NowPlayingItem":null}}]"#)
                };
                (
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    body,
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        addr
    }

    fn route(origin: std::net::SocketAddr, house: &str) -> MediaRoute {
        let route = MediaRoute::fixed(Some(origin), Vec::new());
        route.set_viewer_user(Some(VIEWER_ID.to_string()));
        // What every member's dial carries, and what this poll must NOT use.
        route.set_declared(vec![("authorization".into(), VIEWER_TOKEN.into())]);
        route.set_house(vec![("authorization".into(), house.into())]);
        route
    }

    /// Positive: asked with the house credential, the holder's playback is in
    /// the answer and the library reads as in use.
    #[tokio::test]
    async fn the_house_credential_sees_the_holder_playing() {
        let origin = origin_that_scopes_by_credential().await;
        let client = reqwest::Client::new();
        assert_eq!(
            read_presence(&client, &route(origin, HOUSE)).await,
            Some(IN_USE),
            "the holder is watching their own library; the poll must publish 0.0"
        );
    }

    /// Negative: the declared token is the read-only viewer's, and the origin
    /// shows it only its own sessions — so the same moment reads FREE. This
    /// is the defect the house store closes, stated as a test rather than as
    /// a comment.
    #[tokio::test]
    async fn the_declared_viewer_token_cannot_see_the_holder_playing() {
        let origin = origin_that_scopes_by_credential().await;
        let client = reqwest::Client::new();
        assert_eq!(
            read_presence(&client, &route(origin, VIEWER_TOKEN)).await,
            Some(FREE),
            "a viewer-scoped read sees no holder session, which is exactly why it must not \
             be what the poll asks with"
        );
    }

    /// Negative: no house credential is "could not ask", never FREE — a
    /// viewer must not start a stream on a missing answer (principle 6).
    #[tokio::test]
    async fn no_house_credential_publishes_no_presence() {
        let origin = origin_that_scopes_by_credential().await;
        let route = MediaRoute::fixed(Some(origin), Vec::new());
        route.set_viewer_user(Some(VIEWER_ID.to_string()));
        route.set_declared(vec![("authorization".into(), VIEWER_TOKEN.into())]);
        assert_eq!(
            read_presence(&reqwest::Client::new(), &route).await,
            None,
            "nothing to ask with must read as nobody answered"
        );
    }
}
