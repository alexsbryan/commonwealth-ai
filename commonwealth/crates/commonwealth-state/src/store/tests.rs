use super::*;
use commonwealth_core::ids::NodeId;

fn node(n: u128) -> NodeId {
    NodeId::from_u128(n)
}

/// covers: FE-119
///
/// Neither `gc` nor `gc_app` had any test. Both are silent by
/// construction — they return a count nobody checks and delete rows
/// nobody reads afterwards — so both failure directions ship quietly:
/// a gc that deletes nothing lets a replicated store grow without
/// bound, and a `gc_app` that ignores its `app_id` reaches across
/// namespaces and takes another app's live state with it.
///
/// Timestamps are planted through `merge_entry` rather than `set`,
/// because `set` stamps `now_secs()` and a TTL test that has to sleep
/// is a test that will be deleted the first time it flakes.
#[test]
fn gc_is_bounded_by_both_the_cutoff_and_the_namespace() {
    let store = MeshStore::in_memory().unwrap();
    let now = now_secs();
    let plant = |app: &str, key: &str, age: u64| {
        store
            .merge_entry(StoreEntry {
                app_id: app.to_string(),
                key: key.to_string(),
                value: Bytes::from("v"),
                timestamp: now - age,
                origin: node(1),
            })
            .unwrap()
    };
    const TTL: u64 = 100;

    // Two namespaces. `chat` holds one stale and one live entry;
    // `presence` holds a stale entry that `gc_app("chat", ..)` must not
    // touch.
    plant("chat", "old", 500);
    plant("chat", "fresh", 5);
    plant("presence", "old", 500);
    // Exactly ON the cutoff. `delete_older_than` is `timestamp <
    // cutoff`, so this entry SURVIVES — the boundary is stated here so
    // an off-by-one in either direction is a failure rather than a
    // silent change in how much history a TTL keeps.
    plant("chat", "at-cutoff", TTL);

    // ONE cutoff, derived from the same `now` the rows were planted
    // against. The TTL forms re-read the clock, so a tick between plant
    // and sweep silently moves the boundary this test exists to pin.
    let cutoff = now - TTL;
    let deleted = store.gc_app_before("chat", cutoff).unwrap();
    assert_eq!(deleted, 1, "only `chat`'s stale entry is old enough");
    assert!(store.get("chat", "old").unwrap().is_none());
    assert!(store.get("chat", "fresh").unwrap().is_some());
    assert!(
        store.get("chat", "at-cutoff").unwrap().is_some(),
        "an entry exactly at the cutoff is not yet older than the TTL"
    );
    assert!(
        store.get("presence", "old").unwrap().is_some(),
        "gc_app must not reach outside its namespace"
    );

    // The unscoped sweep spans every app, and reports what it took.
    let deleted = store.gc_before(cutoff).unwrap();
    assert_eq!(deleted, 1, "the remaining stale entry, in the other app");
    assert!(store.get("presence", "old").unwrap().is_none());
    assert!(store.get("chat", "fresh").unwrap().is_some());

    // A sweep with nothing to take reports zero rather than erroring —
    // the caller is a periodic task and a spurious Err would be logged
    // forever.
    assert_eq!(store.gc_before(cutoff).unwrap(), 0);
}

#[test]
fn set_and_get_roundtrip() {
    let store = MeshStore::in_memory().unwrap();
    store
        .set("myapp", "greeting", Bytes::from("hello"), node(1))
        .unwrap();
    let entry = store.get("myapp", "greeting").unwrap().unwrap();
    assert_eq!(entry.value.as_ref(), b"hello");
    assert_eq!(entry.app_id, "myapp");
    assert_eq!(entry.key, "greeting");
    assert_eq!(entry.origin, node(1));
}

#[test]
fn origin_round_trips_for_realistic_node_id() {
    // Regression test: real NodeIds (random 16 bytes, not low-int test
    // fixtures) used to silently byte-reverse on read because writes
    // were verbatim but reads went through `u128::from_le_bytes`. The
    // `node(1)` fixtures don't catch this — `0...01` looks identical
    // reversed at the display layer because Display only shows the
    // first 8 bytes — so we use a value whose low and high halves
    // differ.
    let store = MeshStore::in_memory().unwrap();
    let id = NodeId::from_u128(0x1122_3344_5566_7788_AABB_CCDD_EEFF_0011);
    store.set("a", "k", Bytes::from("v"), id).unwrap();
    let entry = store.get("a", "k").unwrap().unwrap();
    assert_eq!(entry.origin, id);
    let scanned = store.scan("a", "").unwrap();
    assert_eq!(scanned[0].origin, id);
}

