// SPDX-License-Identifier: AGPL-3.0-or-later
//! The client half of the serving-cluster dials: the daemon DIALS the mesh's
//! rails daemon (`cw-rails`) instead of mutating or reading its own copy
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
//! implementation ([`RailsRingRail`]): the journals moved to the rails
//! daemon, so every rail read and write the round, the pump, the donor and
//! the rail routes make goes over `cw-rails`' `/v1/rail/*` doors through
//! [`crate::rail_port::RingRailPort`]. A rail that is not reachable
//! is an ABSENCE every caller already handles (the port returns `RailError`),
//! never an empty journal.

use std::sync::OnceLock;

use crate::rail_port::{RailFut, RingRailPort};
use kernel_types::NodePubkey;

/// The typed ledger ports' dialing implementation (fp-78).
pub mod ledger;

/// The daemon's sync `ReplicatedKv`, dialed (fp-110). One client for every
/// program that dials cw-rails' KV doors, so it lives in the client family.
pub use sovereign_turn_client::rails_kv as kv;
pub use sovereign_turn_client::rails_kv::{resolve_rails_base, DEFAULT_RAILS_BASE};

/// How long a dial may take before it is reported absent. Loopback answers
/// or refuses in milliseconds; the bound exists so a HUNG rails daemon
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
/// rails daemon; none of them is a fall-back.
#[derive(Debug, thiserror::Error)]
pub enum RailsDial {
    /// The rails daemon is not reachable. This is the ABSENCE the route
    /// reports — the mesh's roster is its to serve, and this daemon holds no
    /// answer of its own.
    #[error(
        "the mesh's rails daemon is not reachable at {base}: {detail}; bring it up with `{verb}`",
        verb = sovereign_turn_client::rails_kv::RAILS_BRING_UP_VERB
    )]
    Absent { base: String, detail: String },
    /// The rails daemon answered with a refusal. `kind` names the arm
    /// when the body carried one, so the caller maps refusals without
    /// parsing prose.
    #[error("{message}")]
    Refused {
        status: reqwest::StatusCode,
        kind: Option<String>,
        message: String,
    },
    /// The rails daemon answered with a body this client could not read.
    /// A wire both ends own breaking shape is not silently survivable.
    #[error("the mesh's rails daemon at {base} answered with an unreadable body: {detail}")]
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

/// One GET door's answer: a non-2xx is `Refused` with the door's body as
/// prose (`<verb> refused: <status> <body>`), an unparseable 2xx `Unreadable`.
async fn get_answer<T: serde::de::DeserializeOwned>(
    base: &str,
    path: &str,
    verb: &str,
) -> Result<T, RailsDial> {
    read_answer(base, verb, dial(base, path).await?).await
}

/// One POST door's answer, read exactly as [`get_answer`] reads a GET's.
async fn post_answer<T: serde::de::DeserializeOwned>(
    base: &str,
    path: &str,
    body: &serde_json::Value,
) -> Result<T, RailsDial> {
    let url = format!("{}{}", base.trim_end_matches('/'), path);
    let resp = client()
        .post(&url)
        .json(body)
        .send()
        .await
        .map_err(|e| RailsDial::Absent {
            base: base.to_string(),
            detail: e.to_string(),
        })?;
    read_answer(base, path, resp).await
}

async fn read_answer<T: serde::de::DeserializeOwned>(
    base: &str,
    verb: &str,
    resp: reqwest::Response,
) -> Result<T, RailsDial> {
    let status = resp.status();
    if !status.is_success() {
        let message = resp.text().await.unwrap_or_default();
        return Err(RailsDial::Refused {
            status,
            kind: None,
            message: format!("{verb} refused: {status} {message}"),
        });
    }
    resp.json().await.map_err(|e| RailsDial::Unreadable {
        base: base.to_string(),
        detail: e.to_string(),
    })
}

/// Does the mesh's membership name this key? The rails daemon answers
/// from every member row, tombstones included — the ring-roster rule — so
/// this is the exact question the derived rosters used to answer locally.
pub async fn roster_names(base: &str, key: NodePubkey) -> Result<bool, RailsDial> {
    #[derive(serde::Deserialize)]
    struct Answer {
        named: bool,
    }
    let answer: Answer = get_answer(
        base,
        &format!("/v1/mesh/roster-names/{key}"),
        "roster-names",
    )
    .await?;
    Ok(answer.named)
}

/// The media-presence poll's last reading, as the rails daemon holds it.
///
/// `Ok(None)` is the route's served VALUE `{"media_available": null}` — the
/// poll over there could not ask, and "nobody answered" is the reading — not
/// a refusal; refusals and absences are the `Err` arms.
pub async fn media_presence(base: &str) -> Result<Option<f32>, RailsDial> {
    #[derive(serde::Deserialize)]
    struct Answer {
        media_available: Option<f32>,
    }
    let answer: Answer = get_answer(base, "/v1/mesh/media/presence", "media/presence").await?;
    Ok(answer.media_available)
}

/// Publish this daemon's foreground deadline to cw-rails' donor
/// (`POST /v1/work/yield`, pb-work-donor).
pub async fn post_work_yield(base: &str, until_ms: u64) -> Result<(), RailsDial> {
    let _: serde_json::Value = post_answer(
        base,
        "/v1/work/yield",
        &serde_json::json!({ "until_ms": until_ms }),
    )
    .await?;
    Ok(())
}

