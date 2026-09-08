// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the N=1 adapters must be, spelled as tests.
//!
//! The load-bearing one is [`solo_peer_store_is_total`]: the whole risk this
//! module guards is that a "solo" adapter quietly becomes a null object that
//! refuses or skips instead of answering. A skip is a runtime divergence with
//! no compiler and no gate behind it, which is strictly worse than the
//! link-time coupling the port replaced. So the property is asserted over
//! inputs an implementation might be tempted to reject, and the assertion is
//! on `is_ok()` — a shape no plausible edit to the store can also rewrite.

use super::*;

fn node(n: u128) -> NodeId {
    NodeId::from_u128(n)
}

/// Inputs a `PeerStore` might be tempted to refuse. Deliberately awkward:
/// empty strings on both axes, a key that is itself a prefix of another, a
/// long key, non-ASCII, an empty value, and bytes that are not valid UTF-8.
fn awkward_inputs() -> Vec<(String, String, Bytes)> {
    vec![
        (String::new(), String::new(), Bytes::new()),
        ("notes".into(), String::new(), Bytes::from_static(b"")),
        (String::new(), "key".into(), Bytes::from_static(b"v")),
        ("notes".into(), "a".into(), Bytes::from_static(b"one")),
        ("notes".into(), "a:b".into(), Bytes::from_static(b"two")),
        (
            "notes".into(),
            "x".repeat(4096),
            Bytes::from_static(b"long key"),
        ),
        ("работа".into(), "клавиша".into(), "значение".into()),
        (
            "notes".into(),
            "binary".into(),
            Bytes::from_static(&[0xff, 0x00, 0xfe, 0x80]),
        ),
        ("work-atlas-private".into(), "claim:".into(), Bytes::new()),
    ]
}

/// THE kill-bar test (K11). No input makes the solo store refuse, and no
/// method has an arm that means "not applicable locally".
#[test]
fn solo_peer_store_is_total() {
    let store = SoloPeerStore::new();
    for (app_id, key, value) in awkward_inputs() {
        assert!(
            store.get(&app_id, &key).is_ok(),
            "get refused ({app_id:?}, {key:?}) before it was ever written"
        );
        assert!(
            store.delete(&app_id, &key).is_ok(),
            "delete refused ({app_id:?}, {key:?}) that was never there"
        );
        assert!(
            store.set(&app_id, &key, value.clone(), node(1)).is_ok(),
            "set refused ({app_id:?}, {key:?})"
        );
        assert!(
            store.set(&app_id, &key, value, node(2)).is_ok(),
            "set refused a re-write of ({app_id:?}, {key:?}) from another origin"
        );
        assert!(
            store.get(&app_id, &key).is_ok(),
            "get refused ({app_id:?}, {key:?}) after it was written"
        );
        assert!(
            store.scan(&app_id, "").is_ok(),
            "scan refused namespace {app_id:?}"
        );
        assert!(
            store.delete(&app_id, &key).is_ok(),
            "delete refused ({app_id:?}, {key:?}) that was there"
        );
    }
    assert!(
        store.is_empty(),
        "every awkward input was written and deleted, so the store should be empty"
    );
}

/// A mesh of one still STORES. The failure this catches is a "solo" adapter
/// that accepts every write and remembers none — total, and useless.
#[test]
fn solo_peer_store_reads_back_what_it_wrote() {
    let store = SoloPeerStore::new();
    store
        .set("notes", "k", Bytes::from_static(b"payload"), node(7))
        .unwrap();

    let got = store.get("notes", "k").unwrap().expect("just written");
    assert_eq!(got.app_id, "notes");
    assert_eq!(got.key, "k");
    assert_eq!(got.value, Bytes::from_static(b"payload"));
    assert_eq!(got.origin, node(7));

    assert_eq!(store.get("notes", "absent").unwrap(), None);
    assert_eq!(
        store.get("other-namespace", "k").unwrap(),
        None,
        "namespaces do not leak into each other"
    );
}

/// `set` reports whether the VALUE changed, not whether a write happened — the
/// contract the notes publish sink reads to decide it re-published identical
/// bytes rather than new ones.
#[test]
fn solo_peer_store_set_reports_value_change_only() {
    let store = SoloPeerStore::new();
    assert!(store
        .set("notes", "k", Bytes::from_static(b"a"), node(1))
        .unwrap());
    assert!(
        !store
            .set("notes", "k", Bytes::from_static(b"a"), node(1))
            .unwrap(),
        "identical bytes are not a change"
    );
    assert!(
        !store
            .set("notes", "k", Bytes::from_static(b"a"), node(9))
            .unwrap(),
        "a different origin writing identical bytes is not a change either"
    );
    assert!(store
        .set("notes", "k", Bytes::from_static(b"b"), node(1))
        .unwrap());
}

#[test]
fn solo_peer_store_scan_is_prefix_scoped_and_namespaced() {
    let store = SoloPeerStore::new();
    for (app, key) in [
        ("notes", "claim:1"),
        ("notes", "claim:2"),
        ("notes", "session:1"),
        ("notes-private", "claim:3"),
    ] {
        store
            .set(app, key, Bytes::from_static(b"v"), node(1))
            .unwrap();
    }

    let claims: Vec<String> = store
        .scan("notes", "claim:")
        .unwrap()
        .into_iter()
        .map(|e| e.key)
        .collect();
    assert_eq!(claims, vec!["claim:1".to_string(), "claim:2".to_string()]);

    assert_eq!(store.scan("notes", "").unwrap().len(), 3);
    assert_eq!(store.scan("notes-private", "claim:").unwrap().len(), 1);
    assert!(store.scan("never-written", "").unwrap().is_empty());
}

#[test]
fn solo_peer_store_delete_reports_whether_anything_went() {
    let store = SoloPeerStore::new();
    assert!(!store.delete("notes", "k").unwrap());
    store
        .set("notes", "k", Bytes::from_static(b"v"), node(1))
        .unwrap();
    assert!(store.delete("notes", "k").unwrap());
    assert!(!store.delete("notes", "k").unwrap());
    assert_eq!(store.get("notes", "k").unwrap(), None);
}

/// ARCH §18.3 — a path that has never succeeded reports ABSENT, not "now" and
/// not "converged". The N=1 case is where the temptation to default is
/// strongest, because a mesh of one is trivially converged; it is still not
/// evidence that the publish path ran.
#[test]
fn solo_convergence_reports_absence_until_a_path_actually_runs() {
    let c = SoloConvergence::new();
    assert_eq!(c.snapshot(), (None, None));

    c.record_outbound_publish_success(1_700_000_000);
    assert_eq!(c.snapshot(), (Some(1_700_000_000), None));

    c.record_inbound_ingest_success(1_700_000_042);
    assert_eq!(c.snapshot(), (Some(1_700_000_000), Some(1_700_000_042)));
}

#[test]
fn solo_convergence_keeps_the_latest_stamp_per_direction() {
    let c = SoloConvergence::new();
    c.record_outbound_publish_success(10);
    c.record_outbound_publish_success(20);
    c.record_inbound_ingest_success(30);
    assert_eq!(c.snapshot(), (Some(20), Some(30)));
}

/// Both adapters construct with no I/O and nothing that can fail — the
/// property that makes a local daemon's boot have nothing to mint.
#[test]
fn solo_adapters_construct_infallibly() {
    let store: Box<dyn PeerStore> = Box::new(SoloPeerStore::new());
    let conv: Box<dyn Convergence> = Box::new(SoloConvergence::new());
    assert!(store.scan("anything", "").unwrap().is_empty());
    assert_eq!(conv.snapshot(), (None, None));
}
