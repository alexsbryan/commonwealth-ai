// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use commonwealth_core::ids::NodeId;
use commonwealth_transport::iroh::{CLIENT_ALPN, GUEST_ALPN, MEDIA_ALPN, OFFER_ALPN, RPC_ALPN};

const DIALER: NodePubkey = NodePubkey([7u8; 32]);

fn member() -> MemberIdentity {
    MemberIdentity {
        name: "LittleMac".into(),
        node_id: NodeId::from_u128(0xB0B),
    }
}

fn reg(alpn: &[u8], prefixes: &[&str], port: u16, admit: Admit) -> OriginRegistration {
    OriginRegistration {
        alpn: String::from_utf8_lossy(alpn).into_owned(),
        prefixes: prefixes.iter().map(|p| p.to_string()).collect(),
        port,
        admit,
        framing: Framing::Http,
        ttl_secs: None,
        claims: None,
        namespaces: Vec::new(),
    }
}

fn port_of(f: &Forward) -> Option<u16> {
    match f {
        Forward::Splice(a) => Some(a.port()),
        Forward::Http { origin, .. } => Some(origin.port()),
        _ => None,
    }
}

fn tie_of(f: &Forward) -> Option<&str> {
    match f {
        Forward::Http { headers, .. } => headers
            .iter()
            .find(|(n, _)| n == ORIGIN_TIE_HEADER)
            .map(|(_, v)| v.as_str()),
        _ => None,
    }
}

/// **The table is the registry.** Every ALPN the acceptor serves, every
/// forward it makes and every origin kind gossip advertises is read from what
/// was registered — and an ALPN nobody registered is closed. A hard-coded
/// origin or a per-ALPN arm makes one of these answers disagree with the
/// registry, and this goes red.
#[test]
fn the_acceptor_table_and_the_advertised_origins_are_derived_from_the_registry() {
    let r = OriginRegistry::new(PublishedApps::default());
    assert!(r.alpns().is_empty(), "nothing registered, nothing served");
    assert!(r.advertised_kinds().is_empty());
    for alpn in [
        ALPN,
        CLIENT_ALPN,
        RPC_ALPN,
        MEDIA_ALPN,
        OFFER_ALPN,
        b"cwth/nope/0".as_slice(),
    ] {
        assert!(r.forward_for(alpn, Some(&member()), DIALER).is_none());
    }

    let client = r
        .register(reg(
            CLIENT_ALPN,
            &[],
            9748,
            Admit::MembersElse("cwth/guest/0".into()),
        ))
        .unwrap();
    r.register(reg(GUEST_ALPN, &[], 9744, Admit::Any)).unwrap();
    let mut rpc = reg(RPC_ALPN, &[], 50052, Admit::Members(Vec::new()));
    rpc.framing = Framing::Bytes;
    r.register(rpc).unwrap();
    r.register(reg(
        OFFER_ALPN,
        &[],
        8000,
        Admit::Members(vec!["Someone".into()]),
    ))
    .unwrap();
    r.stand(
        MEDIA_ALPN,
        &[],
        ([127, 0, 0, 1], 8096).into(),
        Admit::Members(Vec::new()),
        vec![("X-Emby-Token".into(), "secret".into())],
    )
    .unwrap();

    let mut want: Vec<Vec<u8>> = [CLIENT_ALPN, GUEST_ALPN, MEDIA_ALPN, OFFER_ALPN, RPC_ALPN]
        .iter()
        .map(|a| a.to_vec())
        .collect();
    want.sort();
    assert_eq!(r.alpns(), want);
    assert_eq!(
        r.advertised_kinds(),
        vec![OriginKind::Media, OriginKind::Offer]
    );

    // A member reaches the client origin, tied; a stranger falls to the guest door.
    let f = r.forward_for(CLIENT_ALPN, Some(&member()), DIALER).unwrap();
    assert_eq!(port_of(&f), Some(9748));
    assert_eq!(tie_of(&f), Some(client.tie.as_str()));
    let f = r.forward_for(CLIENT_ALPN, None, DIALER).unwrap();
    assert_eq!(port_of(&f), Some(9744));
    assert_ne!(
        tie_of(&f),
        Some(client.tie.as_str()),
        "the guest door gets its own tie"
    );
    // rpc: members only, spliced (no HTTP to rewrite).
    assert_eq!(
        r.forward_for(RPC_ALPN, Some(&member()), DIALER),
        Some(Forward::Splice(([127, 0, 0, 1], 50052).into()))
    );
    assert!(r.forward_for(RPC_ALPN, None, DIALER).is_none());
    // offer: a member outside the allow list is closed.
    assert!(r.forward_for(OFFER_ALPN, Some(&member()), DIALER).is_none());
    // media: the standing entry's credential rides, and no tie.
    let f = r.forward_for(MEDIA_ALPN, Some(&member()), DIALER).unwrap();
    assert_eq!(port_of(&f), Some(8096));
    assert_eq!(tie_of(&f), None);
    assert!(
        matches!(&f, Forward::Http { headers, .. } if headers.iter().any(|(n, _)| n == "X-Emby-Token"))
    );
    assert!(r
        .forward_for(b"cwth/nope/0", Some(&member()), DIALER)
        .is_none());

    // The app entry is the app registry: publishing an app serves its ALPN
    // and advertises it.
    r.apps()
        .claim(
            "chores",
            ([127, 0, 0, 1], 5000).into(),
            Duration::from_secs(60),
        )
        .unwrap();
    assert!(r.alpns().contains(&APP_ALPN.to_vec()));
    assert!(r.advertised_kinds().contains(&OriginKind::App));
    assert!(matches!(
        r.forward_for(APP_ALPN, Some(&member()), DIALER),
        Some(Forward::HttpByName { .. })
    ));
}

