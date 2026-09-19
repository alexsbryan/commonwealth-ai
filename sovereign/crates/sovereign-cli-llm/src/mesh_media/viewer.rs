// SPDX-License-Identifier: AGPL-3.0-or-later
//! The read-only account every member reaches this library as.
//!
//! # The defect this closes
//!
//! Until this landed, a holder declared an `/Auth/Keys` API key — and that key
//! is ADMIN-EQUIVALENT: measured 2026-09-19 against a live Jellyfin 12.0.0, a
//! key minted that way has a zero `UserId`, passes elevation, and answers
//! `GET /Auth/Keys` with 200. So every housemate's request arrived at the
//! origin holding the run of the place: delete a library, add a collection,
//! mint more keys. `media_allow` decided WHO reached the origin and nothing
//! decided WHAT they could do once there.
//!
//! An API key cannot be minted FOR a user, so the read-only credential has to
//! be a user's own `AccessToken`. `offer` creates that user once, with a
//! policy that says no to everything a viewer has no business doing, READS
//! THE POLICY BACK, and declares that token in place of the admin key. A
//! policy that does not read back as asked is a refusal, not a warning: an
//! unchecked write here is a library handed over on the strength of a 200.
//!
//! # What the person types
//!
//! Nothing. The elevated credential is the one the INSTALL stage already
//! declared (`svrn mesh media declare`, run by the wizard); `offer` spends it
//! once and replaces it. The viewer's password is generated here, used once
//! for `AuthenticateByName`, and never stored — the token is what is kept.

use std::net::SocketAddr;
use std::time::Duration;

/// The member-facing account and the credential that reaches it.
pub(crate) struct Viewer {
    /// The account's id in the origin's own user space — what
    /// `[iroh] media_viewer_user` records so the presence poll can tell the
    /// holder's sessions from the house's.
    pub(crate) id: String,
    /// The whole `authorization` header value viewers' requests carry.
    pub(crate) credential: String,
}

/// The name the account is created under. Fixed so a second `offer` finds the
/// same one rather than growing a user per run.
const VIEWER_NAME: &str = "commonwealth-mesh";

/// Every policy key a viewer must not have. Read back after the write and
/// compared one by one, so a key the origin silently ignored is caught here
/// rather than by a housemate deleting a film.
const MUST_BE_FALSE: [&str; 5] = [
    "IsAdministrator",
    "EnableContentDeletion",
    "EnableCollectionManagement",
    "EnableSubtitleManagement",
    "EnableLyricManagement",
];

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("could not build an HTTP client: {e}"))
}

