// SPDX-License-Identifier: AGPL-3.0-or-later
//! The operator-side rail client — **one** read and **one** write, for every
//! CLI that touches a ring.
//!
//! # Why it lives here and not beside `svrn ring`
//!
//! It was `ring_cmd`'s, and while `ring` and `job` were the only callers that
//! was the right home. cw-lift 5e added a third — `svrn quality check
//! --distribute` submits this repository's own CI as work units — and that
//! verb lives in `sovereign-cli`, a different crate. The choice there is
//! between one client in a crate both already link and a second one written
//! against the same two routes, and §10.6 settles it: two clients disagree
//! about what a 422 body says, about which port they trust, and eventually
//! about the wire form of an act.
//!
//! # It goes over HTTP rather than opening the journal
//!
//! The reason is `seq`. The daemon serialises appends behind one writer lock
//! per namespace; a second process picking its own next sequence number from
//! its own read would race it, both would land on the same `seq`, and the
//! fork is reported by every node forever.
//!
//! # The target is the AUDITED accessor, not a hardcoded loopback
//!
//! `ring_cmd` built `http://127.0.0.1:{port}` from `SetupConfig.daemon
//! .client_port`, which is a third answer to "where is the daemon" beside
//! `client_daemon_base()` and the env knob it honours — so the sandbox lane
//! pointing `SOVEREIGN_DAEMON_URL` at its own isolated daemon did not move
//! `ring` or `job`, and they acted on the operator's ring instead. Here the
//! base comes from [`crate::urls::daemon_base_url`], the accessor every other
//! reader uses. A base that is not loopback simply fails at the door: these
//! routes are mounted on the operator surface and trust the listener.

use std::collections::BTreeMap;

use commonwealth_rail_core::{Admission, AdmittedOp, Payload, RailAct, RailGap, Roster};

/// The rail's three routes, spelled once for every caller in the workspace.
///
/// The FUNCTIONS below are the clients. `svrn ring dev` cannot use them — it
/// proxies a browser's opaque bytes to a different listener under a grant
/// token — but it must not spell the paths a second time either, so it uses
/// these. A route renamed on the daemon then breaks the build at every caller
/// rather than at runtime on whichever one is exercised first.
pub const RAIL_LOG_PATH: &str = "/v1/rail/log";
/// See [`RAIL_LOG_PATH`].
pub const RAIL_APPEND_PATH: &str = "/v1/rail/append";
/// See [`RAIL_LOG_PATH`]. One path, both directions: POST sends one ephemeral
/// payload, GET drains what peers sent.
pub const RAIL_LIVE_PATH: &str = "/v1/rail/live";

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| format!("http client: {e}"))
}

/// The daemon's refusal, as the daemon worded it.
///
/// A roster miss, a non-canonical payload, an unknown namespace — every one of
/// them is the RAIL's sentence, and rewording it here would be a second
/// wording of one condition (ARCH §10.6). Public because the grant mint beside
/// `ring dev` reads the same daemon's errors and had its own copy.
pub async fn error_text(resp: reqwest::Response) -> String {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let detail = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
        .unwrap_or(body);
    if detail.is_empty() {
        status.to_string()
    } else {
        format!("{status}: {detail}")
    }
}

fn url(path: &str, namespace: &str) -> String {
    url_at(&crate::urls::daemon_base_url(), path, namespace)
}

fn url_at(base: &str, path: &str, namespace: &str) -> String {
    format!("{}{path}?namespace={namespace}", base.trim_end_matches('/'))
}

/// cw-rails' actor door: the key its journal lines are signed with.
pub const RAIL_ACTOR_PATH: &str = "/v1/rail/actor";
/// cw-rails' digest door: a journal's per-actor high-water marks, which move
/// at every append and every seal.
pub const RAIL_DIGEST_PATH: &str = "/v1/rail/digest";

/// One operator-side READ of a namespace: the admitted acts, the gaps, and the
/// roster the DAEMON actually loaded.
///
/// **The roster the daemon holds is what decides which acts are readable.** An
/// act signed by a key the roster does not carry is an `UnknownSigner` gap and
/// not an act, and the on-disk journal has no roster beside it — so folding
/// the file directly produces a projection that is confidently wrong on
/// exactly the ring where membership is the question, and wrong SILENTLY,
/// because a refused act and an absent act look identical once the roster is
/// gone.
pub async fn rail_log(namespace: &str) -> Result<serde_json::Value, String> {
    log_from(url(RAIL_LOG_PATH, namespace), "the daemon")
        .await
        .map_err(|e| e.to_string())
}

/// [`rail_log`] against the rail doors at `base` — cw-rails', whose base the
/// caller resolves (`sovereign_turn_client::rails_kv::resolve_rails_base`).
pub async fn rail_log_at(base: &str, namespace: &str) -> Result<serde_json::Value, RailDoorError> {
    log_from(url_at(base, RAIL_LOG_PATH, namespace), "cw-rails").await
}

