// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

// --- witness: a record that can explain itself -----------------------
//
// These guard the property the store lacked until 2026-07-30: a digest
// change was unattributable, so two runs under different keys could not be
// told apart by anything a reader could act on.

/// [`key`] with the host fingerprint that [`shards`] actually contains, so
/// the fixture matches what the live callers build — the host is always one
/// of the machines in its own placement.
fn witnessed_key() -> MeasurementKey {
    MeasurementKey::for_plan(
        HostIdentity::from_live_mesh(Some(0xF0F)).unwrap(),
        model_fingerprint(&sizes(), 48),
        placement_digest("distributed", 48, &shards()),
        32_768,
        LinkClass::Direct,
    )
}

/// The same fleet, weight moved off the host: 24/24 instead of 12/36.
fn moved_witness() -> PlacementWitness {
    let mut shards = shards();
    shards[0].blocks = Some((0, 23));
    shards[1].blocks = Some((24, 47));
    PlacementWitness {
        shards,
        ..witness()
    }
}

fn moved_key() -> MeasurementKey {
    MeasurementKey::for_plan(
        HostIdentity::from_live_mesh(Some(0xF0F)).unwrap(),
        model_fingerprint(&sizes(), 48),
        moved_witness().digest(),
        32_768,
        LinkClass::Direct,
    )
}

/// covers: FE-90
#[test]
fn a_witness_accounts_for_the_digest_it_was_built_from() {
    assert!(witness().explains(&witnessed_key().placement_digest));
    assert!(!moved_witness().explains(&witnessed_key().placement_digest));
}

#[test]
fn a_description_is_not_part_of_the_identity() {
    // Improving what a peer advertises about itself must not orphan every
    // record naming it, so `machines` is witnessed but never hashed.
    let mut relabelled = witness();
    relabelled.machines[0].vram_gb = 96;
    relabelled.machines[0].backend = Some("rocm".into());
    assert_eq!(relabelled.digest(), witness().digest());
}

#[test]
fn describe_split_counts_blocks_and_marks_the_output_head() {
    assert_eq!(
        witness().describe_split(),
        "beefymac 12 · ruggedfox 36 +head"
    );
}

#[test]
fn a_near_miss_describes_both_splits_when_both_sides_kept_a_witness() {
    // The headline: the reader is planning 24/24 and the store holds 12/36.
    let mut f = MeasurementFile::new();
    let mut rec = rec_at(witnessed_key(), 500, 10.4, Verdict::Valid);
    rec.witness = Some(witness());
    record(&mut f, rec);

    let near = near_misses(&f, &[], &moved_key(), Some(&moved_witness()));
    assert_eq!(near.len(), 1);
    let split = near[0]
        .detail
        .iter()
        .find(|d| d.facet == "split")
        .expect("the split differs");
    assert_eq!(
        split.theirs.as_deref(),
        Some("beefymac 12 · ruggedfox 36 +head")
    );
    assert_eq!(
        split.ours.as_deref(),
        Some("beefymac 24 · ruggedfox 24 +head")
    );
}

#[test]
fn a_near_miss_names_the_machine_when_the_host_hardware_differs() {
    // A measurement that arrived from somewhere else: same model, same
    // split shape, different host silicon. Naming the two machines is the
    // whole value — `differs_by: ["host-hardware"]` is unactionable.
    let mut f = MeasurementFile::new();
    let mut rec = rec_at(witnessed_key(), 500, 10.4, Verdict::Valid);
    rec.witness = Some(witness());
    record(&mut f, rec);

    // Ours: the same placement measured with beefymac as the host.
    let mine = MeasurementKey::for_plan(
        HostIdentity::from_live_mesh(Some(0xBEEF)).unwrap(),
        model_fingerprint(&sizes(), 48),
        placement_digest("distributed", 48, &shards()),
        32_768,
        LinkClass::Direct,
    );
    let near = near_misses(&f, &[], &mine, Some(&witness()));
    let hw = near[0]
        .detail
        .iter()
        .find(|d| d.facet == "host-hardware")
        .expect("the host hardware differs");
    assert_eq!(hw.theirs.as_deref(), Some("128 GB vulkan"));
    assert_eq!(hw.ours.as_deref(), Some("51 GB metal"));
}