/// One request to the origin carrying the elevated credential.
async fn call(
    client: &reqwest::Client,
    elevated: &[(String, String)],
    method: reqwest::Method,
    origin: SocketAddr,
    path: &str,
    body: Option<serde_json::Value>,
) -> Result<String, String> {
    let mut req = client.request(method.clone(), format!("http://{origin}{path}"));
    for (name, value) in elevated {
        req = req.header(name, value);
    }
    if let Some(body) = body {
        req = req.json(&body);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| format!("{method} {path} could not be sent: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("{method} {path} answered {status}: {text}"));
    }
    Ok(text)
}

/// The policy a viewer gets: playback, and nothing that changes the library.
fn read_only_policy() -> serde_json::Value {
    let mut policy = serde_json::Map::new();
    for key in MUST_BE_FALSE {
        policy.insert(key.to_string(), serde_json::Value::Bool(false));
    }
    policy.insert(
        "EnableLiveTvManagement".to_string(),
        serde_json::Value::Bool(false),
    );
    policy.insert(
        "EnableMediaPlayback".to_string(),
        serde_json::Value::Bool(true),
    );
    serde_json::Value::Object(policy)
}

/// Check a policy document the origin handed back. Every key in
/// [`MUST_BE_FALSE`] must be present AND false: a key the origin did not
/// echo is "did not answer", which is not "answered: no".
fn policy_is_read_only(user: &serde_json::Value) -> Result<(), String> {
    let policy = user
        .get("Policy")
        .ok_or_else(|| "the origin returned a user with no Policy to check".to_string())?;
    for key in MUST_BE_FALSE {
        match policy.get(key).and_then(serde_json::Value::as_bool) {
            Some(false) => {}
            Some(true) => return Err(format!("the origin kept {key} = true on the viewer")),
            None => {
                return Err(format!(
                    "the origin's policy does not name {key} — it cannot be read as false"
                ))
            }
        }
    }
    Ok(())
}

/// Create (or find) the read-only viewer on `origin`, prove its policy, and
/// return the credential viewers' requests will carry.
///
/// `elevated` is the credential the install stage declared — the only thing
/// here that can create a user. It is spent once; the caller replaces the
/// declaration with [`Viewer::credential`], which cannot.
pub(crate) async fn provision(
    origin: SocketAddr,
    elevated: &[(String, String)],
) -> Result<Viewer, String> {
    if elevated.is_empty() {
        return Err(
            "no credential is declared for this origin, so no viewer account can be created — \
             `svrn mesh media declare authorization` first"
                .to_string(),
        );
    }
    let client = client()?;
    // A password this process invents, uses once, and forgets. It is never
    // stored: the token is the durable half, and a password on disk would be
    // a second way in that nobody asked for.
    let password: String = uuid::Uuid::new_v4().simple().to_string();

    let created = call(
        &client,
        elevated,
        reqwest::Method::POST,
        origin,
        "/Users/New",
        Some(serde_json::json!({ "Name": VIEWER_NAME, "Password": password })),
    )
    .await?;
    let created: serde_json::Value = serde_json::from_str(&created)
        .map_err(|e| format!("the origin's new-user answer is not JSON ({e}): {created}"))?;
    let id = created
        .get("Id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("the origin created a user with no Id: {created}"))?
        .to_string();

    call(
        &client,
        elevated,
        reqwest::Method::POST,
        origin,
        &format!("/Users/{id}/Policy"),
        Some(read_only_policy()),
    )
    .await?;

    // READ IT BACK. The write answering 204 says the request was accepted,
    // not that the policy is what was asked for.
    let back = call(
        &client,
        elevated,
        reqwest::Method::GET,
        origin,
        &format!("/Users/{id}"),
        None,
    )
    .await?;
    let back: serde_json::Value = serde_json::from_str(&back)
        .map_err(|e| format!("the origin's user document is not JSON ({e}): {back}"))?;
    policy_is_read_only(&back)?;

    let authed = call(
        &client,
        elevated,
        reqwest::Method::POST,
        origin,
        "/Users/AuthenticateByName",
        Some(serde_json::json!({ "Username": VIEWER_NAME, "Pw": password })),
    )
    .await?;
    let authed: serde_json::Value = serde_json::from_str(&authed)
        .map_err(|e| format!("the origin's authenticate answer is not JSON ({e})"))?;
    let token = authed
        .get("AccessToken")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "the origin authenticated the viewer without an AccessToken".to_string())?;

    tracing::info!(
        %origin,
        viewer_user = %id,
        "media offer: read-only viewer account provisioned and its policy read back"
    );
    Ok(Viewer {
        id,
        credential: format!("MediaBrowser Token=\"{token}\""),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Positive: the policy this verb writes says false to every key the
    /// read-back checks, so the two cannot drift apart.
    #[test]
    fn the_policy_written_satisfies_the_policy_checked() {
        let user = serde_json::json!({ "Policy": read_only_policy() });
        assert_eq!(policy_is_read_only(&user), Ok(()));
    }

    /// Negative: an origin that kept administrator rights is refused by name.
    #[test]
    fn an_administrator_viewer_is_refused() {
        let mut policy = read_only_policy();
        policy["IsAdministrator"] = serde_json::Value::Bool(true);
        let got = policy_is_read_only(&serde_json::json!({ "Policy": policy }));
        assert!(
            got.unwrap_err().contains("IsAdministrator"),
            "the refusal must name the key that stayed true"
        );
    }

    /// Negative: a key the origin did not echo is NOT read as false —
    /// "did not answer" is not "answered: no" (ARCH principle 6).
    #[test]
    fn a_policy_key_the_origin_omitted_is_not_read_as_false() {
        let mut policy = read_only_policy();
        policy
            .as_object_mut()
            .unwrap()
            .remove("EnableContentDeletion");
        let got = policy_is_read_only(&serde_json::json!({ "Policy": policy }));
        assert!(
            got.unwrap_err().contains("EnableContentDeletion"),
            "an omitted key must be refused by name"
        );
    }

    /// Negative: a user document with no policy at all is a refusal, not a
    /// pass.
    #[test]
    fn a_user_with_no_policy_is_refused() {
        let got = policy_is_read_only(&serde_json::json!({ "Id": "x" }));
        assert!(got.is_err());
    }
}
