use super::*;
use commonwealth_core::ids::{NodeId, NodePubkey};
use commonwealth_media::MemberIdentity;
use sovereign_contracts::setup_config::IrohSection;

/// What `offer` leaves in `[iroh]`, read back the way the daemon reads it.
fn offered(admit: &[&str]) -> IrohSection {
    let mut doc: toml_edit::DocumentMut = "[iroh]\n# kept\nenabled = true\n".parse().unwrap();
    let admit: Vec<String> = admit.iter().map(|s| s.to_string()).collect();
    set_offer(&mut doc, "127.0.0.1:8096".parse().unwrap(), &admit).unwrap();
    assert!(doc.to_string().contains("# kept"), "comments survive");
    toml::from_str(&doc["iroh"].to_string()).unwrap()
}

fn member(name: &str, id: u128) -> MemberIdentity {
    MemberIdentity {
        name: name.into(),
        node_id: NodeId::from_u128(id),
    }
}

fn reaches(iroh: &IrohSection, who: &MemberIdentity) -> bool {
    let origin = iroh.media_origin.as_deref().map(|o| o.parse().unwrap());
    commonwealth_media::admit_media(
        Some(who),
        NodePubkey([7u8; 32]),
        origin,
        &iroh.media_allow,
        &[],
    )
    .is_some()
}

/// The failing input is the verb ignoring `--admit`: an unnamed member
/// would then reach the origin.
#[test]
fn offer_admit_narrows_through_admit_media() {
    let mac = member("LittleMac", 0xb0b252e4 << 96);
    let quiet = member("Quiet", 0xc0de << 96);
    let narrowed = offered(&["LittleMac"]);
    assert_eq!(narrowed.media_origin.as_deref(), Some("127.0.0.1:8096"));
    assert!(reaches(&narrowed, &mac));
    assert!(!reaches(&narrowed, &quiet), "a member not named is refused");
    let everyone = offered(&[]);
    assert!(everyone.media_allow.is_empty());
    assert!(reaches(&everyone, &mac) && reaches(&everyone, &quiet));
}

/// `admit` restates no origin: it narrows the one `offer` stored. The
/// failing input is an admit that loses or rewrites the origin.
#[test]
fn admit_narrows_the_stored_origin() {
    let mut doc: toml_edit::DocumentMut = "[iroh]\nenabled = true\n".parse().unwrap();
    assert_eq!(
        stored_origin(&doc),
        Ok(None),
        "nothing offered, nothing to narrow"
    );
    let broken: toml_edit::DocumentMut = "[iroh]\nmedia_origin = \"jellyfin\"\n".parse().unwrap();
    assert!(stored_origin(&broken).is_err(), "unparseable is not absent");
    set_offer(&mut doc, "127.0.0.1:8920".parse().unwrap(), &[]).unwrap();
    let origin = stored_origin(&doc)
        .unwrap()
        .expect("offer stored its origin");
    set_offer(&mut doc, origin, &["LittleMac".into()]).unwrap();
    let iroh: IrohSection = toml::from_str(&doc["iroh"].to_string()).unwrap();
    assert_eq!(iroh.media_origin.as_deref(), Some("127.0.0.1:8920"));
    assert!(reaches(&iroh, &member("LittleMac", 0xb0b252e4 << 96)));
    assert!(!reaches(&iroh, &member("Quiet", 0xc0de << 96)));
}

/// `offer` with no origin finds a listener on Jellyfin's port. A server
/// already on 8096 (a real Jellyfin on this host) is the same listener.
#[test]
fn offer_with_no_origin_finds_a_listener_on_8096() {
    let _held = std::net::TcpListener::bind("127.0.0.1:8096");
    assert_eq!(
        probe_origin(&WELL_KNOWN_ORIGINS),
        Some("127.0.0.1:8096".parse().unwrap())
    );
    assert_eq!(probe_origin(&[]), None, "no candidates, nothing found");
}

#[test]
fn offered_to_line_names_everyone_or_the_admitted() {
    assert_eq!(offered_to_line(&[]), "offered to: everyone here");
    assert_eq!(
        offered_to_line(&["LittleMac".into(), "Quiet".into()]),
        "offered to: LittleMac, Quiet"
    );
}

