// SPDX-License-Identifier: AGPL-3.0-or-later
//! `cargo run --release -p commonwealth-api --example rail_read_cost`
//!
//! Prices the RAIL READ PATH — read / parse / fold, separately — against the
//! MeshStore read it would replace, and binary-searches the one-BODY byte
//! ceiling per payload shape. Measured for cw-lift order 2 rung 2a.
//!
//! Since rung 2f the one-body figure is a CHUNK size, not a convergence
//! ceiling: the exchange is budgeted at `RING_SYNC_OPS_BUDGET_BYTES` and
//! repeated, so the ceiling arm below reports both — the body that still
//! cannot be exceeded, and the chunk the loop actually sends.
//!
//! An example rather than a test because a 50k-op sweep is ~40 s and every
//! number here is a CONSTANT in N: the curve's shape is settled, and what a
//! test can usefully pin (the chunk size, and that convergence outruns it) is
//! pinned in `tests/rail_e2e.rs` §"the convergence ceiling, and the budget
//! that ended it". Precedent for the target kind:
//! `commonwealth-transport/examples/tunnel_bench.rs`.
//!
//! Clear `RUSTC_WRAPPER` when timing a rebuild — sccache is on by default on
//! this host and misreports build wall time.

use commonwealth_rail::{
    sign_ring_op, Digest, Ed25519Verifier, Op, Payload, Person, RailAct, RingJournal, RingVerifier,
    Roster, SignedOp,
};
use ed25519_dalek::SigningKey;
use std::collections::BTreeMap;
use std::hint::black_box;
use std::path::Path;
use std::time::Instant;

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn sample<F: FnMut()>(mut f: F, inner: u32) -> f64 {
    let t = Instant::now();
    for _ in 0..inner {
        f();
    }
    t.elapsed().as_nanos() as f64 / inner as f64
}

struct Stats {
    n: usize,
    min: f64,
    p50: f64,
    p90: f64,
    max: f64,
}

fn stats(mut v: Vec<f64>) -> Stats {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    let pick = |q: f64| v[((n as f64 - 1.0) * q).round() as usize];
    Stats {
        n,
        min: v[0],
        p50: pick(0.50),
        p90: pick(0.90),
        max: v[n - 1],
    }
}

fn dur(ns: f64) -> String {
    if ns >= 1_000_000.0 {
        format!("{:>9.2} ms", ns / 1e6)
    } else if ns >= 1000.0 {
        format!("{:>9.2} us", ns / 1e3)
    } else {
        format!("{:>9.1} ns", ns)
    }
}

fn report(label: &str, s: &Stats, per_op: Option<usize>) {
    let per = match per_op {
        Some(n) if n > 0 => format!("   per-op {}", dur(s.p50 / n as f64)),
        _ => String::new(),
    };
    println!(
        "{label:<46} n={:<4} min {}  p50 {}  p90 {}  max {}{per}",
        s.n,
        dur(s.min),
        dur(s.p50),
        dur(s.p90),
        dur(s.max),
    );
}

// ── fixtures ─────────────────────────────────────────────────

/// A work-atlas `ObservationRecord`, verbatim in shape
/// (sovereign-work-atlas/src/model.rs:197). This is the highest-volume
/// namespace order 2 §3 names, and the one `work_in_flight` reads.
fn atlas_observation(i: usize) -> Payload {
    Payload::new(serde_json::json!({
        "session_id": format!("aa6843f9-9481-4cd6-b26f-cee46218{:04x}", i % 0xffff),
        "file_path": "commonwealth/crates/commonwealth-rail-core/src/admit.rs",
        "source": "code_watcher_edit",
        "first_observed_at": 1_757_000_000u64,
        "last_observed_at": 1_757_000_000u64 + i as u64,
        "event_count": (i % 97) as u64 + 1,
        "symbol_refs": [],
    }))
    .expect("payload")
}

/// Order 2 §2's ceiling table is quoted against a 594-BYTE serialised body.
/// Same construction as railbench's `payload_of_size`, so the two harnesses
/// price the same op.
fn body_of_size(target: usize) -> Payload {
    // {"b":"xxxx"} inside RailAct::Record — overhead is measured, not guessed.
    let mut filler = target.saturating_sub(40);
    loop {
        let p = Payload::new(serde_json::json!({ "b": "x".repeat(filler) })).expect("payload");
        let n = commonwealth_rail::body_json(&RailAct::Record { payload: p.clone() }).len();
        if n >= target || filler > target {
            return p;
        }
        filler += target - n;
    }
}