#[test]
fn get_missing_returns_none() {
    let store = MeshStore::in_memory().unwrap();
    assert!(store.get("myapp", "nope").unwrap().is_none());
}

#[test]
fn merge_entry_lww() {
    let store = MeshStore::in_memory().unwrap();

    let old = StoreEntry {
        app_id: "a".into(),
        key: "k".into(),
        value: Bytes::from("old"),
        timestamp: 100,
        origin: node(1),
    };
    let new = StoreEntry {
        app_id: "a".into(),
        key: "k".into(),
        value: Bytes::from("new"),
        timestamp: 200,
        origin: node(2),
    };

    assert!(store.merge_entry(old).unwrap());
    // Older entry should be rejected.
    let stale = StoreEntry {
        app_id: "a".into(),
        key: "k".into(),
        value: Bytes::from("stale"),
        timestamp: 50,
        origin: node(3),
    };
    assert!(!store.merge_entry(stale).unwrap());
    // Newer entry should be accepted.
    assert!(store.merge_entry(new).unwrap());

    let e = store.get("a", "k").unwrap().unwrap();
    assert_eq!(e.value.as_ref(), b"new");
    assert_eq!(e.timestamp, 200);
}

/// **LWW tie-break determinism (clock-skew vector).** Consumer
/// hardware clocks skew, so two nodes can stamp the same key with
/// the *same* second. `upsert_if_newer` accepts only `ts > existing`
/// (a later write at an equal timestamp is rejected), so locally
/// the INCUMBENT wins a tie — deterministic, no last-arrival thrash.
///
/// KNOWN LIMITATION pinned here on purpose: this makes ties
/// *node-local* deterministic but NOT cross-node convergent. If A
/// holds X@100 and B holds Y@100 for the same key (equal stamp,
/// different origins), each rejects the other's value on merge and
/// they stay diverged — origin is not a tiebreaker. Acceptable
/// today because every gossiped namespace keys by origin/content
/// (so two nodes don't co-write one key at one second); if a future
/// shared-key namespace appears, add a deterministic tiebreaker
/// (e.g. higher origin NodeId wins) rather than relying on this.
#[test]
fn merge_entry_equal_timestamp_keeps_incumbent() {
    let store = MeshStore::in_memory().unwrap();
    let first = StoreEntry {
        app_id: "a".into(),
        key: "k".into(),
        value: Bytes::from("incumbent"),
        timestamp: 100,
        origin: node(1),
    };
    assert!(store.merge_entry(first).unwrap());

    // Same timestamp, different value+origin — must be rejected,
    // deterministically, regardless of arrival order.
    let tie = StoreEntry {
        app_id: "a".into(),
        key: "k".into(),
        value: Bytes::from("challenger"),
        timestamp: 100,
        origin: node(2),
    };
    assert!(
        !store.merge_entry(tie).unwrap(),
        "equal-timestamp write must not displace the incumbent"
    );
    assert_eq!(
        store.get("a", "k").unwrap().unwrap().value.as_ref(),
        b"incumbent"
    );
}

#[test]
fn delete_removes_entry() {
    let store = MeshStore::in_memory().unwrap();
    store.set("a", "k", Bytes::from("v"), node(1)).unwrap();
    assert!(store.delete("a", "k").unwrap());
    assert!(store.get("a", "k").unwrap().is_none());
}

#[test]
fn scan_filters_by_prefix() {
    let store = MeshStore::in_memory().unwrap();
    store
        .set("inf", "model:abc", Bytes::from("a"), node(1))
        .unwrap();
    store
        .set("inf", "model:def", Bytes::from("b"), node(1))
        .unwrap();
    store
        .set("inf", "ledger:xyz", Bytes::from("c"), node(1))
        .unwrap();
    store
        .set("other", "model:abc", Bytes::from("d"), node(1))
        .unwrap();

    let model_entries = store.scan("inf", "model:").unwrap();
    assert_eq!(model_entries.len(), 2);
    assert!(model_entries.iter().all(|e| e.key.starts_with("model:")));

    let ledger_entries = store.scan("inf", "ledger:").unwrap();
    assert_eq!(ledger_entries.len(), 1);
    assert_eq!(ledger_entries[0].key, "ledger:xyz");

    // Prefix not present for app returns empty.
    let empty = store.scan("inf", "nope:").unwrap();
    assert!(empty.is_empty());

    // Scan is scoped to app_id.
    let scoped = store.scan("other", "model:").unwrap();
    assert_eq!(scoped.len(), 1);
}

// ── The outbox: what this node wrote, for the rail ──────