/// fp-70: the poll is `cw-rails`', so `offer` keeps the house credential AND
/// the viewer id in rails' house store — resolved by each side from its OWN
/// default root under one HOME, as a real install does. The failing input is
/// the pre-fp-70 verb, which wrote under `svrnmesh_root`: rails never reads
/// there, so the poll saw no house credential and published no presence.
#[test]
fn offer_keeps_house_and_viewer_where_the_rails_poll_reads_them() {
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", home.path());
    std::env::remove_var("CW_RAILS_DIR");
    std::env::remove_var("SVRNMESH_DATA_DIR");
    let before = vec![("authorization".to_string(), "house-key".to_string())];
    let v = viewer::Viewer {
        id: "viewer-id-1".into(),
        credential: "viewer-key".into(),
    };
    keep_for_poll(&poll_house_dir(), &before, &v);

    // What rails' poll reads: its default data dir, then the house store.
    let rails_root = commonwealth_media::rails_data_dir();
    assert_eq!(rails_root, home.path().join(".commonwealth-rails"));
    assert_eq!(
        commonwealth_media::read_house_in(&commonwealth_media::house_dir_under(&rails_root)),
        (before.clone(), Some("viewer-id-1".to_string()))
    );
    // Nothing under the offer's own daemon root, the dir rails never reads.
    let svrnmesh = sovereign_contracts::rebrand::svrnmesh_root();
    assert!(!commonwealth_media::house_dir_under(&svrnmesh).exists());

    // withdraw: the viewer is gone, the house credential stays.
    let mut doc: toml_edit::DocumentMut = "[iroh]\n".parse().unwrap();
    set_offer(&mut doc, "127.0.0.1:8096".parse().unwrap(), &[]).unwrap();
    assert!(clear_offer(&mut doc, &poll_house_dir()));
    assert_eq!(
        commonwealth_media::read_house_in(&commonwealth_media::house_dir_under(&rails_root)),
        (before, None)
    );
}

/// `withdraw` removes every key `offer` wrote, and says whether there was an
/// offer to withdraw — a machine that was not offering is not reported as one
/// that just stopped.
#[test]
fn withdraw_clears_every_key_offer_wrote() {
    let mut doc: toml_edit::DocumentMut = "[iroh]\n# kept\nenabled = true\n".parse().unwrap();
    set_offer(
        &mut doc,
        "127.0.0.1:8096".parse().unwrap(),
        &["LittleMac".into()],
    )
    .unwrap();
    let house = tempfile::tempdir().unwrap();

    assert!(
        clear_offer(&mut doc, house.path()),
        "there was an offer to withdraw"
    );
    let iroh: IrohSection = toml::from_str(&doc["iroh"].to_string()).unwrap();
    assert_eq!(iroh.media_origin, None);
    assert!(iroh.media_allow.is_empty());
    assert_eq!(iroh.media_viewer_user, None);
    assert_eq!(
        iroh.enabled,
        Some(true),
        "withdraw touches only what offer wrote"
    );
    assert!(doc.to_string().contains("# kept"), "comments survive");

    assert!(
        !clear_offer(&mut doc, house.path()),
        "a second withdrawal reports that there was nothing to withdraw"
    );
}

/// Negative: `withdraw` on a config with no `[iroh]` table at all reports
/// "nothing to withdraw" rather than panicking or claiming a takedown.
#[test]
fn withdraw_with_no_iroh_table_reports_nothing_to_withdraw() {
    let mut doc: toml_edit::DocumentMut = "[daemon]\nclient_port = 9741\n".parse().unwrap();
    let house = tempfile::tempdir().unwrap();
    assert!(!clear_offer(&mut doc, house.path()));
}

/// The three presence renderings, including the one that is not silence: a
/// holder that reported nothing must not read as "free".
#[test]
fn the_in_use_line_distinguishes_in_use_free_and_unreported() {
    assert_eq!(
        in_use_line(Some(0.0)).as_deref(),
        Some("in use right now (the holder is watching it)")
    );
    assert_eq!(in_use_line(Some(1.0)), None, "a free library says nothing");
    assert_eq!(
        in_use_line(None).as_deref(),
        Some("in use: not reported by this holder"),
        "absence is printed, never read as free"
    );
}

/// The house credential is local-only. `offer` keeps it in a 0600 file under
/// `commonwealth_media::house_dir_under` and writes NOTHING about it into
/// `[iroh]` — the document the daemon reads and gossip's `NodeCapabilities`
/// is built from. The failing input is a verb that parks the install
/// credential in config "so the poll can find it": every peer would then be
/// one `svrn mesh status` away from the run of this origin.
#[test]
fn offer_writes_no_credential_into_the_config_the_mesh_reads() {
    let mut doc: toml_edit::DocumentMut = "[iroh]\n".parse().unwrap();
    set_offer(
        &mut doc,
        "127.0.0.1:8096".parse().unwrap(),
        &["LittleMac".into()],
    )
    .unwrap();
    let written = doc.to_string();
    for forbidden in ["authorization", "MediaBrowser", "Token=", "media-house"] {
        assert!(
            !written.contains(forbidden),
            "offer put {forbidden:?} in the config the mesh reads:\n{written}"
        );
    }
    let iroh: IrohSection = toml::from_str(&doc["iroh"].to_string()).unwrap();
    assert_eq!(iroh.media_viewer_user, None, "the viewer id lives in rails' house store");
}

/// The two stores are different directories, so a house credential cannot be
/// picked up by the reader whose output becomes headers on a member's dial.
#[test]
fn the_house_store_is_not_the_store_the_acceptor_forwards() {
    let root = std::path::Path::new("/nonexistent/svrnmesh");
    assert_ne!(
        commonwealth_media::dir_under(root),
        commonwealth_media::house_dir_under(root)
    );
}
