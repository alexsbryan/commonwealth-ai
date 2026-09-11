// SPDX-License-Identifier: AGPL-3.0-or-later
//! Federated media, the catalogue half: one request to every member that
//! offers a library, answered as one attributed document.
//!
//! `svrn mesh media` (bare) says WHO offers; `svrn mesh media <peer>` reaches
//! ONE of them. A shim building a merged catalogue needs the third shape:
//! ask them all the same thing — `GET /Items?…`, whatever the origin speaks —
//! and get back a row per member saying what it answered, or why it was not
//! asked, in one round trip. That is `POST /v1/mesh/media/fanout`, and it is
//! `commonwealth_api::fanout` (the peer half of the knowledge fan-out,
//! extracted 2026-09-11) with a media origin at the far end of each row.
//!
//! **What it does not do.** No merge, no dedup, no item schema: item
//! semantics are the origin's, and a shim that speaks Jellyfin merges better
//! than this repository ever could. No streams either — a body is capped per
//! member and the row says `truncated`; a title is played through the
//! per-member URL the verb prints. Rows are collected and returned once, under
//! a per-member timeout, so one stalled relay costs its own row and nothing
//! else's.
//!
//! **Every member named is a row.** `peers` naming a member that offers no
//! origin, is offline, or is this node gets a `never_asked` row carrying the
//! same refusal `svrn mesh media <peer>` would print — never an absence in the
//! list (the cloud-peer flight's lesson, note 60d4d79b). With no `peers`, the
//! targets are exactly the members `svrn mesh media` lists.
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{ConnectInfo, Extension, Json};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use commonwealth_api::fanout::{fan_out, first_endpoint_that_answers, FanoutTarget, PeerFailure};
use commonwealth_core::ids::NodeId;
use commonwealth_transport::{PeerContact, TrafficClass};
use serde::{Deserialize, Serialize};

use crate::daemon::EmbeddedDaemon;
use crate::loopback_guard::enforce_localhost;
use crate::media_reach::{
    offering_members, pick_member, player_url, MediaCandidate, MediaReachRefusal,
};

/// A member that has not answered in this long is a `failed` row; the
/// others are not waited on. Catalogue answers are small; a relay stall is not.
pub const DEFAULT_TIMEOUT_MS: u64 = 10_000;
/// Per-member body cap. A catalogue page is kilobytes; anything past this is
/// a stream and belongs on the per-member URL.
pub const DEFAULT_MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize)]
pub struct MediaFanoutRequest {
    /// The origin path, with query — `/Items?Recursive=true`. Must begin with `/`.
    pub path: String,
    /// HTTP method; `GET` when absent.
    #[serde(default)]
    pub method: Option<String>,
    /// Headers sent to every origin. `x-mesh-*` is the acceptor's namespace
    /// and is stripped at the holder whatever is put here.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Request body as text, sent to every origin.
    #[serde(default)]
    pub body: Option<String>,
    /// Which members to ask, by name or ≥4-char id prefix. Absent: every
    /// member that offers a media origin.
    #[serde(default)]
    pub peers: Option<Vec<String>>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub max_body_bytes: Option<usize>,
}

/// What one origin answered. `body` is the response as text (lossy UTF-8),
/// cut at the cap with `truncated` set; `bytes` is what was read.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MediaAnswer {
    pub status: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    pub body: String,
    pub bytes: usize,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct MediaFanoutResponse {
    pub path: String,
    /// Targets, each of which is a row — asked or not.
    pub asked: usize,
    pub rows: Vec<commonwealth_api::fanout::PeerRow<MediaAnswer>>,
}

/// One target of a fan-out, decided from the roster before anything is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selected {
    /// A member to ask.
    Ask(MediaCandidate),
    /// A member the caller named that the roster refuses, and why.
    Refused {
        name: String,
        refusal: MediaReachRefusal,
    },
}

