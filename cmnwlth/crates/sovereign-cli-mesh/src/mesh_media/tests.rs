use super::*;
use commonwealth_core::ids::{NodeId, NodePubkey};
use commonwealth_media::MemberIdentity;
use commonwealth_rails::config::{Config as RailsConfig, MediaSection};

/// `[media]` as cw-rails reads it from `doc`.
fn as_rails_reads(doc: &toml_edit::DocumentMut) -> RailsConfig {
    toml::from_str(&doc.to_string()).unwrap()
}

/// What `offer` leaves in rails.toml, read back the way cw-rails reads it.
fn offered(admit: &[&str]) -> MediaSection {
    let mut doc: toml_edit::DocumentMut = "# kept\nname = \"holder\"\n".parse().unwrap();
    let admit: Vec<String> = admit.iter().map(|s| s.to_string()).collect();
    set_offer(&mut doc, "127.0.0.1:8096".parse().unwrap(), &admit).unwrap();
    assert!(doc.to_string().contains("# kept"), "comments survive");
    as_rails_reads(&doc).media
}

fn member(name: &str, id: u128) -> MemberIdentity {
    MemberIdentity {
        name: name.into(),
        node_id: NodeId::from_u128(id),
    }
}

fn reaches(media: &MediaSection, who: &MemberIdentity) -> bool {
    commonwealth_media::admit_media(
        Some(who),
        NodePubkey([7u8; 32]),
        media.origin,
        &media.allow,
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
    assert_eq!(narrowed.origin, Some("127.0.0.1:8096".parse().unwrap()));
    assert!(reaches(&narrowed, &mac));
    assert!(!reaches(&narrowed, &quiet), "a member not named is refused");
    let everyone = offered(&[]);
    assert!(everyone.allow.is_empty());
    assert!(reaches(&everyone, &mac) && reaches(&everyone, &quiet));
}

/// `admit` restates no origin: it narrows the one `offer` stored. The
/// failing input is an admit that loses or rewrites the origin.
#[test]
fn admit_narrows_the_stored_origin() {
    let mut doc: toml_edit::DocumentMut = "name = \"holder\"\n".parse().unwrap();
    assert_eq!(
        stored_origin(&doc),
        Ok(None),
        "nothing offered, nothing to narrow"
    );
    let broken: toml_edit::DocumentMut = "[media]\norigin = \"jellyfin\"\n".parse().unwrap();
    assert!(stored_origin(&broken).is_err(), "unparseable is not absent");
    set_offer(&mut doc, "127.0.0.1:8920".parse().unwrap(), &[]).unwrap();
    let origin = stored_origin(&doc)
        .unwrap()
        .expect("offer stored its origin");
    set_offer(&mut doc, origin, &["LittleMac".into()]).unwrap();
    let media = as_rails_reads(&doc).media;
    assert_eq!(media.origin, Some("127.0.0.1:8920".parse().unwrap()));
    assert!(reaches(&media, &member("LittleMac", 0xb0b252e4 << 96)));
    assert!(!reaches(&media, &member("Quiet", 0xc0de << 96)));
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
    let mut doc = toml_edit::DocumentMut::new();
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
    let mut doc: toml_edit::DocumentMut = "# kept\nname = \"holder\"\n".parse().unwrap();
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
    let read = as_rails_reads(&doc);
    assert_eq!(read.media.origin, None);
    assert!(read.media.allow.is_empty());
    assert_eq!(
        read.name, "holder",
        "withdraw touches only what offer wrote"
    );
    assert!(doc.to_string().contains("# kept"), "comments survive");

    assert!(
        !clear_offer(&mut doc, house.path()),
        "a second withdrawal reports that there was nothing to withdraw"
    );
}

/// Negative: `withdraw` on a rails.toml with no `[media]` table at all reports
/// "nothing to withdraw" rather than panicking or claiming a takedown.
#[test]
fn withdraw_with_no_media_table_reports_nothing_to_withdraw() {
    let mut doc: toml_edit::DocumentMut = "name = \"holder\"\n".parse().unwrap();
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
/// rails.toml — the document cw-rails serves the origin and gossips from.
/// The failing input is a verb that parks the install credential in config
/// "so the poll can find it": every peer would then be one `svrn mesh
/// status` away from the run of this origin.
#[test]
fn offer_writes_no_credential_into_the_config_the_mesh_reads() {
    let mut doc = toml_edit::DocumentMut::new();
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
    // `[media]` is `deny_unknown_fields`: a viewer id parked there would not load.
    as_rails_reads(&doc);
}

/// The verbs write the file cw-rails loads and the dir it reads the declared
/// credential from, each side resolving its own default under one HOME. The
/// failing input is the verb before 2026-10-04, which wrote svrn's
/// config.toml and svrn's root: cw-rails started with `media_origin=None`
/// and members were offered nothing (ring-room film leg).
#[test]
fn offer_writes_where_cw_rails_reads() {
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", home.path());
    std::env::remove_var("CW_RAILS_DIR");
    let (path, mut doc) = rails_media::load().unwrap();
    set_offer(&mut doc, "127.0.0.1:8096".parse().unwrap(), &["Cy".into()]).unwrap();
    rails_media::write(&path, &doc).unwrap();

    let rails_dir = RailsConfig::resolve_data_dir(None);
    let read = RailsConfig::load(&rails_dir, None).unwrap();
    assert_eq!(read.media.origin, Some("127.0.0.1:8096".parse().unwrap()));
    assert_eq!(read.media.allow, vec!["Cy".to_string()]);
    assert_eq!(
        rails_media::declared_dir(),
        commonwealth_media::dir_under(&rails_dir)
    );
}

/// An edit cw-rails' reader refuses is put back, so a verb never leaves a
/// rails.toml that stops cw-rails from starting.
#[test]
fn an_edit_cw_rails_would_refuse_leaves_rails_toml_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rails.toml");
    std::fs::write(&path, "name = \"holder\"\n").unwrap();
    let bad: toml_edit::DocumentMut = "[media]\norigin = \"jellyfin\"\n".parse().unwrap();
    assert!(rails_media::write(&path, &bad).is_err());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "name = \"holder\"\n"
    );
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