fn order2_594(_i: usize) -> Payload {
    body_of_size(594)
}

/// The ledger fixture order 2's ceiling table is quoted against (594 B body).
fn ledger_expense(i: usize) -> Payload {
    Payload::new(serde_json::json!({
        "kind": "expense",
        "payer": "alex",
        "amount_cents": 6000 + i as i64,
        "description": "groceries and a long enough description that the \
                        serialised body lands near the 594-byte fixture the \
                        order-2 measurement priced the append door against, \
                        which is what the ceiling table in section 2 assumes",
        "participants": ["alex", "bo"],
    }))
    .expect("payload")
}

fn roster_of(actor: &str) -> Roster {
    let mut m: BTreeMap<Person, Vec<String>> = BTreeMap::new();
    m.insert(Person::from("alex"), vec![actor.to_string()]);
    Roster::new(m)
}

/// Write `n` signed ops straight to disk. One `append_all`, so seeding is
/// linear rather than quadratic.
fn seed(j: &RingJournal, k: &SigningKey, actor: &str, n: usize, mk: &dyn Fn(usize) -> Payload) {
    let mut ops = Vec::with_capacity(n);
    for seq in 0..n as u64 {
        let ts = 1_700_000_000i64 + seq as i64;
        let act = RailAct::Record {
            payload: mk(seq as usize),
        };
        let body = commonwealth_rail::body_json(&act);
        let sig = sign_ring_op(k, j.namespace(), ts, seq, &body);
        ops.push(Op::new(SignedOp { seq, sig, act }, ts, actor.to_string()));
    }
    assert_eq!(j.ingest_all(&ops).expect("seed"), n, "seed short");
}

fn journal_path(j: &RingJournal) -> std::path::PathBuf {
    j.dir().join("ring_oplog.jsonl")
}