/// `cwth/http/0` is a prefix table: each prefix to its own origin with its
/// own tie, a members-only prefix refused to a stranger by name, and the
/// connection itself open to any dialer (a joiner must reach `/internal/join`).
#[test]
fn cwth_http_forwards_by_registered_prefix() {
    let r = OriginRegistry::new(PublishedApps::default());
    r.stand(
        ALPN,
        &["/internal/gossip", "/internal/join"],
        ([127, 0, 0, 1], 1).into(),
        Admit::Any,
        Vec::new(),
    )
    .unwrap();
    let ring = r
        .register(reg(
            ALPN,
            &["/internal/ring"],
            2,
            Admit::Members(Vec::new()),
        ))
        .unwrap();
    assert_eq!(ring.slots, vec!["cwth/http/0/internal/ring".to_string()]);
    let Some(Forward::HttpByPrefix { routes, headers }) = r.forward_for(ALPN, None, DIALER) else {
        panic!("cwth/http/0 forwards by prefix");
    };
    assert!(routes["/internal/join"].admitted, "a joiner reaches join");
    assert!(
        !routes["/internal/ring"].admitted,
        "a stranger does not reach the ring"
    );
    assert!(
        routes["/internal/gossip"].headers.is_empty(),
        "the endpoint's own routes carry no tie"
    );
    assert_eq!(
        routes["/internal/ring"].headers,
        vec![(ORIGIN_TIE_HEADER.to_string(), ring.tie.clone())]
    );
    assert!(headers.iter().any(|(n, _)| n == "X-Mesh-Pubkey"));
    let Some(Forward::HttpByPrefix { routes, .. }) = r.forward_for(ALPN, Some(&member()), DIALER)
    else {
        panic!("cwth/http/0 forwards by prefix");
    };
    assert!(routes["/internal/ring"].admitted);
}