/// Which members a fan-out asks. Without `peers`, the members `svrn mesh media`
/// lists; with it, each name resolved exactly as `svrn mesh media <peer>`
/// resolves it, a refusal becoming a row rather than a dropped name.
pub fn select_targets(
    candidates: &[MediaCandidate],
    self_id: NodeId,
    peers: Option<&[String]>,
) -> Vec<Selected> {
    match peers {
        None => offering_members(candidates, self_id)
            .into_iter()
            .map(Selected::Ask)
            .collect(),
        Some(names) => names
            .iter()
            .map(|name| match pick_member(candidates, self_id, name) {
                Ok(c) => Selected::Ask(c),
                Err(refusal) => Selected::Refused {
                    name: name.clone(),
                    refusal,
                },
            })
            .collect(),
    }
}

/// The one request, ready to send to any origin.
#[derive(Debug, Clone)]
pub struct OriginRequest {
    pub method: reqwest::Method,
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: Option<String>,
}

impl OriginRequest {
    /// Validate a wire request: the path must be origin-relative and the
    /// method must be one HTTP knows. Refused by name, never defaulted.
    pub fn from_request(req: &MediaFanoutRequest) -> Result<Self, String> {
        if !req.path.starts_with('/') {
            return Err(format!(
                "`path` must begin with `/` (an origin-relative path like `/Items`), got {:?}",
                req.path
            ));
        }
        let method = match req.method.as_deref() {
            None => reqwest::Method::GET,
            Some(m) => m
                .parse::<reqwest::Method>()
                .map_err(|_| format!("`method` {m:?} is not an HTTP method"))?,
        };
        Ok(Self {
            method,
            path: req.path.clone(),
            headers: req.headers.clone(),
            body: req.body.clone(),
        })
    }
}

/// Send `req` to the origin behind `base_url` and read the answer, at most
/// `max_body` bytes of it. Any transport or HTTP-level failure is a reason,
/// and a non-2xx status is still an ANSWER (the row carries the status), so
/// a shim can tell "this member's origin said 401" from "this member is
/// unreachable".
pub async fn ask_origin(
    http: &reqwest::Client,
    base_url: &str,
    req: &OriginRequest,
    max_body: usize,
) -> Result<MediaAnswer, String> {
    let url = format!("{base_url}{}", req.path);
    let mut builder = http.request(req.method.clone(), &url);
    for (k, v) in &req.headers {
        builder = builder.header(k, v);
    }
    if let Some(body) = &req.body {
        builder = builder.body(body.clone());
    }
    let mut resp = builder
        .send()
        .await
        .map_err(|e| format!("request to {url} failed: {e}"))?;
    let status = resp.status().as_u16();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let mut buf: Vec<u8> = Vec::new();
    let mut truncated = false;
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                let room = max_body.saturating_sub(buf.len());
                if chunk.len() > room {
                    buf.extend_from_slice(&chunk[..room]);
                    truncated = true;
                    break;
                }
                buf.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(e) => return Err(format!("reading the answer from {url} failed: {e}")),
        }
    }
    Ok(MediaAnswer {
        status,
        content_type,
        bytes: buf.len(),
        body: String::from_utf8_lossy(&buf).into_owned(),
        truncated,
    })
}