/// **THE SENDER-SIDE OUTBOX GUARD** (fp-107). A local-only write IS
/// queued — the pump journals it and the ring never offers it (fp-76's
/// class); a rail-carried write is not, since its namespace left the KV
/// rail for the ring rail and a second transport would double it.
///
/// The named failing input is a local-only write missing from the queue,
/// or a rail-carried one in it. Watched red by restoring
/// `is_gossip_excluded` in `backend::memory`'s `enqueue`.
#[test]
fn the_outbox_queues_local_only_writes_and_never_rail_carried_ones() {
    use crate::peer_preferences::RAIL_CARRIED_APP_IDS;
    use commonwealth_rail_core::LOCAL_ONLY_NAMESPACES;

    let store = MeshStore::in_memory().unwrap();
    // Driving the LISTS rather than a hand-picked few means a namespace
    // added to either later is covered without editing this test.
    for app in LOCAL_ONLY_NAMESPACES.iter().chain(RAIL_CARRIED_APP_IDS) {
        store.set(app, "k", Bytes::from("mine"), node(1)).unwrap();
    }

    let queued = store.outbox_take(100).unwrap();
    let apps: Vec<&str> = queued.iter().map(|q| q.app_id.as_str()).collect();
    for app in LOCAL_ONLY_NAMESPACES {
        assert!(apps.contains(app), "local-only {app} not queued");
    }
    for app in RAIL_CARRIED_APP_IDS {
        assert!(!apps.contains(app), "rail-carried {app} queued");
    }
    assert_eq!(queued.len(), LOCAL_ONLY_NAMESPACES.len(), "{apps:?}");
    store
        .outbox_ack(&queued.iter().map(|q| q.id).collect::<Vec<_>>())
        .unwrap();

    // A local-only DELETE queues its tombstone: the journal is the
    // namespace's durable copy, and a missing tombstone resurrects the row.
    let local = LOCAL_ONLY_NAMESPACES[0];
    assert!(store.delete(local, "k").unwrap());
    let tomb = store.outbox_take(100).unwrap();
    assert_eq!(tomb.len(), 1, "{tomb:?}");
    assert_eq!(tomb[0].app_id, local);
    assert!(tomb[0].op.value.is_none(), "a delete queues a tombstone");
}

/// A LOCAL write queues an act; a write learned from a peer does not.
/// `merge_entry` is the receive side, and a row that re-entered the outbox
/// would be republished by every node that saw it, forever.
#[test]
fn a_local_write_queues_an_act_and_a_received_one_does_not() {
    let store = MeshStore::in_memory().unwrap();
    store.set("app", "a", Bytes::from("1"), node(1)).unwrap();
    assert!(store.delete("app", "a").unwrap());
    assert_eq!(store.outbox_len().unwrap(), 2, "set + delete");

    store.outbox_ack(&[1, 2]).unwrap();
    store
        .merge_entry(StoreEntry {
            app_id: "app".into(),
            key: "fromapeer".into(),
            value: Bytes::from("v"),
            timestamp: 9_000,
            origin: node(2),
        })
        .unwrap();
    assert_eq!(
        store.outbox_len().unwrap(),
        0,
        "a row learned from a peer must not be re-published"
    );

    // A write LWW rejected queues nothing either — there is no act.
    assert!(!store
        .merge_entry(StoreEntry {
            app_id: "app".into(),
            key: "fromapeer".into(),
            value: Bytes::from("older"),
            timestamp: 1,
            origin: node(2),
        })
        .unwrap());
    assert!(!store.delete("app", "never-existed").unwrap());
    assert_eq!(store.outbox_len().unwrap(), 0);
}

/// The tie rule, both shapes: the SAME origin rewriting a key in the same
/// second wins (program order); a DIFFERENT origin at the same timestamp
/// does not (the incumbent keeps, deterministically — see
/// `merge_entry_equal_timestamp_keeps_incumbent`).
#[test]
fn a_same_second_rewrite_by_the_same_origin_wins() {
    let store = MeshStore::in_memory().unwrap();
    assert!(store
        .set("app", "k", Bytes::from("first"), node(1))
        .unwrap());
    assert!(
        store
            .set("app", "k", Bytes::from("second"), node(1))
            .unwrap(),
        "the daemon's own second write in one second was being dropped"
    );
    assert_eq!(
        store.get("app", "k").unwrap().unwrap().value.as_ref(),
        b"second"
    );
    let queued = store.outbox_len().unwrap();
    assert!(
        !store
            .set("app", "k", Bytes::from("second"), node(1))
            .unwrap(),
        "the same bytes again is not a write"
    );
    assert_eq!(store.outbox_len().unwrap(), queued, "and queues nothing");
    let ts = store.get("app", "k").unwrap().unwrap().timestamp;
    let rival = StoreEntry {
        app_id: "app".into(),
        key: "k".into(),
        value: Bytes::from("rival"),
        timestamp: ts,
        origin: node(2),
    };
    assert!(
        !store.merge_entry(rival).unwrap(),
        "another origin at the same second is refused"
    );
    assert_eq!(
        store.get("app", "k").unwrap().unwrap().value.as_ref(),
        b"second"
    );
}

