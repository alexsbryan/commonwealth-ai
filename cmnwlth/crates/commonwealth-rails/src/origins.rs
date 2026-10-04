// SPDX-License-Identifier: AGPL-3.0-or-later
//! The endpoint serves every program's registered loopback origin to members
//! (FIVE_PROGRAMS §4 rule 8; phase-b pb-rails-origins). This module is the
//! loopback half of that: the registration doors over
//! [`OriginRegistry`], this endpoint's own standing entries, the ring
//! namespaces a registration holds, and the claims it declares into gossip.
//!
//! `POST /v1/mesh/origins` · `GET /v1/mesh/origins` ·
//! `POST /v1/mesh/origins/{claim_id}/renew` · `DELETE /v1/mesh/origins/{claim_id}`.
//! Loopback is the auth, as for every door here (`api`). The registration's
//! answer carries its tie once; the listing never does.
//!
//! **What cw-rails owns here is the table, never the origin** (rule 8). Each
//! program registers and renews its own; one that stops renewing drops at
//! its TTL, as an app claim does.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use commonwealth_core::capabilities::NodeCapabilities;
use commonwealth_media::origins::{Admit, OriginRefusal, OriginRegistration, OriginRegistry};
use commonwealth_transport::iroh::{ALPN, MEDIA_ALPN};
use serde::Deserialize;

use crate::rail::MembershipRosterSource;
use crate::RailsDaemon;

/// The endpoint's own origins, which no claim holds: its gossip and join
/// routes on `cwth/http/0` (any dialer — a joiner is not a member yet, and
/// the gossip merge authorizes for itself), its ring routes under
/// `/internal/ring` (members only), and the media origin
/// `rails.toml` declares, for members inside `media.allow`, with the
/// credentials declared for it.
pub fn stand_own(
    registry: &OriginRegistry,
    internal: SocketAddr,
    media: Option<SocketAddr>,
    media_allow: Vec<String>,
    media_declared: Vec<(String, String)>,
) -> Result<(), OriginRefusal> {
    registry.stand(
        ALPN,
        &["/internal/gossip", "/internal/join"],
        internal,
        Admit::Any,
        Vec::new(),
    )?;
    // The ring's peer routes (pb-rails-parity): members only; each ring then
    // asks its own roster about the stamped key (`crate::ring_routes`).
    registry.stand(
        ALPN,
        &["/internal/ring"],
        internal,
        Admit::Members(Vec::new()),
        Vec::new(),
    )?;
    if let Some(origin) = media {
        registry.stand(
            MEDIA_ALPN,
            &[],
            origin,
            Admit::Members(media_allow),
            media_declared,
        )?;
    }
    Ok(())
}

/// `GET /v1/mesh/origins` — what this endpoint serves, with no tie.
pub async fn listing(State(daemon): State<Arc<RailsDaemon>>) -> impl IntoResponse {
    Json(serde_json::json!({ "origins": daemon.origins.listing() }))
}

/// `POST /v1/mesh/origins` — register a loopback origin; the answer carries
/// the claim id and the tie.
pub async fn register(
    State(daemon): State<Arc<RailsDaemon>>,
    Json(req): Json<OriginRegistration>,
) -> Response {
    let namespaces = req.namespaces.clone();
    let claim = match daemon.origins.register(req) {
        Ok(c) => c,
        Err(e) => return refused(e),
    };
    if let Err(e) = hold_namespaces(&daemon, &namespaces) {
        // The registration named a ring this rail cannot hold, so it is not
        // half-made: withdrawn, and the rail's reason returned.
        if let Err(gone) = daemon.origins.release(&claim.claim_id) {
            tracing::warn!(target: "rails", error = %gone,
                           "origins: the refused registration was already gone when withdrawn");
        }
        tracing::info!(target: "rails", error = %e, "origins: registration refused on a namespace");
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e })),
        )
            .into_response();
    }
    (StatusCode::OK, Json(serde_json::json!(claim))).into_response()
}

/// `POST /v1/mesh/origins/{claim_id}/renew` body.
#[derive(Debug, Deserialize)]
pub struct Renew {
    #[serde(default)]
    pub ttl_secs: Option<u64>,
    /// Replaces the claim's declaration; absent keeps it.
    #[serde(default)]
    pub claims: Option<NodeCapabilities>,
    /// Replaces the ring namespaces the claim declares; absent keeps them.
    #[serde(default)]
    pub namespaces: Option<Vec<String>>,
}