/// **Refused by name, never last-writer-wins.** A second claim on a taken
/// slot — including one of the endpoint's own — names who holds it.
#[test]
fn a_second_registration_of_a_slot_is_refused_by_name() {
    let r = OriginRegistry::new(PublishedApps::default());
    r.stand(
        ALPN,
        &["/internal/join"],
        ([127, 0, 0, 1], 1).into(),
        Admit::Any,
        Vec::new(),
    )
    .unwrap();
    let first = r
        .register(reg(CLIENT_ALPN, &[], 9748, Admit::Members(Vec::new())))
        .unwrap();
    match r.register(reg(CLIENT_ALPN, &[], 9999, Admit::Any)) {
        Err(OriginRefusal::Taken { slot, by }) => {
            assert_eq!(slot, "cwth/client/0");
            assert!(by.contains(&first.claim_id), "{by}");
        }
        other => panic!("a taken ALPN must be refused: {other:?}"),
    }
    assert!(matches!(
        r.register(reg(ALPN, &["/internal/join"], 2, Admit::Any)),
        Err(OriginRefusal::Taken { .. })
    ));
    let f = r.forward_for(CLIENT_ALPN, Some(&member()), DIALER).unwrap();
    assert_eq!(
        port_of(&f),
        Some(9748),
        "the first registration still answers"
    );
}

#[test]
fn malformed_registrations_are_refused() {
    let r = OriginRegistry::new(PublishedApps::default());
    let refused = |req| r.register(req).unwrap_err();
    assert!(matches!(
        refused(reg(APP_ALPN, &[], 1, Admit::Any)),
        OriginRefusal::BadAlpn(_)
    ));
    assert!(matches!(
        refused(reg(b"", &[], 1, Admit::Any)),
        OriginRefusal::BadAlpn(_)
    ));
    assert!(matches!(
        refused(reg(ALPN, &[], 1, Admit::Any)),
        OriginRefusal::PrefixShape
    ));
    assert!(matches!(
        refused(reg(CLIENT_ALPN, &["/x"], 1, Admit::Any)),
        OriginRefusal::PrefixShape
    ));
    assert!(matches!(
        refused(reg(
            ALPN,
            &["/x"],
            1,
            Admit::MembersElse("cwth/guest/0".into())
        )),
        OriginRefusal::PrefixShape
    ));
    assert!(matches!(
        refused(reg(ALPN, &["/a/../b"], 1, Admit::Any)),
        OriginRefusal::BadPrefix(_)
    ));
    assert!(matches!(
        refused(reg(ALPN, &["internal"], 1, Admit::Any)),
        OriginRefusal::BadPrefix(_)
    ));
    assert!(r.alpns().is_empty());
}

/// Release and TTL take an origin out of the table, the ALPN set and the
/// hook's view, so a program that exits stops being reachable.
#[test]
fn release_and_expiry_withdraw_the_origin() {
    let r = OriginRegistry::new(PublishedApps::default());
    let seen: Arc<Mutex<Vec<Vec<Vec<u8>>>>> = Arc::default();
    let log = seen.clone();
    r.on_alpns_change(Arc::new(move |set| log.lock().unwrap().push(set)));
    let c = r
        .register(reg(CLIENT_ALPN, &[], 9748, Admit::Members(Vec::new())))
        .unwrap();
    assert_eq!(r.renew(&c.claim_id, Duration::from_secs(60), None), Ok(60));
    assert_eq!(
        r.release(&c.claim_id),
        Ok(vec!["cwth/client/0".to_string()])
    );
    assert!(r
        .forward_for(CLIENT_ALPN, Some(&member()), DIALER)
        .is_none());
    assert!(matches!(
        r.release(&c.claim_id),
        Err(OriginRefusal::NoSuchClaim(_))
    ));

    let mut short = reg(RPC_ALPN, &[], 50052, Admit::Members(Vec::new()));
    short.ttl_secs = Some(0);
    r.register(short).unwrap();
    std::thread::sleep(Duration::from_millis(5));
    assert!(r.alpns().is_empty(), "an expired claim is not served");
    let seen = seen.lock().unwrap();
    assert_eq!(
        seen.first(),
        Some(&Vec::new()),
        "installing the hook tells it the current set"
    );
    assert!(seen.contains(&vec![CLIENT_ALPN.to_vec()]));
    assert_eq!(seen.last(), Some(&Vec::new()));
}