#[test]
fn an_unfaithful_witness_is_treated_as_absent_rather_than_quoted() {
    // A witness that does not account for its own key describes some other
    // configuration. Quoting it would be worse than saying nothing.
    let mut f = MeasurementFile::new();
    let mut rec = rec_at(witnessed_key(), 500, 10.4, Verdict::Valid);
    rec.witness = Some(moved_witness()); // explains a digest this key doesn't have
    record(&mut f, rec);

    let near = near_misses(&f, &[], &moved_key(), Some(&moved_witness()));
    let split = near[0]
        .detail
        .iter()
        .find(|d| d.facet == "split")
        .expect("the split differs");
    assert_eq!(split.theirs, None, "an unfaithful witness must not be read");
    assert!(split.ours.is_some(), "ours is faithful and still described");
}

#[test]
fn a_witnessless_record_still_names_the_facet_it_cannot_describe() {
    // Every record written before 2026-07-30 is this case, and they are kept
    // rather than discarded — so the surface has to degrade honestly.
    let mut f = MeasurementFile::new();
    record(&mut f, rec_at(witnessed_key(), 500, 10.4, Verdict::Valid));

    let near = near_misses(&f, &[], &moved_key(), Some(&moved_witness()));
    assert_eq!(near[0].differs_by, vec!["split"]);
    assert_eq!(near[0].detail[0].theirs, None);
    assert_eq!(
        near[0].detail[0].ours.as_deref(),
        Some("beefymac 24 · ruggedfox 24 +head")
    );
}

#[test]
fn the_settings_are_described_without_any_witness_at_all() {
    // n_ctx, link and probe_version live in the key itself, so even the
    // oldest record gains "32768 vs 8192" over a bare "context".
    let mut f = MeasurementFile::new();
    record(&mut f, rec_at(witnessed_key(), 500, 10.4, Verdict::Valid));

    let asked = MeasurementKey::for_plan(
        HostIdentity::from_live_mesh(Some(0xF0F)).unwrap(),
        model_fingerprint(&sizes(), 48),
        placement_digest("distributed", 48, &shards()),
        8_192,
        LinkClass::Tunnel,
    );
    let near = near_misses(&f, &[], &asked, None);
    let d: Vec<(&str, Option<&str>, Option<&str>)> = near[0]
        .detail
        .iter()
        .map(|d| (d.facet, d.theirs.as_deref(), d.ours.as_deref()))
        .collect();
    assert_eq!(
        d,
        vec![
            ("context", Some("32768"), Some("8192")),
            ("link", Some("direct"), Some("tunnel")),
        ]
    );
}

#[test]
fn differs_by_is_exactly_the_facets_of_detail_in_order() {
    let mut f = MeasurementFile::new();
    let mut rec = rec_at(witnessed_key(), 500, 10.4, Verdict::Valid);
    rec.witness = Some(witness());
    record(&mut f, rec);

    let asked = MeasurementKey::for_plan(
        HostIdentity::from_live_mesh(Some(0xBEEF)).unwrap(),
        model_fingerprint(&sizes(), 48),
        moved_witness().digest(),
        8_192,
        LinkClass::Tunnel,
    );
    let near = near_misses(&f, &[], &asked, Some(&moved_witness()));
    let facets: Vec<&str> = near[0].detail.iter().map(|d| d.facet).collect();
    assert_eq!(near[0].differs_by, facets);
    assert_eq!(
        facets,
        vec!["split", "host-hardware", "context", "link"],
        "facet order is by how much it should move a reader, not alphabetical"
    );
}

