// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

/// A daemon store: a key, a node id, the active mesh with its invite key,
/// and one parked mesh.
pub(crate) fn daemon_store(
    dir: &Path,
) -> (
    Vec<u8>,
    commonwealth_core::mesh::Mesh,
    commonwealth_core::mesh::Mesh,
) {
    let key = vec![7u8; 32];
    std::fs::write(dir.join(NODE_KEY_FILE), &key).unwrap();
    let (active, active_key) =
        commonwealth_discovery::membership::init_mesh("Home", "RuggedFox", Vec::new());
    let self_id = *active.members.keys().next().expect("the founder");
    std::fs::write(
        sovereign_contracts::node_identity::node_id_file(dir),
        self_id.as_bytes(),
    )
    .unwrap();
    let (parked, _) = commonwealth_discovery::membership::init_mesh("Lab", "RuggedFox", Vec::new());
    crate::daemon_store::save(dir, &parked, self_id).unwrap();
    crate::daemon_store::save_and_activate(dir, &active, self_id).unwrap();
    crate::daemon_store::save_join_key(dir, &active_key).unwrap();
    (key, active, parked)
}

/// A main-era data dir is told to run `svrn mesh up`, donor included, and the
/// handover itself is what stops it (pb-distribution-f8): one decider, the
/// handover's own marker. Failing input: a notice keyed on anything the
/// handover does not change keeps naming the verb after it ran.
#[test]
fn the_upgrade_notice_names_mesh_up_until_the_handover_runs() {
    use sovereign_contracts::node_identity::mesh_handover_notice;
    let svrn = tempfile::tempdir().unwrap();
    let rails = tempfile::tempdir().unwrap();
    assert_eq!(mesh_handover_notice(svrn.path(), true), None, "a fresh dir");
    daemon_store(svrn.path());

    let before = mesh_handover_notice(svrn.path(), true).expect("a main-era dir");
    assert!(
        before.contains("`svrn mesh up`") && before.contains("[compute.work_offer]"),
        "{before}"
    );
    hand_over(svrn.path(), rails.path(), false).unwrap();
    assert_eq!(mesh_handover_notice(svrn.path(), true), None);
}

/// The daemon's key, node id and meshes become cw-rails', over a solo key
/// cw-rails minted for itself. Failing input: skip the handover, and
/// cw-rails keeps its solo key and an empty store.
#[test]
fn the_daemon_identity_becomes_cw_rails_identity() {
    let svrn = tempfile::tempdir().unwrap();
    let rails = tempfile::tempdir().unwrap();
    let (key, active, parked) = daemon_store(svrn.path());
    std::fs::write(rails.path().join(NODE_KEY_FILE), [9u8; 32]).unwrap();

    let done = hand_over(svrn.path(), rails.path(), false).unwrap();

    assert_eq!(done, Handover::Moved { meshes: 2 });
    assert_eq!(
        std::fs::read(rails.path().join(NODE_KEY_FILE)).unwrap(),
        key
    );
    assert_eq!(
        std::fs::read(rails.path().join("node_key.pre-handover")).unwrap(),
        vec![9u8; 32],
        "cw-rails' own key is kept aside, never deleted"
    );
    let self_id = *active.members.keys().next().unwrap();
    assert_eq!(identity::load_node_id(rails.path()).unwrap(), Some(self_id));
    let mesh = identity::load_mesh(rails.path())
        .unwrap()
        .expect("the active mesh");
    assert_eq!((mesh.id, mesh.name.as_str()), (active.id, "Home"));
    assert_eq!(mesh.mesh_secret, active.mesh_secret);
    assert!(identity::load_join_key(rails.path()).unwrap().is_some());
    let known = commonwealth_rails::known::parked(rails.path()).unwrap();
    assert_eq!(known.len(), 1);
    assert_eq!(known[0].mesh.id, parked.id);
    // The daemon keeps its node_id (svrn-side readers) and retires its key.
    assert!(sovereign_contracts::node_identity::node_id_file(svrn.path()).exists());
    assert!(!svrn.path().join(NODE_KEY_FILE).exists());
    assert!(svrn.path().join(HANDED_OVER_KEY).exists());
    // A second run finds no key to move.
    assert_eq!(
        hand_over(svrn.path(), rails.path(), false).unwrap(),
        Handover::NothingToMove
    );
}

/// The two-key node (phase-b-4): cw-rails runs on its own solo key, and the
/// work ring's roster names the DAEMON's key, as `svrn ring roster add
/// --self` wrote it before the flip. After the handover cw-rails signs with
/// the daemon's key, so its work acts are admitted by that roster and the
/// node keeps its roster identity. Failing input: skip the handover, and
/// cw-rails' solo key is refused by the roster (the control below).
#[test]
fn a_two_key_node_keeps_the_daemon_keys_roster_identity() {
    use commonwealth_rail::{actor_of, Person, RailAct, RingRail, Roster, SigningKey};
    use std::sync::Arc;
    let svrn = tempfile::tempdir().unwrap();
    let rails = tempfile::tempdir().unwrap();
    let (key, _active, _parked) = daemon_store(svrn.path());
    let daemon_key = SigningKey::from_bytes(&key.clone().try_into().unwrap());
    let solo_key = SigningKey::from_bytes(&[9u8; 32]);
    std::fs::write(rails.path().join(NODE_KEY_FILE), solo_key.to_bytes()).unwrap();
    let mut members = std::collections::BTreeMap::new();
    members.insert(Person::from("RuggedFox"), vec![actor_of(&daemon_key)]);
    let roster = Roster::new(members);
    RingRail::new(rails.path(), Arc::new(solo_key.clone()))
        .journal("work")
        .unwrap()
        .set_roster(&roster)
        .unwrap();
    let act = || {
        RailAct::from_json(serde_json::json!({ "op": "record", "payload": { "n": 1 } })).unwrap()
    };
    let signs_with = |k: SigningKey| {
        let rail = RingRail::new(rails.path(), Arc::new(k));
        let journal = rail.journal("work").unwrap();
        journal.append(
            act(),
            rail.signer(),
            &roster,
            None,
            &commonwealth_rail::Ed25519Verifier,
        )
    };
    assert!(
        signs_with(solo_key).is_err(),
        "control: without the handover cw-rails' solo key is not on the work roster"
    );

    assert_eq!(
        hand_over(svrn.path(), rails.path(), false).unwrap(),
        Handover::Moved { meshes: 2 }
    );
    let rails_key: [u8; 32] = std::fs::read(rails.path().join(NODE_KEY_FILE))
        .unwrap()
        .try_into()
        .unwrap();
    signs_with(SigningKey::from_bytes(&rails_key))
        .expect("cw-rails' work act, signed with the handed-over key, is admitted");
}

/// A running cw-rails holds its key in memory: nothing moves, and the
/// daemon's key waits for the next `svrn mesh up`.
#[test]
fn nothing_moves_under_a_running_cw_rails() {
    let svrn = tempfile::tempdir().unwrap();
    let rails = tempfile::tempdir().unwrap();
    daemon_store(svrn.path());
    std::fs::write(rails.path().join(NODE_KEY_FILE), [9u8; 32]).unwrap();

    assert_eq!(
        hand_over(svrn.path(), rails.path(), true).unwrap(),
        Handover::Deferred
    );
    assert_eq!(
        std::fs::read(rails.path().join(NODE_KEY_FILE)).unwrap(),
        vec![9u8; 32]
    );
    assert!(svrn.path().join(NODE_KEY_FILE).exists());
}
