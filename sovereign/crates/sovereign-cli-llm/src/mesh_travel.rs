// SPDX-License-Identifier: AGPL-3.0-or-later
//! The CLI's side of measurement travel: publish what we measured, read what
//! peers did.
//!
//! ## Why the daemon is in the loop at all
//!
//! `svrn mesh bench` and `svrn mesh plan` run in this process. Gossip publishes
//! from the daemon's `MeshStore`, which is built `in_memory()` — there is no file
//! to open and no lock to share, so a measurement taken here cannot reach the
//! mesh, and a peer's cannot reach this process, without the daemon handing over
//! a door. That door is `POST`/`GET /v1/mesh/measurements` (`mesh_http.rs`), and
//! this module is the only thing in the CLI that knocks on it.
//!
//! ## Why a failure here is never an error
//!
//! Both operations degrade to nothing and say so. A run is written to
//! `~/.svrnmesh/mesh-measurements.json` *before* it is published, and the daemon
//! republishes that file at every boot — so a failed publish is genuinely "not
//! yet" rather than "lost", and phrasing it as a failure would send the operator
//! looking for a problem that will fix itself. Likewise `mesh plan` is a useful
//! command on a solo machine with no daemon running: a missing peer half makes
//! the answer smaller, not wrong.
//!
//! Nothing here decides *what* may travel or under what key. That policy is
//! `sovereign_core::mesh_measurements` — shared with the daemon precisely so the
//! two cannot come to disagree about it.

use serde::Deserialize;
use sovereign_core::mesh_measurements as mm;

/// How long to wait on the local daemon. Generous for a loopback call, but the
/// daemon can be mid-model-load and briefly slow to answer, and the alternative
/// to waiting is telling the operator their measurement did not travel when it
/// would have.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The measurements endpoint on the local daemon.
///
/// Resolved from `SetupConfig` exactly as `devices_from_live_mesh` does, rather
/// than from the compiled default: a sandbox pointed at its own daemon must not
/// silently publish into the operator's.
fn endpoint() -> String {
    let port = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.daemon.client_port)
        .unwrap_or(9741);
    format!("http://127.0.0.1:{port}/v1/mesh/measurements")
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| format!("http client: {e}"))
}

// ---------------------------------------------------------------------------
// Publish
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct PublishBody {
    published: bool,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    refused: Option<String>,
}

/// What became of a publish attempt.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Published {
    /// On the wire, under this KV key.
    Yes { key: String },
    /// The daemon took the request and declined it, for the reason it gave.
    /// Usually "no mesh yet", which is the normal state of a solo node.
    Declined(String),
    /// The daemon could not be asked.
    Unreachable(String),
}

impl Published {
    /// One line for the operator, indented to sit under the run summary.
    ///
    /// Every phrasing is about the *mesh*, never about the record — the record is
    /// already safely on disk by the time this is called, and a line that read
    /// like a write failure would be a lie about the thing the operator cares
    /// most about.
    pub(crate) fn note(&self) -> String {
        match self {
            Published::Yes { key } => format!("published to the mesh as {key}"),
            Published::Declined(why) => {
                format!("not on the mesh yet — {why}; the daemon republishes at boot")
            }
            Published::Unreachable(why) => {
                format!("not on the mesh yet — {why}; the daemon republishes at boot")
            }
        }
    }

    /// The machine-readable form for `--json`.
    pub(crate) fn as_json(&self) -> serde_json::Value {
        match self {
            Published::Yes { key } => serde_json::json!({ "published": true, "key": key }),
            Published::Declined(why) => {
                serde_json::json!({ "published": false, "reason": why, "daemon_reachable": true })
            }
            Published::Unreachable(why) => {
                serde_json::json!({ "published": false, "reason": why, "daemon_reachable": false })
            }
        }
    }
}

