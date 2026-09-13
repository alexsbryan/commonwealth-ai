// SPDX-License-Identifier: AGPL-3.0-or-later
//! `rail_kv_pump`'s tests. A sibling file only so `rail_kv_pump.rs` stays
//! under ARCH §3.1's 1200-line ceiling — moved verbatim, nothing renamed.
//! The pump's own decisions. The two-node path — a write reaching a peer's
//! store, a delete, a seal, an excluded namespace — is driven end to end
//! against the real router in `crate::ring_sync::tests`.

use super::*;
use crate::ring_roster::tests::{member, mesh_of, pubkey_of};
use crate::ring_roster::DAEMON_OWN_NAMESPACES;
use commonwealth_core::ids::NodeId;
use commonwealth_rail::SigningKey;

/// **The declaration is checkable, and this is the check.**
///
/// Two properties, both of which have already failed once in this
/// workspace. The charset one is `wikipedia-newsworthy:tracked`: a colon is
/// legal in an `app_id` and not in a ring namespace, and the mismatch was
/// silent until something tried to open the directory. The exclusion one is
/// the split between the six namespaces that reach the ring through the
/// OUTBOX and the one that does not — `mesh-measurements` is
/// gossip-excluded, publishes straight onto its journal from
/// `POST /v1/mesh/measurements`, and would be refused by both the outbox
/// and `apply_projection` if anything tried to route it through the store.
#[tokio::test]
async fn every_declared_namespace_is_one_the_rail_and_the_store_agree_about() {
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let me = NodeId::from_u128(1);
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::new(me, mesh_of(vec![member(me, "me", Some(pubkey_of(&key)))]));
    let rail = RingRail::new(dir.path(), Arc::new(key));

    // The charset check lives in `derive_roster`, so a namespace the rail
    // would refuse to open cannot be installed either.
    crate::ring_roster::MeshRosterSource::install(&rail, &state).unwrap();

    let mut seen = std::collections::BTreeSet::new();
    for ns in DAEMON_OWN_NAMESPACES {
        assert!(seen.insert(*ns), "{ns} is declared twice");
        assert_eq!(
            rail.roster_origin(ns),
            commonwealth_rail::RosterOrigin::Derived,
            "{ns} still reads a roster file"
        );
        assert_eq!(
            commonwealth_state::is_gossip_excluded(ns),
            ns == &MEASUREMENTS_NAMESPACE,
            "{ns}: a namespace on this list either rides the outbox or is \
             the one that publishes straight onto its journal, and which \
             one it is decides whether the store will carry it at all"
        );
        assert_eq!(
            projector_for(ns),
            if ns == &MEASUREMENTS_NAMESPACE {
                None
            } else {
                Some(Projector::Kv)
            },
            "{ns}: a daemon-owned namespace is either the measurement \
             vocabulary or the KV one — the work plane is deliberately not \
             on this list, so `Work` must never appear here"
        );
    }
}

