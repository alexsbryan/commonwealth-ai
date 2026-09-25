// SPDX-License-Identifier: AGPL-3.0-or-later
//! The daemon-owned namespace list against the rail and the store — moved
//! from sovereign-daemon's `rail_kv_pump_loop_tests` in five-programs fp-83,
//! beside the crate that owns `projector_for` and the roster. The assertions
//! are verbatim; the node is built from the mesh and identity handles
//! `MeshRosterSource::install` reads, rather than from a daemon `AppState`.

use commonwealth_core::ids::NodeId;
use commonwealth_rail_core::SigningKey;
use sovereign_contracts::identity::IdentityReader;
use sovereign_mesh::rail_kv_pump::*;
use sovereign_mesh::rail_port::LocalRingRail;
use sovereign_mesh::ring_roster::tests::{member, mesh_of, pubkey_of};
use std::sync::Arc;
use tokio::sync::RwLock;

/// The rings the daemon writes on its own behalf. No longer a declaration the
/// roster reads — every ring nobody narrowed derives from membership now
/// (`ring_roster::MeshRosterSource::install`) — but still the set whose
/// charset and store routing must agree, which is what the test below checks.
const DAEMON_OWN_NAMESPACES: &[&str] = &[
    sovereign_mesh::mesh_measurements::MEASUREMENTS_APP_ID,
    commonwealth_state::store_adapter::INFERENCE_APP_ID,
    commonwealth_state::CONTRIBUTIONS_APP_ID,
    commonwealth_state::PROCESSED_SHARDS_APP_ID,
    sovereign_contracts::peer::NOTES_APP_ID,
    sovereign_contracts::peer::WORK_ATLAS_APP_ID_PUBLIC,
    corpus_engine::update::newsworthy_watcher::APP_ID_TRACKED,
];

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
    // Held for the test's length: `install` keeps only a weak ref.
    let mesh = Arc::new(RwLock::new(mesh_of(vec![member(
        me,
        "me",
        Some(pubkey_of(&key)),
    )])));
    let rail = LocalRingRail::new(dir.path(), Arc::new(key.clone()));

    // The charset check lives in `derive_roster`, so a namespace the rail
    // would refuse to open cannot be installed either.
    sovereign_mesh::ring_roster::MeshRosterSource::install(
        rail.inner(),
        &mesh,
        &IdentityReader::new(me),
        Some(pubkey_of(&key)),
    )
    .unwrap();

    let mut seen = std::collections::BTreeSet::new();
    for ns in DAEMON_OWN_NAMESPACES {
        assert!(seen.insert(*ns), "{ns} is declared twice");
        // The charset check: a namespace the rail would refuse to open.
        rail.inner()
            .journal(ns)
            .unwrap_or_else(|e| panic!("{ns}: {e}"));
        assert_eq!(
            rail.inner().roster_origin(ns),
            commonwealth_rail_core::RosterOrigin::Derived,
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