/// Hand a freshly-taken measurement to the daemon so it can gossip.
///
/// Called after the record is on disk, never before. An `Err`-shaped outcome is
/// returned as a variant rather than a `Result` because there is no caller who
/// should treat it as a failure — `mesh bench` reports it and still exits on the
/// verdict of the *run*, which is what the operator asked about.
pub(crate) async fn publish(record: &mm::MeasurementRecord) -> Published {
    if !record.verdict.is_valid() {
        return Published::Declined("an invalid run does not travel".to_string());
    }
    let url = endpoint();
    let client = match client() {
        Ok(c) => c,
        Err(e) => return Published::Unreachable(e),
    };
    let resp = match client.post(&url).json(record).send().await {
        Ok(r) => r,
        Err(e) => {
            return Published::Unreachable(format!("the daemon at {url} did not answer ({e})"))
        }
    };
    if !resp.status().is_success() {
        return Published::Unreachable(format!("the daemon answered HTTP {}", resp.status()));
    }
    match resp.json::<PublishBody>().await {
        Ok(b) if b.published => match b.key {
            Some(key) => Published::Yes { key },
            // Published without telling us where. Not worth an error, but not
            // worth claiming a key we do not have either.
            None => Published::Yes {
                key: "(key not reported)".to_string(),
            },
        },
        Ok(b) => Published::Declined(
            b.refused
                .unwrap_or_else(|| "the daemon declined without saying why".to_string()),
        ),
        Err(e) => Published::Unreachable(format!("the daemon's answer was unreadable ({e})")),
    }
}

// ---------------------------------------------------------------------------
// Read
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct PeerBody {
    #[serde(default)]
    records: Vec<PeerRow>,
    #[serde(default)]
    unreadable: usize,
}

#[derive(Debug, Deserialize)]
struct PeerRow {
    origin_node: String,
    #[serde(default)]
    origin_name: Option<String>,
    record: mm::MeasurementRecord,
}

/// What peers have measured, plus why the list might be shorter than expected.
#[derive(Debug, Default, Clone)]
pub(crate) struct PeerHistory {
    /// Peer records, ready to hand to [`mm::near_misses`].
    pub(crate) records: Vec<mm::ForeignRecord>,
    /// Entries the daemon held but could not read — usually a peer on an
    /// incompatible schema. Surfaced so that "no peer has measured this" is
    /// distinguishable from "a peer has, in a dialect we do not speak."
    pub(crate) unreadable: usize,
    /// Why nothing came back, when the reason is worth an operator's attention.
    /// `None` on the ordinary path, including the ordinary empty one.
    pub(crate) note: Option<String>,
}

impl PeerHistory {
    /// Nothing, because the daemon could not be asked. Deliberately not an
    /// error: `mesh plan` answers plenty of questions without a mesh.
    fn unreachable(why: String) -> Self {
        Self {
            records: Vec::new(),
            unreadable: 0,
            note: Some(why),
        }
    }
}