#[test]
fn a_store_written_before_witnesses_still_loads() {
    // The v1 -> v2 change discarded old rows because the missing field was a
    // KEY field. A witness is explanatory, so these rows are kept and simply
    // cannot describe themselves.
    let mut f = MeasurementFile::new();
    record(&mut f, rec_at(witnessed_key(), 500, 10.4, Verdict::Valid));
    let json = serde_json::to_string(&f).unwrap();
    assert!(
        !json.contains("\"witness\""),
        "a None witness must not be written as a field that looks recorded"
    );

    let back = parse(&json);
    assert_eq!(back.records().len(), 1, "the row survives");
    assert!(back.records()[0].witness.is_none());
}

#[test]
fn a_witness_round_trips_through_the_store() {
    let mut f = MeasurementFile::new();
    let mut rec = rec_at(witnessed_key(), 500, 10.4, Verdict::Valid);
    rec.witness = Some(witness());
    record(&mut f, rec);

    let back = parse(&serde_json::to_string(&f).unwrap());
    let w = back.records()[0].witness.as_ref().expect("witness kept");
    assert!(
        w.explains(&back.records()[0].key.placement_digest),
        "a witness must still account for its key after a round trip"
    );
    assert_eq!(w.describe_split(), witness().describe_split());
}

// -- Travel -------------------------------------------------------------

/// A record as a peer would publish it, with the origin the daemon stamps.
fn from_peer(rec: MeasurementRecord, name: Option<&str>) -> ForeignRecord {
    ForeignRecord {
        origin_node: "b88252e4325bc3771122334455667788".into(),
        origin_name: name.map(str::to_string),
        record: rec,
    }
}

#[test]
fn a_record_survives_the_wire_unchanged() {
    let mut rec = rec_at(witnessed_key(), 500, 10.4, Verdict::Valid);
    rec.witness = Some(witness());
    let bytes = to_wire(&rec).expect("a valid run travels");
    let back = from_wire(&bytes).expect("and arrives");

    // Everything identity-bearing must be bit-identical.
    assert_eq!(back.key, rec.key);
    assert_eq!(back.witness, rec.witness);
    assert_eq!(back.verdict, rec.verdict);
    assert_eq!(back.trials, rec.trials);
    assert_eq!(back.content_frames, rec.content_frames);
    assert_eq!(back.measured_at, rec.measured_at);
    assert_eq!(back.build, rec.build);
    assert_eq!(back.model_name, rec.model_name);
    assert_eq!(back.placement_human, rec.placement_human);
    assert_eq!(back.backend, rec.backend);
    assert!(
        (back.decode_tok_s - rec.decode_tok_s).abs() < 1e-9,
        "and the number must survive to well past any precision a reader \
             could act on"
    );
    assert!(
        back.witness
            .as_ref()
            .is_some_and(|w| w.explains(&back.key.placement_digest)),
        "the witness must still explain its own key on the far side — a \
             record that cannot account for itself is exactly what travel is for"
    );
}

#[test]
fn the_wire_may_shift_a_rate_by_one_ulp_and_the_key_does_not_move() {
    // Not hypothetical: `serde_json` is built without `float_roundtrip`, so
    // this is what the pipe actually does. Documented as a test because the
    // consequence — an orphan KV entry LWW can never overwrite — is only
    // obvious once you know the cause.
    let mut rec = rec_at(key(), 500, 0.0, Verdict::Valid);
    rec.decode_tok_s = 10.4 - 0.2; // 10.200000000000001
    let before = wire_key(&rec);

    let back = from_wire(&to_wire(&rec).unwrap()).unwrap();
    assert_ne!(
        back.decode_tok_s.to_bits(),
        rec.decode_tok_s.to_bits(),
        "if this ever starts holding, `float_roundtrip` was enabled \
             somewhere and the quantization below is merely belt-and-braces"
    );
    assert_eq!(
        wire_key(&back),
        before,
        "the same measurement must not compute two different keys depending \
             on which copy of it you happen to be holding"
    );
}

#[test]
fn an_invalid_run_never_travels() {
    let rec = rec_at(
        key(),
        500,
        10.4,
        Verdict::Invalid {
            problems: vec!["trial spread 41% exceeds 25%".into()],
        },
    );
    assert!(
        to_wire(&rec).is_none(),
        "a failed run is glassbox material at home and noise on a peer"
    );
}