/// Declared claims and namespaces are the registrations', and only theirs.
#[test]
fn declarations_are_read_from_live_registrations() {
    let r = OriginRegistry::new(PublishedApps::default());
    let mut serve = reg(CLIENT_ALPN, &[], 9748, Admit::Members(Vec::new()));
    let mut caps: NodeCapabilities = serde_json::from_value(serde_json::json!({
        "hardware": {"gpus": [], "system_ram_gb": 128, "cpu_cores": 16,
                     "total_storage_gb": 0, "free_storage_gb": 0},
        "available": {"free_vram_gb": 0.0, "free_ram_gb": 0.0, "free_storage_gb": 0.0,
                      "gpu_utilization": 0.0, "cpu_utilization": 0.0,
                      "available_for_mesh": true},
        "hosted_corpora": [], "reported_at": 0
    }))
    .unwrap();
    caps.inference_capable = true;
    serve.claims = Some(caps.clone());
    serve.namespaces = vec!["mesh-measurements".into()];
    let c = r.register(serve).unwrap();
    let declared = r.declared_claims();
    assert_eq!(declared.len(), 1);
    assert_eq!(
        serde_json::to_value(&declared[0]).unwrap(),
        serde_json::to_value(&caps).unwrap()
    );
    assert_eq!(r.namespaces(), vec!["mesh-measurements".to_string()]);
    r.release(&c.claim_id).unwrap();
    assert!(r.declared_claims().is_empty());
}

/// A renew's declaration replaces the claim's, on the one slot that holds
/// it, and a renew without one keeps it (pb-mesh-exit-transport-claims).
/// Failing input: a renew that moves only the deadline.
#[test]
fn a_renew_replaces_the_declaration_and_none_keeps_it() {
    let r = OriginRegistry::new(PublishedApps::default());
    let caps = |model: &str| -> NodeCapabilities {
        serde_json::from_value(serde_json::json!({
            "hardware": {"gpus": [], "system_ram_gb": 0, "cpu_cores": 0,
                         "total_storage_gb": 0, "free_storage_gb": 0},
            "available": {"free_vram_gb": 0.0, "free_ram_gb": 0.0, "free_storage_gb": 0.0,
                          "gpu_utilization": 0.0, "cpu_utilization": 0.0,
                          "available_for_mesh": true},
            "hosted_corpora": [], "reported_at": 0, "loaded_models": [model]
        }))
        .unwrap()
    };
    let models = |r: &OriginRegistry| -> Vec<Vec<String>> {
        r.declared_claims()
            .into_iter()
            .map(|c| c.loaded_models)
            .collect()
    };
    let mut two = reg(ALPN, &["/b/x", "/a/x"], 9, Admit::Members(Vec::new()));
    two.claims = Some(caps("a"));
    let c = r.register(two).unwrap();
    assert_eq!(models(&r), vec![vec!["a".to_string()]]);
    r.renew(&c.claim_id, Duration::from_secs(60), Some(caps("b")))
        .unwrap();
    assert_eq!(
        models(&r),
        vec![vec!["b".to_string()]],
        "replaced, still one"
    );
    r.renew(&c.claim_id, Duration::from_secs(60), None).unwrap();
    assert_eq!(models(&r), vec![vec!["b".to_string()]], "none keeps it");

    let bare = r
        .register(reg(CLIENT_ALPN, &[], 9748, Admit::Members(Vec::new())))
        .unwrap();
    r.renew(&bare.claim_id, Duration::from_secs(60), Some(caps("c")))
        .unwrap();
    // Slot order: `cwth/client/0` sorts before `cwth/http/0/a/x`.
    assert_eq!(
        models(&r),
        vec![vec!["c".to_string()], vec!["b".to_string()]],
        "a claim that declared nothing declares at renew"
    );
}

