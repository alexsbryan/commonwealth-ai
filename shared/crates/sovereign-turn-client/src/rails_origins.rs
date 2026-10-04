// SPDX-License-Identifier: AGPL-3.0-or-later
//! Registering a loopback origin in cw-rails' origin table
//! (`POST /v1/mesh/origins`, pb-rails-origins) and keeping it there: the one
//! register/renew loop svrn's work origin and serve's origins share. It moved
//! here from the daemon's `work_origin::keep_registered` and
//! `rails_client::{register_origin, renew_origin}`
//! (pb-serve-distributes-standalone) because serve cannot link the daemon,
//! and both link this crate beside [`crate::rails_kv::resolve_rails_base`].

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use oicp_types::capabilities::NodeCapabilities;
use oicp_types::origin::{OriginClaim, OriginRegistration};
use tracing::{debug, info, warn};

/// The trace target every event below rides; serve's filter lists it.
pub const TRACE_TARGET: &str = "rails_origins";

/// A registration dial is loopback coordination: slower is unreachable.
const DIAL_TIMEOUT: Duration = Duration::from_secs(5);

fn client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(DIAL_TIMEOUT)
            .build()
            .expect("a plain reqwest client")
    })
}

/// POST `body` to `path` on cw-rails and read the answer. `Err` names the URL
/// and what went wrong: no answer, a refusal with cw-rails' own wording, or a
/// body this build cannot read.
async fn post<T: serde::de::DeserializeOwned>(
    base: &str,
    path: &str,
    body: &impl serde::Serialize,
) -> Result<T, String> {
    let url = format!("{}{path}", base.trim_end_matches('/'));
    let resp = client()
        .post(&url)
        .json(body)
        .send()
        .await
        .map_err(|e| format!("cw-rails did not answer at {url}: {e}"))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("cw-rails' answer at {url} is unreadable: {e}"))?;
    if !status.is_success() {
        return Err(format!("cw-rails refused {url}: {status}: {text}"));
    }
    serde_json::from_str(&text)
        .map_err(|e| format!("cw-rails' answer at {url} is a shape this build cannot read: {e}"))
}

/// Register one loopback origin; the claim holds its id and its tie.
pub async fn register_origin(
    base: &str,
    registration: &OriginRegistration,
) -> Result<OriginClaim, String> {
    post(base, "/v1/mesh/origins", registration).await
}

/// Push a registered origin's deadline out by `ttl_secs`. `Some(claims)`
/// replaces the claim's declaration; `None` keeps it.
pub async fn renew_origin(
    base: &str,
    claim_id: &str,
    ttl_secs: u64,
    claims: Option<&NodeCapabilities>,
) -> Result<(), String> {
    renew_origin_declaring(base, claim_id, ttl_secs, claims, None).await
}

/// [`renew_origin`], and `Some(namespaces)` replaces the ring namespaces the
/// claim declares; `None` keeps them.
pub async fn renew_origin_declaring(
    base: &str,
    claim_id: &str,
    ttl_secs: u64,
    claims: Option<&NodeCapabilities>,
    namespaces: Option<&[String]>,
) -> Result<(), String> {
    let _: serde_json::Value = post(
        base,
        &format!("/v1/mesh/origins/{claim_id}/renew"),
        &serde_json::json!({ "ttl_secs": ttl_secs, "claims": claims, "namespaces": namespaces }),
    )
    .await?;
    Ok(())
}

/// What a registrant declares about the node NOW, read at every register and
/// renew so the declaration moves with the registrant's state.
pub type ClaimsSource =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = NodeCapabilities> + Send>> + Send + Sync>;

/// The ring namespaces a registrant drains NOW, read at every register and
/// renew: cw-rails buffers a live payload only for a namespace some
/// registration declares (commonwealth-rails ring_routes.rs `ring_live`).
pub type NamespacesSource = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

/// Register, then renew every `every` for `ttl_secs`; a renew cw-rails
/// refuses (it restarted, or the claim lapsed) registers again. cw-rails being
/// absent is named once at `warn` and then at `debug`, so a node that runs no
/// cw-rails is told once why nothing reaches this origin. Runs until dropped.
pub async fn keep_registered(
    rails_base: String,
    registration: OriginRegistration,
    ttl_secs: u64,
    every: Duration,
) {
    keep_registered_tied(rails_base, registration, ttl_secs, every, None).await
}

/// [`keep_registered`], publishing the live claim's tie on `tie`: `Some`
/// while cw-rails holds the claim, `None` from a refused renew until the
/// next registration takes. An origin that reads `x-mesh-*` believes it only
/// on a forward carrying this tie (`kernel_types::member::ORIGIN_TIE_HEADER`).
/// The tie is a secret: it goes to the channel and nowhere else, never to a
/// trace.
pub async fn keep_registered_tied(
    rails_base: String,
    registration: OriginRegistration,
    ttl_secs: u64,
    every: Duration,
    tie: Option<tokio::sync::watch::Sender<Option<String>>>,
) {
    keep_registered_declaring(rails_base, registration, ttl_secs, every, tie, None).await
}