#[test]
fn from_wire_drops_a_record_written_by_another_schema() {
    let rec = rec_at(key(), 500, 10.4, Verdict::Valid);
    let mut env: serde_json::Value =
        serde_json::from_slice(&to_wire(&rec).unwrap()).expect("envelope is json");
    env["schema_version"] = serde_json::json!(SCHEMA_VERSION + 7);
    assert!(
        from_wire(serde_json::to_vec(&env).unwrap().as_slice()).is_none(),
        "an unrecognised schema must be dropped, not half-read"
    );
    assert!(
        from_wire(b"{not json").is_none(),
        "and a corrupt entry must not be an error the reader has to handle"
    );
}

#[test]
fn republishing_a_record_overwrites_its_own_entry() {
    let rec = rec_at(key(), 500, 10.4, Verdict::Valid);
    assert_eq!(
        wire_key(&rec),
        wire_key(&rec.clone()),
        "the boot republish runs on every start; a key derived from the \
             record is what keeps that from accumulating copies"
    );

    // Same configuration, same second, different number: two real runs, so
    // two entries.
    let faster = rec_at(key(), 500, 11.9, Verdict::Valid);
    assert_ne!(
        wire_key(&rec),
        wire_key(&faster),
        "two runs must not collide just because they share a timestamp"
    );
}

#[test]
fn wire_keys_sort_chronologically() {
    let early = wire_key(&rec_at(key(), 900, 10.4, Verdict::Valid));
    let late = wire_key(&rec_at(key(), 1_700_000_000, 10.4, Verdict::Valid));
    assert!(
        early < late,
        "zero-padding is what makes a raw `scan` of the namespace readable \
             oldest-first without decoding anything: {early} vs {late}"
    );
}

#[test]
fn a_peer_measurement_is_offered_and_says_whose_it_is() {
    let f = MeasurementFile::new();
    let theirs = from_peer(
        rec_at(witnessed_key(), 500, 11.08, Verdict::Valid),
        Some("BeefyMac"),
    );
    let near = near_misses(&f, &[theirs], &moved_key(), Some(&moved_witness()));
    assert_eq!(near.len(), 1, "an empty local store is not an empty answer");
    assert_eq!(
        near[0].taken_by.as_deref(),
        Some("BeefyMac"),
        "a number from hardware the reader has never seen must be named as such"
    );
}

#[test]
fn a_local_measurement_is_attributed_to_nobody() {
    let mut f = MeasurementFile::new();
    let mut rec = rec_at(witnessed_key(), 500, 10.4, Verdict::Valid);
    rec.witness = Some(witness());
    record(&mut f, rec);
    let near = near_misses(&f, &[], &moved_key(), Some(&moved_witness()));
    assert_eq!(near.len(), 1);
    assert!(
        near[0].taken_by.is_none(),
        "`None` is the reader's own run — the one thing they can re-measure"
    );
}

#[test]
fn a_peer_who_measured_this_exact_configuration_is_kept_and_marked() {
    let f = MeasurementFile::new();
    let asked = witnessed_key();
    let theirs = from_peer(
        rec_at(asked.clone(), 500, 11.08, Verdict::Valid),
        Some("BeefyMac"),
    );
    let near = near_misses(&f, &[theirs], &asked, Some(&witness()));
    assert_eq!(
        near.len(),
        1,
        "an exact peer hit is the most informative thing travel delivers; \
             dropping it because the key matched would throw away the answer"
    );
    assert!(near[0].is_exact());
    assert!(near[0].differs_by.is_empty());
}

#[test]
fn lookup_never_serves_a_peer_number() {
    // The property that lets `near_misses` merge the two sources safely:
    // there is no path by which a peer's record can reach `lookup`, so
    // `mesh plan` keeps saying "not measured **here**".
    let f = MeasurementFile::new();
    let asked = witnessed_key();
    let theirs = from_peer(
        rec_at(asked.clone(), 500, 11.08, Verdict::Valid),
        Some("BeefyMac"),
    );

    assert!(
        lookup(&f, &asked, "0.10.0").is_none(),
        "an exact peer hit must not become a local measurement"
    );
    assert_eq!(
        near_misses(&f, &[theirs], &asked, Some(&witness())).len(),
        1,
        "it is still offered — beside the miss, attributed, not as the answer"
    );
}