impl EmbeddedDaemon {
    /// Ask every selected member the same request through its own media
    /// bridge, concurrently, and return one row per member.
    pub async fn media_fanout(
        &self,
        req: MediaFanoutRequest,
    ) -> Result<MediaFanoutResponse, MediaReachRefusal> {
        let origin_req =
            OriginRequest::from_request(&req).map_err(MediaReachRefusal::BadRequest)?;
        let app_state = self.app_state().await.ok_or(MediaReachRefusal::NoMesh)?;
        let self_id = app_state.self_node_id();
        let roster = crate::media_reach::roster_candidates(&app_state).await;
        let candidates: Vec<MediaCandidate> = roster.iter().map(|(c, _)| c.clone()).collect();
        let contact_of = |id: NodeId| -> PeerContact {
            roster
                .iter()
                .find(|(c, _)| c.node_id == id)
                .map(|(_, contact)| contact.clone())
                .unwrap_or_else(|| PeerContact {
                    node_id: id,
                    addresses: Vec::new(),
                    node_pubkey: None,
                    relay_url: None,
                    iroh_direct_addrs: Vec::new(),
                })
        };
        let selected = select_targets(&candidates, self_id, req.peers.as_deref());
        let targets: Vec<(FanoutTarget, Option<String>)> = selected
            .into_iter()
            .map(|s| match s {
                Selected::Ask(c) => (
                    FanoutTarget {
                        contact: contact_of(c.node_id),
                        node_id: c.node_id,
                        name: c.name,
                    },
                    None,
                ),
                Selected::Refused { name, refusal } => (
                    FanoutTarget {
                        // A refused name may match nothing; the row still
                        // needs an identity, and the name the caller used
                        // is the honest one.
                        node_id: NodeId::from_u128(0),
                        name,
                        contact: contact_of(NodeId::from_u128(0)),
                    },
                    Some(refusal.to_string()),
                ),
            })
            .collect();
        let timeout = Duration::from_millis(req.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
        let max_body = req.max_body_bytes.unwrap_or(DEFAULT_MAX_BODY_BYTES);
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| MediaReachRefusal::BadRequest(format!("HTTP client: {e}")))?;
        let transport = app_state.peer_transport();
        let origin_req = Arc::new(origin_req);
        let asked = targets.len();
        tracing::info!(
            target: "transport",
            path = %origin_req.path,
            method = %origin_req.method,
            asked,
            timeout_ms = timeout.as_millis() as u64,
            "media fan-out: asking every selected member through its own bridge"
        );
        let rows = fan_out(
            app_state.inner.clone(),
            targets,
            Some(timeout),
            move |t, refusal| {
                let http = http.clone();
                let transport = transport.clone();
                let origin_req = origin_req.clone();
                async move {
                    if let Some(why) = refusal {
                        return Err(PeerFailure::NeverAsked(why));
                    }
                    let name = t.name.clone();
                    first_endpoint_that_answers(
                        &transport,
                        t.node_id,
                        &t.contact,
                        TrafficClass::Media,
                        |ep| {
                            let http = http.clone();
                            let origin_req = origin_req.clone();
                            let name = name.clone();
                            async move {
                                let url = player_url(&name, &ep).map_err(|e| e.to_string())?;
                                ask_origin(&http, &url, &origin_req, max_body).await
                            }
                        },
                    )
                    .await
                }
            },
        )
        .await;
        Ok(MediaFanoutResponse {
            path: req.path,
            asked,
            rows,
        })
    }
}

