// SPDX-License-Identifier: AGPL-3.0-or-later
//! The plane seal arms over this process's rail — moved from sovereign-daemon's
//! `rail_kv_pump_loop_tests` with the arms (pb-mesh-exit-mesh). The assertions
//! are verbatim; the node is a `RingRail` on a temp root rather than a daemon
//! `AppState` over the rail port, and the unit is sealed with
//! `commonwealth_work::seal` rather than through a spawned cw-rails' seal door.

use std::sync::Arc;

use commonwealth_rail::{
    actor_of, body_json, sign_ring_op, Ed25519Verifier, Op, Person, RailAct, RingRail, RingSigner,
    Roster, SignedOp, SigningKey,
};
use commonwealth_work::projection::WorkUnitStatus;
use commonwealth_work::{ActorKey, HandoffId, Submission, UnitRef, WorkAct};
use oicp_types::job::{Isolation, JobKind, JobRequirements, WorkOffer};

use super::*;

/// **A seal on `work` must not delete the queue it is sealing.**
///
/// The KV snapshot re-appends this node's live set from the STORE and the
/// measurements one from the local FILE. The work plane has neither: the
/// journal IS its state, so the live set has to be captured from the fold
/// BEFORE the prune runs. A seal with no snapshot behind it retires this
/// node's own `Lease` and hands a unit it is still running back to the
/// queue for somebody else to take — the work-plane shape of the same
/// "without this a seal is a delete" the KV snapshot's docs name.
///
/// **The submitter is a PEER on purpose.** A seal retires only the
/// sealer's own lines below its own floor, so the `Submit` survives and
/// the donor's re-appended `Lease` still has a unit to name. That is also
/// the two-node shape the whole rung exists for: submitted on one node,
/// leased on another.
///
/// Two things are asserted, and they fail differently: the seal HAPPENED
/// (`sealed`/`snapshot_rows`), and the fold of what is left on disk still
/// puts this node in the lease. A green on the first alone would be a
/// journal that shrank and a queue that forgot.
#[tokio::test]
async fn work_namespace_seals_and_keeps_live_leases() {
    let donor = SigningKey::from_bytes(&[3u8; 32]);
    let submitter = SigningKey::from_bytes(&[4u8; 32]);
    let dir = tempfile::tempdir().unwrap();
    let rail = RingRail::new(dir.path(), Arc::new(donor.clone()));

    assert_eq!(projector_for(WORK_NAMESPACE), Some(Projector::Work));

    let journal = rail.journal(WORK_NAMESPACE).unwrap();
    let mut members = std::collections::BTreeMap::new();
    members.insert(Person::from("donor"), vec![donor.actor()]);
    members.insert(Person::from("submitter"), vec![submitter.actor()]);
    let roster = Roster::new(members);
    journal.set_roster(&roster).unwrap();
    assert_eq!(
        rail.roster_origin(WORK_NAMESPACE),
        commonwealth_rail::RosterOrigin::File,
        "the operator's roster.json names who may write `work`"
    );

    // ── the peer submits, this node leases and offers ──
    let kind = JobKind::parse("process:v1").unwrap();
    let unit = commonwealth_work::seal(
        kind.clone(),
        serde_json::json!({ "argv": ["uname", "-a"] }),
        JobRequirements::default(),
        None,
    )
    .unwrap();
    let handoff = HandoffId::from_u128(11);
    let unit_ref = UnitRef {
        handoff,
        unit_hash: unit.unit_hash.clone(),
    };
    let offer = WorkOffer {
        kinds: vec![kind.clone()],
        max_concurrent: 2,
        yield_to_foreground: false,
        isolation: Isolation::Subprocess,
        os: "linux".into(),
        arch: "x86_64".into(),
        repos: vec![],
        accept_from: None,
    };

    // Signed by hand rather than through `journal.append`, for two
    // reasons: the padding below has to cross `SEAL_AFTER_OWN_OPS` and
    // `append` re-reads the whole log per call, and the total order is
    // `(ts_unix, actor, id)` — so a submit, a lease and its renews written
    // in the same second would be ordered by op id, which is a hash.
    // Distinct seconds make the order the one the scenario means.
    let now = commonwealth_core::clock::unix_now_secs() as i64;
    let sign = |key: &SigningKey, seq: u64, ts: i64, act: &WorkAct| -> Op<SignedOp> {
        let act = RailAct::Record {
            payload: commonwealth_work::to_payload(act).expect("a work act is a payload"),
        };
        let sig = sign_ring_op(key, WORK_NAMESPACE, ts, seq, &body_json(&act, None));
        Op::new(
            SignedOp {
                seq,
                sig,
                act,
                on_behalf_of: None,
            },
            ts,
            actor_of(key),
        )
    };

    let mut ops = vec![
        sign(
            &submitter,
            0,
            now - 5,
            &WorkAct::Submit(Submission::new(
                handoff,
                kind.clone(),
                vec![unit.clone()],
                None,
                None,
            )),
        ),
        sign(&donor, 0, now - 4, &WorkAct::Offer(offer.clone())),
        sign(&donor, 1, now - 3, &WorkAct::Lease(unit_ref.clone())),
    ];
    // Enough of THIS node's own history above its floor to trip the seal.
    // Renews, because that is what a donor holding a lease actually
    // writes at volume, and the last one is what keeps the lease live.
    let renew = WorkAct::Renew(unit_ref.clone());
    for i in 0..SEAL_AFTER_OWN_OPS as u64 {
        ops.push(sign(&donor, 2 + i, now - 2, &renew));
    }
    assert_eq!(journal.ingest_all(&ops).unwrap(), ops.len());

    // ── the pump's own seal check reaches this namespace ──
    let out = seal_once(&rail).await;
    assert_eq!(
        out.sealed, 1,
        "the work namespace was never sealed: {out:?}"
    );
    assert!(
        out.snapshot_rows >= 2,
        "the seal must re-append this node's live lease AND its offer: {out:?}"
    );

    // ── and the queue survived it ──
    // Folded the way the work doors fold it, over the roster file above —
    // the read the donor makes.
    let roster = rail.roster(&journal).await.unwrap();
    let projection =
        commonwealth_work::projection::fold(&journal.admit(&roster, &Ed25519Verifier).unwrap());
    let mine = ActorKey::parse(donor.actor()).unwrap();
    let now_ms = commonwealth_core::clock::unix_now_millis();
    let held = projection
        .handoffs
        .get(&handoff)
        .unwrap_or_else(|| panic!("the peer's submission was retired by our own seal"))
        .units
        .get(&unit.unit_hash)
        .expect("the unit is still on the handoff")
        .status_at(now_ms);
    match held {
        WorkUnitStatus::Leased { lessee, .. } => assert_eq!(lessee, mine),
        other => panic!("the seal handed a running unit back to the queue: {other:?}"),
    }
    assert!(
        projection.offers.contains_key(&mine),
        "the seal retired this node's offer and nothing put it back"
    );
}
