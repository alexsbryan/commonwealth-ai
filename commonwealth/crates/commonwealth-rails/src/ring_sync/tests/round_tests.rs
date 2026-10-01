// SPDX-License-Identifier: AGPL-3.0-or-later
//! A ring is read by its roster — offered to it by the round, and served to
//! it by the route (the daemon's `ring_sync_by_roster`, here since the flip).
//!
//! Three nodes, because two cannot express the case: A writes, B is on the
//! roster, C is a full mesh member and is not. (a) is the SENDER's filter and
//! (b) the SERVER's, which is why dropping the server's check leaves (a)
//! green.

use std::net::SocketAddr;
use std::sync::Arc;

use commonwealth_core::ids::{NodeId, NodePubkey};
use commonwealth_core::mesh::{MemberRecord, Mesh, NodeStatus};
use commonwealth_rail::{actor_of, Op, Person, RailAct, RingRail, Roster, SignedOp, SigningKey};

use super::super::*;
use super::{ring_router, serve_at};

/// The narrowed ring: a `roster.json` naming two of the three members.
const NS_FILE: &str = "house-expenses";
/// A ring nobody narrowed: membership answers it.
const NS_OPEN: &str = "house-photos";

pub(in crate::ring_sync) fn member(
    id: NodeId,
    name: &str,
    key: &SigningKey,
    addr: Option<SocketAddr>,
) -> MemberRecord {
    MemberRecord {
        node_id: id,
        name: name.to_string(),
        invited_by: id,
        joined_at: 1,
        last_seen: 1,
        status: NodeStatus::Online,
        capabilities: crate::gossip::minimal_capabilities(1, &[], None),
        addresses: addr.into_iter().collect(),
        node_pubkey: Some(NodePubkey(key.verifying_key().to_bytes())),
        relay_url: None,
        iroh_direct_addrs: Vec::new(),
        dial_info_version: 0,
        dial_info_sig: None,
        removed_at: None,
    }
}

pub(in crate::ring_sync) fn mesh_of(members: Vec<MemberRecord>) -> Mesh {
    let (mut mesh, _invite) =
        commonwealth_discovery::membership::init_mesh("three", "founder", Vec::new());
    mesh.members.clear();
    for m in members {
        mesh.members.insert(m.node_id, m);
    }
    mesh
}

/// A round's host over a fixed membership, dialling members' addresses with
/// the plain-IP transport.
struct Host {
    rail: Arc<RingRail>,
    mesh: Mesh,
    self_id: NodeId,
}

impl RingSyncHost for Host {
    fn journal(&self) -> Option<Arc<dyn RingSyncJournal>> {
        Some(self.rail.clone() as Arc<dyn RingSyncJournal>)
    }

    fn http(&self) -> Result<&reqwest::Client, &str> {
        peer_client()
    }