/// Take is non-destructive and ack is what removes: the pump appends
/// first and acks after, so a crash between them re-sends rather than
/// loses. The rail's op id is content-derived, so a duplicate append is
/// the same op.
#[test]
fn outbox_take_leaves_the_rows_and_ack_removes_them() {
    let store = MeshStore::in_memory().unwrap();
    for i in 0..5 {
        store
            .set("app", &format!("k{i}"), Bytes::from("v"), node(1))
            .unwrap();
    }
    let first = store.outbox_take(2).unwrap();
    assert_eq!(first.len(), 2, "the limit is honoured");
    assert_eq!(
        store.outbox_take(2).unwrap().len(),
        2,
        "taking must not consume"
    );

    let ids: Vec<i64> = first.iter().map(|r| r.id).collect();
    assert_eq!(store.outbox_ack(&ids).unwrap(), 2);
    assert_eq!(store.outbox_len().unwrap(), 3);
    // Acking the same ids again reports zero rather than erroring — the
    // caller is a loop and a spurious Err would be logged forever.
    assert_eq!(store.outbox_ack(&ids).unwrap(), 0);
    assert_eq!(store.outbox_ack(&[]).unwrap(), 0);
}

// ── The projection: what the ring says we hold ──────────

fn projected(key: &str, value: Option<&[u8]>, t: u64, actor: &str) -> crate::rail_kv::Projected {
    crate::rail_kv::Projected {
        key: key.to_string(),
        value: value.map(Bytes::copy_from_slice),
        t,
        actor: actor.to_string(),
    }
}

/// A projection of `rows` and NO claim about anyone's whole live set —
/// what the fold returns for a namespace nobody has sealed, which is every
/// namespace until one does.
fn unsealed(rows: Vec<crate::rail_kv::Projected>) -> crate::rail_kv::Projection {
    crate::rail_kv::Projection {
        rows,
        ..Default::default()
    }
}

/// The projection a sealed actor produces: its rows, and the live set it
/// vouches is whole.
fn sealed(
    rows: Vec<crate::rail_kv::Projected>,
    actor: &str,
    live: &[&str],
) -> crate::rail_kv::Projection {
    let mut p = unsealed(rows);
    p.sealed_actors.insert(
        actor.to_string(),
        live.iter().map(|k| k.to_string()).collect(),
    );
    p
}

/// This node, wherever a test needs one. Deliberately not any `node(n)` an
/// actor resolves to, so nothing is skipped as self by accident.
fn me() -> NodeId {
    node(99)
}

/// **THE RECEIVER-SIDE PRIVACY GUARD.** A peer that puts a private
/// namespace on the ring gets it projected NOWHERE, and the refusal is an
/// `Err` naming the namespace rather than a quiet `Ok(0 rows)` that reads
/// as "there was nothing to do" (ARCH §18.3). This is the half the old
/// `routes_app_internal` inbound check did.
#[test]
fn apply_projection_refuses_an_excluded_namespace() {
    use crate::GOSSIP_EXCLUDED_APP_IDS;

    let store = MeshStore::in_memory().unwrap();
    let rows = unsealed(vec![projected("k", Some(b"leaked"), 100, "aa")]);
    for app in GOSSIP_EXCLUDED_APP_IDS {
        let err = store
            .apply_projection(app, &rows, |_| Some(node(1)), me())
            .expect_err("an excluded namespace must be refused, not applied");
        assert!(
            err.to_string().contains(app),
            "the refusal must name the namespace: {err}"
        );
        assert!(store.get(app, "k").unwrap().is_none(), "{app} was written");
    }
    // The control: a public namespace goes through the same call. Its row
    // is stamped `now` rather than reusing the fixture's `t: 100` — the
    // ledger declares a thirty-day retention window and the fold applies
    // it, so a 1970 timestamp would be withheld for a reason that has
    // nothing to do with what this test is about.
    let live = unsealed(vec![projected("k", Some(b"leaked"), now_secs(), "aa")]);
    let applied = store
        .apply_projection("contributions", &live, |_| Some(node(1)), me())
        .unwrap();
    assert_eq!(applied.merged, 1);
}

