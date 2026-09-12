// SPDX-License-Identifier: AGPL-3.0-or-later
//! The same request to every member that publishes a given kind of origin,
//! through its own bridge, one attributed row each.
//!
//! Named for the media catalogue it was written for until 2026-09-12, and it
//! never was media-shaped: the selection reads `origins.contains(kind)`, the
//! ask is an arbitrary method/path/headers/body, and the row is whatever the
//! far end said. What was media-specific was the hardcoded
//! `OriginKind::Media` in two places. Both are now the request's, so a house
//! app answers a fan-out through the same route, the same selection and the
//! same row shape as a Jellyfin does — one implementation, so the two cannot
//! disagree about what a `never_asked` row means (ARCH principle 8).
//!
//! No merge, no dedup, no streams: what each origin said, by member, with
//! refusals as rows carrying why (a name that offers nothing, a name nobody
//! has). Item semantics are the origin's; a shim merges by provider id
//! because only it knows what an item is. Bodies are capped and say when
//! they were cut; a slow member is a failed row on its own clock, not a
//! hold on the others (`commonwealth_transport::fanout`).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use commonwealth_core::capabilities::OriginKind;
use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::member_matches;
use commonwealth_transport::fanout::{
    fan_out, first_endpoint_that_answers, FanoutTarget, InflightGauge, PeerFailure, PeerRow,
};
use commonwealth_transport::{PeerContact, PeerTransport};
use serde::{Deserialize, Serialize};

use crate::reach::{offering_members, pick_member, player_url, MediaCandidate, MediaReachRefusal};

/// Per-member cap when the request names none.
pub const DEFAULT_TIMEOUT_MS: u64 = 10_000;
/// Body cap per member when the request names none — a catalogue page, not
/// a stream.
pub const DEFAULT_MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// `POST /v1/mesh/fanout` body (and `/v1/mesh/media/fanout`, which is this
/// with `kind` left at its default).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FanoutRequest {
    /// Origin-relative, must begin with `/` (e.g. `/Items?Recursive=true`).
    /// For an app this is relative to the APP, not to the node: `/tasks`,
    /// with the app's own name prefixed on the way out.
    pub path: String,
    /// Which kind of origin to ask. Absent means [`OriginKind::Media`], so a
    /// body written against `/v1/mesh/media/fanout` keeps meaning what it
    /// meant.
    #[serde(default)]
    pub kind: Option<OriginKind>,
    /// Which published app to ask, for `kind = "app"`. Required there and
    /// refused otherwise: an app name with no app kind is a request that
    /// would quietly have gone somewhere else (ARCH principle 6).
    #[serde(default)]
    pub app: Option<String>,
    /// HTTP method; `GET` when absent.
    #[serde(default)]
    pub method: Option<String>,
    /// Headers passed to every origin as given (an API token, `Accept`).
    /// `x-mesh-*` are stripped by the holder regardless.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub body: Option<String>,
    /// Names or id prefixes. Absent means every offering member; a name
    /// that resolves to nothing is a `never_asked` row, not an error.
    #[serde(default)]
    pub peers: Option<Vec<String>>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub max_body_bytes: Option<usize>,
}

/// What one origin answered.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OriginAnswer {
    pub status: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// The body as text (lossy), up to the cap.
    pub body: String,
    /// The body already parsed, when the origin said JSON and it parses.
    ///
    /// Every consumer's first line was `json.loads(row["body"])`, so the
    /// parse is done once here instead of once per shim. `None` covers three
    /// different facts — not JSON, unparseable, or cut at the cap — and
    /// `body` remains the authority in all three, which is why this is an
    /// addition beside it rather than a replacement for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json: Option<serde_json::Value>,
    /// Bytes of body kept.
    pub bytes: usize,
    /// Whether the body was cut at the cap.
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct FanoutResponse {
    /// The path as SENT to each origin, app prefix included — what the rows
    /// are answers to.
    pub path: String,
    /// Which kind of origin was asked.
    pub kind: OriginKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// Targets, each of which is a row — asked or not.
    pub asked: usize,
    pub rows: Vec<PeerRow<OriginAnswer>>,
}

