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

use commonwealth_rail::{Admission, AdmittedOp, RailAct, RailGap};

/// The rail's two routes, spelled once for every caller in the workspace.
///
/// The FUNCTIONS below are the clients. `svrn ring dev` cannot use them — it
/// proxies a browser's opaque bytes to a different listener under a grant
/// token — but it must not spell the paths a second time either, so it uses
/// these. A route renamed on the daemon then breaks the build at every caller
/// rather than at runtime on whichever one is exercised first.
pub const RAIL_LOG_PATH: &str = "/v1/rail/log";
/// See [`RAIL_LOG_PATH`].
pub const RAIL_APPEND_PATH: &str = "/v1/rail/append";

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
    let base = crate::urls::daemon_base_url();
    format!("{}{path}?namespace={namespace}", base.trim_end_matches('/'))
}

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
    let url = url(RAIL_LOG_PATH, namespace);
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

/// One operator-side WRITE to a namespace: hand the daemon one act, and get
/// back what it assigned.
///
/// The act is TYPED rather than a hand-built `json!` object. `RailAct`'s own
/// `Serialize` is the wire form the door parses with `RailAct::from_json`, so
/// a caller cannot spell `{"op": "sealed"}` and learn about it from a 422.
pub async fn rail_append(namespace: &str, act: &RailAct) -> Result<serde_json::Value, String> {
    let url = url(RAIL_APPEND_PATH, namespace);
    let resp = client()?
        .post(&url)
        .json(act)
        .send()
        .await
        .map_err(|e| format!("cannot reach the daemon at {url}: {e}"))?;
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