/// **A node in no mesh queues its writes; it does not lose them.**
///
/// `NotInRoster` is the one refusal that is not a refusal — a solo daemon
/// is a normal daemon, and its writes travel the moment it joins. Dropping
/// them would make a legitimate condition permanent, and retrying an
/// append the rail will never accept would be the other failure. The
/// second half is the control: the same row, the same pump, one member
/// added.
#[tokio::test]
async fn a_node_in_no_mesh_keeps_its_writes_queued_until_membership_exists() {
    const KV: &str = sovereign_serving::INFERENCE_APP_ID;
    let key = SigningKey::from_bytes(&[2u8; 32]);
    let me = NodeId::from_u128(7);
    let dir = tempfile::tempdir().unwrap();
    // A mesh with nobody in it: this node cannot place its own key.
    let state = AppState::new(me, mesh_of(vec![]));
    let rail = Arc::new(RingRail::new(dir.path(), Arc::new(key.clone())));
    crate::ring_roster::MeshRosterSource::install(&rail, &state).unwrap();
    state.install_ring_rail(rail.clone());

    assert!(state
        .inner
        .mesh_store
        .set(KV, "plan", bytes::Bytes::from_static(b"v1"), me)
        .unwrap());

    let out = pump_once(&state).await;
    assert_eq!(
        (out.appended, out.deferred, out.refused),
        (0, 1, 0),
        "{out:?}"
    );
    assert_eq!(
        state.inner.mesh_store.outbox_len().unwrap(),
        1,
        "a deferred write stays queued"
    );

    // Membership arrives, and the same row goes out.
    state
        .inner
        .mesh
        .write()
        .await
        .members
        .insert(me, member(me, "me", Some(pubkey_of(&key))));
    let out = pump_once(&state).await;
    assert_eq!(
        (out.appended, out.deferred, out.refused),
        (1, 0, 0),
        "{out:?}"
    );
    assert_eq!(state.inner.mesh_store.outbox_len().unwrap(), 0);
}

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
    use commonwealth_core::ids::HandoffId;
    use commonwealth_rail::{
        actor_of, body_json, sign_ring_op, Op, Person, RingSigner, Roster, SignedOp,
    };
    use commonwealth_work::projection::{WorkProjection, WorkUnitStatus};
    use commonwealth_work::{ActorKey, Submission, UnitRef, WorkAct};
    use oicp_types::job::{Isolation, JobKind, JobRequirements, WorkOffer};

    let donor = SigningKey::from_bytes(&[3u8; 32]);
    let submitter = SigningKey::from_bytes(&[4u8; 32]);
    let me = NodeId::from_u128(9);
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::new(me, mesh_of(vec![member(me, "me", Some(pubkey_of(&donor)))]));
    let rail = Arc::new(RingRail::new(dir.path(), Arc::new(donor.clone())));
    crate::ring_roster::MeshRosterSource::install(&rail, &state).unwrap();
    state.install_ring_rail(rail.clone());

    // The roster in v0 is the operator's file, and it stays that way:
    // `work` is deliberately not in `DAEMON_OWN_NAMESPACES`, so nothing
    // derived it out from under the file this test is about to write.
    assert_eq!(
        rail.roster_origin(WORK_NAMESPACE),
        commonwealth_rail::RosterOrigin::File,
        "joining DAEMON_OWN_NAMESPACES would orphan the operator's roster.json"
    );
    assert_eq!(projector_for(WORK_NAMESPACE), Some(Projector::Work));

    let journal = rail.journal(WORK_NAMESPACE).unwrap();
    let mut members = std::collections::BTreeMap::new();
    members.insert(Person::from("donor"), vec![donor.actor()]);
    members.insert(Person::from("submitter"), vec![submitter.actor()]);
    let roster = Roster::new(members);
    journal.set_roster(&roster).unwrap();

    // ── the peer submits, this node leases and offers ──
    let kind = JobKind::parse("process:v1").unwrap();
    let unit = commonwealth_work::seal::seal(
        kind.clone(),
        serde_json::json!({ "argv": ["uname", "-a"] }),
        JobRequirements::any(),
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
        let payload = commonwealth_work::to_payload(act).expect("a work act is payloadable");
        let act = RailAct::Record { payload };
        let sig = sign_ring_op(key, WORK_NAMESPACE, ts, seq, &body_json(&act));
        Op::new(SignedOp { seq, sig, act }, ts, actor_of(key))
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
    let out = pump_once(&state).await;
    assert_eq!(
        out.sealed, 1,
        "the work namespace was never sealed: {out:?}"
    );
    assert!(
        out.snapshot_rows >= 2,
        "the seal must re-append this node's live lease AND its offer: {out:?}"
    );

    // ── and the queue survived it ──
    let admission = journal.admit(&roster, &Ed25519Verifier).unwrap();
    let projection = WorkProjection::fold(&admission);
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