/// A peer's number is the one the reader cannot check, so the load it was
/// taken under has to travel with it. Without this the reader is back in the
/// position that produced a false 43% variance: two rates, no way to know
/// they were taken on differently-loaded machines.
#[test]
fn a_peers_conditions_travel_with_their_number() {
    let f = MeasurementFile::new();
    let asked = witnessed_key();
    let mut busy = rec_at(asked.clone(), 500, 7.75, Verdict::Valid);
    busy.conditions = Some(RunConditions {
        co_resident_roles: vec!["embed".into(), "fast".into()],
        host_rss_mb_before: Some(4_100),
        host_rss_mb_after: Some(4_260),
        host_uptime_s: Some(2_320),
        run_span_s: Some(41.5),
        rpc_endpoints: Vec::new(),
    });
    let theirs = from_peer(busy, Some("BeefyMac"));

    let near = near_misses(&f, &[theirs], &asked, Some(&witness()));
    assert_eq!(near.len(), 1);
    let line = near[0]
        .conditions
        .as_deref()
        .expect("a peer's conditions must reach the surface that offers their number");
    assert!(line.contains("embed"), "{line}");
    assert!(line.contains("fast"), "{line}");
    assert_eq!(near[0].taken_by.as_deref(), Some("BeefyMac"));
}

/// An old record carries no conditions, and the surface must say nothing
/// rather than imply the box was quiet.
#[test]
fn a_peer_record_without_conditions_offers_none() {
    let f = MeasurementFile::new();
    let asked = witnessed_key();
    let theirs = from_peer(
        rec_at(asked.clone(), 500, 11.08, Verdict::Valid),
        Some("BeefyMac"),
    );
    let near = near_misses(&f, &[theirs], &asked, Some(&witness()));
    assert_eq!(near.len(), 1);
    assert!(
        near[0].conditions.is_none(),
        "absent conditions must not be rendered as a claim about the box"
    );
}

#[test]
fn an_invalid_peer_record_is_not_offered() {
    // `to_wire` refuses to publish these, so one can only arrive from a
    // peer on a build that did not yet refuse. Filter on read as well:
    // the wire is not a trust boundary we control.
    let f = MeasurementFile::new();
    let theirs = from_peer(
        rec_at(
            witnessed_key(),
            500,
            2.1,
            Verdict::Invalid {
                problems: vec!["decode stalled".into()],
            },
        ),
        Some("BeefyMac"),
    );
    assert!(near_misses(&f, &[theirs], &moved_key(), Some(&moved_witness())).is_empty());
}

#[test]
fn a_peer_measuring_a_different_model_is_not_a_near_miss() {
    let f = MeasurementFile::new();
    let mut other = witnessed_key();
    other.model_fingerprint = "mf1:something-else".into();
    let theirs = from_peer(rec_at(other, 500, 44.0, Verdict::Valid), Some("BeefyMac"));
    assert!(
        near_misses(&f, &[theirs], &moved_key(), Some(&moved_witness())).is_empty(),
        "a different model's number is an unrelated fact, not a weaker answer"
    );
}

#[test]
fn local_and_peer_measurements_rank_together_by_recency() {
    let mut f = MeasurementFile::new();
    let mut older_local = rec_at(witnessed_key(), 100, 10.4, Verdict::Valid);
    older_local.witness = Some(witness());
    record(&mut f, older_local);

    let newer_peer = from_peer(
        rec_at(witnessed_key(), 900, 11.08, Verdict::Valid),
        Some("BeefyMac"),
    );
    let near = near_misses(&f, &[newer_peer], &moved_key(), Some(&moved_witness()));
    assert_eq!(near.len(), 2);
    assert_eq!(
        near[0].taken_by.as_deref(),
        Some("BeefyMac"),
        "the question is what is the closest thing anyone measured, so the \
             two sources rank in one list rather than local-always-first"
    );
    assert!(near[1].taken_by.is_none());
}