/// **A local origin is listed and never reachable from the mesh**
/// (pb-work-donor). The execute origin a donor finds through the listing
/// is on this node's loopback for this node's processes; a member or a
/// stranger dialing its ALPN is closed, and the ALPN is not advertised. The
/// failing input is a `Local` read as `Members([])`: a member could then
/// run units through another node's origin.
#[test]
fn a_local_origin_is_listed_but_never_advertised_or_forwarded() {
    let r = OriginRegistry::new(PublishedApps::default());
    let c = r
        .register(reg(b"cwth/work/ingest:v1", &[], 9750, Admit::Local))
        .expect("a local registration");
    let listed = r.listing();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].slot, "cwth/work/ingest:v1");
    assert_eq!(listed[0].admit, Admit::Local);
    assert_eq!(listed[0].claim_id.as_deref(), Some(c.claim_id.as_str()));
    assert!(
        r.alpns().is_empty(),
        "a local origin is not served to dialers"
    );
    assert!(r.advertised_kinds().is_empty());
    assert!(r
        .forward_for(b"cwth/work/ingest:v1", Some(&member()), DIALER)
        .is_none());
    assert!(r
        .forward_for(b"cwth/work/ingest:v1", None, DIALER)
        .is_none());

    // The control: the same registration as a members' origin IS forwarded,
    // so the refusal above is the admission's, not the ALPN's.
    r.release(&c.claim_id).unwrap();
    r.register(reg(
        b"cwth/work/ingest:v1",
        &[],
        9750,
        Admit::Members(Vec::new()),
    ))
    .unwrap();
    assert_eq!(r.alpns(), vec![b"cwth/work/ingest:v1".to_vec()]);
    assert!(r
        .forward_for(b"cwth/work/ingest:v1", Some(&member()), DIALER)
        .is_some());
}

/// The PROOF line "an app outside `app_allow` is refused" (phase-b-81 (3)):
/// the publisher's list rides its claim, so a member it does not name is
/// closed on `cwth/app/0` and one it names is forwarded. Failing input: the
/// claim's list ignored at the acceptor (`snapshot()` in `forward_for`).
#[test]
fn an_app_outside_its_publishers_allow_list_is_refused() {
    let r = OriginRegistry::new(PublishedApps::default());
    r.apps()
        .claim_allowing(
            "films",
            ([127, 0, 0, 1], 5000).into(),
            Duration::from_secs(60),
            vec!["Mira".into()],
        )
        .unwrap();
    assert!(
        r.forward_for(APP_ALPN, Some(&member()), DIALER).is_none(),
        "LittleMac is outside films' allow list"
    );
    let mira = MemberIdentity {
        name: "Mira".into(),
        node_id: NodeId::from_u128(0x111),
    };
    let Some(Forward::HttpByName { apps, .. }) = r.forward_for(APP_ALPN, Some(&mira), DIALER)
    else {
        panic!("Mira is named by films' allow list");
    };
    assert!(apps.contains_key("films"));
}

/// A non-member dialing `cwth/client/0` goes to the registered fallback
/// origin, and is closed while none is registered, as the daemon's acceptor
/// closed when guest did not bind (pb-mesh-exit-transport, phase-b-76 fork 2).
/// A member reaches the client origin either way.
#[test]
fn a_non_member_is_closed_while_the_fallback_origin_is_unregistered() {
    let r = OriginRegistry::new(PublishedApps::default());
    r.register(reg(
        CLIENT_ALPN,
        &[],
        9748,
        Admit::MembersElse("cwth/guest/0".into()),
    ))
    .unwrap();
    assert!(r.forward_for(CLIENT_ALPN, None, DIALER).is_none());
    assert!(r
        .forward_for(CLIENT_ALPN, Some(&member()), DIALER)
        .is_some());

    let guest = r.register(reg(GUEST_ALPN, &[], 9744, Admit::Any)).unwrap();
    let f = r.forward_for(CLIENT_ALPN, None, DIALER).unwrap();
    assert_eq!(port_of(&f), Some(9744));

    r.release(&guest.claim_id).unwrap();
    assert!(
        r.forward_for(CLIENT_ALPN, None, DIALER).is_none(),
        "a released fallback closes the non-member again"
    );
}