    fn members(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<RoundMembers>> + Send + '_>>
    {
        Box::pin(async move { Some(RoundMembers::of(&self.mesh, self.self_id, 1)) })
    }

    fn transport(&self) -> Arc<dyn commonwealth_transport::PeerTransport> {
        Arc::new(commonwealth_transport::IpTransport::default())
    }
}

fn file_roster(named: &[(&str, &SigningKey)]) -> Roster {
    let mut members = std::collections::BTreeMap::new();
    for (person, key) in named {
        members.insert(Person::from(*person), vec![actor_of(key)]);
    }
    Roster::new(members)
}

/// A rail under `dir` signing as `key`: `NS_FILE` rostered to `rostered`,
/// `NS_OPEN` answered by `mesh` through the membership source.
fn rail(
    dir: &std::path::Path,
    key: &SigningKey,
    self_id: NodeId,
    mesh: Mesh,
    rostered: &[(&str, &SigningKey)],
) -> Arc<RingRail> {
    let rail = Arc::new(RingRail::new(dir, Arc::new(key.clone())));
    rail.journal(NS_FILE)
        .unwrap()
        .set_roster(&file_roster(rostered))
        .unwrap();
    rail.journal(NS_OPEN).unwrap();
    let mesh = Arc::new(tokio::sync::RwLock::new(mesh));
    crate::rail::MembershipRosterSource::install(
        &rail,
        &mesh,
        self_id,
        Some(NodePubkey(key.verifying_key().to_bytes())),
    );
    // The source holds the mesh weakly; the test holds it for the rail's life.
    std::mem::forget(mesh);
    rail
}

async fn append(rail: &RingRail, ns: &str, amount: u64) {
    let journal = rail.journal(ns).unwrap();
    let roster = RingRail::roster(rail, &journal).await.unwrap();
    let act = RailAct::from_json(
        serde_json::json!({ "op": "record", "payload": { "kind": "expense", "amount": amount } }),
    )
    .unwrap();
    journal
        .append(act, rail.signer(), &roster, None)
        .unwrap_or_else(|e| panic!("append to {ns}: {e}"));
}

fn ops(rail: &RingRail, ns: &str) -> Vec<Op<SignedOp>> {
    rail.journal(ns).unwrap().read().unwrap().0
}

struct Cast {
    ka: SigningKey,
    kb: SigningKey,
    kc: SigningKey,
    a: NodeId,
    b: NodeId,
    c: NodeId,
}

fn cast() -> Cast {
    Cast {
        ka: SigningKey::from_bytes(&[21u8; 32]),
        kb: SigningKey::from_bytes(&[22u8; 32]),
        kc: SigningKey::from_bytes(&[23u8; 32]),
        a: NodeId::from_u128(401),
        b: NodeId::from_u128(402),
        c: NodeId::from_u128(403),
    }
}

/// A's round host, with B's and C's ring routes live at the addresses A's
/// membership names. `a_rostered` is what A's `NS_FILE` roster names.
async fn three_nodes(
    c: &Cast,
    dirs: &[tempfile::TempDir; 3],
    a_rostered: &[(&str, &SigningKey)],
) -> (Host, Arc<RingRail>, Arc<RingRail>) {
    let both = [("alex", &c.ka), ("bea", &c.kb)];
    let full = |b_addr, c_addr| {
        mesh_of(vec![
            member(c.a, "alex", &c.ka, None),
            member(c.b, "bea", &c.kb, b_addr),
            member(c.c, "cass", &c.kc, c_addr),
        ])
    };
    let b_rail = rail(dirs[1].path(), &c.kb, c.b, full(None, None), &both);
    let b_addr = serve_at(ring_router(b_rail.clone())).await;
    let c_rail = rail(dirs[2].path(), &c.kc, c.c, full(None, None), &both);
    let c_addr = serve_at(ring_router(c_rail.clone())).await;
    let mesh = full(Some(b_addr), Some(c_addr));
    let a_rail = rail(dirs[0].path(), &c.ka, c.a, mesh.clone(), a_rostered);
    (
        Host {
            rail: a_rail,
            mesh,
            self_id: c.a,
        },
        b_rail,
        c_rail,
    )
}

fn dirs() -> [tempfile::TempDir; 3] {
    [
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    ]
}

/// **(a) A ring is offered to its roster and to nobody else.** Failing
/// input: the round's per-namespace filter reverted to the round-level peer
/// list, which sends C the whole journal it is on no roster for.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_ring_is_offered_to_its_roster_and_to_no_other_member() {
    let c = cast();
    let d = dirs();
    let (a, b_rail, c_rail) = three_nodes(&c, &d, &[("alex", &c.ka), ("bea", &c.kb)]).await;
    append(&a.rail, NS_FILE, 1).await;
    append(&a.rail, NS_FILE, 2).await;
    for _ in 0..2 {
        run_one_round(&a).await;
    }
    assert_eq!(ops(&b_rail, NS_FILE), ops(&a.rail, NS_FILE));
    assert!(
        ops(&c_rail, NS_FILE).is_empty(),
        "C is a mesh member on no roster for {NS_FILE} and must hold nothing"
    );
}

