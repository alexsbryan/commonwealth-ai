// SPDX-License-Identifier: AGPL-3.0-or-later
//! The client half of the serving-cluster dials: the daemon DIALS the mesh's
//! serving process (`cw-rails`) instead of mutating or reading its own copy
//! (FIVE_PROGRAMS fp-6 / §12 decision 2 — the mesh owns the roster; a daemon
//! holding another's lifecycle is the line drawn wrong).
//!
//! The routes live in `commonwealth-rails/src/api.rs` and speak plain JSON —
//! the same shapes this daemon's own `/v1/mesh/*` routes serve, because a
//! client works against either. Every failure here is a NAMED absence
//! (principle 6): a dial that does not answer is reported with the URL, never
//! defaulted to a local read.
//!
//! Since fp-54's flip this file also carries the ring rail's dialing
//! implementation ([`RailsRingRail`]): the journals moved to the serving
//! process, so every rail read and write the round, the pump, the donor and
//! the rail routes make goes over `cw-rails`' `/v1/rail/*` doors through
//! [`sovereign_mesh::rail_port::RingRailPort`]. A rail that is not reachable
//! is an ABSENCE every caller already handles (the port returns `RailError`),
//! never an empty journal.

use std::sync::OnceLock;

use commonwealth_core::ids::NodePubkey;
use sovereign_mesh::fabric::ForgottenMember;
use sovereign_mesh::rail_port::{RailFut, RingRailPort};

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