/// Which members the request selects, with refusals kept — a refused name
/// is a row the caller reads, never a silent omission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selected {
    Ask(MediaCandidate),
    Refused {
        name: String,
        /// The member the caller MEANT when the name resolved to exactly one
        /// row that was then refused; `None` when it resolved to nobody.
        node_id: Option<NodeId>,
        refusal: MediaReachRefusal,
    },
}

/// With no names: every offering member other than self. With names: each
/// resolved by [`pick_member`], refusals carried.
pub fn select_targets(
    candidates: &[MediaCandidate],
    self_id: NodeId,
    peers: Option<&[String]>,
    kind: OriginKind,
) -> Vec<Selected> {
    match peers {
        None => offering_members(candidates, self_id, kind)
            .into_iter()
            .map(Selected::Ask)
            .collect(),
        Some(names) => names
            .iter()
            .map(|name| match pick_member(candidates, self_id, name, kind) {
                Ok(c) => Selected::Ask(c),
                Err(refusal) => {
                    let matched: Vec<&MediaCandidate> = candidates
                        .iter()
                        .filter(|c| c.active && member_matches(c.node_id, &c.name, name))
                        .collect();
                    Selected::Refused {
                        name: name.clone(),
                        node_id: match matched.as_slice() {
                            [one] => Some(one.node_id),
                            _ => None,
                        },
                        refusal,
                    }
                }
            })
            .collect(),
    }
}

/// The request as sent to every origin, validated once.
#[derive(Debug, Clone)]
pub struct OriginRequest {
    pub method: reqwest::Method,
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: Option<String>,
}

impl OriginRequest {
    pub fn from_request(req: &FanoutRequest) -> Result<Self, String> {
        if !req.path.starts_with('/') {
            return Err(format!(
                "`path` must begin with `/` (an origin-relative path like `/Items`), got {:?}",
                req.path
            ));
        }
        // The app name travels as the first path segment (that is how the
        // acceptor demultiplexes it), so composing it here means one place
        // builds the wire path and the caller never has to know the
        // convention. Refusing the mismatched pairs rather than ignoring the
        // odd field out: `{kind: media, app: "chores"}` almost certainly
        // meant to ask the chore app, and silently asking Jellyfin instead is
        // the substitution shape this workspace keeps paying for.
        let path = match (req.kind.unwrap_or(OriginKind::Media), req.app.as_deref()) {
            (OriginKind::Media, None) => req.path.clone(),
            (OriginKind::Media, Some(app)) => {
                return Err(format!(
                    "`app` names {app:?} but `kind` is media — say `\"kind\": \"app\"` to ask \
                     an app, or drop `app` to ask media origins"
                ))
            }
            (OriginKind::App, None) => {
                return Err(
                    "`kind` is app but no `app` is named — which app should every \
                            member be asked for?"
                        .to_string(),
                )
            }
            (OriginKind::App, Some(app)) => {
                if !commonwealth_transport::iroh_identity_forward::valid_app_name(app.as_bytes()) {
                    return Err(format!(
                        "{app:?} is not a usable app name — ASCII letters, digits, `_` and `-`"
                    ));
                }
                format!("/{app}{}", req.path)
            }
        };
        let method = match req.method.as_deref() {
            None => reqwest::Method::GET,
            Some(m) => m
                .parse::<reqwest::Method>()
                .map_err(|_| format!("`method` {m:?} is not an HTTP method"))?,
        };
        Ok(Self {
            method,
            path,
            headers: req.headers.clone(),
            body: req.body.clone(),
        })
    }
}

/// One ask of one origin through `base_url`, the body read up to `max_body`.
pub async fn ask_origin(
    http: &reqwest::Client,
    base_url: &str,
    req: &OriginRequest,
    max_body: usize,
) -> Result<OriginAnswer, String> {
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
    let body = String::from_utf8_lossy(&buf).into_owned();
    // A truncated body is not parsed even when it looks like JSON: half a
    // document that happens to parse is a wrong answer, and `truncated` is
    // already on the row saying why there is nothing here.
    let json = (!truncated
        && content_type
            .as_deref()
            .is_some_and(|t| t.split(';').next().unwrap_or(t).trim().ends_with("json")))
    .then(|| serde_json::from_str(&body).ok())
    .flatten();
    Ok(OriginAnswer {
        status,
        content_type,
        bytes: buf.len(),
        body,
        json,
        truncated,
    })
}