/// Fetch every measurement peers have gossiped to this node.
///
/// Excludes our own by construction — the daemon filters on the KV entry's
/// origin, and our copy on disk is the authoritative one.
pub(crate) async fn peer_history() -> PeerHistory {
    let url = endpoint();
    let client = match client() {
        Ok(c) => c,
        Err(e) => return PeerHistory::unreachable(e),
    };
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(_) => {
            // Silent on the common case: `mesh plan` with no daemon running is a
            // normal invocation and a warning here would be noise on every run.
            return PeerHistory::default();
        }
    };
    if !resp.status().is_success() {
        return PeerHistory::unreachable(format!(
            "the daemon answered HTTP {} for peer measurements",
            resp.status()
        ));
    }
    match resp.json::<PeerBody>().await {
        Ok(b) => PeerHistory {
            records: b
                .records
                .into_iter()
                .map(|r| mm::ForeignRecord {
                    origin_node: r.origin_node,
                    origin_name: r.origin_name,
                    record: r.record,
                })
                .collect(),
            unreadable: b.unreadable,
            note: None,
        },
        Err(e) => PeerHistory::unreachable(format!(
            "the daemon's peer-measurement answer was unreadable ({e})"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_publish_failure_never_reads_as_a_lost_record() {
        // The record is on disk before `publish` is called and the daemon
        // republishes at boot, so every unhappy phrasing has to be about the
        // mesh. An operator who reads "failed to record" goes looking for data
        // loss that did not happen.
        for outcome in [
            Published::Declined("the daemon has no mesh state yet".into()),
            Published::Unreachable("the daemon at http://127.0.0.1:9741 did not answer".into()),
        ] {
            let note = outcome.note();
            assert!(
                note.contains("not on the mesh yet"),
                "unhelpful phrasing: {note}"
            );
            assert!(
                note.contains("republishes at boot"),
                "the note must say the record is not lost: {note}"
            );
            assert!(
                !note.to_lowercase().contains("not recorded"),
                "must not be confusable with the store-write failure: {note}"
            );
        }
    }

    #[test]
    fn an_invalid_run_is_declined_without_asking_the_daemon() {
        // `to_wire` would refuse it anyway, so the round trip is pure cost — and
        // the reason the operator sees should be the real one (the verdict), not
        // whatever the daemon happened to say.
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let mut rec = sample_record();
        rec.verdict = mm::Verdict::Invalid {
            problems: vec!["trial spread 41% exceeds 25%".into()],
        };
        let out = rt.block_on(publish(&rec));
        assert_eq!(
            out,
            Published::Declined("an invalid run does not travel".into())
        );
    }

    #[test]
    fn a_peer_row_becomes_a_foreign_record_with_its_origin_intact() {
        let body: PeerBody = serde_json::from_value(serde_json::json!({
            "records": [{
                "origin_node": "b88252e4325bc3771122334455667788",
                "origin_name": "BeefyMac",
                "record": serde_json::to_value(sample_record()).unwrap(),
            }],
            "unreadable": 2,
        }))
        .expect("the daemon's shape parses");
        assert_eq!(body.unreadable, 2);
        let f = mm::ForeignRecord {
            origin_node: body.records[0].origin_node.clone(),
            origin_name: body.records[0].origin_name.clone(),
            record: body.records[0].record.clone(),
        };
        assert_eq!(f.describe_origin(), "BeefyMac");
    }

    /// The seam that would fail silently.
    ///
    /// `PeerMeasurementDto` (daemon, `Serialize`) and `PeerRow` (here,
    /// `Deserialize`) are two separate declarations of one wire shape. If a field
    /// is renamed on one side, nothing fails to compile — `#[serde(default)]`
    /// swallows the mismatch and `peer_history` quietly returns an empty list
    /// forever. `mesh plan` would then say "not measured" on a mesh full of
    /// measurements, and there would be no error anywhere to explain it. So the
    /// two are pinned against each other, with the daemon's own type doing the
    /// serializing rather than a hand-written JSON literal that could drift with
    /// it.
    #[test]
    fn the_daemons_shape_and_this_readers_shape_are_the_same_shape() {
        let body = sovereign_mesh::mesh_http::PeerMeasurementsResponse {
            records: vec![sovereign_mesh::mesh_http::PeerMeasurementDto {
                origin_node: "b88252e4325bc3771122334455667788".into(),
                origin_name: Some("BeefyMac".into()),
                record: sample_record(),
            }],
            unreadable: 3,
        };
        let parsed: PeerBody = serde_json::from_value(serde_json::to_value(&body).unwrap())
            .expect("the daemon's response must parse as this reader's body");
        assert_eq!(parsed.unreadable, 3, "unreadable must survive the seam");
        assert_eq!(parsed.records.len(), 1, "records must survive the seam");
        assert_eq!(parsed.records[0].origin_name.as_deref(), Some("BeefyMac"));
        assert_eq!(
            parsed.records[0].record.decode_tok_s,
            sample_record().decode_tok_s
        );
        assert_eq!(parsed.records[0].origin_node, "b88252e4325bc3771122334455667788");
    }

    #[test]
    fn a_peer_with_no_name_is_still_identifiable() {
        let body: PeerBody = serde_json::from_value(serde_json::json!({
            "records": [{
                "origin_node": "b88252e4325bc3771122334455667788",
                "record": serde_json::to_value(sample_record()).unwrap(),
            }],
        }))
        .expect("origin_name is optional — a peer that left keeps its records");
        let f = mm::ForeignRecord {
            origin_node: body.records[0].origin_node.clone(),
            origin_name: body.records[0].origin_name.clone(),
            record: body.records[0].record.clone(),
        };
        assert_eq!(f.describe_origin(), "node-b88252e4325bc377");
    }

    fn sample_record() -> mm::MeasurementRecord {
        let host = mm::HostIdentity::from_live_mesh(Some(0xf0f)).expect("a fingerprint is a host");
        mm::MeasurementRecord {
            key: mm::MeasurementKey::for_plan(
                host,
                "mf1:deadbeef".into(),
                "pd2:cafef00d".into(),
                32768,
                mm::LinkClass::Direct,
            ),
            decode_tok_s: 11.08,
            decode_tok_s_min: 10.29,
            decode_tok_s_max: 11.53,
            ttft_ms: 2203.0,
            itl_p50_ms: 90.0,
            itl_p95_ms: 98.0,
            prefill_tok_s: Some(14.0),
            cold_load_s: None,
            trials: 3,
            content_frames: 256,
            model_name: "Qwen3.5-122B".into(),
            placement_human: "36 local + 12 @beefymac".into(),
            nodes: 2,
            hops: 1,
            measured_at: 1_785_000_000,
            build: "0.10.0".into(),
            backend: Some("vulkan".into()),
            link_rtt_ms: Some(0.4),
            verdict: mm::Verdict::Valid,
            witness: None,
            conditions: None,
        }
    }
}