/// **(c) A ring nobody narrowed still reaches every member** — the clause
/// the filter could break invisibly.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_derived_roster_still_reaches_every_member() {
    let c = cast();
    let d = dirs();
    let (a, b_rail, c_rail) = three_nodes(&c, &d, &[("alex", &c.ka), ("bea", &c.kb)]).await;
    assert_eq!(
        a.rail.roster_origin(NS_OPEN),
        commonwealth_rail::RosterOrigin::Derived,
        "{NS_OPEN} must be answered by membership or this is not the case it names"
    );
    append(&a.rail, NS_OPEN, 7).await;
    run_one_round(&a).await;
    assert_eq!(ops(&b_rail, NS_OPEN).len(), 1, "{NS_OPEN} reaches B");
    assert_eq!(ops(&c_rail, NS_OPEN).len(), 1, "{NS_OPEN} reaches C");
}

/// **(b) The route refuses a namespace whose roster does not name the
/// verified asker, by name**, and serves the member it names. The asker's
/// key rides `x-mesh-pubkey` as cw-rails' acceptor stamps it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_sync_route_refuses_a_member_the_roster_does_not_name() {
    let c = cast();
    let dir = tempfile::tempdir().unwrap();
    let mesh = mesh_of(vec![
        member(c.a, "alex", &c.ka, None),
        member(c.b, "bea", &c.kb, None),
        member(c.c, "cass", &c.kc, None),
    ]);
    let a_rail = rail(
        dir.path(),
        &c.ka,
        c.a,
        mesh,
        &[("alex", &c.ka), ("bea", &c.kb)],
    );
    append(&a_rail, NS_FILE, 3).await;
    let a_addr = serve_at(ring_router(a_rail)).await;
    let ask = |ns: &str, key: &SigningKey| {
        reqwest::Client::new()
            .post(format!("http://{a_addr}/internal/ring/sync"))
            .header("x-mesh-pubkey", hex::encode(key.verifying_key().to_bytes()))
            .json(&serde_json::json!({
                "namespace": ns,
                "digest": commonwealth_rail::Digest::default(),
                "ops": [],
            }))
            .send()
    };
    let refused = ask(NS_FILE, &c.kc).await.unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::FORBIDDEN);
    let said = refused.text().await.unwrap();
    let c_key = hex::encode(c.kc.verifying_key().to_bytes());
    assert!(
        said.contains(NS_FILE) && said.contains(&c_key),
        "the refusal names the namespace and the asker: {said}"
    );
    let served = ask(NS_FILE, &c.kb).await.unwrap();
    assert_eq!(
        served.status(),
        reqwest::StatusCode::OK,
        "the roster member is served"
    );
    let open = ask(NS_OPEN, &c.kc).await.unwrap();
    assert_eq!(
        open.status(),
        reqwest::StatusCode::OK,
        "{NS_OPEN} is answered by membership, which names C"
    );
}

/// **(d) A member added to the roster holds the whole journal one round
/// later, with no restart** — including what was written before it joined.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_added_to_the_roster_receives_the_whole_journal_next_round() {
    let c = cast();
    let d = dirs();
    let (a, _b_rail, c_rail) = three_nodes(&c, &d, &[("alex", &c.ka), ("bea", &c.kb)]).await;
    append(&a.rail, NS_FILE, 4).await;
    append(&a.rail, NS_FILE, 5).await;
    run_one_round(&a).await;
    assert!(
        ops(&c_rail, NS_FILE).is_empty(),
        "control: C starts off the roster"
    );
    a.rail
        .journal(NS_FILE)
        .unwrap()
        .set_roster(&file_roster(&[
            ("alex", &c.ka),
            ("bea", &c.kb),
            ("cass", &c.kc),
        ]))
        .unwrap();
    run_one_round(&a).await;
    assert_eq!(
        ops(&c_rail, NS_FILE),
        ops(&a.rail, NS_FILE),
        "the newly rostered member holds the journal entire"
    );
}
