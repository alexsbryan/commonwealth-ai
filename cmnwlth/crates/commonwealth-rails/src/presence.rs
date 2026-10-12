// SPDX-License-Identifier: AGPL-3.0-or-later
//! The holder's media-presence poll, run by THIS process — "is somebody in
//! this house watching the library right now?", asked of the origin this
//! config declares and published into this node's own gossip.
//!
//! Five-programs fp-46 (§12 decision 2): the serving cluster owns the media
//! verbs, and the poll belongs beside the media origin it reads — the
//! inference daemon polling Jellyfin was a component holding another's
//! lifecycle. Rails is a SECOND CALLER of `commonwealth_media`'s decisions,
//! never a second decider (ARCH 8): the session verdict is
//! [`commonwealth_media::media_available_from_sessions`], the house
//! credential's home is [`commonwealth_media::house_dir_under`], and this
//! module only owns WHEN to ask and WHERE the answer lands
//! (`NodeCapabilities::media_available`, `gossip.rs`).
//!
//! The two inputs, and where each lives:
//!
//! - **the origin** — `[media] origin` in `rails.toml` (a `SocketAddr`; a
//!   node that declares none offers nothing and this poll idles);
//! - **the house credential** — `<data_dir>/secrets/media-house/`
//!   (0600 files named for their header; `commonwealth_media::declared`).
//!   Re-read every tick, so an operator who places or rotates the credential
//!   needs no restart — the same property `declare` then `offer` has on the
//!   inference daemon.
//!
//! The **viewer account** — the origin's id for the read-only user every
//! member arrives as — lives beside the credential as
//! `secrets/media-house/viewer_user` ([`commonwealth_media::VIEWER_FILE`],
//! read and written only through that crate). Both are written there by
//! `svrn mesh media offer`, which resolves this process's default data dir
//! through the same [`commonwealth_media::rails_data_dir`]. Missing any input, the
//! poll publishes `None` — nobody answered — rather than guessing at `FREE`,
//! because a viewer starts a stream on `FREE` (principle 6).

use std::sync::Arc;
use std::time::Duration;

use commonwealth_media::house_dir_under;

use crate::RailsDaemon;

/// How often the origin is asked. Matched to the gossip round (10 s): a
/// faster poll cannot reach a peer sooner, and a slower one would let the
/// wall show "free" after the holder pressed play.
pub const POLL_INTERVAL: Duration = Duration::from_secs(10);

/// How long the origin is given to answer. Short on purpose — the origin is
/// normally a server on this machine, and a hung origin must read as "did not
/// answer" this round rather than stall the next.
const ASK_TIMEOUT: Duration = Duration::from_secs(3);

/// Read the poll's two credential-store inputs: the house headers and the
/// viewer account id. Absent files are empty arms, never errors — a node
/// nobody has offered for simply has an empty dir.
fn read_credentials(daemon: &RailsDaemon) -> (Vec<(String, String)>, Option<String>) {
    commonwealth_media::read_house_in(&house_dir_under(&daemon.node.data_dir))
}

/// Ask the origin once and turn its answer into what this node publishes.
///
/// `Ok(Some(v))` is a real reading; `Ok(None)` is an honest "could not ask".
/// There is no error case on purpose: every failure here is a `None` the
/// caller publishes, and collapsing it into an `Err` the caller would then
/// have to re-widen is how a refusal becomes a default.
async fn read_presence(
    client: &reqwest::Client,
    origin: std::net::SocketAddr,
    house: &[(String, String)],
    viewer: &str,
) -> Option<f32> {
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
    for (name, value) in house {
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
    match commonwealth_media::media_available_from_sessions(&body, viewer) {
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

/// One tick: derive the inputs from this process's own config and secret
/// store, ask the origin, and leave the reading in the shared cell the
/// gossip round stamps from.
async fn tick(daemon: &RailsDaemon, client: &reqwest::Client) -> Option<f32> {
    let Some(origin) = daemon.media().origin else {
        // A node with no origin offers nothing; the poll idles. Debug, not
        // warn — this is the config absence with a visible consequence, not
        // a failure.
        tracing::debug!(target: "rails", "media presence: no [media] origin declared — idle");
        return None;
    };
    let (house, viewer) = read_credentials(daemon);
    let Some(viewer) = viewer else {
        tracing::debug!(
            %origin,
            "media presence: no viewer account declared for this origin — publishing no presence"
        );
        return None;
    };
    read_presence(client, origin, &house, &viewer).await
}

/// Loop until the task is dropped. Writes the cell every tick (a write is
/// free) and logs only a CHANGE, so the transition is one line in the log
/// rather than one every ten seconds.
pub async fn run_forever(daemon: Arc<RailsDaemon>) {
    let client = match reqwest::Client::builder().build() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(target: "rails", error = %e, "media presence: no HTTP client — the poll will not run");
            return;
        }
    };
    tracing::info!(
        target: "rails",
        interval_s = POLL_INTERVAL.as_secs(),
        "media presence: poll started"
    );
    let mut last: Option<Option<f32>> = None;
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        if daemon.is_solo() {
            continue;
        }
        let now = tick(&daemon, &client).await;
        if last != Some(now) {
            tracing::info!(target: "rails", ?now, "media presence: reading changed");
            last = Some(now);
        }
        if let Ok(mut cell) = daemon.media_presence.write() {
            *cell = now;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUSE: &str = r#"MediaBrowser Token="house-key""#;
    const VIEWER_TOKEN: &str = r#"MediaBrowser Token="viewer-key""#;
    const VIEWER_ID: &str = "1111111111111111111111111111aaaa";
    const HOLDER_ID: &str = "2222222222222222222222222222bbbb";

    /// A stand-in for the origin that answers `GET /Sessions` the way
    /// Jellyfin 12 does: an administrator sees every session, and the
    /// read-only viewer is scoped to the sessions it may remote-control —
    /// its own. `controllableByUserId` is the only user filter `/Sessions`
    /// takes, so there is no way for the viewer's token to see the holder's
    /// row — which is exactly why the poll must ask with the HOUSE credential.
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

    /// Positive: asked with the house credential, the holder's playback is in
    /// the answer and the library reads as in use.
    #[tokio::test]
    async fn the_house_credential_sees_the_holder_playing() {
        let origin = origin_that_scopes_by_credential().await;
        let client = reqwest::Client::new();
        let house = vec![("authorization".into(), HOUSE.into())];
        assert_eq!(
            read_presence(&client, origin, &house, VIEWER_ID).await,
            Some(commonwealth_media::IN_USE),
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
        let viewer_token = vec![("authorization".into(), VIEWER_TOKEN.into())];
        assert_eq!(
            read_presence(&client, origin, &viewer_token, VIEWER_ID).await,
            Some(commonwealth_media::FREE),
            "a viewer-scoped read sees no holder session, which is exactly why it must not \
             be what the poll asks with"
        );
    }

    /// Negative: no house credential is "could not ask", never FREE — a
    /// viewer must not start a stream on a missing answer (principle 6).
    #[tokio::test]
    async fn no_house_credential_publishes_no_presence() {
        let origin = origin_that_scopes_by_credential().await;
        assert_eq!(
            read_presence(&reqwest::Client::new(), origin, &[], VIEWER_ID).await,
            None,
            "nothing to ask with must read as nobody answered"
        );
    }
}
