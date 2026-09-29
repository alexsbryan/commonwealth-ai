// SPDX-License-Identifier: AGPL-3.0-or-later
//! The CLI's side of measurement travel: publish what we measured, read what
//! peers did.
//!
//! ## Why cw-rails is in the loop at all
//!
//! `svrn mesh bench` and `svrn mesh plan` run in this process, and the ring
//! journal a record travels on is cw-rails': it signs, it seals, and its
//! anti-entropy carries the lines to every member. So this module appends and
//! reads through cw-rails' rail doors (`crate::measurements_rail`), and no
//! other process sits between (pb-serve-placement retired the daemon's
//! `/v1/mesh/measurements` hop).
//!
//! ## Why a failure here is never an error
//!
//! Both operations degrade to nothing and say so. A run is written to
//! `~/.svrnmesh/mesh-measurements.json` *before* it is published, and serve's
//! reconcile loop republishes that file whenever cw-rails answers — so a
//! failed publish is genuinely "not yet" rather than "lost", and phrasing it as
//! a failure would send the operator looking for a problem that will fix
//! itself. Likewise `mesh plan` is a useful command on a solo machine with no
//! cw-rails running: a missing peer half makes the answer smaller, not wrong.
//!
//! Nothing here decides *what* may travel or under what key. That policy is
//! `crate::mesh_measurements`.

use crate::measurements_rail;
use crate::mesh_measurements as mm;
use sovereign_cli_base::rail::RailDoorError;

// ---------------------------------------------------------------------------
// Publish
// ---------------------------------------------------------------------------

/// What became of a publish attempt.
#[derive(Debug, Clone, PartialEq)]
pub enum Published {
    /// On the journal, under this op id.
    Yes { key: String },
    /// cw-rails took the request and declined it, for the reason it gave.
    /// Usually "not in a mesh yet", which is the normal state of a solo node.
    Declined(String),
    /// cw-rails could not be asked.
    Unreachable(String),
}

impl Published {
    /// One line for the operator, indented to sit under the run summary.
    ///
    /// Every phrasing is about the *mesh*, never about the record — the record is
    /// already safely on disk by the time this is called, and a line that read
    /// like a write failure would be a lie about the thing the operator cares
    /// most about.
    pub fn note(&self) -> String {
        match self {
            Published::Yes { key } => format!("published to the mesh as {key}"),
            Published::Declined(why) | Published::Unreachable(why) => {
                format!("not on the mesh yet — {why}; serve republishes it once cw-rails can")
            }
        }
    }

    /// The machine-readable form for `--json`. `daemon_reachable` keeps its
    /// name for the readers of this shape; the daemon it names is cw-rails.
    pub fn as_json(&self) -> serde_json::Value {
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

/// Hand a freshly-taken measurement to cw-rails' journal.
///
/// Called after the record is on disk, never before. An `Err`-shaped outcome is
/// returned as a variant rather than a `Result` because there is no caller who
/// should treat it as a failure — `mesh bench` reports it and still exits on the
/// verdict of the *run*, which is what the operator asked about.
pub async fn publish(record: &mm::MeasurementRecord) -> Published {
    if !record.verdict.is_valid() {
        return Published::Declined("an invalid run does not travel".to_string());
    }
    let base = measurements_rail::rails_base(None);
    match measurements_rail::publish(&base, record).await {
        Ok(key) => Published::Yes { key },
        Err(RailDoorError::Unreachable(why)) => Published::Unreachable(why),
        Err(RailDoorError::Refused(why)) => Published::Declined(why),
    }
}

// ---------------------------------------------------------------------------
// Read
// ---------------------------------------------------------------------------

/// What peers have measured, plus why the list might be shorter than expected.
#[derive(Debug, Default, Clone)]
pub struct TravelHistory {
    /// Peer records, ready to hand to [`mm::near_misses`].
    pub records: Vec<mm::ForeignRecord>,
    /// Journal lines admitted but unreadable as measurements — usually a peer
    /// on an incompatible schema — plus every line admission could not account
    /// for. Surfaced so that "no peer has measured this" is distinguishable
    /// from "a peer has, in a dialect we do not speak."
    pub unreadable: usize,
    /// Why nothing came back, when the reason is worth an operator's attention.
    /// `None` on the ordinary path, including the ordinary empty one.
    pub note: Option<String>,
}

/// Every measurement peers have put on the journal.
///
/// Excludes our own by construction — cw-rails' own authorship is dropped, and
/// our copy on disk is the authoritative one. The publisher is named by the
/// roster cw-rails admitted the line under, never by the payload; the signing
/// key stands in for the node id.
pub async fn peer_history() -> TravelHistory {
    let base = measurements_rail::rails_base(None);
    match measurements_rail::peers(&base).await {
        Ok(seen) => TravelHistory {
            // One count, not two: an undecodable line and an unplaceable one
            // both mean "this answer covers less than the ring holds".
            unreadable: seen.unreadable + seen.gaps,
            records: seen
                .found
                .into_iter()
                .map(|m| mm::ForeignRecord {
                    origin_node: m.actor,
                    origin_name: Some(m.person.as_str().to_string()),
                    record: m.record,
                })
                .collect(),
            note: None,
        },
        // Silent on the common case: `mesh plan` with no cw-rails running is a
        // normal invocation and a warning here would be noise on every run.
        Err(RailDoorError::Unreachable(_)) => TravelHistory::default(),
        Err(RailDoorError::Refused(why)) => TravelHistory {
            note: Some(format!(
                "cw-rails' peer measurements were unreadable ({why})"
            )),
            ..TravelHistory::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_publish_failure_never_reads_as_a_lost_record() {
        // The record is on disk before `publish` is called and serve's
        // reconcile republishes it, so every unhappy phrasing has to be about
        // the mesh. An operator who reads "failed to record" goes looking for data
        // loss that did not happen.
        for outcome in [
            Published::Declined("this node is not in a mesh yet".into()),
            Published::Unreachable("cannot reach cw-rails at http://127.0.0.1:9747".into()),
        ] {
            let note = outcome.note();
            assert!(
                note.contains("not on the mesh yet"),
                "unhelpful phrasing: {note}"
            );
            assert!(
                note.contains("serve republishes it"),
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
        // whatever cw-rails happened to say.
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