/// The own-journal door (fp-108) takes local-only namespaces ONLY: a shared
/// namespace's journal holds peers' writes, and folding it as "ours" would
/// stamp them with this node's origin.
#[test]
fn apply_own_projection_refuses_a_namespace_that_is_not_local_only() {
    let store = MeshStore::in_memory().unwrap();
    let rows = unsealed(vec![projected("k", Some(b"v"), now_secs(), "aa")]);
    let err = store
        .apply_own_projection("contributions", &rows, "aa", me())
        .expect_err("a shared namespace must be refused by the own door");
    assert!(err.to_string().contains("contributions"), "{err}");
    assert!(store.get("contributions", "k").unwrap().is_none());
    // The control: a local-only namespace goes through, own rows only.
    let local = commonwealth_rail_core::LOCAL_ONLY_NAMESPACES[0];
    let mixed = unsealed(vec![
        projected("mine", Some(b"v"), now_secs(), "aa"),
        projected("theirs", Some(b"v"), now_secs(), "bb"),
    ]);
    let applied = store
        .apply_own_projection(local, &mixed, "aa", me())
        .unwrap();
    assert_eq!((applied.merged, applied.unattributed), (1, 1));
    assert_eq!(store.get(local, "mine").unwrap().unwrap().origin, me());
    assert!(store.get(local, "theirs").unwrap().is_none());
}

/// An actor the roster cannot place is COUNTED and skipped. Inventing an
/// origin — a zero id, our own — would attribute a peer's write to
/// somebody who did not make it, and `StoreEntry.origin` is what every
/// per-peer reader resolves by.
#[test]
fn apply_projection_reports_an_unattributable_actor_rather_than_inventing_an_origin() {
    let store = MeshStore::in_memory().unwrap();
    let rows = unsealed(vec![
        projected("known", Some(b"v"), 100, "aa"),
        projected("stranger", Some(b"v"), 100, "zz"),
    ]);
    let applied = store
        .apply_projection("app", &rows, |actor| (actor == "aa").then(|| node(7)), me())
        .unwrap();
    assert_eq!(applied.merged, 1);
    assert_eq!(applied.unattributed, 1);
    assert_eq!(store.get("app", "known").unwrap().unwrap().origin, node(7));
    assert!(
        store.get("app", "stranger").unwrap().is_none(),
        "an unplaceable actor's row is not written under some other node"
    );
}

/// A tombstone deletes only what is not NEWER than it. The failing input
/// is a delete at `t` racing a `set` at `t+1` that arrived first: without
/// the bound the tombstone takes a live row and the key is gone from this
/// node until somebody writes it again.
#[test]
fn a_tombstone_does_not_take_a_row_written_after_it() {
    let store = MeshStore::in_memory().unwrap();
    let plant = |key: &str, ts: u64| {
        store
            .merge_entry(StoreEntry {
                app_id: "app".into(),
                key: key.to_string(),
                value: Bytes::from("live"),
                timestamp: ts,
                origin: node(1),
            })
            .unwrap()
    };
    plant("newer", 200);
    plant("older", 100);
    plant("equal", 150);

    let applied = store
        .apply_projection(
            "app",
            &unsealed(vec![
                projected("newer", None, 150, "aa"),
                projected("older", None, 150, "aa"),
                projected("equal", None, 150, "aa"),
                // A tombstone for a key this node never held is not a
                // failure; there is simply nothing to take.
                projected("absent", None, 150, "aa"),
            ]),
            |_| Some(node(1)),
            me(),
        )
        .unwrap();

    assert_eq!(applied.deleted, 2, "`older` and `equal`, not `newer`");
    assert!(
        store.get("app", "newer").unwrap().is_some(),
        "a tombstone must not take a row written after it"
    );
    assert!(store.get("app", "older").unwrap().is_none());
    assert!(
        store.get("app", "equal").unwrap().is_none(),
        "a delete at the row's own second wins — the write happened, then the delete"
    );
}

