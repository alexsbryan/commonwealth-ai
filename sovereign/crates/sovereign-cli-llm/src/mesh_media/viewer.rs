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
//! be a user's own `AccessToken`. `offer` FINDS that user or creates it, gives
//! it a policy that says no to everything a viewer has no business doing,
//! READS THE POLICY BACK, and declares that token in place of the admin key. A
//! policy that does not read back as asked is a refusal, not a warning: an
//! unchecked write here is a library handed over on the strength of a 200.
//!
//! Two shapes of the origin's API decide how this is written, and both were
//! learned by being refused (2026-09-19, run under `RING_ROOM_TOPOLOGY=room`):
//! `POST /Users/{id}/Policy` REPLACES a policy rather than patching it, so
//! what is sent must be the origin's own document with this verb's keys
//! written over it ([`merge_read_only`]); and [`VIEWER_NAME`] is fixed, so
//! `POST /Users/New` collides on the second offer and the account must be
//! looked up first ([`viewer_id_in`]).
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

/// The viewer's id in a `GET /Users` listing, if this origin already holds
/// the account. Matched on `Name` because that is what `POST /Users/New`
/// collides on.
fn viewer_id_in(users: &serde_json::Value) -> Option<String> {
    users.as_array()?.iter().find_map(|u| {
        (u.get("Name").and_then(serde_json::Value::as_str)? == VIEWER_NAME)
            .then(|| u.get("Id").and_then(serde_json::Value::as_str))
            .flatten()
            .map(str::to_string)
    })
}

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

/// Every key this verb decides, and what it decides them to. Applied ON TOP
/// of the policy the origin already holds, never sent alone — see
/// [`merge_read_only`].
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

/// Fields `UserPolicy` declares `required` with `minLength: 1`, so a partial
/// document is refused whole. Read from the running origin's own OpenAPI
/// (`target/ralph/jf-inventory/openapi.json`, Jellyfin 12.0.0:
/// `components.schemas.UserPolicy.required`), not from memory.
///
/// They are provider NAMES — which authentication and password-reset plugin
/// owns this account. This verb has no business choosing them, so it carries
/// the origin's own values forward and refuses if the origin did not state
/// them: inventing one would silently re-home the account on a provider
/// nobody asked for.
const PROVIDER_IDS: [&str; 2] = ["AuthenticationProviderId", "PasswordResetProviderId"];

/// The document `POST /Users/{id}/Policy` actually takes: the policy the
/// origin handed back, with [`read_only_policy`]'s keys written over it.
///
/// `POST .../Policy` REPLACES a user's whole policy — it is not a patch. Sent
/// the bare read-only keys, Jellyfin 12 answers 400 naming the two required
/// provider ids and writes nothing, which is what shipped until now: the
/// account stayed at its creation defaults and `offer` declared no viewer at
/// all (measured 2026-09-19, `target/ring-room-rr2-demo/room-holder-setup-little.out:10`).
fn merge_read_only(base: &serde_json::Value) -> Result<serde_json::Value, String> {
    let base = base
        .get("Policy")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "the origin returned a user with no Policy to build on".to_string())?;
    for key in PROVIDER_IDS {
        match base.get(key).and_then(serde_json::Value::as_str) {
            Some(v) if !v.is_empty() => {}
            _ => {
                return Err(format!(
                    "the origin's policy does not state {key}, which it requires on the way \
                     back in — this verb will not invent one"
                ))
            }
        }
    }
    let mut merged = base.clone();
    for (key, value) in read_only_policy()
        .as_object()
        .expect("read_only_policy builds an object")
    {
        merged.insert(key.clone(), value.clone());
    }
    Ok(serde_json::Value::Object(merged))
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