/// [`keep_registered_tied`], declaring `claims`' answer at every register and
/// renew in place of the registration's fixed `claims`. `None` declares what
/// the registration carries at register and keeps it on every renew.
pub async fn keep_registered_declaring(
    rails_base: String,
    registration: OriginRegistration,
    ttl_secs: u64,
    every: Duration,
    tie: Option<tokio::sync::watch::Sender<Option<String>>>,
    claims: Option<ClaimsSource>,
) {
    keep_registered_with(rails_base, registration, ttl_secs, every, tie, claims, None).await
}

/// [`keep_registered_declaring`], declaring `namespaces`' answer at every
/// register and renew in place of the registration's fixed `namespaces`.
pub async fn keep_registered_with(
    rails_base: String,
    mut registration: OriginRegistration,
    ttl_secs: u64,
    every: Duration,
    tie: Option<tokio::sync::watch::Sender<Option<String>>>,
    claims: Option<ClaimsSource>,
    namespaces: Option<NamespacesSource>,
) {
    let slot = registration.alpn.clone();
    let publish = |value: Option<String>| {
        if let Some(tx) = &tie {
            tx.send_replace(value);
        }
    };
    let mut claim: Option<String> = None;
    let mut told_absent = false;
    loop {
        match &claim {
            None => {
                if let Some(source) = &claims {
                    registration.claims = Some(source().await);
                }
                if let Some(source) = &namespaces {
                    registration.namespaces = source();
                }
                match register_origin(&rails_base, &registration).await {
                    Ok(c) => {
                        info!(target: TRACE_TARGET, claim = %c.claim_id, %slot,
                          prefixes = ?registration.prefixes, port = registration.port,
                          namespaces = ?registration.namespaces,
                          tie_published = tie.is_some(),
                          "origin registered with cw-rails");
                        publish(Some(c.tie));
                        claim = Some(c.claim_id);
                        told_absent = false;
                    }
                    Err(e) if !told_absent => {
                        warn!(target: TRACE_TARGET, error = %e, %slot,
                          "cw-rails did not take the origin's registration, so nothing reaches \
                           it through the mesh; retrying");
                        told_absent = true;
                    }
                    Err(e) => debug!(target: TRACE_TARGET, error = %e, %slot,
                                 "origin registration still not taken; retrying"),
                }
            }
            Some(id) => {
                let declared = match &claims {
                    Some(source) => Some(source().await),
                    None => None,
                };
                let held = namespaces.as_ref().map(|source| source());
                if let Err(e) =
                    renew_origin_declaring(&rails_base, id, ttl_secs, declared.as_ref(), held.as_deref())
                        .await
                {
                    info!(target: TRACE_TARGET, claim = %id, %slot, error = %e,
                          "the origin's renew was refused — registering again");
                    publish(None);
                    claim = None;
                    continue;
                }
                debug!(target: TRACE_TARGET, claim = %id, %slot,
                       declares = declared.is_some(), namespaces = ?held, "origin renewed");
            }
        }
        tokio::time::sleep(every).await;
    }
}

/// Publish one app in cw-rails' app registry (`POST /v1/mesh/publish`) for
/// the members `allow` names (empty = every member), then renew it every
/// `every` for `ttl_secs`; a refused renew publishes again. The app tier's
/// [`keep_registered`]: `cwth/app/0` is the app registry's, never an origin
/// registration (phase-b-81 (3)). Runs until dropped.
pub async fn keep_published(
    rails_base: String,
    name: String,
    port: u16,
    allow: Vec<String>,
    ttl_secs: u64,
    every: Duration,
) {
    #[derive(serde::Deserialize)]
    struct Claimed {
        claim_id: String,
    }
    let mut claim: Option<String> = None;
    let mut told_absent = false;
    loop {
        match &claim {
            None => {
                let body = serde_json::json!({
                    "name": name, "port": port, "ttl_secs": ttl_secs, "allow": allow,
                });
                match post::<Claimed>(&rails_base, "/v1/mesh/publish", &body).await {
                    Ok(c) => {
                        info!(target: TRACE_TARGET, claim = %c.claim_id, app = %name, port,
                              allow = ?allow, "app published with cw-rails");
                        claim = Some(c.claim_id);
                        told_absent = false;
                    }
                    Err(e) if !told_absent => {
                        warn!(target: TRACE_TARGET, error = %e, app = %name,
                              "cw-rails did not take the app's publish, so no member reaches \
                               it; retrying");
                        told_absent = true;
                    }
                    Err(e) => debug!(target: TRACE_TARGET, error = %e, app = %name,
                                     "app publish still not taken; retrying"),
                }
            }
            Some(id) => {
                let renewed: Result<serde_json::Value, String> = post(
                    &rails_base,
                    &format!("/v1/mesh/publish/{id}/renew"),
                    &serde_json::json!({ "ttl_secs": ttl_secs }),
                )
                .await;
                if let Err(e) = renewed {
                    info!(target: TRACE_TARGET, claim = %id, app = %name, error = %e,
                          "the app's renew was refused — publishing again");
                    claim = None;
                    continue;
                }
                debug!(target: TRACE_TARGET, claim = %id, app = %name, "app renewed");
            }
        }
        tokio::time::sleep(every).await;
    }
}

#[cfg(test)]
#[path = "rails_origins/tests.rs"]
mod tests;