#[test]
fn a_peer_falls_back_to_its_node_id_when_the_mesh_cannot_name_it() {
    let rec = rec_at(key(), 500, 10.4, Verdict::Valid);
    assert_eq!(
        from_peer(rec.clone(), Some("BeefyMac")).describe_origin(),
        "BeefyMac"
    );
    assert_eq!(
        from_peer(rec.clone(), None).describe_origin(),
        "node-b88252e4325bc377",
        "a peer that has left the mesh is still matchable against `mesh status`"
    );
    assert_eq!(
        from_peer(rec, Some("   ")).describe_origin(),
        "node-b88252e4325bc377",
        "a blank name is no name"
    );
}

// -----------------------------------------------------------------------
// Run conditions — the half a witness does not explain
// -----------------------------------------------------------------------

fn conditions() -> RunConditions {
    RunConditions {
        co_resident_roles: vec!["embed".into(), "fast".into()],
        host_rss_mb_before: Some(4_100),
        host_rss_mb_after: Some(4_260),
        host_uptime_s: Some(2_320),
        run_span_s: Some(41.5),
        rpc_endpoints: Vec::new(),
    }
}

/// The load-bearing rule. Conditions are explanatory, never identity: if
/// they reached the key, two runs of one configuration taken under any
/// different load would file under different keys, `lookup` would never
/// find more than one run, and the variance this field exists to expose
/// would become structurally invisible.
#[test]
fn conditions_never_reach_the_key() {
    let quiet = RunConditions {
        co_resident_roles: vec![],
        host_rss_mb_before: Some(900),
        host_rss_mb_after: Some(905),
        host_uptime_s: Some(90_000),
        run_span_s: Some(38.0),
        rpc_endpoints: Vec::new(),
    };
    let busy = conditions();

    let a = MeasurementRecord {
        conditions: Some(quiet),
        ..rec_at(key(), 1_000, 11.08, Verdict::Valid)
    };
    let b = MeasurementRecord {
        conditions: Some(busy),
        ..rec_at(key(), 2_000, 7.75, Verdict::Valid)
    };

    assert_eq!(a.key, b.key, "conditions must not participate in identity");

    // And both survive in one file, under one key, which is the whole point.
    let mut f = MeasurementFile::new();
    record(&mut f, a);
    record(&mut f, b);
    let s = lookup(&f, &key(), "0.10.0").expect("both runs are one configuration");
    assert_eq!(
        s.runs, 2,
        "a quiet run and a busy run belong to the same configuration"
    );
    assert!(
        (s.decode_tok_s_min - 7.75).abs() < 1e-9 && (s.decode_tok_s_max - 11.08).abs() < 1e-9,
        "the spread of run medians stays visible across conditions, got {}–{}",
        s.decode_tok_s_min,
        s.decode_tok_s_max
    );
}

/// A record written before conditions existed must still load. The field is
/// explanatory, so unlike the v1->v2 key change there is nothing to discard.
#[test]
fn a_record_with_no_conditions_still_loads() {
    let mut json = serde_json::to_value(rec_at(key(), 10, 9.0, Verdict::Valid)).unwrap();
    json.as_object_mut().unwrap().remove("conditions");
    assert!(
        !json.as_object().unwrap().contains_key("conditions"),
        "fixture must actually lack the field"
    );
    let back: MeasurementRecord = serde_json::from_value(json).unwrap();
    assert!(back.conditions.is_none(), "absent means not recorded");
}