/// `POST /v1/mesh/origins/{claim_id}/renew`
pub async fn renew(
    State(daemon): State<Arc<RailsDaemon>>,
    Path(claim_id): Path<String>,
    body: Option<Json<Renew>>,
) -> Response {
    let (ttl_secs, claims, namespaces) =
        body.map_or((None, None, None), |Json(b)| (b.ttl_secs, b.claims, b.namespaces));
    let ttl = crate::api::ttl_of(ttl_secs);
    match daemon
        .origins
        .renew_declaring(&claim_id, ttl, claims, namespaces)
    {
        Ok(secs) => Json(serde_json::json!({ "claim_id": claim_id, "expires_in_secs": secs }))
            .into_response(),
        Err(e) => refused(e),
    }
}

/// `DELETE /v1/mesh/origins/{claim_id}`
pub async fn release(
    State(daemon): State<Arc<RailsDaemon>>,
    Path(claim_id): Path<String>,
) -> Response {
    match daemon.origins.release(&claim_id) {
        Ok(slots) => Json(serde_json::json!({ "released": slots })).into_response(),
        Err(e) => refused(e),
    }
}

/// A taken slot is a 409, a malformed registration a 400, an unknown claim a
/// 404 — the publish doors' mapping, so a registrant reads one contract.
fn refused(e: OriginRefusal) -> Response {
    let code = match e {
        OriginRefusal::Taken { .. } => StatusCode::CONFLICT,
        OriginRefusal::NoSuchClaim(_) => StatusCode::NOT_FOUND,
        OriginRefusal::BadAlpn(_) | OriginRefusal::BadPrefix(_) | OriginRefusal::PrefixShape => {
            StatusCode::BAD_REQUEST
        }
    };
    tracing::info!(target: "rails", status = code.as_u16(), error = %e, "origins: refused");
    (code, Json(serde_json::json!({ "error": e.to_string() }))).into_response()
}

/// Every ring namespace a registered program writes on its own behalf is
/// answered by membership from now on, so no `roster.json` narrows it — the
/// registered-namespace rule, installed where registration lives (seat
/// 2026-09-26, serve-mesh-census.md §8.2). Held for the process's life: the
/// rail has no un-derive, and a namespace that stays membership-derived after
/// its writer leaves never under-shares.
fn hold_namespaces(daemon: &RailsDaemon, namespaces: &[String]) -> Result<(), String> {
    for ns in namespaces {
        let source = Arc::new(MembershipRosterSource {
            mesh: Arc::downgrade(&daemon.mesh),
            self_id: daemon.node.self_id,
            self_pubkey: Some(daemon.node.pubkey()),
        });
        daemon
            .rail
            .derive_roster(ns, source)
            .map_err(|e| format!("namespace `{ns}`: {e}"))?;
        tracing::info!(target: "rails", namespace = %ns,
                       "origins: a registered program writes this ring — no roster.json narrows it");
    }
    Ok(())
}

/// What this node gossips about itself: the endpoint's own zeroed report,
/// plus what every live registration declares (rule 8: the advertisement is
/// the registrants', never cw-rails' own guess).
///
/// Lists concatenate, flags OR, availability takes the highest, and a
/// single-valued field takes the first declaration that sets it. Hardware
/// and live load are the node's, measured by cw-rails (`measured`, read when
/// any registration declares); a declaration only adjusts them, through
/// [`crate::self_measure::apply`].
pub fn merge_declared(
    caps: &mut NodeCapabilities,
    measured: Option<&crate::self_measure::SelfMeasurement>,
    declared: &[NodeCapabilities],
) {
    if let Some(measured) = measured {
        crate::self_measure::apply(caps, measured, declared);
    }
    for d in declared {
        caps.active_processes
            .extend(d.active_processes.iter().cloned());
        caps.hosted_corpora.extend(d.hosted_corpora.iter().cloned());
        for model in &d.loaded_models {
            if !caps.loaded_models.contains(model) {
                caps.loaded_models.push(model.clone());
            }
        }
        caps.inference_capable |= d.inference_capable;
        caps.inference_availability = caps.inference_availability.max(d.inference_availability);
        caps.embed_model = caps.embed_model.take().or_else(|| d.embed_model.clone());
        caps.benchmark = caps.benchmark.take().or_else(|| d.benchmark.clone());
        caps.current_in_flight = caps.current_in_flight.or(d.current_in_flight);
        caps.anchor = caps.anchor.take().or_else(|| d.anchor.clone());
    }
}
