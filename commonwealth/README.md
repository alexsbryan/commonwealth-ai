# Commonwealth

A substrate for software that a small group of people runs together on their
own machines.

Not a cluster and not a cloud: a mesh here is your household, your band, your
research group — a dozen machines belonging to people who already trust each
other, with no server in the middle and no account anywhere. Commonwealth is
the layer under that. It answers who is in the group, how to reach them, how
two machines reconcile when they disagree, and how a shared record of what
happened stays honest.

It is six crates and no binary. You build a peer against it; you do not run it.
The one shipped consumer is [Sovereign](../sovereign/), which embeds it and
exposes it as `sovereign mesh` — if you came here wanting to pool machines with
people you trust, [Run a model bigger than your machine](../docs/RUN_A_BIGGER_MODEL.md)
is the door, not this file.

Pre-release, AGPL-3.0-or-later.

## Two halves that share no types

The package is two independent stacks. Nothing in the rail knows about a mesh,
and nothing in the mesh knows about the rail. Take one and you never compile
the other.

```
  the rail — a shared record                the mesh — a group of machines
  ──────────────────────────                ──────────────────────────────
  commonwealth-rail                         commonwealth-discovery
    the journal on disk                       founding, joining, mDNS
  commonwealth-rail-core                    commonwealth-transport
    signing, admission, one order             (peer, traffic class) -> a URL
                                            commonwealth-state
                                              gossip-replicated key/value
                                            ────────────────────────────
                                            commonwealth-core
                                              ids, roster, the merge rule
```

| Crate | What it is |
|---|---|
| [`commonwealth-core`](crates/commonwealth-core) | The vocabulary every node agrees on — ids, the roster and its merge rule, what a machine can do, what it has done. No I/O, no async, no sockets. |
| [`commonwealth-discovery`](crates/commonwealth-discovery) | How a mesh starts and how a machine gets in: join keys, mDNS, the local hardware survey. |
| [`commonwealth-transport`](crates/commonwealth-transport) | One place that answers "how do I reach that peer" — IP overlay today, dial-by-Ed25519-key behind the `iroh` feature. |
| [`commonwealth-state`](crates/commonwealth-state) | `MeshStore`: a small SQLite key/value board every node keeps a whole copy of, reconciled by gossip. |
| [`commonwealth-rail-core`](crates/commonwealth-rail-core) | The fold: signing an act, admitting it, putting every peer's acts in one order all of them agree on. Zero I/O. |
| [`commonwealth-rail`](crates/commonwealth-rail) | The journal that fold runs over — an append-only JSONL log under `<root>/rings/<namespace>/`. |

Each crate's own `lib.rs` opens with what it is for and the two or three
decisions you need before the code makes sense. Start with whichever half you
came for; `commonwealth-rail-core` is the best single read in the package.

## The shortest real program

A ring is an append-only log a group keeps together. This mints an identity,
signs one act onto a ring, and reads it back verified. Add
`commonwealth-rail`, `ed25519-dalek` and `serde_json` to a `Cargo.toml`:

```rust
use commonwealth_rail::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. An identity. The key signs; nothing else proves who wrote a line.
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let me: Person = "alex".into();
    let roster = Roster::new([(me, vec![actor_of(&key)])].into());

    // 2. A ring — one append-only journal under <root>/rings/<namespace>/.
    let dir = std::env::temp_dir().join("cw-readme-demo");
    let journal = RingJournal::open(&dir, "tools")?;
    journal.set_roster(&roster)?;

    // 3. Write an act. The payload is yours; the rail never reads into it.
    let payload = Payload::new(serde_json::json!({ "borrowed": "drill" }))?;
    journal.append(RailAct::Record { payload }, &key, &roster)?;

    // 4. Read it back. Every op here is signature-checked, roster-admitted,
    //    deduplicated and in an order every peer will agree on.
    let admitted = journal.admit(&roster, &Ed25519Verifier)?;
    println!("{} acts, complete: {}", admitted.ops.len(), admitted.is_complete());
    for op in admitted.applied() {
        println!("  {} -> {}", op.person, op.payload.as_ref().unwrap().as_value());
    }
    Ok(())
}
```

Four things that program shows, and they are the four worth carrying:

**The payload is opaque.** The rail knows delivery, authenticity and
convergence. It does not know that a drill was borrowed. Your act is your JSON
and no layer below you parses it.

**Verification is not a formality.** Change `drill` to `lathe` in
`ring_oplog.jsonl` with a text editor and re-run: the act count drops to zero,
`is_complete()` goes false, and `admitted.gaps` names a `BadSignature` and a
`TamperedId`. It does not quietly serve you the edited value, and it does not
claim a complete log when it is missing something.

**Reading is where the ordering happens.** `admit` returns acts already
deduplicated, signature-checked, roster-admitted, sequence-audited and totally
ordered. Whatever you fold over that list cannot reintroduce a divergence,
because there is no ordering decision left to make.

**A second peer catches up by difference.** `ops_missing_from(&their_digest)`
and `ingest_all` are the whole sync protocol; both peers then `admit` to the
same order. There is no leader in that exchange.

## Where to go next

- **[BOUNDARY.md](BOUNDARY.md)** — what may and may not cross into this package,
  the dependency closure of each crate, and what a green gate still does not
  prove. Read this before adding a dependency.
- **The crate docs.** `cargo doc -p commonwealth-core --no-deps --open`, or the
  `//!` block at the top of any `lib.rs`.
- **[ARCHITECTURE.md](ARCHITECTURE.md)** — the original design and its
  reasoning. Kept for the *why*; it has drifted from the code and says so at
  the top.
- **[../sovereign/SYSTEM_OVERVIEW.md](../sovereign/SYSTEM_OVERVIEW.md) §5** —
  the current shape of the running system, kept current with the code.
- **[docs/oicp-v0.4.md](docs/oicp-v0.4.md)** — the capability-advertisement
  protocol peers speak when they are doing inference for each other.

## What else is in this directory

`commonwealth/crates/` also holds `commonwealth-api`, `-inference`,
`-knowledge`, `-app` and `-test-harness`. Those are **applications built on**
the substrate, not part of it — they name the corpus engine, which puts their
dependency closures an order of magnitude above the six crates above (the
figures are tabled in [BOUNDARY.md](BOUNDARY.md)). They are useful reading for
how the substrate gets used, and they are not something you lift.

## Working on it

From the repo root, `cargo test -p commonwealth-core -p commonwealth-discovery
-p commonwealth-state -p commonwealth-transport -p commonwealth-rail -p
commonwealth-rail-core`. No hardware, no network and no model weights: the mesh
crates are pure functions and a SQLite file, and the rail is a directory of
text.

Before pushing anything that touches this package, run `cargo xtask
boundary-gate` from `corpus-engine/` — it is one of the eight blocking
pre-push ratchets and it is what keeps the package liftable.

## License

[AGPL-3.0-or-later](../LICENSE), one license across the monorepo. The
network-use clause is the part that bites here, since a mesh is a network
service.