/// Drain the live lane's payloads peers pushed to `namespace` since the last
/// drain: cw-rails holds the buffer (its `/internal/ring/live` receives every
/// peer's push), so the drain is its `GET /v1/rail/live`, body verbatim
/// (`{payloads, dropped}`).
pub async fn live_drain(base: &str, namespace: &str) -> Result<serde_json::Value, RailsDial> {
    get_answer(
        base,
        &format!("/v1/rail/live?namespace={namespace}"),
        "rail live drain",
    )
    .await
}

/// The node's signed word that `name` may write in `namespace` until
/// `expires_at` (unix seconds): cw-rails signs it with the node's one key
/// (`POST /v1/rail/attest`, pb-mesh-exit-transport), and its append door
/// honours it as it honoured the daemon's own (decision five-programs-34).
pub async fn attest_guest(
    base: &str,
    name: &str,
    namespace: &str,
    expires_at: i64,
) -> Result<commonwealth_rail_core::GuestAttestation, RailsDial> {
    post_answer(
        base,
        "/v1/rail/attest",
        &serde_json::json!({ "name": name, "namespace": namespace, "expires_at": expires_at }),
    )
    .await
}

// ── The ring rail's dialing implementation ───────────────────

/// Map a dial outcome onto the port's error type. Every variant keeps the
/// sentence the rails daemon (or the failed dial) produced: the absence is
/// named, never defaulted (principle 6).
fn rail_error(base: &str, e: RailsDial) -> commonwealth_rail_core::RailError {
    use commonwealth_rail_core::RailError;
    match e {
        e @ RailsDial::Absent { .. } => RailError::Io(e.to_string()),
        RailsDial::Refused { message, .. } => RailError::Rejected(message),
        RailsDial::Unreadable { base, detail } => RailError::Io(format!(
            "the mesh's rails daemon at {base} answered with an unreadable body: {detail}"
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
    // A namespace that is not one keeps its type across the dial, so the
    // rail routes answer 400 for it as they do for a local rail, not 500.
    if body.get("kind").and_then(|k| k.as_str()) == Some("bad_namespace") {
        return RailError::BadNamespace(
            body.get("namespace")
                .and_then(|n| n.as_str())
                .unwrap_or_default()
                .to_string(),
        );
    }
    // A refused guest attestation keeps its name across the dial, so the
    // door hands the caller the rails daemon's own verdict (principle 6).
    if let Some(refusal) = body
        .get("kind")
        .and_then(|k| k.as_str())
        .and_then(commonwealth_rail_core::AttestRefusal::from_name)
    {
        return RailError::AttestRefused(refusal);
    }
    RailError::Rejected(match body.get("error").and_then(|e| e.as_str()) {
        Some(msg) => msg.to_string(),
        None => format!("the mesh's rails daemon at {base} refused: {status}"),
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

/// One append dial: the act's JSON, with `attestation` beside it when a
/// guest's name rides along — the body cw-rails' append door reads.
async fn post_append(
    base: &str,
    namespace: &str,
    act: &commonwealth_rail_core::RailAct,
    attestation: Option<&commonwealth_rail_core::GuestAttestation>,
) -> Result<
    commonwealth_rail_core::Op<commonwealth_rail_core::SignedOp>,
    commonwealth_rail_core::RailError,
> {
    #[derive(serde::Deserialize)]
    struct Answer {
        op: commonwealth_rail_core::Op<commonwealth_rail_core::SignedOp>,
    }
    let mut body = serde_json::to_value(act).map_err(|e| {
        commonwealth_rail_core::RailError::Io(format!("the act does not serialise: {e}"))
    })?;
    if let Some(attestation) = attestation {
        let attestation = serde_json::to_value(attestation).map_err(|e| {
            commonwealth_rail_core::RailError::Io(format!(
                "the attestation does not serialise: {e}"
            ))
        })?;
        match body.as_object_mut() {
            Some(obj) => {
                obj.insert("attestation".into(), attestation);
            }
            None => {
                return Err(commonwealth_rail_core::RailError::Io(
                    "the act did not serialise to an object".into(),
                ))
            }
        }
    }
    let a: Answer = post_json(
        base,
        &format!("/v1/rail/append?namespace={namespace}"),
        &body,
    )
    .await?;
    Ok(a.op)
}

/// The ring rail, as the rails daemon holds it. Every method is one dial
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
                    "the mesh's rails daemon at {base} named an unknown roster origin: {other}"
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
        Box::pin(async move { post_append(&base, &namespace, &act, None).await })
    }

    fn journal_append_attested(
        &self,
        namespace: &str,
        act: commonwealth_rail_core::RailAct,
        roster: &commonwealth_rail_core::Roster,
        attestation: &commonwealth_rail_core::GuestAttestation,
    ) -> RailFut<'_, commonwealth_rail_core::Op<commonwealth_rail_core::SignedOp>> {
        let base = self.base.clone();
        let namespace = namespace.to_string();
        // Rails verifies against ITS roster; this one is the local impl's.
        let _ = roster;
        let attestation = attestation.clone();
        Box::pin(async move { post_append(&base, &namespace, &act, Some(&attestation)).await })
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
                            "the mesh's rails daemon at {base} answered with an unreadable prune report: {e}"
                        ))
                    }),
                },
                None => Err(commonwealth_rail_core::RailError::Io(format!(
                    "the mesh's rails daemon at {base} answered a seal with no prune report"
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

    fn work_projection(
        &self,
    ) -> RailFut<'_, sovereign_contracts::oicp::work::projection::WorkProjection> {
        let base = self.base.clone();
        Box::pin(async move { dial_json(&base, "/v1/work/projection").await })
    }
}
