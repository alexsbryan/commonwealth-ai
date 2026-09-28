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
    assert_eq!(r.renew(&c.claim_id, Duration::from_secs(60)), Ok(60));
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