/// The viewer as it already stands, when `declared` IS this account's own
/// token — the case on every offer after the first.
///
/// `offer` REPLACES the install credential with the viewer's
/// (`mesh_media.rs:650-652`), so a second offer reaches here holding a
/// read-only token and no elevation at all. Everything the mint path does
/// next needs an administrator: measured 2026-09-19 against Jellyfin 12.0.0,
/// `POST /Users/Password` answered `403 "Invalid user or password entered."`
/// — that string is the non-admin branch of `UpdateUserPassword`, which
/// verifies `CurrentPw` only when the caller is NOT an administrator. So the
/// reuse path could never complete, and clause (f) of the room's film bar
/// could never pass (`target/ring-room-rr2-demo/room-offer.out`).
///
/// Nothing needs minting in that case. `GET /Users/Me` answers both questions
/// this verb has — who the declared credential is, and what that account may
/// do — in one request, so the account is proven rather than rebuilt.
/// Returns `None` when the credential is not this viewer's, which sends the
/// caller down the mint path where a missing elevation fails loudly.
async fn already_provisioned(
    client: &reqwest::Client,
    declared: &[(String, String)],
    origin: SocketAddr,
    found: &str,
) -> Option<Result<Viewer, String>> {
    // The single header the caller re-declares. A set with no `authorization`,
    // or more than one credential in it, is not one this verb can hand back.
    let credential = match declared {
        [(name, value)] if name == "authorization" => value.clone(),
        _ => {
            tracing::debug!(
                %origin,
                declared_headers = declared.len(),
                "media offer: the declaration is not one `authorization` header — taking the mint path"
            );
            return None;
        }
    };
    let me = match call(
        client,
        declared,
        reqwest::Method::GET,
        origin,
        "/Users/Me",
        None,
    )
    .await
    {
        Ok(body) => body,
        Err(e) => {
            tracing::debug!(
                %origin,
                error = %e,
                "media offer: `GET /Users/Me` did not answer for the declared credential — taking the mint path"
            );
            return None;
        }
    };
    let me: serde_json::Value = match serde_json::from_str(&me) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(
                %origin,
                error = %e,
                "media offer: `/Users/Me` did not answer JSON — taking the mint path"
            );
            return None;
        }
    };
    let declared_user = me.get("Id").and_then(serde_json::Value::as_str);
    if declared_user != Some(found) {
        tracing::debug!(
            %origin,
            ?declared_user,
            viewer_user = %found,
            "media offer: the declared credential is not this viewer's — taking the mint path"
        );
        return None;
    }
    // From here it IS the viewer's token, so this verb holds no elevation and
    // cannot repair anything. A policy that does not read back as asked is a
    // refusal, not a fall-through to a write that would only 403.
    if let Err(e) = policy_is_read_only(&me) {
        return Some(Err(format!(
            "{e} — and the credential declared here is the viewer's own, which cannot \
             change a policy; re-declare an administrator's with `svrn mesh media declare \
             authorization` and offer again"
        )));
    }
    tracing::info!(
        %origin,
        viewer_user = %found,
        "media offer: the declared credential is already this viewer's own and its policy \
         reads back read-only — nothing to mint"
    );
    Some(Ok(Viewer {
        id: found.to_string(),
        credential,
    }))
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

    // FIND, then create. `VIEWER_NAME` is fixed so a second `offer` reuses
    // the one account -- but `POST /Users/New` answers 400 on a name that
    // already exists, so an unconditional create makes every offer after the
    // first fail (measured 2026-09-19,
    // `target/ring-room-rr2-demo/room-offer.out:4`).
    let existing = call(
        &client,
        elevated,
        reqwest::Method::GET,
        origin,
        "/Users",
        None,
    )
    .await?;
    let existing: serde_json::Value = serde_json::from_str(&existing)
        .map_err(|e| format!("the origin's user list is not JSON ({e})"))?;
    let found = viewer_id_in(&existing);

    let id = match &found {
        Some(id) => {
            if let Some(done) = already_provisioned(&client, elevated, origin, id).await {
                return done;
            }
            tracing::debug!(
                viewer_user = %id,
                name = VIEWER_NAME,
                "media offer: viewer account found, reusing it"
            );
            id.clone()
        }
        None => {
            let created = call(
                &client,
                elevated,
                reqwest::Method::POST,
                origin,
                "/Users/New",
                Some(serde_json::json!({ "Name": VIEWER_NAME, "Password": password })),
            )
            .await?;
            let created: serde_json::Value = serde_json::from_str(&created).map_err(|e| {
                format!("the origin's new-user answer is not JSON ({e}): {created}")
            })?;
            let id = created
                .get("Id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| format!("the origin created a user with no Id: {created}"))?
                .to_string();
            tracing::debug!(
                viewer_user = %id,
                name = VIEWER_NAME,
                "media offer: no viewer account on this origin, created one"
            );
            id
        }
    };

    if found.is_some() {
        tracing::debug!(
            viewer_user = %id,
            "media offer: resetting the reused viewer's password — this process keeps none"
        );
        // An account left by an earlier offer, whose password this process
        // never kept (see the module header -- the token is the durable half).
        // The elevated credential sets the one invented above so the
        // `AuthenticateByName` below has something to spend.
        call(
            &client,
            elevated,
            reqwest::Method::POST,
            origin,
            &format!("/Users/Password?userId={id}"),
            Some(serde_json::json!({ "NewPw": password, "ResetPassword": false })),
        )
        .await?;
    }

    // The policy to send is the origin's own, with this verb's keys written
    // over it -- `POST .../Policy` replaces rather than patches. See
    // [`merge_read_only`].
    let before = call(
        &client,
        elevated,
        reqwest::Method::GET,
        origin,
        &format!("/Users/{id}"),
        None,
    )
    .await?;
    let before: serde_json::Value = serde_json::from_str(&before)
        .map_err(|e| format!("the origin's user document is not JSON ({e}): {before}"))?;

    call(
        &client,
        elevated,
        reqwest::Method::POST,
        origin,
        &format!("/Users/{id}/Policy"),
        Some(merge_read_only(&before)?),
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

    /// A stand-in origin that routes by path and records what was asked of
    /// it. A raw listener rather than a test-server dependency: the contract
    /// under test is which requests this verb makes, which is exactly what
    /// the recording answers.
    struct Origin {
        addr: SocketAddr,
        asked: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    /// `routes` maps an EXACT path to the document it answers with. Anything
    /// else answers `403 "Invalid user or password entered."` — which is what
    /// the live origin answers a non-admin caller, so a verb that reaches for
    /// something a viewer's own token cannot have fails here exactly as it
    /// failed there. Exact, not prefix: `/Users` as a prefix would quietly
    /// serve `/Users/Password` too, and the mint path would walk right past
    /// the request that is the whole defect.
    async fn origin(routes: Vec<(&'static str, serde_json::Value)>) -> Origin {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = asked.clone();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let routes = routes.clone();
                let recorder = recorder.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let head = String::from_utf8_lossy(&buf[..n]).to_string();
                    let line = head.lines().next().unwrap_or_default().to_string();
                    recorder.lock().unwrap().push(line.clone());
                    let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                    let (status, body) = match routes.iter().find(|(p, _)| &path == p) {
                        Some((_, body)) => ("200 OK", body.to_string()),
                        None => (
                            "403 Forbidden",
                            "\"Invalid user or password entered.\"".to_string(),
                        ),
                    };
                    let resp = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: \
                         {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        Origin { addr, asked }
    }

    fn declared(value: &str) -> Vec<(String, String)> {
        vec![("authorization".to_string(), value.to_string())]
    }

    fn viewer_user(id: &str, policy: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "Id": id, "Name": VIEWER_NAME, "Policy": policy })
    }

    /// THE DEFECT THIS CLOSES. `offer` replaces the install credential with
    /// the viewer's own (`mesh_media.rs:650-652`), so every offer after the
    /// first reaches `provision` holding a read-only token. The mint path
    /// needs an administrator at every step, and the first of them —
    /// `POST /Users/Password` — answered `403 "Invalid user or password
    /// entered."` on the live origin, which is the non-admin branch of
    /// Jellyfin 12's `UpdateUserPassword` (an administrator skips the
    /// `CurrentPw` check). So `offer` declared no viewer and the room's film
    /// bar could not read "in use" (clause (f), three runs of three:
    /// `target/ring-room-rr2-demo/room-offer.out`).
    ///
    /// The account is proven, not rebuilt: this asserts the credential comes
    /// back unchanged AND that nothing on the mint path was ever requested.
    #[tokio::test]
    async fn a_second_offer_proves_the_viewer_instead_of_re_minting_it() {
        let o = origin(vec![
            ("/Users/Me", viewer_user("abc123", read_only_policy())),
            (
                "/Users",
                serde_json::json!([viewer_user("abc123", read_only_policy())]),
            ),
        ])
        .await;
        let viewer = provision(o.addr, &declared("tok"))
            .await
            .expect("a declared viewer token with a read-only policy provisions");
        assert_eq!(viewer.id, "abc123");
        assert_eq!(viewer.credential, "tok");
        let asked = o.asked.lock().unwrap().clone();
        assert!(
            !asked.iter().any(|l| l.contains("/Users/Password")),
            "the mint path was walked with a non-admin credential: {asked:?}"
        );
        assert!(
            !asked.iter().any(|l| l.contains("/Policy")),
            "a policy write was attempted with a credential that cannot make one: {asked:?}"
        );
    }

    /// The credential belongs to somebody else on the origin, so this verb
    /// has learned nothing about the viewer: `None` sends the caller down the
    /// mint path, where a missing elevation fails loudly rather than here.
    #[tokio::test]
    async fn a_credential_for_another_account_does_not_shortcut() {
        let o = origin(vec![(
            "/Users/Me",
            viewer_user("someone-else", read_only_policy()),
        )])
        .await;
        assert!(
            already_provisioned(&client().unwrap(), &declared("tok"), o.addr, "abc123")
                .await
                .is_none()
        );
    }

    /// It is the viewer's token, so this verb holds no elevation and cannot
    /// repair the policy. Principle 6: refuse and name it, never fall through
    /// to a write that can only 403.
    #[tokio::test]
    async fn a_viewer_whose_policy_is_not_read_only_is_refused_not_repaired() {
        let mut policy = read_only_policy();
        policy["IsAdministrator"] = serde_json::Value::Bool(true);
        let o = origin(vec![("/Users/Me", viewer_user("abc123", policy))]).await;
        let err = match already_provisioned(&client().unwrap(), &declared("tok"), o.addr, "abc123")
            .await
        {
            // `Viewer` has no `Debug` on purpose — it holds the credential —
            // so the Err is taken by match rather than `expect_err`.
            Some(Err(e)) => e,
            Some(Ok(_)) => panic!("an administrator policy was accepted as read-only"),
            None => panic!("the shortcut did not apply"),
        };
        assert!(err.contains("IsAdministrator"), "{err}");
        assert!(err.contains("svrn mesh media declare"), "{err}");
    }

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

    /// A policy document as Jellyfin 12 actually hands one back: the keys
    /// this verb decides, plus the two it requires and this verb does not own.
    fn origin_policy() -> serde_json::Value {
        serde_json::json!({ "Policy": {
            "AuthenticationProviderId":
                "Jellyfin.Server.Implementations.Users.DefaultAuthenticationProvider",
            "PasswordResetProviderId":
                "Jellyfin.Server.Implementations.Users.DefaultPasswordResetProvider",
            "IsAdministrator": true,
            "EnableContentDeletion": true,
            "EnableCollectionManagement": true,
            "EnableSubtitleManagement": true,
            "EnableLyricManagement": true,
            "EnableMediaPlayback": false,
            "EnableLiveTvManagement": true,
            "MaxActiveSessions": 3
        }})
    }

    /// The document sent to `POST /Users/{id}/Policy` carries the origin's own
    /// provider ids forward. Sent without them, Jellyfin 12 answers 400 and
    /// writes NOTHING -- the account keeps whatever rights it had, and the
    /// offer publishes no viewer. That is the defect measured on
    /// 2026-09-19 (`room-holder-setup-little.out:10`), and this is the
    /// assertion that goes red if the partial document comes back.
    #[test]
    fn the_merged_policy_carries_the_origins_required_provider_ids() {
        let merged = merge_read_only(&origin_policy()).expect("a stated policy merges");
        for key in PROVIDER_IDS {
            assert_eq!(
                merged.get(key).and_then(serde_json::Value::as_str),
                origin_policy()["Policy"][key].as_str(),
                "{key} must reach the origin unchanged, or the write is refused whole"
            );
        }
    }

    /// The merge OVERRULES an administrator: `POST .../Policy` replaces the
    /// document, so every key this verb decides must be in what it sends.
    #[test]
    fn the_merged_policy_overrules_every_right_a_viewer_must_not_have() {
        let merged = merge_read_only(&origin_policy()).expect("a stated policy merges");
        policy_is_read_only(&serde_json::json!({ "Policy": merged.clone() }))
            .expect("what is sent must satisfy what is checked");
        assert_eq!(
            merged.get("EnableMediaPlayback"),
            Some(&serde_json::json!(true))
        );
        assert_eq!(
            merged.get("EnableLiveTvManagement"),
            Some(&serde_json::json!(false))
        );
        // Keys this verb does not decide survive: replacing the document must
        // not quietly drop the holder's own settings.
        assert_eq!(merged.get("MaxActiveSessions"), Some(&serde_json::json!(3)));
    }

    /// Negative: an origin that did not state a provider id is refused by
    /// name rather than sent a guess (ARCH principle 6).
    #[test]
    fn a_policy_missing_a_provider_id_is_refused_not_invented() {
        let mut doc = origin_policy();
        doc["Policy"]
            .as_object_mut()
            .unwrap()
            .remove("PasswordResetProviderId");
        let got = merge_read_only(&doc).unwrap_err();
        assert!(
            got.contains("PasswordResetProviderId"),
            "the refusal must name the field the origin left unstated: {got}"
        );
    }

    /// A second `offer` FINDS the account the first one made. Without this,
    /// `POST /Users/New` collides on the name and answers 400 -- every offer
    /// after the first fails (`room-offer.out:4`, 2026-09-19).
    #[test]
    fn a_viewer_left_by_an_earlier_offer_is_found_not_recreated() {
        let users = serde_json::json!([
            { "Name": "alex", "Id": "aaa" },
            { "Name": VIEWER_NAME, "Id": "bbb" },
        ]);
        assert_eq!(viewer_id_in(&users).as_deref(), Some("bbb"));
    }

    /// Negative: an origin holding no such account reports absence, so the
    /// create path runs. "Not found" is not "found something else".
    #[test]
    fn an_origin_without_the_viewer_reports_absence() {
        let users = serde_json::json!([{ "Name": "alex", "Id": "aaa" }]);
        assert_eq!(viewer_id_in(&users), None);
    }
}