/// Applying a projection does NOT re-queue it. The whole point of the
/// receive side going through `merge_entry` is that a row learned from a
/// peer cannot echo back onto the rail — and the reconciliation is on the
/// same side of the wire, so its delete must not queue a tombstone either.
#[test]
fn applying_a_projection_queues_nothing() {
    let store = MeshStore::in_memory().unwrap();
    store
        .merge_entry(StoreEntry {
            app_id: "app".into(),
            key: "retired".into(),
            value: Bytes::from("v"),
            timestamp: 50,
            origin: node(1),
        })
        .unwrap();
    let applied = store
        .apply_projection(
            "app",
            &sealed(
                vec![
                    projected("a", Some(b"v"), 100, "aa"),
                    projected("b", None, 100, "aa"),
                ],
                "aa",
                &["a"],
            ),
            |_| Some(node(1)),
            me(),
        )
        .unwrap();
    assert_eq!(applied.reconciled, 1, "the retired row went: {applied:?}");
    assert_eq!(store.outbox_len().unwrap(), 0);
}

/// **A closed snapshot retires what it does not name, and speaks only for
/// its own author.** `stale` is the tombstone case: this node never
/// received the delete, and the seal that retired it means it never will —
/// so the actor's own live set is the only thing left that can say the row
/// is gone (ARCH §7.1). The two controls in the same call are the whole
/// rule: `kept` is named and stays, and `bb` never sealed, so nothing of
/// `bb`'s is touched even though it is in the same namespace.
///
/// Watched RED by dropping the reconciliation loop: `stale` survives.
#[test]
fn apply_projection_retires_a_sealed_actors_rows_and_only_that_actors() {
    let store = MeshStore::in_memory().unwrap();
    let plant = |key: &str, origin: NodeId| {
        store
            .merge_entry(StoreEntry {
                app_id: "app".into(),
                key: key.to_string(),
                value: Bytes::from("held"),
                timestamp: 100,
                origin,
            })
            .unwrap()
    };
    plant("kept", node(7));
    plant("stale", node(7));
    plant("bb-row", node(8));

    let applied = store
        .apply_projection(
            "app",
            &sealed(
                vec![projected("kept", Some(b"v"), 200, "aa")],
                "aa",
                &["kept"],
            ),
            |actor| match actor {
                "aa" => Some(node(7)),
                "bb" => Some(node(8)),
                _ => None,
            },
            me(),
        )
        .unwrap();

    assert_eq!(applied.reconciled, 1, "{applied:?}");
    assert_eq!(applied.deleted, 0, "no tombstone was in the projection");
    assert!(
        store.get("app", "stale").unwrap().is_none(),
        "a row the actor's whole live set does not name is retired"
    );
    assert_eq!(
        store.get("app", "kept").unwrap().unwrap().value.as_ref(),
        b"v",
        "a named key keeps the value the projection gave it"
    );
    assert!(
        store.get("app", "bb-row").unwrap().is_some(),
        "bb has not sealed, so bb's rows are nobody's to retire"
    );

    // The control on the claim itself: the same rows, no live set, and
    // `stale` would have survived.
    plant("stale", node(7));
    let applied = store
        .apply_projection(
            "app",
            &unsealed(vec![projected("kept", Some(b"v"), 200, "aa")]),
            |_| Some(node(7)),
            me(),
        )
        .unwrap();
    assert_eq!(applied.reconciled, 0);
    assert!(store.get("app", "stale").unwrap().is_some());
}

/// **This node's own rows are never reconciled, because for them the store
/// is upstream of the journal.** A local `set` writes the row and QUEUES
/// the act; until the pump drains it the journal has never heard of the
/// key, so our own snapshot — folded from the journal — cannot name it.
/// Reconciling self would read the outbox lag as a retirement and delete a
/// write this node just made.
///
/// Watched RED by removing the `origin == self_id` skip: the second half
/// of this test is that run, and `local` goes.
#[test]
fn apply_projection_never_reconciles_this_nodes_own_rows() {
    let store = MeshStore::in_memory().unwrap();
    assert!(store
        .set("app", "local", Bytes::from_static(b"just-written"), me())
        .unwrap());
    assert_eq!(
        store.outbox_len().unwrap(),
        1,
        "still on its way to the rail"
    );

    // Our own actor, our own seal, folded before the write reached it.
    let ours = sealed(vec![], "self", &[]);
    let applied = store
        .apply_projection("app", &ours, |_| Some(me()), me())
        .unwrap();
    assert_eq!(applied.reconciled, 0, "{applied:?}");
    assert!(
        store.get("app", "local").unwrap().is_some(),
        "a write still in the outbox is not a retired row"
    );

    // The control: the identical claim about somebody ELSE's node id takes
    // it, which is what the skip is holding back.
    let applied = store
        .apply_projection("app", &ours, |_| Some(me()), node(1))
        .unwrap();
    assert_eq!(applied.reconciled, 1, "{applied:?}");
    assert!(store.get("app", "local").unwrap().is_none());
}

