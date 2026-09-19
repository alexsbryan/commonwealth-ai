use super::*;
use commonwealth_core::ids::{NodeId, NodePubkey};
use commonwealth_media::MemberIdentity;
use sovereign_contracts::setup_config::IrohSection;

/// What `offer` leaves in `[iroh]`, read back the way the daemon reads it.
fn offered(admit: &[&str]) -> IrohSection {
    let mut doc: toml_edit::DocumentMut = "[iroh]\n# kept\nenabled = true\n".parse().unwrap();
    let admit: Vec<String> = admit.iter().map(|s| s.to_string()).collect();
    set_offer(&mut doc, "127.0.0.1:8096".parse().unwrap(), &admit, None).unwrap();
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
    set_offer(&mut doc, "127.0.0.1:8920".parse().unwrap(), &[], None).unwrap();
    let origin = stored_origin(&doc)
        .unwrap()
        .expect("offer stored its origin");
    set_offer(&mut doc, origin, &["LittleMac".into()], None).unwrap();
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

/// `offer` records the viewer account it minted, and `admit` — which shares
/// the writer — leaves it alone rather than un-recording it.
#[test]
fn offer_records_the_viewer_account_and_admit_keeps_it() {
    let mut doc: toml_edit::DocumentMut = "[iroh]\n".parse().unwrap();
    let origin = "127.0.0.1:8096".parse().unwrap();
    set_offer(&mut doc, origin, &[], Some("viewer-id-1")).unwrap();
    let iroh: IrohSection = toml::from_str(&doc["iroh"].to_string()).unwrap();
    assert_eq!(iroh.media_viewer_user.as_deref(), Some("viewer-id-1"));

    set_offer(&mut doc, origin, &["LittleMac".into()], None).unwrap();
    let iroh: IrohSection = toml::from_str(&doc["iroh"].to_string()).unwrap();
    assert_eq!(
        iroh.media_viewer_user.as_deref(),
        Some("viewer-id-1"),
        "narrowing the admit list must not un-record the viewer account"
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
        Some("viewer-id-1"),
    )
    .unwrap();

    assert!(clear_offer(&mut doc), "there was an offer to withdraw");
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
        !clear_offer(&mut doc),
        "a second withdrawal reports that there was nothing to withdraw"
    );
}

/// Negative: `withdraw` on a config with no `[iroh]` table at all reports
/// "nothing to withdraw" rather than panicking or claiming a takedown.
#[test]
fn withdraw_with_no_iroh_table_reports_nothing_to_withdraw() {
    let mut doc: toml_edit::DocumentMut = "[daemon]\nclient_port = 9741\n".parse().unwrap();
    assert!(!clear_offer(&mut doc));
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