/// The media-presence poll's last reading, as the serving process holds it.
///
/// `Ok(None)` is the route's served VALUE `{"media_available": null}` — the
/// poll over there could not ask, and "nobody answered" is the reading — not
/// a refusal; refusals and absences are the `Err` arms.
pub async fn media_presence(base: &str) -> Result<Option<f32>, RailsDial> {
    let resp = dial(base, "/v1/mesh/media/presence").await?;
    let status = resp.status();
    if !status.is_success() {
        let message = resp.text().await.unwrap_or_default();
        return Err(RailsDial::Refused {
            status,
            kind: None,
            message: format!("media/presence refused: {status} {message}"),
        });
    }
    #[derive(serde::Deserialize)]
    struct Answer {
        media_available: Option<f32>,
    }
    let answer: Answer = resp.json().await.map_err(|e| RailsDial::Unreadable {
        base: base.to_string(),
        detail: e.to_string(),
    })?;
    Ok(answer.media_available)
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

// ── The ring rail's dialing implementation ───────────────────

/// Map a dial outcome onto the port's error type. Every variant keeps the
/// sentence the serving process (or the failed dial) produced: the absence is
/// named, never defaulted (principle 6).
fn rail_error(base: &str, e: RailsDial) -> commonwealth_rail_core::RailError {
    use commonwealth_rail_core::RailError;
    match e {
        RailsDial::Absent { base, detail } => RailError::Io(format!(
            "the mesh's serving process is not reachable at {base}: {detail}"
        )),
        RailsDial::Refused { message, .. } => RailError::Rejected(message),
        RailsDial::Unreadable { base, detail } => RailError::Io(format!(
            "the mesh's serving process at {base} answered with an unreadable body: {detail}"
        )),
    }
}

/// The typed not-in-roster refusal, reconstructed from the append door's
/// `kind` field. The KV pump's defer-on-not-in-roster is a real decision and
/// must survive the dial — matching the refusal's PROSE would be the string
/// match ARCH principle 9 forbids.
fn refused_error(
    base: &str,
    status: reqwest::StatusCode,
    body: serde_json::Value,
) -> commonwealth_rail_core::RailError {
    use commonwealth_rail_core::RailError;
    if body.get("kind").and_then(|k| k.as_str()) == Some("not_in_roster") {
        return RailError::NotInRoster {
            actor: body
                .get("actor")
                .and_then(|a| a.as_str())
                .unwrap_or_default()
                .to_string(),
            namespace: body
                .get("namespace")
                .and_then(|a| a.as_str())
                .unwrap_or_default()
                .to_string(),
        };
    }
    RailError::Rejected(match body.get("error").and_then(|e| e.as_str()) {
        Some(msg) => msg.to_string(),
        None => format!("the mesh's serving process at {base} refused: {status}"),
    })
}

async fn dial_json<T: serde::de::DeserializeOwned>(
    base: &str,
    path: &str,
) -> Result<T, commonwealth_rail_core::RailError> {
    let resp = dial(base, path).await.map_err(|e| rail_error(base, e))?;
    let status = resp.status();
    if !status.is_success() {
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        return Err(refused_error(base, status, body));
    }
    resp.json::<T>().await.map_err(|e| {
        rail_error(
            base,
            RailsDial::Unreadable {
                base: base.to_string(),
                detail: e.to_string(),
            },
        )
    })
}

async fn post_json<T: serde::de::DeserializeOwned>(
    base: &str,
    path: &str,
    body: &serde_json::Value,
) -> Result<T, commonwealth_rail_core::RailError> {
    let url = format!("{}{}", base.trim_end_matches('/'), path);
    let resp = client().post(&url).json(body).send().await.map_err(|e| {
        rail_error(
            base,
            RailsDial::Absent {
                base: base.to_string(),
                detail: e.to_string(),
            },
        )
    })?;
    let status = resp.status();
    if !status.is_success() {
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        return Err(refused_error(base, status, body));
    }
    resp.json::<T>().await.map_err(|e| {
        rail_error(
            base,
            RailsDial::Unreadable {
                base: base.to_string(),
                detail: e.to_string(),
            },
        )
    })
}

/// The ring rail, as the serving process holds it. Every method is one dial
/// to the door that serves that verb; the answers are rail-core types, so
/// the round and the pump run unchanged over either implementation.
pub struct RailsRingRail {
    base: String,
}

impl RailsRingRail {
    pub fn new(base: impl Into<String>) -> Self {
        Self { base: base.into() }
    }
}

impl RingRailPort for RailsRingRail {
    fn namespaces(&self) -> RailFut<'_, Vec<String>> {
        let base = self.base.clone();
        Box::pin(async move {
            #[derive(serde::Deserialize)]
            struct Answer {
                namespaces: Vec<String>,
            }
            dial_json::<Answer>(&base, "/v1/rail/namespaces")
                .await
                .map(|a| a.namespaces)
        })
    }

    fn actor(&self) -> RailFut<'_, String> {
        let base = self.base.clone();
        Box::pin(async move {
            #[derive(serde::Deserialize)]
            struct Answer {
                actor: String,
            }
            dial_json::<Answer>(&base, "/v1/rail/actor")
                .await
                .map(|a| a.actor)
        })
    }

    fn roster_origin(&self, namespace: &str) -> RailFut<'_, commonwealth_rail_core::RosterOrigin> {
        let base = self.base.clone();
        let namespace = namespace.to_string();
        Box::pin(async move {
            #[derive(serde::Deserialize)]
            struct Answer {
                origin: String,
            }
            let a: Answer =
                dial_json(&base, &format!("/v1/rail/roster?namespace={namespace}")).await?;
            match a.origin.as_str() {
                "file" => Ok(commonwealth_rail_core::RosterOrigin::File),
                "derived" => Ok(commonwealth_rail_core::RosterOrigin::Derived),
                other => Err(commonwealth_rail_core::RailError::Io(format!(
                    "the mesh's serving process at {base} named an unknown roster origin: {other}"
                ))),
            }
        })
    }

    fn roster(&self, namespace: &str) -> RailFut<'_, commonwealth_rail_core::Roster> {
        let base = self.base.clone();
        let namespace = namespace.to_string();
        Box::pin(async move {
            #[derive(serde::Deserialize)]
            struct Answer {
                roster: commonwealth_rail_core::Roster,
            }
            let a: Answer =
                dial_json(&base, &format!("/v1/rail/roster?namespace={namespace}")).await?;
            Ok(a.roster)
        })
    }

    fn journal_read(
        &self,
        namespace: &str,
    ) -> RailFut<'_, Vec<commonwealth_rail_core::Op<commonwealth_rail_core::SignedOp>>> {
        let base = self.base.clone();
        let namespace = namespace.to_string();
        Box::pin(async move {
            #[derive(serde::Deserialize)]
            struct Answer {
                ops: Vec<commonwealth_rail_core::Op<commonwealth_rail_core::SignedOp>>,
            }
            let a: Answer =
                dial_json(&base, &format!("/v1/rail/read?namespace={namespace}")).await?;
            Ok(a.ops)
        })
    }

    fn journal_admit(
        &self,
        namespace: &str,
        roster: &commonwealth_rail_core::Roster,
    ) -> RailFut<'_, commonwealth_rail_core::Admission> {
        let base = self.base.clone();
        let namespace = namespace.to_string();
        let roster = roster.clone();
        Box::pin(async move {
            let body = serde_json::json!({ "roster": roster });
            post_json(
                &base,
                &format!("/v1/rail/admit?namespace={namespace}"),
                &body,
            )
            .await
        })
    }

    fn journal_append(
        &self,
        namespace: &str,
        act: commonwealth_rail_core::RailAct,
        roster: &commonwealth_rail_core::Roster,
    ) -> RailFut<'_, commonwealth_rail_core::Op<commonwealth_rail_core::SignedOp>> {
        let base = self.base.clone();
        let namespace = namespace.to_string();
        // The roster stays a parameter for the LOCAL implementation's
        // not-in-roster check; the door signs against its own rail's roster
        // and answers a typed refusal when it does not name the signer.
        let _ = roster;
        Box::pin(async move {
            #[derive(serde::Deserialize)]
            struct Answer {
                op: commonwealth_rail_core::Op<commonwealth_rail_core::SignedOp>,
            }
            let act_json = serde_json::to_value(&act).map_err(|e| {
                commonwealth_rail_core::RailError::Io(format!("the act does not serialise: {e}"))
            })?;
            let a: Answer = post_json(
                &base,
                &format!("/v1/rail/append?namespace={namespace}"),
                &act_json,
            )
            .await?;
            Ok(a.op)
        })
    }

    fn journal_seal(
        &self,
        namespace: &str,
        roster: &commonwealth_rail_core::Roster,
    ) -> RailFut<
        '_,
        (
            commonwealth_rail_core::Op<commonwealth_rail_core::SignedOp>,
            Result<commonwealth_rail_core::Compaction, commonwealth_rail_core::RailError>,
        ),
    > {
        let base = self.base.clone();
        let namespace = namespace.to_string();
        let _ = roster;
        Box::pin(async move {
            #[derive(serde::Deserialize)]
            struct Answer {
                op: commonwealth_rail_core::Op<commonwealth_rail_core::SignedOp>,
                #[serde(default)]
                retired: Option<serde_json::Value>,
            }
            let act_json =
                serde_json::to_value(&commonwealth_rail_core::RailAct::Seal).map_err(|e| {
                    commonwealth_rail_core::RailError::Io(format!(
                        "the act does not serialise: {e}"
                    ))
                })?;
            let a: Answer = post_json(
                &base,
                &format!("/v1/rail/append?namespace={namespace}"),
                &act_json,
            )
            .await?;
            // The door renders the prune outcome beside the seal: a refused
            // prune is not a failed seal, and the pair stays one fact over
            // the wire (the local `Sealed.retired` shape, re-read).
            let retired = match a.retired {
                Some(v) => match v.get("refused").and_then(|r| r.as_str()) {
                    Some(why) => Err(commonwealth_rail_core::RailError::Rejected(why.to_string())),
                    None => serde_json::from_value(v).map_err(|e| {
                        commonwealth_rail_core::RailError::Io(format!(
                            "the mesh's serving process at {base} answered with an unreadable prune report: {e}"
                        ))
                    }),
                },
                None => Err(commonwealth_rail_core::RailError::Io(format!(
                    "the mesh's serving process at {base} answered a seal with no prune report"
                ))),
            };
            Ok((a.op, retired))
        })
    }

    fn journal_compact(
        &self,
        namespace: &str,
        roster: &commonwealth_rail_core::Roster,
    ) -> RailFut<'_, commonwealth_rail_core::Compaction> {
        let base = self.base.clone();
        let namespace = namespace.to_string();
        let roster = roster.clone();
        Box::pin(async move {
            let body = serde_json::json!({ "roster": roster });
            post_json(
                &base,
                &format!("/v1/rail/compact?namespace={namespace}"),
                &body,
            )
            .await
        })
    }

    fn journal_digest(&self, namespace: &str) -> RailFut<'_, commonwealth_rail_core::Digest> {
        let base = self.base.clone();
        let namespace = namespace.to_string();
        Box::pin(async move {
            #[derive(serde::Deserialize)]
            struct Answer {
                digest: commonwealth_rail_core::Digest,
            }
            let a: Answer =
                dial_json(&base, &format!("/v1/rail/digest?namespace={namespace}")).await?;
            Ok(a.digest)
        })
    }

    fn journal_ingest_all(
        &self,
        namespace: &str,
        ops: &[commonwealth_rail_core::Op<commonwealth_rail_core::SignedOp>],
    ) -> RailFut<'_, usize> {
        let base = self.base.clone();
        let namespace = namespace.to_string();
        let ops = ops.to_vec();
        Box::pin(async move {
            #[derive(serde::Deserialize)]
            struct Answer {
                ingested: usize,
            }
            let body = serde_json::json!({ "ops": ops });
            let a: Answer = post_json(
                &base,
                &format!("/v1/rail/ingest?namespace={namespace}"),
                &body,
            )
            .await?;
            Ok(a.ingested)
        })
    }

    fn journal_ops_missing_from_within(
        &self,
        namespace: &str,
        theirs: &commonwealth_rail_core::Digest,
        budget_bytes: usize,
    ) -> RailFut<
        '_,
        (
            Vec<commonwealth_rail_core::Op<commonwealth_rail_core::SignedOp>>,
            bool,
        ),
    > {
        let base = self.base.clone();
        let namespace = namespace.to_string();
        let theirs = theirs.clone();
        Box::pin(async move {
            #[derive(serde::Deserialize)]
            struct Answer {
                ops: Vec<commonwealth_rail_core::Op<commonwealth_rail_core::SignedOp>>,
                more: bool,
            }
            let body = serde_json::json!({ "digest": theirs, "budget": budget_bytes });
            let a: Answer = post_json(
                &base,
                &format!("/v1/rail/missing?namespace={namespace}"),
                &body,
            )
            .await?;
            Ok((a.ops, a.more))
        })
    }
}
