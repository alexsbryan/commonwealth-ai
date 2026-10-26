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
        "mf1:00000000000000aa".into(),
        "pd2:00000000000000bb".into(),
        32_768,
        LinkClass::Direct,
    )
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
    assert_eq!(w, &witness(), "a witness survives a round trip unchanged");
}

// -- Travel -------------------------------------------------------------

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
/// different load would file under different keys, no key would ever hold
/// more than one run, and the variance this field exists to expose
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
    assert_eq!(
        f.records().iter().filter(|r| r.key == key()).count(),
        2,
        "a quiet run and a busy run belong to the same configuration"
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
