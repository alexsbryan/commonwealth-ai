// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stage one membership act as a key the roster FILE does not carry — the
//! write path ra-6's invite redemption will eventually own, exercised now
//! for the demo that needs it (`scripts/ring-membership-demo.sh`, demo 5).
//!
//! WHY STAGING IS NEEDED, named rather than hidden: every append door
//! refuses a signer the roster file does not name (`RingJournal::append`'s
//! `NotInRoster`), while the fold admits keys standing through `Admit`
//! acts — the amendment's "any admitted member may write either"
//! (`RING_APPLICATIONS.md` "Amendment 2026-09-18"). Until the join surface
//! exists, the only product path such an op can ride is
//! [`commonwealth_rail::RingJournal::ingest`] — the same entry sync uses for
//! peer ops — over bytes signed by the crate's own signer against the
//! crate's own digest. No second signing rule, no second view fold.
//!
//! Concurrency, honestly: this is a second process touching a journal the
//! daemon also holds open. The `seq` warning that keeps CLI callers on the
//! HTTP door (`sovereign-cli-shared/src/rail.rs`) is about the DOOR's per-
//! actor counter — a staged actor's acts are never handed out by any door,
//! so there is no counter to race. The demo runs this while the node is
//! quiescent between synced phases; `ingest`'s id dedupe makes a re-run a
//! no-op rather than a duplicate.
//!
//! Deterministic keys on purpose: the caller names a 64-hex seed, so a
//! rerun of the demo reproduces the same actor and the same op ids.

use commonwealth_rail::{
    actor_of, body_json, sign_ring_op, Ed25519Verifier, Op, Person, RailAct, RingJournal, SignedOp,
    SigningKey,
};

fn key_from(seed_hex: &str) -> Result<SigningKey, String> {
    let bytes = hex_to_32(seed_hex)?;
    Ok(SigningKey::from_bytes(&bytes))
}

fn hex_to_32(s: &str) -> Result<[u8; 32], String> {
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("a seed is 64 hex chars, got {s:?}"));
    }
    let mut out = [0u8; 32];
    for (i, cell) in out.iter_mut().enumerate() {
        *cell = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

fn usage() -> ! {
    eprintln!(
        "stage_member_act — one membership act, signed by a key standing through Admit\n\
         \n\
         \x20 stage_member_act --print-actor <seed-64hex>\n\
         \x20 stage_member_act --root <daemon-data-dir> --namespace <ns> --seed <64hex> \\\n\
         \x20                    --person <name> --admit-key <64hex>\n\
         \n\
         \x20 --print-actor   print the actor hex a seed signs as (derive FK with it too)\n\
         \x20 --admit-key     the key being admitted, as actor hex"
    );
    std::process::exit(2);
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut print_actor, mut root, mut ns, mut seed, mut person, mut admit_key) =
        (None, None, None, None, None, None);
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next().unwrap_or_else(|| {
                eprintln!("stage_member_act: {flag} needs a value");
                usage();
            })
        };
        match flag.as_str() {
            "--print-actor" => print_actor = Some(value()),
            "--root" => root = Some(value()),
            "--namespace" => ns = Some(value()),
            "--seed" => seed = Some(value()),
            "--person" => person = Some(value()),
            "--admit-key" => admit_key = Some(value()),
            _ => usage(),
        }
    }

    // `--print-actor <seed>` IS the seed for that mode: print and go.
    if let Some(seed_hex) = print_actor {
        let signer = key_from(&seed_hex).unwrap_or_else(|e| {
            eprintln!("stage_member_act: {e}");
            std::process::exit(2);
        });
        println!("{}", actor_of(&signer));
        return;
    }

    let seed_hex = seed.unwrap_or_else(|| usage());
    let signer = key_from(&seed_hex).unwrap_or_else(|e| {
        eprintln!("stage_member_act: {e}");
        std::process::exit(2);
    });

    let (Some(root), Some(ns), Some(person), Some(admit_key)) = (root, ns, person, admit_key)
    else {
        usage();
    };
    if !admit_key.bytes().all(|b| b.is_ascii_hexdigit()) || admit_key.len() != 64 {
        eprintln!("stage_member_act: --admit-key is actor hex (64 chars), got {admit_key:?}");
        std::process::exit(2);
    }

    let journal = RingJournal::open(std::path::Path::new(&root), &ns)
        .unwrap_or_else(|e| panic!("open {root}/rings/{ns}: {e}"));
    // The view the door would stamp: this journal's digest over everything
    // authentic it holds — computed by the crate's own fold, never here.
    let view = journal
        .digest(&Ed25519Verifier)
        .unwrap_or_else(|e| panic!("digest: {e}"));
    let (existing, _) = journal.read().unwrap_or_else(|e| panic!("read: {e}"));
    let seq = existing
        .iter()
        .filter(|o| o.actor == actor_of(&signer))
        .map(|o| o.kind.seq)
        .max()
        .map_or(0, |m| m + 1);
    let ts_unix = {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the wall clock reads after the epoch")
            .as_secs() as i64;
        // Strictly AFTER everything held: the rail's order breaks a ts tie
        // by actor hex, so a same-second write can sort BEFORE the Admit
        // that grants its signer standing — a member who has seen the
        // journal writes later than it (measured in demo 5's sitting,
        // 2026-09-30: sam's key hex sorted under ada's and the act never
        // counted).
        let held_max = existing.iter().map(|o| o.ts_unix).max().unwrap_or(i64::MIN);
        now.max(held_max + 1)
    };
    let act = RailAct::Admit {
        person: Person::from(person.as_str()),
        key: admit_key.clone(),
    };
    let body = body_json(&act, None, Some(&view));
    let sig = sign_ring_op(&signer, &ns, ts_unix, seq, &body);
    let op = Op::new(
        SignedOp {
            seq,
            sig,
            act,
            on_behalf_of: None,
            view: Some(view),
        },
        ts_unix,
        actor_of(&signer),
    );
    let id = op.id.clone();
    let was_new = journal
        .ingest(&op)
        .unwrap_or_else(|e| panic!("ingest: {e}"));
    println!(
        "{}\t{}\t{}",
        id,
        actor_of(&signer),
        if was_new { "staged" } else { "already-held" }
    );
}