fn main() {
    let profile = if cfg!(debug_assertions) {
        "DEBUG"
    } else {
        "RELEASE"
    };
    println!(
        "== railread :: profile={profile} arch={} debug_assertions={} ==",
        std::env::consts::ARCH,
        cfg!(debug_assertions)
    );
    println!("   STORE NAMED (order 2 §5.5): the rail arms read a FILE-BACKED");
    println!("   RingJournal on disk; the MeshStore arm names each store inline.");

    let k = key(7);
    let actor = commonwealth_rail::actor_of(&k);
    let roster = roster_of(&actor);
    let verifier: &dyn RingVerifier = &Ed25519Verifier;
    let ns = "cw-lift-read";

    let root = std::env::var("RAILREAD_ROOT")
        .unwrap_or_else(|_| format!("{}/railread-tmp", std::env::var("HOME").unwrap()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    println!("   journal root: {root}\n");

    let depths: Vec<usize> = std::env::var("RAILREAD_DEPTHS")
        .ok()
        .map(|s| s.split(',').map(|d| d.trim().parse().unwrap()).collect())
        .unwrap_or_else(|| vec![1_000, 5_000, 10_000, 50_000]);

    let shapes: Vec<(&str, &dyn Fn(usize) -> Payload)> = vec![
        ("atlas-observation", &atlas_observation),
        ("ledger-expense", &ledger_expense),
        ("order2-594B-body", &order2_594),
    ];

    for (shape, mk) in &shapes {
        println!("── SHAPE: {shape} ───────────────────────────────────────────");
        for &n in &depths {
            let dir = Path::new(&root).join(format!("{shape}-{n}"));
            std::fs::create_dir_all(&dir).unwrap();
            let j = RingJournal::open(&dir, ns).expect("open");
            seed(&j, &k, &actor, n, *mk);

            let path = journal_path(&j);
            let file_bytes = std::fs::metadata(&path).unwrap().len() as usize;
            println!(
                "  N={n:<6} file {:>10} B   {:>4} B/op on disk",
                file_bytes,
                file_bytes / n
            );

            // (1) READ — bytes off disk plus line framing, nothing decoded.
            let reps = if n >= 50_000 { 10 } else { 30 };
            let s = stats(
                (0..reps)
                    .map(|_| {
                        sample(
                            || {
                                let raw = std::fs::read_to_string(&path).unwrap();
                                black_box(raw.lines().count());
                            },
                            1,
                        )
                    })
                    .collect(),
            );
            report("    (1) read      file -> lines", &s, Some(n));
            let read_p50 = s.p50;

            // (2) PARSE — serde_json per line into the wire type. Lines are
            // pre-read so this term is decode only.
            let lines: Vec<String> = std::fs::read_to_string(&path)
                .unwrap()
                .lines()
                .map(|l| l.to_string())
                .collect();
            let s = stats(
                (0..reps)
                    .map(|_| {
                        sample(
                            || {
                                let ops: Vec<Op<SignedOp>> = lines
                                    .iter()
                                    .map(|l| serde_json::from_str(l).unwrap())
                                    .collect();
                                black_box(ops.len());
                            },
                            1,
                        )
                    })
                    .collect(),
            );
            report("    (2) parse     lines -> Op<SignedOp>", &s, Some(n));
            let parse_p50 = s.p50;

            // (1+2) the production read: oplog::read_all_with_skips
            // interleaves them, so this is the honest sum and the two terms
            // above are the attribution.
            let s = stats(
                (0..reps)
                    .map(|_| {
                        sample(
                            || {
                                black_box(j.read().unwrap());
                            },
                            1,
                        )
                    })
                    .collect(),
            );
            report("    (1+2) journal.read()  PRODUCTION", &s, Some(n));
            let readparse_p50 = s.p50;

            // (3) FOLD — admit() over already-parsed ops: Ed25519 verify,
            // id re-derivation, roster lookup, sequence audit, void set.
            let (ops, skips) = j.read().unwrap();
            let s = stats(
                (0..reps.min(10))
                    .map(|_| {
                        sample(
                            || {
                                black_box(commonwealth_rail::admit(
                                    &ops, &skips, &roster, ns, verifier,
                                ));
                            },
                            1,
                        )
                    })
                    .collect(),
            );
            report("    (3) fold      admit()", &s, Some(n));
            let fold_p50 = s.p50;

            // The whole read path a consumer pays: journal.admit().
            let s = stats(
                (0..reps.min(10))
                    .map(|_| {
                        sample(
                            || {
                                black_box(j.admit(&roster, verifier).unwrap());
                            },
                            1,
                        )
                    })
                    .collect(),
            );
            report("    (1+2+3) journal.admit() TOTAL", &s, Some(n));
            let total_p50 = s.p50;

            let share = |x: f64| 100.0 * x / total_p50;
            println!(
                "    SPLIT at N={n}: read {:.1}%  parse {:.1}%  fold {:.1}%   \
                 (read+parse measured together {:.1}%)",
                share(read_p50),
                share(parse_p50),
                share(fold_p50),
                share(readparse_p50),
            );

            // (4) digest + ops_missing_from — the replication read.
            let s = stats(
                (0..reps.min(10))
                    .map(|_| {
                        sample(
                            || {
                                black_box(j.digest().unwrap());
                            },
                            1,
                        )
                    })
                    .collect(),
            );
            report("    (4) journal.digest()", &s, Some(n));

            // ONE-EXCHANGE PAYLOAD BYTES: what a fresh peer (empty digest)
            // is sent, or must push. Envelope matches RingSyncRequest /
            // RingSyncResponse field-for-field.
            let missing = j.ops_missing_from(&Digest::new()).unwrap();
            assert_eq!(missing.len(), n, "an empty digest must ask for all of it");
            let req = serde_json::json!({
                "namespace": ns,
                "digest": j.digest().unwrap(),
                "ops": missing,
            });
            let resp = serde_json::json!({
                "namespace": ns,
                "digest": j.digest().unwrap(),
                "ops": missing,
                "ingested": 0usize,
            });
            let rq = serde_json::to_vec(&req).unwrap().len();
            let rs = serde_json::to_vec(&resp).unwrap().len();
            // Derived, never re-typed: this instrument's numbers have to be
            // quoted against the limit the receiver actually enforces.
            const LIMIT: usize = commonwealth_api::server::MAX_REQUEST_BODY_BYTES;
            println!(
                "    WIRE: RingSyncRequest {rq} B ({} B/op) · Response {rs} B · \
                 {:.1}% of the 8 MiB body limit · ceiling ~{} ops",
                rq / n,
                100.0 * rq as f64 / LIMIT as f64,
                LIMIT / (rq / n).max(1),
            );
            println!();
        }
    }

    // ── the EXACT ceiling, by binary search on the real wire body ──
    println!("── exact ceiling per fixture (real RingSyncRequest bytes) ───");
    const LIMIT: usize = commonwealth_api::server::MAX_REQUEST_BODY_BYTES;
    const BUDGET: usize = commonwealth_api::routes_internal::RING_SYNC_OPS_BUDGET_BYTES;
    println!("   body limit {LIMIT} B · exchange budget {BUDGET} B (rung 2f)");
    for (label, mk) in &shapes {
        let dir = Path::new(&root).join(format!("ceiling-{label}"));
        std::fs::create_dir_all(&dir).unwrap();
        let j = RingJournal::open(&dir, ns).expect("open");
        // Seed generously past the ceiling once, then take prefixes.
        let cap = 20_000usize;
        seed(&j, &k, &actor, cap, *mk);
        let all = j.ops_missing_from(&Digest::new()).unwrap();
        let dg = j.digest().unwrap();
        let body_bytes = |n: usize| {
            serde_json::to_vec(&serde_json::json!({
                "namespace": ns, "digest": dg, "ops": &all[..n],
            }))
            .unwrap()
            .len()
        };
        let (mut lo, mut hi) = (1usize, cap);
        while lo < hi {
            let mid = (lo + hi + 1) / 2;
            if body_bytes(mid) <= LIMIT {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        // The same search against the BUDGET: what one chunk carries.
        let (mut blo, mut bhi) = (1usize, cap);
        while blo < bhi {
            let mid = (blo + bhi + 1) / 2;
            if body_bytes(mid) <= BUDGET {
                blo = mid;
            } else {
                bhi = mid - 1;
            }
        }
        println!(
            "  {label:<20} last N that FITS = {lo:>6}  ({} B)   first that does NOT = {} ({} B)",
            body_bytes(lo),
            lo + 1,
            body_bytes(lo + 1)
        );
        println!(
            "  {:<20} one CHUNK = {blo:>6} ops ({} B) — and the exchange repeats, so \
             convergence is not bounded by either figure",
            "",
            body_bytes(blo),
        );
    }
    println!();

    // ── the denominator: what a MeshStore read costs today ──────
    println!("── MeshStore read (the bound the rail replaces) ─────────────");
    println!("   STORE: MeshStore::in_memory() — the daemon's atlas store");
    println!("   (bootstrap.rs:2460). The CLI's .sovereign/mesh.db is a");
    println!("   DIFFERENT store and is measured separately below.");
    {
        use bytes::Bytes;
        use commonwealth_state::MeshStore;
        let nid = commonwealth_core::NodeId::from_u128(0x1234_5678);
        let val = Bytes::from(serde_json::to_vec(atlas_observation(1).as_value()).unwrap());
        println!("   row value = {} B", val.len());

        for keys in [100usize, 1_000, 10_000, 15_534] {
            let mem = MeshStore::in_memory().unwrap();
            for i in 0..keys {
                mem.set(
                    "work-atlas",
                    &format!("observation:aa6843f9-9481-4cd6-b26f-cee462188e23:crates/f{i:06}.rs"),
                    val.clone(),
                    nid,
                )
                .unwrap();
            }
            let s = stats(
                (0..30)
                    .map(|_| {
                        sample(
                            || {
                                black_box(mem.scan("work-atlas", "observation:").unwrap());
                            },
                            1,
                        )
                    })
                    .collect(),
            );
            report(
                &format!("  scan  in-memory  distinct_keys={keys:>6}"),
                &s,
                Some(keys),
            );

            // The AMPLIFICATION arm: the same distinct keys, rewritten.
            // On the store this is an upsert (row count flat); on the rail
            // every rewrite is a new op. This is what makes writes-ever the
            // rail's denominator.
            if keys == 1_000 {
                for _round in 0..9 {
                    for i in 0..keys {
                        mem.set(
                            "work-atlas",
                            &format!(
                                "observation:aa6843f9-9481-4cd6-b26f-cee462188e23:crates/f{i:06}.rs"
                            ),
                            val.clone(),
                            nid,
                        )
                        .unwrap();
                    }
                }
                let after = mem.scan("work-atlas", "observation:").unwrap().len();
                let s = stats(
                    (0..30)
                        .map(|_| {
                            sample(
                                || {
                                    black_box(mem.scan("work-atlas", "observation:").unwrap());
                                },
                                1,
                            )
                        })
                        .collect(),
                );
                report(
                    &format!("  scan  after 10x rewrite  rows={after:>6}"),
                    &s,
                    Some(after),
                );
                println!("    10,000 writes -> {after} rows on the store; on the rail that is 10,000 ops.");
            }
        }
    }

    let _ = std::fs::remove_dir_all(&root);
}