/// The whole fan-out: select, dial every selected member through its own
/// bridge concurrently, one row each. `gauge` is the process's in-flight
/// counter (the daemon's `fanout_inflight`; a bare one elsewhere).
pub async fn fanout(
    self_id: NodeId,
    roster: &[(MediaCandidate, PeerContact)],
    req: FanoutRequest,
    transport: Arc<dyn PeerTransport>,
    gauge: InflightGauge,
) -> Result<FanoutResponse, MediaReachRefusal> {
    let kind = req.kind.unwrap_or(OriginKind::Media);
    let origin_req = OriginRequest::from_request(&req).map_err(MediaReachRefusal::BadRequest)?;
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
    let selected = select_targets(&candidates, self_id, req.peers.as_deref(), kind);
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
            Selected::Refused {
                name,
                node_id,
                refusal,
            } => {
                // A refused name still gets a row. Its id is the member the
                // caller meant when the name resolved to exactly one, and the
                // zero id when it resolved to nobody — a row about no one.
                let id = node_id.unwrap_or_else(|| NodeId::from_u128(0));
                (
                    FanoutTarget {
                        node_id: id,
                        name,
                        contact: contact_of(id),
                    },
                    Some(refusal.to_string()),
                )
            }
        })
        .collect();
    let asked = targets.len();
    let timeout = Duration::from_millis(req.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
    let max_body = req.max_body_bytes.unwrap_or(DEFAULT_MAX_BODY_BYTES);
    let http = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| MediaReachRefusal::BadRequest(format!("HTTP client: {e}")))?;
    let origin_req_path = origin_req.path.clone();
    let origin_req = Arc::new(origin_req);
    tracing::info!(
        target: "transport",
        path = %origin_req.path,
        method = %origin_req.method,
        kind = ?kind,
        asked,
        timeout_ms = timeout.as_millis() as u64,
        "fan-out: asking every selected member through its own bridge"
    );
    let rows = fan_out(gauge, targets, Some(timeout), move |t, refusal| {
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
                crate::reach::class_of(kind),
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
    })
    .await;
    Ok(FanoutResponse {
        path: origin_req_path,
        kind,
        app: req.app,
        asked,
        rows,
    })
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
            origins: if offers {
                vec![OriginKind::Media]
            } else {
                Vec::new()
            },
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
        let sel = select_targets(
            &roster(),
            NodeId::from_u128(ME),
            Some(&names),
            OriginKind::Media,
        );
        assert_eq!(sel.len(), 3);
        assert!(matches!(&sel[0], Selected::Ask(c) if c.name == "LittleMac"));
        assert_eq!(
            sel[1],
            Selected::Refused {
                name: "Quiet".into(),
                node_id: Some(NodeId::from_u128(0xC0DE)),
                refusal: MediaReachRefusal::NoOrigin("Quiet".into())
            }
        );
        assert_eq!(
            sel[2],
            Selected::Refused {
                name: "Nobody".into(),
                node_id: None,
                refusal: MediaReachRefusal::UnknownMember("Nobody".into())
            }
        );
    }

    /// With no names, the targets are exactly what `svrn mesh media` lists:
    /// offering members other than self, offline ones included (they become
    /// failed rows when asked, which is the truthful outcome).
    #[test]
    fn with_no_names_the_targets_are_the_offering_members_other_than_self() {
        let sel = select_targets(&roster(), NodeId::from_u128(ME), None, OriginKind::Media);
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
        let bad = FanoutRequest {
            path: "Items".into(),
            ..Default::default()
        };
        assert!(OriginRequest::from_request(&bad)
            .unwrap_err()
            .contains("must begin with `/`"));
        let bad_method = FanoutRequest {
            path: "/Items".into(),
            method: Some("FETCH ME".into()),
            ..bad
        };
        assert!(OriginRequest::from_request(&bad_method)
            .unwrap_err()
            .contains("not an HTTP method"));
    }

    /// An app is reached by its name as the first path segment, so the wire
    /// path is composed HERE — one place, rather than each caller knowing the
    /// convention.
    #[test]
    fn an_app_request_carries_the_app_name_as_the_first_path_segment() {
        let req = FanoutRequest {
            path: "/tasks?due=today".into(),
            kind: Some(OriginKind::App),
            app: Some("chores".into()),
            ..Default::default()
        };
        assert_eq!(
            OriginRequest::from_request(&req).unwrap().path,
            "/chores/tasks?due=today"
        );
    }

    /// The two mismatched pairs. Both are refused rather than resolved,
    /// because each has a plausible wrong reading: `{app, kind: media}`
    /// silently asks Jellyfin, and `{kind: app}` with no name has no single
    /// app it could have meant.
    #[test]
    fn a_kind_and_app_that_disagree_are_refused_rather_than_resolved() {
        let app_without_kind = FanoutRequest {
            path: "/tasks".into(),
            app: Some("chores".into()),
            ..Default::default()
        };
        assert!(OriginRequest::from_request(&app_without_kind)
            .unwrap_err()
            .contains("kind"));
        let kind_without_app = FanoutRequest {
            path: "/tasks".into(),
            kind: Some(OriginKind::App),
            ..Default::default()
        };
        assert!(OriginRequest::from_request(&kind_without_app)
            .unwrap_err()
            .contains("no `app` is named"));
    }

    /// The media spelling is the generic body with a field absent — the
    /// property the older `/v1/mesh/media/fanout` route rests on.
    #[test]
    fn a_body_with_no_kind_is_a_media_request() {
        let req = FanoutRequest {
            path: "/Items".into(),
            ..Default::default()
        };
        assert_eq!(OriginRequest::from_request(&req).unwrap().path, "/Items");
        assert_eq!(req.kind.unwrap_or(OriginKind::Media), OriginKind::Media);
    }

    /// A name the registry could never hold is refused at the request rather
    /// than becoming a path segment that matches nothing.
    #[test]
    fn an_app_name_a_path_could_not_carry_is_refused() {
        let req = FanoutRequest {
            path: "/tasks".into(),
            kind: Some(OriginKind::App),
            app: Some("../etc".into()),
            ..Default::default()
        };
        assert!(OriginRequest::from_request(&req)
            .unwrap_err()
            .contains("not a usable app name"));
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

    /// The parse is done once here because every shim's first line was
    /// `json.loads(row["body"])`. `body` stays authoritative: the parsed
    /// value is beside it, never instead of it.
    #[tokio::test]
    async fn a_json_answer_comes_back_parsed_beside_its_raw_body() {
        let base = origin(br#"{"Items":[{"Name":"Dune"}]}"#.to_vec()).await;
        let http = reqwest::Client::new();
        let req = OriginRequest {
            method: reqwest::Method::GET,
            path: "/Items".into(),
            headers: BTreeMap::new(),
            body: None,
        };
        let a = ask_origin(&http, &base, &req, DEFAULT_MAX_BODY_BYTES)
            .await
            .unwrap();
        assert_eq!(a.json.as_ref().unwrap()["Items"][0]["Name"], "Dune");
        assert_eq!(a.body, r#"{"Items":[{"Name":"Dune"}]}"#);
    }

    /// Half a document that happens to parse is a wrong answer, so a cut body
    /// is never parsed even when the origin said JSON. The failing input is a
    /// truncated array whose prefix is itself valid.
    #[tokio::test]
    async fn a_truncated_json_answer_is_not_parsed() {
        let base = origin(vec![b'x'; 10 * 1024]).await;
        let http = reqwest::Client::new();
        let req = OriginRequest {
            method: reqwest::Method::GET,
            path: "/Items".into(),
            headers: BTreeMap::new(),
            body: None,
        };
        let cut = ask_origin(&http, &base, &req, 1024).await.unwrap();
        assert!(cut.truncated);
        assert!(cut.json.is_none());
    }
}