/// An actor the roster cannot place has no rows to match: its live set
/// names nothing this store holds under that name, and inventing an origin
/// to match against would retire another node's rows (ARCH §18.3).
#[test]
fn a_live_set_from_an_unplaceable_actor_retires_nothing() {
    let store = MeshStore::in_memory().unwrap();
    store
        .merge_entry(StoreEntry {
            app_id: "app".into(),
            key: "k".into(),
            value: Bytes::from("v"),
            timestamp: 100,
            origin: node(7),
        })
        .unwrap();
    let applied = store
        .apply_projection("app", &sealed(vec![], "zz", &[]), |_| None, me())
        .unwrap();
    assert_eq!(applied.reconciled, 0);
    assert!(store.get("app", "k").unwrap().is_some());
}

// ── The retention floor: the projection's half of the window ──

/// **The bug this closes.** The store is a projection, so a row a sweep
/// deleted has no incumbent and the next fold re-inserts it. Both halves
/// are here because either alone leaves the loop open: the fold must not
/// (re-)take a row past the window (`withheld`), and it must take out one
/// that is already here (`expired`).
///
/// Watched RED by deleting the `floor.is_some_and(..)` arm from the row
/// loop — `withheld` is 0 and the swept row is back — and again by deleting
/// the `delete_older_than_in_app` block, which leaves `expired` 0 and the
/// aged-in-place row in the store.
#[test]
fn the_projection_neither_takes_nor_keeps_a_row_past_the_retention_window() {
    const LEDGER: &str = crate::CONTRIBUTIONS_APP_ID;
    let store = MeshStore::in_memory().unwrap();
    let now = now_secs();
    let floor = crate::retention::floor_at(LEDGER, now).expect("the ledger declares a window");
    // A day either side of the boundary, so no clock tick between this
    // read and the one inside `apply_projection` can move a row across it.
    let (expired_t, fresh_t) = (floor - 86_400, now - 60);

    // The row is ALREADY HERE — planted through `merge_entry`, which is
    // how a peer's fold put it here before it aged out.
    store
        .merge_entry(StoreEntry {
            app_id: LEDGER.into(),
            key: "aged-in-place".into(),
            value: Bytes::from("v"),
            timestamp: expired_t,
            origin: node(1),
        })
        .unwrap();

    let rows = unsealed(vec![
        projected("from-the-journal", Some(b"v"), expired_t, "aa"),
        projected("in-window", Some(b"v"), fresh_t, "aa"),
    ]);
    let applied = store
        .apply_projection(LEDGER, &rows, |_| Some(node(1)), me())
        .unwrap();

    assert_eq!(
        applied.withheld, 1,
        "the fold re-took an expired row: {applied:?}"
    );
    assert_eq!(
        applied.expired, 1,
        "an aged-in-place row survived: {applied:?}"
    );
    assert_eq!(
        applied.merged, 1,
        "the in-window row must still land: {applied:?}"
    );
    assert!(store.get(LEDGER, "from-the-journal").unwrap().is_none());
    assert!(store.get(LEDGER, "aged-in-place").unwrap().is_none());
    assert!(
        store.get(LEDGER, "in-window").unwrap().is_some(),
        "the window took only what is past it"
    );
}

/// The control the test above needs: a namespace that declares NO window
/// is not swept for age at all. Without it, `expired`/`withheld` firing on
/// every namespace would look identical from inside the ledger's test — and
/// `work-atlas` claims, `processed_shards` markers and
/// `corpus-engine/handoff:*` records are all deliberately never rewritten.
#[test]
fn a_namespace_with_no_declared_window_is_never_swept_for_age() {
    const UNDECLARED: &str = "processed_shards";
    assert_eq!(crate::retention::window_days(UNDECLARED), None);
    let store = MeshStore::in_memory().unwrap();
    store
        .merge_entry(StoreEntry {
            app_id: UNDECLARED.into(),
            key: "corpus:shard-0".into(),
            value: Bytes::from("v"),
            timestamp: 100,
            origin: node(1),
        })
        .unwrap();
    let rows = unsealed(vec![projected("ancient", Some(b"v"), 100, "aa")]);
    let applied = store
        .apply_projection(UNDECLARED, &rows, |_| Some(node(1)), me())
        .unwrap();
    assert_eq!((applied.expired, applied.withheld), (0, 0), "{applied:?}");
    assert_eq!(applied.merged, 1);
    assert!(store.get(UNDECLARED, "corpus:shard-0").unwrap().is_some());
    assert!(store.get(UNDECLARED, "ancient").unwrap().is_some());
}