/// Why a dial to a rail door returned no answer: nothing listened, or the door
/// answered with its own refusal. Typed so a caller that reports the two
/// differently never matches on the sentence (ARCH principle 9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RailDoorError {
    /// Nothing answered at the URL.
    Unreachable(String),
    /// The door answered and refused, or answered with a body this build
    /// cannot read — the door's own sentence.
    Refused(String),
}

impl std::fmt::Display for RailDoorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RailDoorError::Unreachable(why) | RailDoorError::Refused(why) => f.write_str(why),
        }
    }
}

async fn log_from(url: String, who: &str) -> Result<serde_json::Value, RailDoorError> {
    let resp = client()
        .map_err(RailDoorError::Unreachable)?
        .get(&url)
        .send()
        .await
        .map_err(|e| RailDoorError::Unreachable(format!("cannot reach {who} at {url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(RailDoorError::Refused(error_text(resp).await));
    }
    resp.json()
        .await
        .map_err(|e| RailDoorError::Refused(format!("bad response: {e}")))
}

/// One namespace's record, frozen — the v1 checkpoint document the daemon
/// composes from its own journal and roster.
///
/// Unlike [`rail_log`] this targets the INTERNAL listener
/// (`sovereign_contracts::setup_config::internal_daemon_base`), because the
/// checkpoint route is mounted there on purpose: freezing a copy of a node's
/// record is a local operator's act, not something a mesh peer does — a peer
/// syncs by digest. The path carries the namespace (no query string), which
/// is the route's own shape.
pub async fn rail_checkpoint(namespace: &str) -> Result<serde_json::Value, String> {
    let url = format!(
        "{}/internal/ring/checkpoint/{namespace}",
        sovereign_contracts::setup_config::internal_daemon_base()
    );
    let resp = client()?
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("cannot reach the daemon at {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(error_text(resp).await);
    }
    resp.json().await.map_err(|e| format!("bad response: {e}"))
}

/// One namespace's roster and its admitted acts, together — the pair every
/// warrant question needs.
///
/// Through the DAEMON, never the file, for [`rail_log`]'s reason: the roster
/// the daemon loaded is what decides which acts are readable, and a fold
/// against a different roster is confidently and silently wrong on exactly
/// the ring where membership is the question.
///
/// One implementation, because there are two callers now — `svrn ring roster
/// show` and `svrn mesh offers --why` — and two would let the roster page and
/// the catalogue disagree about the same row (ARCH §10.6).
pub async fn roster_and_admission(namespace: &str) -> Result<(Roster, Admission), String> {
    let v = rail_log(namespace).await?;
    let roster: Roster = serde_json::from_value(
        v.get("roster").cloned().ok_or_else(|| {
            format!("the daemon's log answer carried no `roster` — this build and that daemon do not agree on the shape of `{RAIL_LOG_PATH}`")
        })?,
    )
    .map_err(|e| format!("the daemon's roster is a shape this build cannot read: {e}"))?;
    let admission = admission_from_wire(&v)?;
    Ok((roster, admission))
}

/// One operator-side WRITE to a namespace: hand the daemon one act, and get
/// back what it assigned.
///
/// The act is TYPED rather than a hand-built `json!` object. `RailAct`'s own
/// `Serialize` is the wire form the door parses with `RailAct::from_json`, so
/// a caller cannot spell `{"op": "sealed"}` and learn about it from a 422.
pub async fn rail_append(namespace: &str, act: &RailAct) -> Result<serde_json::Value, String> {
    append_to(url(RAIL_APPEND_PATH, namespace), act, "the daemon")
        .await
        .map_err(|e| e.to_string())
}

/// [`rail_append`] against the rail doors at `base`. See [`rail_log_at`].
pub async fn rail_append_at(
    base: &str,
    namespace: &str,
    act: &RailAct,
) -> Result<serde_json::Value, RailDoorError> {
    append_to(url_at(base, RAIL_APPEND_PATH, namespace), act, "cw-rails").await
}

async fn append_to(url: String, act: &RailAct, who: &str) -> Result<serde_json::Value, RailDoorError> {
    let resp = client()
        .map_err(RailDoorError::Unreachable)?
        .post(&url)
        .json(act)
        .send()
        .await
        .map_err(|e| RailDoorError::Unreachable(format!("cannot reach {who} at {url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(RailDoorError::Refused(error_text(resp).await));
    }
    resp.json()
        .await
        .map_err(|e| RailDoorError::Refused(format!("bad response: {e}")))
}

/// One operator-side `Record` write: wrap `payload` in the act and append it.
///
/// The typed constructor lives with the client so a caller reaches the ONE
/// rail route without naming the rail journal's act type itself.
pub async fn rail_append_record(
    namespace: &str,
    payload: Payload,
) -> Result<serde_json::Value, String> {
    rail_append(namespace, &RailAct::Record { payload }).await
}

/// cw-rails' `work` doors (pb-work-doors), spelled once for every caller: the
/// folded queue, and the four a submitter that links no `commonwealth-work`
/// seals, submits, surveys and attributes through.
pub const WORK_PROJECTION_PATH: &str = "/v1/work/projection";
/// See [`WORK_PROJECTION_PATH`].
pub const WORK_SEAL_PATH: &str = "/v1/work/seal";
/// See [`WORK_PROJECTION_PATH`].
pub const WORK_SUBMIT_PATH: &str = "/v1/work/submit";
/// See [`WORK_PROJECTION_PATH`].
pub const WORK_REFUSALS_PATH: &str = "/v1/work/refusals";
/// See [`WORK_PROJECTION_PATH`].
pub const WORK_ATTRIBUTION_PATH: &str = "/v1/work/attribution";

/// GET one of cw-rails' doors at `base` — the caller resolves the base
/// (`sovereign_turn_client::rails_kv::resolve_rails_base`, the one reader of
/// `[daemon] rails_base`). A refusal is cw-rails' own sentence.
pub async fn rails_get(
    base: &str,
    path: &str,
    query: &[(&str, &str)],
) -> Result<serde_json::Value, String> {
    let url = format!("{}{path}", base.trim_end_matches('/'));
    let resp = client()?
        .get(&url)
        .query(query)
        .send()
        .await
        .map_err(|e| format!("cannot reach cw-rails at {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(error_text(resp).await);
    }
    resp.json().await.map_err(|e| format!("bad response: {e}"))
}

/// POST a JSON body to one of cw-rails' doors at `base`. See [`rails_get`].
pub async fn rails_post(
    base: &str,
    path: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let url = format!("{}{path}", base.trim_end_matches('/'));
    let resp = client()?
        .post(&url)
        .json(body)
        .send()
        .await
        .map_err(|e| format!("cannot reach cw-rails at {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(error_text(resp).await);
    }
    resp.json().await.map_err(|e| format!("bad response: {e}"))
}

/// Rebuild the [`Admission`] the daemon already computed, so a caller's fold
/// is the SAME function the donor loop runs.
///
/// The alternative was walking `ops` at each call site and deciding what an
/// act means, which is a second fold and therefore a second answer to who
/// holds a lease (ARCH §10.6). `AdmittedOp` gained `Deserialize` for exactly
/// this. It lives here rather than in `svrn job` because cw-lift 5e gave it a
/// second reader in a different crate — `svrn quality check --distribute`
/// folds the same answer to merge a distributed run's verdicts.
///
/// `floors` is empty and that is correct rather than lossy: it is an INPUT to
/// admission — the sealed floor below which a missing op is absent by
/// agreement rather than a hole — and the daemon has already applied it to the
/// `ops` and `gaps` on the wire. The fold reads neither.
///
/// THE KEYS ARE REQUIRED, the contents are not, and the asymmetry is the point
/// (ARCH §18.3). An answer with no `ops` at all folds to an empty projection,
/// and a caller would then report "nothing submitted yet" — a confident claim
/// about the ring composed out of a shape this build could not read. Absence
/// of the key is a daemon/CLI mismatch and says so; absence of any op is a
/// quiet ring and is `[]` on the wire.
pub fn admission_from_wire(v: &serde_json::Value) -> Result<Admission, String> {
    let missing = |k: &str| {
        format!("the daemon's log answer carried no `{k}` — this build and that daemon do not agree on the shape of `{RAIL_LOG_PATH}`")
    };
    let ops: Vec<AdmittedOp> = serde_json::from_value(
        v.get("ops").cloned().ok_or_else(|| missing("ops"))?,
    )
    .map_err(|e| format!("the daemon's log answer carried ops this build cannot read: {e}"))?;
    // Gaps are decoded for their COUNT, which the fold reports; the sentences
    // are rendered from the wire value itself by the caller. A gap kind a newer
    // daemon added is not a reason to refuse the whole answer — but a missing
    // key still is, because "no gaps" is what a caller reports as complete.
    let gaps: Vec<RailGap> =
        serde_json::from_value(v.get("gaps").cloned().ok_or_else(|| missing("gaps"))?)
            .unwrap_or_default();
    let held = v
        .get("held")
        .and_then(|h| h.as_u64())
        .ok_or_else(|| missing("held"))? as usize;
    Ok(Admission {
        ops,
        gaps,
        held,
        floors: BTreeMap::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The two routes are built off the audited base, not a compiled
    /// loopback.** Failing input: `SOVEREIGN_DAEMON_URL` pointed at a sandbox
    /// daemon — the shape `cli-journey-sandbox.sh` uses — where a hardcoded
    /// `127.0.0.1:9741` would act on the operator's ring instead of the
    /// sandbox's, silently and successfully.
    ///
    /// The env var is read through `client_daemon_base()`, which is process-
    /// global, so this test sets and restores it rather than running in
    /// parallel with a reader that would see it.
    #[test]
    fn the_url_follows_the_daemon_knob() {
        let key = "SOVEREIGN_DAEMON_URL";
        let prior = std::env::var(key).ok();
        std::env::set_var(key, "http://127.0.0.1:19741/");
        let got = url(RAIL_LOG_PATH, "work");
        match prior {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        assert_eq!(got, "http://127.0.0.1:19741/v1/rail/log?namespace=work");
    }
}
