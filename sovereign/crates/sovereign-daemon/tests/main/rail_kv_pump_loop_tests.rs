// SPDX-License-Identifier: AGPL-3.0-or-later
//! `rail_kv_pump`'s seal arms, driven over the daemon's assembled node.
//! The KV drain and KV seal moved to cw-rails in five-programs fp-83; their
//! tests are `commonwealth_rails::kv`'s, and the namespace-agreement check
//! is sovereign-mesh's `rail_kv_pump_namespaces`.

use commonwealth_rail_core::{RailAct, SigningKey};
use kernel_types::NodeId;
use sovereign_daemon::state::AppState;
use sovereign_mesh::rail_kv_pump::*;
use sovereign_mesh::rail_port::LocalRingRail;
use sovereign_mesh::ring_roster::tests::{member, mesh_of, pubkey_of};
use std::sync::Arc;

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
    use commonwealth_rail_core::{
        actor_of, body_json, sign_ring_op, Op, Person, RingSigner, Roster, SignedOp,
    };
    use kernel_types::ActorKey;
    use kernel_types::HandoffId;
    use oicp_types::job::{Isolation, JobKind, WorkOffer};
    use oicp_types::work::projection::WorkUnitStatus;
    use oicp_types::work::{Submission, UnitRef, WorkAct};
    use sovereign_mesh::rail_port::RingRailPort;

    use crate::common::work_rails::{payload_of, WorkRails};

    let donor = SigningKey::from_bytes(&[3u8; 32]);
    let submitter = SigningKey::from_bytes(&[4u8; 32]);
    let me = NodeId::from_u128(9);
    let dir = tempfile::tempdir().unwrap();
    let local = LocalRingRail::new(dir.path(), Arc::new(donor.clone()));
    let state = AppState::new_with_platform_and_engine_and_gauge_and_fabric(
        me,
        mesh_of(vec![member(me, "me", Some(pubkey_of(&donor)))]),
        None,
        None,
        sovereign_daemon::state::FabricSeed {
            ring_rail: Some(Arc::new(local.clone())),
            ..Default::default()
        },
    );
    sovereign_mesh::ring_roster::MeshRosterSource::install(
        local.inner(),
        &state.inner.fabric.mesh,
        &state.inner.fabric.identity,
        state.self_node_pubkey(),
    )
    .unwrap();

    assert_eq!(projector_for(WORK_NAMESPACE), Some(Projector::Work));

    let journal = local.inner().journal(WORK_NAMESPACE).unwrap();
    let mut members = std::collections::BTreeMap::new();
    members.insert(Person::from("donor"), vec![donor.actor()]);
    members.insert(Person::from("submitter"), vec![submitter.actor()]);
    let roster = Roster::new(members);
    journal.set_roster(&roster).unwrap();
    // The operator's file narrows `work` and the membership default must
    // not outrank it — the submitter is in no mesh row, only in this file.
    assert_eq!(
        local.inner().roster_origin(WORK_NAMESPACE),
        commonwealth_rail_core::RosterOrigin::File,
        "the default roster would orphan the operator's roster.json"
    );

    // ── the peer submits, this node leases and offers ──
    let kind = JobKind::parse("process:v1").unwrap();
    // Sealed by cw-rails' seal door: the daemon links no rail (pb-work-donor).
    let unit = WorkRails::spawn(None, "")
        .await
        .seal(&kind, vec![serde_json::json!({ "argv": ["uname", "-a"] })])
        .await
        .remove(0);
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
    let now = sovereign_time::unix_now_u64() as i64;
    let sign = |key: &SigningKey, seq: u64, ts: i64, act: &WorkAct| -> Op<SignedOp> {
        let act = RailAct::Record {
            payload: payload_of(act),
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
    let out = seal_once(&*state.inner.fabric).await;
    assert_eq!(
        out.sealed, 1,
        "the work namespace was never sealed: {out:?}"
    );
    assert!(
        out.snapshot_rows >= 2,
        "the seal must re-append this node's live lease AND its offer: {out:?}"
    );

    // ── and the queue survived it ──
    // Folded through the rail port's own projection, over the roster file
    // above — the read the donor makes.
    let projection = local.work_projection().await.unwrap();
    let mine = ActorKey::parse(donor.actor()).unwrap();
    let now_ms = sovereign_time::unix_millis();
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