/// The sweep and the fold agree ROW FOR ROW, because they read one window
/// (ARCH §10.6). Asserted behaviourally: the same three rows put through
/// `RetentionGc::sweep` and through `apply_projection` leave the same two
/// survivors. Two cutoffs that differ by an hour would pass a check on the
/// constants and fail here on the row between them.
#[test]
fn the_sweep_and_the_fold_keep_exactly_the_same_rows() {
    const LEDGER: &str = crate::CONTRIBUTIONS_APP_ID;
    let now = now_secs();
    let floor = crate::retention::floor_at(LEDGER, now).unwrap();
    let ages = [
        ("old", floor - 86_400),
        ("edge", floor + 60),
        ("new", now - 60),
    ];

    let plant = || {
        let store = std::sync::Arc::new(MeshStore::in_memory().unwrap());
        for (key, t) in ages {
            store
                .merge_entry(StoreEntry {
                    app_id: LEDGER.into(),
                    key: key.into(),
                    value: Bytes::from("v"),
                    timestamp: t,
                    origin: node(1),
                })
                .unwrap();
        }
        store
    };
    let survivors = |store: &MeshStore| -> Vec<String> {
        let mut k: Vec<String> = store
            .scan(LEDGER, "")
            .unwrap()
            .into_iter()
            .map(|e| e.key)
            .collect();
        k.sort();
        k
    };

    let swept = plant();
    crate::RetentionGc::for_namespace(
        std::sync::Arc::clone(&swept),
        LEDGER,
        std::time::Duration::from_secs(3_600),
    )
    .expect("the ledger declares a window")
    .sweep()
    .unwrap();

    let folded = plant();
    folded
        .apply_projection(LEDGER, &unsealed(vec![]), |_| Some(node(1)), me())
        .unwrap();

    assert_eq!(survivors(&swept), vec!["edge", "new"], "the sweep");
    assert_eq!(survivors(&folded), survivors(&swept), "the fold disagrees");
}

/// **Why the work atlas's eviction is safe and a sweep is not.**
///
/// `WorkAtlasGc` drops an expired claim through `MeshStore::delete` (via
/// `MeshReplicatedKv`), which queues a TOMBSTONE — so the fold carries the
/// removal instead of undoing it. The same row taken by `gc_app_before`
/// queues nothing, and on a namespace that declares no retention window
/// there is no floor either, so the next fold puts it straight back.
///
/// Both halves in one test, because the difference between them IS the
/// finding: on a projection, a delete is an act and a sweep is a wish.
/// A namespace that needs rows dropped for age declares a window
/// (`crate::retention`); one that needs a specific row dropped calls
/// `delete`. There is no third way, and the second assertion is what says
/// so out loud.
#[test]
fn a_delete_survives_the_fold_and_an_undeclared_sweep_does_not() {
    const ATLAS: &str = "work-atlas";
    assert_eq!(
        crate::retention::window_days(ATLAS),
        None,
        "the atlas evicts by its own per-record deadline, not by row age"
    );
    let store = MeshStore::in_memory().unwrap();
    let now = now_secs();
    let claim = |k: &str| projected(k, Some(b"claim"), now - 10, "aa");

    store
        .apply_projection(
            ATLAS,
            &unsealed(vec![claim("evicted"), claim("swept")]),
            |_| Some(node(1)),
            me(),
        )
        .unwrap();

    // The atlas's door: a delete queues a tombstone.
    assert!(store.delete(ATLAS, "evicted").unwrap());
    let queued = store.outbox_take(8).unwrap();
    assert_eq!(queued.len(), 1, "{queued:?}");
    assert!(queued[0].op.value.is_none(), "a delete queues a tombstone");

    // The other door: a sweep queues nothing at all.
    assert_eq!(store.gc_app_before(ATLAS, now).unwrap(), 1);
    assert_eq!(
        store.outbox_take(8).unwrap().len(),
        1,
        "a sweep is not an act — the outbox still holds only the tombstone"
    );

    // The next fold. The tombstone is on the journal by now; the sweep is
    // on no journal anywhere, so the row it took is still a winning row.
    let next = unsealed(vec![
        projected("evicted", None, queued[0].op.t, "aa"),
        claim("swept"),
    ]);
    store
        .apply_projection(ATLAS, &next, |_| Some(node(1)), me())
        .unwrap();
    assert!(
        store.get(ATLAS, "evicted").unwrap().is_none(),
        "a tombstone the fold carries keeps the row gone"
    );
    assert!(
        store.get(ATLAS, "swept").unwrap().is_some(),
        "and a sweep with neither a tombstone nor a declared window is \
         undone by the next round — which is why there is no third way"
    );
}