/// `POST /v1/mesh/media/fanout` — loopback-only like every `/v1/mesh/*`
/// route. A malformed request is 400 with the reason; no mesh is 409.
pub async fn mesh_media_fanout(
    ConnectInfo(caller): ConnectInfo<std::net::SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Json(req): Json<MediaFanoutRequest>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&caller) {
        return r;
    }
    match daemon.media_fanout(req).await {
        Ok(doc) => (StatusCode::OK, Json(serde_json::json!(doc))).into_response(),
        Err(e @ MediaReachRefusal::BadRequest(_)) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonwealth_core::mesh::NodeStatus;

    fn cand(id: u128, name: &str, status: NodeStatus, offers: bool) -> MediaCandidate {
        MediaCandidate {
            node_id: NodeId::from_u128(id),
            name: name.into(),
            status,
            has_identity: true,
            active: true,
            offers_media: offers,
        }
    }

    const ME: u128 = 0xA11CE;

    fn roster() -> Vec<MediaCandidate> {
        vec![
            cand(ME, "RuggedFox", NodeStatus::Online, true),
            cand(0xB0B, "LittleMac", NodeStatus::Online, true),
            cand(0xB0B0, "BeefyMac", NodeStatus::Offline, true),
            cand(0xC0DE, "Quiet", NodeStatus::Online, false),
        ]
    }

    /// Every name the caller gives is a row: a member that offers nothing is
    /// `Refused(NoOrigin)`, an unknown name `Refused(UnknownMember)`, and the
    /// offering one is asked. The failing input is a refused name vanishing
    /// from the list, which is what a filter would do.
    #[test]
    fn every_named_member_is_a_target_and_a_refused_one_says_why() {
        let names = vec!["LittleMac".to_string(), "Quiet".into(), "Nobody".into()];
        let sel = select_targets(&roster(), NodeId::from_u128(ME), Some(&names));
        assert_eq!(sel.len(), 3);
        assert!(matches!(&sel[0], Selected::Ask(c) if c.name == "LittleMac"));
        assert_eq!(
            sel[1],
            Selected::Refused {
                name: "Quiet".into(),
                refusal: MediaReachRefusal::NoOrigin("Quiet".into())
            }
        );
        assert_eq!(
            sel[2],
            Selected::Refused {
                name: "Nobody".into(),
                refusal: MediaReachRefusal::UnknownMember("Nobody".into())
            }
        );
    }

    /// With no names, the targets are exactly what `svrn mesh media` lists:
    /// offering members other than self, offline ones included (they become
    /// failed rows when asked, which is the truthful outcome).
    #[test]
    fn with_no_names_the_targets_are_the_offering_members_other_than_self() {
        let sel = select_targets(&roster(), NodeId::from_u128(ME), None);
        let names: Vec<&str> = sel
            .iter()
            .map(|s| match s {
                Selected::Ask(c) => c.name.as_str(),
                Selected::Refused { name, .. } => name.as_str(),
            })
            .collect();
        assert_eq!(names, vec!["BeefyMac", "LittleMac"]);
    }

    #[test]
    fn a_request_is_validated_by_name() {
        let bad = MediaFanoutRequest {
            path: "Items".into(),
            method: None,
            headers: BTreeMap::new(),
            body: None,
            peers: None,
            timeout_ms: None,
            max_body_bytes: None,
        };
        assert!(OriginRequest::from_request(&bad)
            .unwrap_err()
            .contains("must begin with `/`"));
        let bad_method = MediaFanoutRequest {
            path: "/Items".into(),
            method: Some("FETCH ME".into()),
            ..bad
        };
        assert!(OriginRequest::from_request(&bad_method)
            .unwrap_err()
            .contains("not an HTTP method"));
    }

    async fn origin(body: Vec<u8>) -> String {
        use axum::routing::get;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new().route(
            "/Items",
            get(move || {
                let body = body.clone();
                async move {
                    (
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        body,
                    )
                }
            }),
        );
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    /// The row carries what the origin said — status, type, bytes — and a
    /// body past the cap is cut with `truncated` set rather than read whole.
    /// The failing input is the cap ignored: 10 KiB read against a 1 KiB cap.
    #[tokio::test]
    async fn an_answer_is_read_up_to_the_cap_and_says_when_it_was_cut() {
        let base = origin(vec![b'x'; 10 * 1024]).await;
        let http = reqwest::Client::new();
        let req = OriginRequest {
            method: reqwest::Method::GET,
            path: "/Items".into(),
            headers: BTreeMap::new(),
            body: None,
        };
        let whole = ask_origin(&http, &base, &req, DEFAULT_MAX_BODY_BYTES)
            .await
            .unwrap();
        assert_eq!(whole.status, 200);
        assert_eq!(whole.content_type.as_deref(), Some("application/json"));
        assert_eq!(whole.bytes, 10 * 1024);
        assert!(!whole.truncated);

        let cut = ask_origin(&http, &base, &req, 1024).await.unwrap();
        assert_eq!(cut.bytes, 1024);
        assert!(cut.truncated);
        assert_eq!(cut.body.len(), 1024);

        // A path the origin does not serve is still an ANSWER, with its status.
        let missing = OriginRequest {
            path: "/Nope".into(),
            ..req
        };
        let gone = ask_origin(&http, &base, &missing, 1024).await.unwrap();
        assert_eq!(gone.status, 404);
    }
}