/// Every record filed before routes were captured lacks the field entirely.
/// It must load as "no route recorded" — and specifically NOT be mistaken
/// for a local load, which is the one reading that would silently turn a
/// missing measurement into a claim about the topology.
#[test]
fn a_record_with_no_rpc_endpoints_still_loads() {
    let r = MeasurementRecord {
        conditions: Some(conditions()),
        ..rec_at(key(), 10, 9.0, Verdict::Valid)
    };
    let mut json = serde_json::to_value(&r).unwrap();
    let c = json
        .get_mut("conditions")
        .and_then(|c| c.as_object_mut())
        .expect("fixture has conditions");
    c.remove("rpc_endpoints");
    assert!(
        !c.contains_key("rpc_endpoints"),
        "fixture must actually lack the field"
    );

    let back: MeasurementRecord = serde_json::from_value(json).unwrap();
    let back_c = back.conditions.expect("conditions survive");
    assert!(
        back_c.rpc_endpoints.is_empty(),
        "an unrecorded route reads as unrecorded"
    );
    assert!(
        back_c.describe().is_some_and(|d| !d.contains("rpc via")),
        "an unrecorded route must not be described as a route"
    );
}

/// The route is why two runs of one configuration can differ, so it has to
/// be legible in the line an operator actually reads — and named, because
/// a count cannot distinguish the LAN address from the overlay address.
#[test]
fn a_recorded_route_is_named_in_the_operator_line() {
    let c = RunConditions {
        rpc_endpoints: vec!["192.168.1.2:50052".into()],
        ..conditions()
    };
    let line = c.describe().expect("conditions render");
    assert!(
        line.contains("192.168.1.2:50052"),
        "the dialled address must appear verbatim, got {line}"
    );
}

#[test]
fn conditions_round_trip_through_json() {
    let r = MeasurementRecord {
        conditions: Some(conditions()),
        ..rec_at(key(), 10, 9.0, Verdict::Valid)
    };
    let back: MeasurementRecord =
        serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
    assert_eq!(back.conditions, r.conditions);
}

/// covers: FE-92
///
/// An empty slot list is a FINDING — "nothing else was resident" — not
/// missing information. A reader who cannot tell those apart cannot use the
/// field to compare two runs, which is its only purpose.
#[test]
fn no_co_residents_is_reported_as_a_finding_not_as_silence() {
    let c = RunConditions {
        co_resident_roles: vec![],
        host_rss_mb_before: None,
        host_rss_mb_after: None,
        host_uptime_s: None,
        run_span_s: None,
        rpc_endpoints: Vec::new(),
    };
    let line = c
        .describe()
        .expect("an empty slot list still says something");
    assert!(
        line.contains("nothing else resident"),
        "expected an explicit finding, got: {line}"
    );
    assert!(!c.had_co_residents());
}

#[test]
fn describe_names_every_co_resident_and_the_rss_climb() {
    let line = conditions().describe().unwrap();
    assert!(line.contains("embed"), "{line}");
    assert!(line.contains("fast"), "{line}");
    assert!(line.contains("4100 MB"), "{line}");
    assert!(
        line.contains("+160 MB"),
        "expected a signed delta, got: {line}"
    );
    assert!(
        line.contains("38m"),
        "expected a compact uptime, got: {line}"
    );
}

/// A negative delta means a slot was evicted mid-run, which invalidates the
/// comparison as surely as a climb does. Reporting it unsigned would hide
/// the direction and make the two look alike.
#[test]
fn an_rss_drop_reports_as_negative() {
    let c = RunConditions {
        host_rss_mb_before: Some(90_000),
        host_rss_mb_after: Some(4_000),
        ..conditions()
    };
    assert_eq!(c.rss_delta_mb(), Some(-86_000));
    assert!(c.describe().unwrap().contains("-86000 MB"));
}

#[test]
fn an_rss_delta_needs_both_ends() {
    let c = RunConditions {
        host_rss_mb_after: None,
        ..conditions()
    };
    assert_eq!(c.rss_delta_mb(), None, "one reading is not a delta");
    let line = c.describe().unwrap();
    assert!(line.contains("4100 MB"), "{line}");
    assert!(!line.contains("+"), "no delta should be claimed: {line}");
}

#[test]
fn human_duration_stays_compact_across_the_ranges() {
    assert_eq!(human_duration(45), "45s");
    assert_eq!(human_duration(89), "89s");
    assert_eq!(human_duration(90), "1m");
    assert_eq!(human_duration(2_320), "38m");
    assert_eq!(human_duration(5_400), "1h30m");
    assert_eq!(human_duration(90_061), "25h01m");
}
