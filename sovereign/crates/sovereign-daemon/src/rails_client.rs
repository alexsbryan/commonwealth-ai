// SPDX-License-Identifier: AGPL-3.0-or-later
//! The client half of the roster verbs: the daemon DIALS the mesh's serving
//! process (`cw-rails`) instead of mutating or reading its own copy
//! (FIVE_PROGRAMS fp-6 / §12 decision 2 — the mesh owns the roster; a daemon
//! holding another's lifecycle is the line drawn wrong).
//!
//! The routes live in `commonwealth-rails/src/api.rs` and speak plain JSON —
//! the same shapes this daemon's own `/v1/mesh/*` routes serve, because a
//! client works against either. Every failure here is a NAMED absence
//! (principle 6): a dial that does not answer is reported with the URL, never
//! defaulted to a local read.

use std::sync::OnceLock;

use commonwealth_core::ids::NodePubkey;
use sovereign_mesh::fabric::ForgottenMember;

/// Where the mesh's serving process listens. Mirrors cw-rails'
/// `commonwealth_rails::config::DEFAULT_LISTEN` — 9747, outside the
/// 9741..9745 family this daemon binds (the two programs are built and
/// versioned separately, so the convention is mirrored and documented on
/// both sides rather than imported across the lift boundary).
pub const DEFAULT_RAILS_BASE: &str = "http://127.0.0.1:9747";

/// How long a dial may take before it is reported absent. Loopback answers
/// or refuses in milliseconds; the bound exists so a HUNG serving process
/// turns into a named refusal rather than a wedged route.
const DIAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(DIAL_TIMEOUT)
            .build()
            .expect("a plain reqwest client with a timeout")
    })
}

/// Why a dial did not produce the verb's answer. Every variant names the
/// serving process; none of them is a fall-back.
#[derive(Debug, thiserror::Error)]
pub enum RailsDial {
    /// The serving process is not reachable. This is the ABSENCE the route
    /// reports — the mesh's roster is its to serve, and this daemon holds no
    /// answer of its own.
    #[error("the mesh's serving process is not reachable at {base}: {detail}")]
    Absent { base: String, detail: String },
    /// The serving process answered with a refusal. `kind` names the arm
    /// when the body carried one, so the caller maps refusals without
    /// parsing prose.
    #[error("{message}")]
    Refused {
        status: reqwest::StatusCode,
        kind: Option<String>,
        message: String,
    },
    /// The serving process answered with a body this client could not read.
    /// A wire both ends own breaking shape is not silently survivable.
    #[error("the mesh's serving process at {base} answered with an unreadable body: {detail}")]
    Unreadable { base: String, detail: String },
}

async fn dial(base: &str, path: &str) -> Result<reqwest::Response, RailsDial> {
    let url = format!("{}{}", base.trim_end_matches('/'), path);
    client()
        .get(&url)
        .send()
        .await
        .map_err(|e| RailsDial::Absent {
            base: base.to_string(),
            detail: e.to_string(),
        })
}

/// Does the mesh's membership name this key? The serving process answers
/// from every member row, tombstones included — the ring-roster rule — so
/// this is the exact question the derived rosters used to answer locally.
pub async fn roster_names(base: &str, key: NodePubkey) -> Result<bool, RailsDial> {
    let resp = dial(base, &format!("/v1/mesh/roster-names/{key}")).await?;
    let status = resp.status();
    if !status.is_success() {
        let message = resp.text().await.unwrap_or_default();
        return Err(RailsDial::Refused {
            status,
            kind: None,
            message: format!("roster-names refused: {status} {message}"),
        });
    }
    #[derive(serde::Deserialize)]
    struct Answer {
        named: bool,
    }
    let answer: Answer = resp.json().await.map_err(|e| RailsDial::Unreadable {
        base: base.to_string(),
        detail: e.to_string(),
    })?;
    Ok(answer.named)
}

/// Retire one member row, on the process that owns the roster.
pub async fn forget_member(
    base: &str,
    member: &str,
    force: bool,
) -> Result<ForgottenMember, RailsDial> {
    let url = format!("{}{}", base.trim_end_matches('/'), "/v1/mesh/forget-member");
    let resp = client()
        .post(&url)
        .json(&serde_json::json!({ "member": member, "force": force }))
        .send()
        .await
        .map_err(|e| RailsDial::Absent {
            base: base.to_string(),
            detail: e.to_string(),
        })?;
    let status = resp.status();
    if !status.is_success() {
        #[derive(serde::Deserialize)]
        struct Refusal {
            #[serde(default)]
            error: String,
            #[serde(default)]
            kind: Option<String>,
        }
        let body: Refusal = resp.json().await.unwrap_or(Refusal {
            error: String::new(),
            kind: None,
        });
        return Err(RailsDial::Refused {
            status,
            kind: body.kind,
            message: if body.error.is_empty() {
                format!("forget-member refused: {status}")
            } else {
                body.error
            },
        });
    }
    serde_json::from_str::<ForgottenMember>(&resp.text().await.map_err(|e| {
        RailsDial::Unreadable {
            base: base.to_string(),
            detail: e.to_string(),
        }
    })?)
    .map_err(|e| RailsDial::Unreadable {
        base: base.to_string(),
        detail: e.to_string(),
    })
}
